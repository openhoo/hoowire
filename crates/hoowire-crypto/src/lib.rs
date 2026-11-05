//! Teamwire's observed legacy session and chat encryption format.
//!
//! The wire envelope is `IV[16] || SHA256(IV)[32] || AES-256-CBC-PKCS7(data)`.
//! JSON transport base64 encodes the envelope; binary attachments do not.
//!
//! This format provides **no ciphertext authentication**. The digest is only an
//! IV checksum. It is implemented for interoperability and requires HTTPS/WSS.
//! Transport keys zeroize on drop and never appear in their `Debug` output.

use std::fmt;

use aes::Aes256;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use k256::{PublicKey, SecretKey, ecdh, elliptic_curve::sec1::ToEncodedPoint};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const IV_LEN: usize = 16;
const DIGEST_LEN: usize = 32;
const HEADER_LEN: usize = IV_LEN + DIGEST_LEN;
const BLOCK_LEN: usize = 16;

/// A failure to parse a peer key or legacy encrypted envelope.
///
/// Error strings never include secret bytes or plaintext.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("legacy transport key must contain exactly 32 ASCII bytes")]
    InvalidKey,
    #[error("invalid secp256k1 private key")]
    InvalidPrivateKey,
    #[error("invalid SEC1 secp256k1 peer public key")]
    InvalidPublicKey,
    #[error("invalid base64 envelope")]
    InvalidEncoding,
    #[error("invalid encrypted envelope")]
    InvalidEnvelope,
    #[error("operating system randomness is unavailable")]
    RandomnessUnavailable,
}

/// The crate's result type.
pub type Result<T> = std::result::Result<T, CryptoError>;

/// A legacy AES-256 transport key, cleared when its last owned copy is dropped.
#[derive(Clone)]
pub struct TransportKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for TransportKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TransportKey([REDACTED])")
    }
}

impl TransportKey {
    /// Parse the 32 ASCII bytes used directly as a legacy AES-256 key.
    ///
    /// Chat keys and ECDH keys are a 32-character base64 string. The string's
    /// ASCII bytes are the key; do not base64 decode it into 24 key bytes.
    pub fn from_secret(secret: &str) -> Result<Self> {
        if secret.len() != 32 || !secret.is_ascii() {
            return Err(CryptoError::InvalidKey);
        }
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(secret.as_bytes());
        Ok(Self(key))
    }

    /// Copy key material for immediate authenticated encryption at rest.
    /// The returned copy clears itself on drop; never log or serialize it.
    pub fn export_secret(&self) -> Zeroizing<[u8; 32]> {
        self.0.clone()
    }

    /// Derive a legacy transport key from a private scalar and SEC1 peer key.
    ///
    /// The peer key is hexadecimal. Both standard compressed and uncompressed
    /// SEC1 representations are accepted. Newly emitted keys are uncompressed.
    pub fn from_ecdh(private_key: &[u8], peer_public_key_hex: &str) -> Result<Self> {
        let private =
            SecretKey::from_slice(private_key).map_err(|_| CryptoError::InvalidPrivateKey)?;
        let peer = parse_public_key(peer_public_key_hex)?;
        let shared = ecdh::diffie_hellman(private.to_nonzero_scalar(), peer.as_affine());
        Ok(Self::from_shared_x(shared.raw_secret_bytes()))
    }

