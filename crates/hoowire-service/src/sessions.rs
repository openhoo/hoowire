//! Session keys are authenticated-encrypted with token and owner binding.
use super::*;
use aes_gcm::aead::Payload;

const PREFIX: &[u8; 4] = b"HWS1";
const ENVELOPE_LEN: usize = 4 + 12 + 32 + 16;

pub(super) fn token_hash(from: &str) -> [u8; 32] {
    Sha256::digest(from.as_bytes()).into()
}

fn associated_data(token_hash: &[u8; 32], principal: &Principal) -> Vec<u8> {
    [
        b"hoowire-session:v1\0".as_slice(),
        token_hash,
        principal.tenant_id.as_bytes(),
        principal.user_id.as_bytes(),
    ]
    .concat()
}

impl App {
    pub(super) fn seal_session_key(
        &self,
        token_hash: &[u8; 32],
        principal: &Principal,
        key: &TransportKey,
    ) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let secret = key.export_secret();
        let sealed = self
            .0
            .secrets
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: secret.as_slice(),
                    aad: &associated_data(token_hash, principal),
                },
            )
            .map_err(|_| Error::Internal)?;
        Ok([PREFIX.as_slice(), &nonce, &sealed].concat())
    }

    pub(super) fn open_session_key(
        &self,
        token_hash: &[u8; 32],
        principal: &Principal,
        envelope: &[u8],
    ) -> Result<TransportKey> {
        if envelope.len() != ENVELOPE_LEN || !envelope.starts_with(PREFIX) {
            return Err(Error::Unauthorized);
        }
        let plain = Zeroizing::new(
            self.0
                .secrets
                .decrypt(
                    envelope[4..16].into(),
                    Payload {
                        msg: &envelope[16..],
                        aad: &associated_data(token_hash, principal),
                    },
                )
                .map_err(|_| Error::Unauthorized)?,
        );
        TransportKey::from_secret(std::str::from_utf8(&plain).map_err(|_| Error::Unauthorized)?)
            .map_err(|_| Error::Unauthorized)
    }
}
