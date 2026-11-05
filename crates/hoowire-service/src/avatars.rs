//! Small avatar images are encrypted before entering the durable repository.
use super::*;
use aes_gcm::aead::Payload;

pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;
const MAX_AVATAR_SIDE: u32 = 4096;
const PREFIX: &[u8; 4] = b"HWA1";

fn image_type(bytes: &[u8]) -> Result<&'static str> {
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(Error::Invalid("avatar must contain 1 byte–2 MiB".into()));
    }
    let (kind, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes.len() >= 45
        && bytes[8..16] == *b"\0\0\0\rIHDR"
        && bytes.ends_with(b"\0\0\0\0IEND\xaeB`\x82")
    {
        (
            "image/png",
            u32::from_be_bytes(
                bytes[16..20]
                    .try_into()
                    .map_err(|_| Error::Invalid("invalid PNG dimensions".into()))?,
            ),
            u32::from_be_bytes(
                bytes[20..24]
                    .try_into()
                    .map_err(|_| Error::Invalid("invalid PNG dimensions".into()))?,
            ),
        )
    } else if bytes.len() >= 14
        && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
        && bytes.ends_with(b";")
    {
        (
            "image/gif",
            u16::from_le_bytes([bytes[6], bytes[7]]) as u32,
            u16::from_le_bytes([bytes[8], bytes[9]]) as u32,
        )
    } else if bytes.starts_with(b"\xff\xd8\xff") && bytes.ends_with(b"\xff\xd9") {
        let (width, height) = jpeg_dimensions(bytes)?;
        ("image/jpeg", width, height)
    } else {
        return Err(Error::Invalid(
            "avatar must be a PNG, JPEG or GIF image".into(),
        ));
    };
    if width == 0 || height == 0 || width > MAX_AVATAR_SIDE || height > MAX_AVATAR_SIDE {
        return Err(Error::Invalid(
            "avatar dimensions must be 1–4096 pixels".into(),
        ));
    }
    Ok(kind)
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let invalid = || Error::Invalid("invalid JPEG header".into());
    let mut offset = 2;
    while offset + 3 < bytes.len() {
        if bytes[offset] != 0xff {
            return Err(invalid());
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset).ok_or_else(invalid)?;
        offset += 1;
        if marker == 0xda || marker == 0xd9 {
            return Err(invalid());
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let segment = bytes.get(offset..offset + 2).ok_or_else(invalid)?;
        let length = u16::from_be_bytes([segment[0], segment[1]]) as usize;
        if length < 2 || offset + length > bytes.len() {
            return Err(invalid());
        }
        if matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf) {
            if length < 8 {
                return Err(invalid());
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as u32;
            return Ok((width, height));
        }
        offset += length;
    }
    Err(invalid())
}

fn seal(cipher: &Aes256Gcm, aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut nonce);
    let sealed = cipher
        .encrypt((&nonce).into(), Payload { msg: bytes, aad })
        .map_err(|_| Error::Internal)?;
    Ok([PREFIX.as_slice(), &nonce, &sealed].concat())
}

fn open(cipher: &Aes256Gcm, aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 33 || !bytes.starts_with(PREFIX) {
        return Err(Error::Internal);
    }
    cipher
        .decrypt(
            bytes[4..16].into(),
            Payload {
                msg: &bytes[16..],
                aad,
            },
        )
        .map_err(|_| Error::Internal)
}

impl App {
    pub async fn set_avatar(
        &self,
        auth: &Auth,
        kind: &str,
        owner: Uuid,
        bytes: &[u8],
        mime: &str,
    ) -> Result<()> {
        let image_type = image_type(bytes)?;
        let requested = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !requested.is_empty()
            && requested != "application/octet-stream"
            && requested != image_type
        {
            return Err(Error::Invalid(
                "avatar MIME type does not match its image header".into(),
            ));
        }
        let aad = format!(
            "hoowire-avatar:v1:{}:{kind}:{owner}",
            auth.principal.tenant_id
        );
        let sealed = seal(&self.0.secrets, aad.as_bytes(), bytes)?;
        self.0
            .repository
            .set_avatar(auth.principal.user_id, kind, owner, &sealed, image_type)
            .await
    }

    pub async fn avatar(&self, auth: &Auth, kind: &str, owner: Uuid) -> Result<Vec<u8>> {
        let record = self
            .0
            .repository
            .avatar(auth.principal.user_id, kind, owner)
            .await?;
        let aad = format!(
            "hoowire-avatar:v1:{}:{kind}:{owner}",
            auth.principal.tenant_id
        );
        open(&self.0.secrets, aad.as_bytes(), &record.bytes_sealed)
    }

    pub async fn avatar_info(
        &self,
        auth: &Auth,
        kind: &str,
        owner: Uuid,
    ) -> Result<Option<hoowire_db::AvatarInfo>> {
        Ok(self
            .0
            .repository
            .avatar_infos(auth.principal.user_id, kind, &[owner])
            .await?
            .pop())
    }

    pub async fn avatar_infos(
        &self,
        auth: &Auth,
        kind: &str,
        owners: &[Uuid],
    ) -> Result<Vec<hoowire_db::AvatarInfo>> {
        self.0
            .repository
            .avatar_infos(auth.principal.user_id, kind, owners)
            .await
    }

    pub async fn remove_avatar(&self, auth: &Auth, kind: &str, owner: Uuid) -> Result<()> {
        self.0
            .repository
            .remove_avatar(auth.principal.user_id, kind, owner)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA6kAAAAASUVORK5CYII=").unwrap()
    }

    #[test]
    fn avatar_image_headers_and_bounds_are_checked() {
        assert_eq!(image_type(&png()).unwrap(), "image/png");
        let mut wide = png();
        wide[16..20].copy_from_slice(&5000u32.to_be_bytes());
        assert!(image_type(&wide).is_err());
        assert!(image_type(b"<svg>unsafe active content</svg>").is_err());
        assert!(image_type(&vec![0; MAX_AVATAR_BYTES + 1]).is_err());
        assert!(image_type(&png()[..25]).is_err());
        let gif = STANDARD
            .decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")
            .unwrap();
        assert_eq!(image_type(&gif).unwrap(), "image/gif");
    }

    #[test]
    fn avatar_sealing_authenticates_content_and_owner() {
        let cipher = Aes256Gcm::new((&[7; 32]).into());
        let data = png();
        let first = seal(&cipher, b"tenant:user:one", &data).unwrap();
        let second = seal(&cipher, b"tenant:user:one", &data).unwrap();
        assert_ne!(first, second);
        assert_ne!(first, data);
        assert_eq!(open(&cipher, b"tenant:user:one", &first).unwrap(), data);
        assert!(open(&cipher, b"tenant:user:two", &first).is_err());
        let mut tampered = first;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open(&cipher, b"tenant:user:one", &tampered).is_err());
    }
}