    /// Encrypt bytes with a fresh IV, returning the raw binary wire envelope.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut iv = [0; IV_LEN];
        OsRng
            .try_fill_bytes(&mut iv)
            .map_err(|_| CryptoError::RandomnessUnavailable)?;
        Ok(self.encrypt_with_iv(plaintext, iv))
    }

    /// Decrypt a raw wire envelope, checking framing, IV checksum and padding.
    ///
    /// A successful result does not authenticate ciphertext. Use the same
    /// external error response for all decryption failures to avoid exposing
    /// internal framing distinctions to unauthenticated callers.
    pub fn decrypt(&self, envelope: &[u8]) -> Result<Vec<u8>> {
        if envelope.len() < HEADER_LEN + BLOCK_LEN
            || !(envelope.len() - HEADER_LEN).is_multiple_of(BLOCK_LEN)
        {
            return Err(CryptoError::InvalidEnvelope);
        }
        let iv = &envelope[..IV_LEN];
        let checksum = Sha256::digest(iv);
        if checksum.as_slice() != &envelope[IV_LEN..HEADER_LEN] {
            return Err(CryptoError::InvalidEnvelope);
        }
        cbc::Decryptor::<Aes256>::new_from_slices(self.0.as_ref(), iv)
            .map_err(|_| CryptoError::InvalidKey)?
            .decrypt_padded_vec_mut::<Pkcs7>(&envelope[HEADER_LEN..])
            .map_err(|_| CryptoError::InvalidEnvelope)
    }

    /// Encrypt bytes and encode the envelope for JSON HTTP or WebSocket use.
    pub fn encrypt_base64(&self, plaintext: &[u8]) -> Result<String> {
        self.encrypt(plaintext)
            .map(|envelope| STANDARD.encode(envelope))
    }

    /// Decode and decrypt an envelope from JSON HTTP or a WebSocket text frame.
    pub fn decrypt_base64(&self, encoded: &str) -> Result<Vec<u8>> {
        let envelope = STANDARD
            .decode(encoded)
            .map_err(|_| CryptoError::InvalidEncoding)?;
        self.decrypt(&envelope)
    }

    fn from_shared_x(shared_x: &[u8]) -> Self {
        // Teamwire takes SEC1 bytes [1..25], which are the first 24 bytes of X.
        let mut key = Zeroizing::new([0; 32]);
        // The output length for exactly 24 bytes is always 32 bytes.
        STANDARD
            .encode_slice(&shared_x[..24], key.as_mut())
            .expect("32-byte buffer exactly fits 24-byte base64 output");
        Self(key)
    }

    fn encrypt_with_iv(&self, plaintext: &[u8], iv: [u8; IV_LEN]) -> Vec<u8> {
        let ciphertext = cbc::Encryptor::<Aes256>::new((&*self.0).into(), (&iv).into())
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        let checksum = Sha256::digest(iv);
        let mut envelope = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        envelope.extend_from_slice(&iv);
        envelope.extend_from_slice(&checksum);
        envelope.extend_from_slice(&ciphertext);
        envelope
    }
}

fn parse_public_key(encoded: &str) -> Result<PublicKey> {
    // Bound parsing before allocating memory for an attacker-controlled value.
    if !matches!(encoded.len(), 66 | 130) {
        return Err(CryptoError::InvalidPublicKey);
    }
    let bytes = hex::decode(encoded).map_err(|_| CryptoError::InvalidPublicKey)?;
    PublicKey::from_sec1_bytes(&bytes).map_err(|_| CryptoError::InvalidPublicKey)
}

/// Creates an ephemeral secp256k1 server handshake for a client public key.
pub struct Handshake;

