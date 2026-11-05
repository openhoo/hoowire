//! Immutable local object adapter. PostgreSQL owns metadata and authorization.
//! Objects are addressed only by server generated UUIDs, never by client paths.
use hoowire_domain::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref()).await.map_err(io_error)?;
        Ok(Self {
            root: fs::canonicalize(root.as_ref()).await.map_err(io_error)?,
        })
    }

    pub async fn put(&self, id: Uuid, bytes: &[u8]) -> Result<String> {
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(Error::Invalid("asset exceeds 16 MiB".into()));
        }
        let path = self.path(id);
        let temp = self.root.join(format!(".{}.tmp", Uuid::new_v4()));
        let result = async {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .await?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
            drop(file);
            // Hard-link publishes without replacing an existing immutable object.
            fs::hard_link(&temp, &path).await?;
            // Persist the published directory entry before PostgreSQL may commit
            // a reference to it. File fsync alone does not persist a new name.
            #[cfg(unix)]
            fs::File::open(&self.root).await?.sync_all().await?;
            std::io::Result::Ok(())
        }
        .await;
        let _ = fs::remove_file(&temp).await;
        result.map_err(io_error)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    pub async fn get(&self, id: Uuid, expected_digest: &str) -> Result<Vec<u8>> {
        let path = self.path(id);
        let metadata = fs::metadata(&path).await.map_err(io_error)?;
        if metadata.len() > MAX_ASSET_BYTES as u64 {
            return Err(Error::Internal);
        }
        let bytes = fs::read(path).await.map_err(io_error)?;
        if hex::encode(Sha256::digest(&bytes)) != expected_digest {
            tracing::error!(asset_id = %id, "asset checksum mismatch");
            return Err(Error::Internal);
        }
        Ok(bytes)
    }

    pub async fn remove(&self, id: Uuid) -> Result<()> {
        match fs::remove_file(self.path(id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(e)),
        }
    }

    fn path(&self, id: Uuid) -> PathBuf {
        self.root.join(id.to_string())
    }
}

fn io_error(error: std::io::Error) -> Error {
    tracing::error!(kind = ?error.kind(), "asset storage failure");
    Error::Internal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn published_assets_are_immutable_and_corruption_is_detected() {
        let directory = std::env::temp_dir().join(format!("hoowire-blob-{}", Uuid::new_v4()));
        let store = BlobStore::open(&directory).await.unwrap();
        let id = Uuid::new_v4();
        let digest = store.put(id, b"original bytes").await.unwrap();
        assert!(store.put(id, b"replacement").await.is_err());
        assert_eq!(store.get(id, &digest).await.unwrap(), b"original bytes");
        fs::write(store.path(id), b"corrupt").await.unwrap();
        assert!(store.get(id, &digest).await.is_err());
        store.remove(id).await.unwrap();
        store.remove(id).await.unwrap();
        fs::remove_dir_all(directory).await.unwrap();
    }
}
