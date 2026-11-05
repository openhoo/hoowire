//! Exercise the public immutable store against a real, disposable filesystem.
use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

use hoowire_blob::{BlobStore, MAX_ASSET_BYTES};
use hoowire_domain::Error;
use tokio::{fs, sync::Barrier};
use uuid::Uuid;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("hoowire-blob-contract-{}", Uuid::new_v4())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn bounded(future: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("filesystem scenario exceeded its 60-second deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publication_has_one_complete_winner_and_no_temporary_files() {
    bounded(async {
        let scratch = Scratch::new();
        let store = BlobStore::open(&scratch.0).await.unwrap();
        let id = Uuid::new_v4();
        let barrier = Arc::new(Barrier::new(13));
        let mut tasks = Vec::new();
        for index in 0..12 {
            let store = store.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let bytes = vec![index; 32 * 1024 + usize::from(index)];
                barrier.wait().await;
                let result = store.put(id, &bytes).await;
                (bytes, result)
            }));
        }
        barrier.wait().await;
        let mut winners = Vec::new();
        for task in tasks {
            let (bytes, result) = task.await.unwrap();
            match result {
                Ok(digest) => winners.push((bytes, digest)),
                Err(Error::Internal) => {}
                other => panic!("unexpected publication result: {other:?}"),
            }
        }
        assert_eq!(winners.len(), 1, "one writer publishes the immutable UUID");
        let (expected, digest) = winners.pop().unwrap();
        assert_eq!(store.get(id, &digest).await.unwrap(), expected);
        let mut entries = fs::read_dir(&scratch.0).await.unwrap();
        assert_eq!(
            entries.next_entry().await.unwrap().unwrap().file_name(),
            id.to_string().as_str()
        );
        assert!(
            entries.next_entry().await.unwrap().is_none(),
            "failed writers leave no .tmp files"
        );
    })
    .await;
}

#[tokio::test]
async fn empty_and_exact_limit_objects_round_trip_but_oversize_is_never_published() {
    bounded(async {
        let scratch = Scratch::new();
        let store = BlobStore::open(&scratch.0).await.unwrap();
        let empty = Uuid::new_v4();
        let digest = store.put(empty, b"").await.unwrap();
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(store.get(empty, &digest).await.unwrap().is_empty());

        let full = Uuid::new_v4();
        let bytes = (0..MAX_ASSET_BYTES)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let digest = store.put(full, &bytes).await.unwrap();
        assert_eq!(store.get(full, &digest).await.unwrap(), bytes);

        let oversized = Uuid::new_v4();
        assert!(matches!(
            store.put(oversized, &vec![0; MAX_ASSET_BYTES + 1]).await,
            Err(Error::Invalid(_))
        ));
        assert!(!scratch.0.join(oversized.to_string()).exists());
        let mut entries = fs::read_dir(&scratch.0).await.unwrap();
        let mut count = 0;
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
            count += 1;
        }
        assert_eq!(count, 2);
    })
    .await;
}

#[tokio::test]
async fn wrong_digest_corruption_and_oversized_disk_objects_are_rejected() {
    bounded(async {
        let scratch = Scratch::new();
        let store = BlobStore::open(&scratch.0).await.unwrap();
        let id = Uuid::new_v4();
        let digest = store.put(id, b"hello").await.unwrap();
        assert_eq!(
            digest,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(matches!(
            store.get(id, "invalid-digest").await,
            Err(Error::Internal)
        ));
        // Mutate the external storage fixture, then read through the public API.
        fs::write(scratch.0.join(id.to_string()), b"other")
            .await
            .unwrap();
        assert!(matches!(store.get(id, &digest).await, Err(Error::Internal)));
        let file = fs::OpenOptions::new()
            .write(true)
            .open(scratch.0.join(id.to_string()))
            .await
            .unwrap();
        file.set_len((MAX_ASSET_BYTES + 1) as u64).await.unwrap();
        assert!(matches!(store.get(id, &digest).await, Err(Error::Internal)));
        drop(file);
        store.remove(id).await.unwrap();
        store.remove(id).await.unwrap();
        assert!(matches!(store.get(id, &digest).await, Err(Error::Internal)));
    })
    .await;
}

#[tokio::test]
async fn filesystem_errors_never_report_success_or_remove_an_object_directory() {
    bounded(async {
        let scratch = Scratch::new();
        let store = BlobStore::open(&scratch.0).await.unwrap();
        let regular_file = scratch.0.join("file-instead-of-directory");
        fs::write(&regular_file, b"fixture").await.unwrap();
        assert!(matches!(
            BlobStore::open(regular_file.join("child")).await,
            Err(Error::Internal)
        ));
        let id = Uuid::new_v4();
        let object_directory = scratch.0.join(id.to_string());
        fs::create_dir(&object_directory).await.unwrap();
        assert!(matches!(
            store.put(id, b"must-not-replace-a-directory").await,
            Err(Error::Internal)
        ));
        assert!(matches!(
            store.get(id, "anything").await,
            Err(Error::Internal)
        ));
        assert!(matches!(store.remove(id).await, Err(Error::Internal)));
        assert!(object_directory.is_dir());
        let mut entries = fs::read_dir(&scratch.0).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
        }
        fs::remove_dir_all(&scratch.0).await.unwrap();
        assert!(matches!(
            store.put(Uuid::new_v4(), b"missing-root").await,
            Err(Error::Internal)
        ));
    })
    .await;
}