impl Handshake {
    /// Return the server's uncompressed public key as hex and its transport key.
    ///
    /// The private scalar is destroyed before returning. Only the derived
    /// session key needs to be retained by the session registry.
    #[allow(
        clippy::new_ret_no_self,
        reason = "the server consumes a public-key/key tuple"
    )]
    pub fn new(client_public_key_hex: &str) -> Result<(String, TransportKey)> {
        let client = parse_public_key(client_public_key_hex)?;
        let private = k256::ecdh::EphemeralSecret::random(&mut OsRng);
        let public = PublicKey::from(&private);
        let shared = private.diffie_hellman(&client);
        let transport_key = TransportKey::from_shared_x(shared.raw_secret_bytes());
        let public_hex = hex::encode(public.to_encoded_point(false).as_bytes());
        Ok((public_hex, transport_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated independently with CPython 3.11 / cryptography 48.0.0:
    // SECP256K1 private scalars 1 and 2; ECDH X[:24]; base64 ASCII AES key;
    // PKCS7(128), AES-CBC, IV bytes(range(16)). No vendor code is used.
    const PEER_PUBLIC: &str = concat!(
        "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
        "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
    );
    const KEY: &str = "xgR/lEHtfW0wRUBulcB82Fx3jkuM7zyn";
    const PLAINTEXT: &str = "{\"text\":\"HooWire 👋\",\"n\":42}";
    const VECTOR: &str = concat!(
        "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomR",
        "Nza7CQl3q/BKyIauhRJlCNGsajSPeu7SLEI6nwyHsmY="
    );

    #[test]
    fn python_ecdh_and_aes_vector() {
        let mut private = [0; 32];
        private[31] = 1;
        let key = TransportKey::from_ecdh(&private, PEER_PUBLIC).unwrap();
        assert_eq!(&*key.0, KEY.as_bytes());
        let iv = std::array::from_fn(|i| i as u8);
        let encoded = STANDARD.encode(key.encrypt_with_iv(PLAINTEXT.as_bytes(), iv));
        assert_eq!(encoded, VECTOR);
        assert_eq!(key.decrypt_base64(VECTOR).unwrap(), PLAINTEXT.as_bytes());
    }

    #[test]
    fn generated_handshake_matches_client_ecdh() {
        let mut client_private = [0; 32];
        client_private[31] = 2;
        let (server_public, server_key) = Handshake::new(PEER_PUBLIC).unwrap();
        assert_eq!(server_public.len(), 130);
        assert!(server_public.starts_with("04"));
        let client_key = TransportKey::from_ecdh(&client_private, &server_public).unwrap();
        let encrypted = client_key.encrypt_base64(b"client to server").unwrap();
        assert_eq!(
            server_key.decrypt_base64(&encrypted).unwrap(),
            b"client to server"
        );
        let encrypted = server_key.encrypt_base64(b"server to client").unwrap();
        assert_eq!(
            client_key.decrypt_base64(&encrypted).unwrap(),
            b"server to client"
        );
    }

    #[test]
    fn binary_payloads_and_empty_blocks_round_trip_with_random_ivs() {
        let key = TransportKey::from_secret(KEY).unwrap();
        for plaintext in [vec![], vec![0; 16], (0..=255).collect::<Vec<u8>>()] {
            let encrypted = key.encrypt(&plaintext).unwrap();
            assert_eq!(key.decrypt(&encrypted).unwrap(), plaintext);
            assert_ne!(encrypted, key.encrypt(&plaintext).unwrap());
        }
    }

    #[test]
    fn invalid_base64_truncation_checksum_and_padding_are_rejected() {
        let key = TransportKey::from_secret(KEY).unwrap();
        assert!(key.decrypt_base64("not base64!").is_err());
        let envelope = STANDARD.decode(VECTOR).unwrap();
        for length in 0..envelope.len() {
            if length < HEADER_LEN + BLOCK_LEN || !(length - HEADER_LEN).is_multiple_of(BLOCK_LEN) {
                assert!(key.decrypt(&envelope[..length]).is_err());
            }
        }
        let mut bad_checksum = envelope.clone();
        bad_checksum[IV_LEN] ^= 1;
        assert!(key.decrypt(&bad_checksum).is_err());
        let mut bad_padding = envelope;
        // Change the previous CBC block at the final padding byte. This flips
        // the final plaintext padding byte deterministically.
        let previous_block_last = bad_padding.len() - BLOCK_LEN - 1;
        bad_padding[previous_block_last] ^= 0xff;
        assert!(key.decrypt(&bad_padding).is_err());
    }

    #[test]
    fn key_validation_and_redacted_debug() {
        assert!(TransportKey::from_secret("short").is_err());
        assert!(TransportKey::from_secret(&"é".repeat(16)).is_err());
        assert!(TransportKey::from_ecdh(&[0; 32], PEER_PUBLIC).is_err());
        assert!(Handshake::new(&"00".repeat(65)).is_err());
        assert!(Handshake::new(&"a".repeat(1_000_000)).is_err());
        let key = TransportKey::from_secret(KEY).unwrap();
        assert_eq!(format!("{key:?}"), "TransportKey([REDACTED])");
        assert!(!format!("{key:?}").contains(KEY));
    }
}
