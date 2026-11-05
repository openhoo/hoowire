//! Public transport behavior, including independent legacy wire vectors.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hoowire_crypto::{CryptoError, Handshake, TransportKey};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const PEER_PUBLIC: &str = concat!(
    "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
);
const COMPRESSED_PEER_PUBLIC: &str =
    "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
const ECDH_SECRET: &str = "xgR/lEHtfW0wRUBulcB82Fx3jkuM7zyn";

#[test]
fn base64_requires_the_standard_alphabet_padding_and_canonical_pad_bits() {
    let key = TransportKey::from_secret(SECRET).unwrap();
    for encoded in [
        "=", "AAAA===", "AA A=", "AA\nA=", "AA-A", "AA_A", "Zg", "Zh==", "Zm9=", "AA==AAAA",
        "Zg==\n", "Grüße",
    ] {
        assert!(
            matches!(
                key.decrypt_base64(encoded),
                Err(CryptoError::InvalidEncoding)
            ),
            "accepted noncanonical base64 {encoded:?}"
        );
    }
}

#[test]
fn independent_python_vectors_cover_empty_and_both_sides_of_block_boundaries() {
    // CPython 3.11.15 / cryptography 48.0.0; AES-256-CBC, PKCS7(128),
    // IV bytes 0..15, key = SECRET's ASCII bytes, plaintext bytes 0..length.
    // Expectations are generated outside this crate, not by its encrypt method.
    let vectors = [
        (
            0,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRb7GfSdrqnR1cMmy4e2fc6A==",
        ),
        (
            15,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomR4IesasVvaMcEEUeGMHjQig==",
        ),
        (
            16,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRfb2IJy+yWTdpKjuBAEdBdVzCRgU8FQbFdK3yxA9c3n4=",
        ),
        (
            17,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRfb2IJy+yWTdpKjuBAEdBdfmxNBXwBFMtYo15V0Mbhvg=",
        ),
        (
            31,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRfb2IJy+yWTdpKjuBAEdBdVaSuxVn98/T4FunsXnuZOY=",
        ),
        (
            32,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRfb2IJy+yWTdpKjuBAEdBdSCukl9VW/WwnTkq4d6tAvO3H5z0hhB7kfI9mAlkq8hi",
        ),
        (
            33,
            "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRfb2IJy+yWTdpKjuBAEdBdSCukl9VW/WwnTkq4d6tAvP4MFgXsvBvBqHCuUckVACj",
        ),
    ];
    let key = TransportKey::from_secret(SECRET).unwrap();
    for (length, encoded) in vectors {
        let expected: Vec<u8> = (0..length)
            .map(|value| u8::try_from(value).unwrap())
            .collect();
        assert_eq!(
            key.decrypt_base64(encoded).unwrap(),
            expected,
            "length {length}"
        );
    }
}

#[test]
fn every_binary_and_base64_prefix_rejects_truncated_multiblock_ciphertext() {
    let key = TransportKey::from_secret(SECRET).unwrap();
    // Each full plaintext block ends in 'A', which is never legal PKCS7 padding.
    // This includes aligned truncations that pass the envelope length checks.
    let plaintext = [b'A'; 49];
    let envelope = key.encrypt(&plaintext).unwrap();
    assert_eq!(key.decrypt(&envelope).unwrap(), plaintext);
    for length in 0..envelope.len() {
        assert!(
            matches!(
                key.decrypt(&envelope[..length]),
                Err(CryptoError::InvalidEnvelope)
            ),
            "accepted binary prefix with {length} bytes"
        );
        assert!(
            matches!(
                key.decrypt_base64(&STANDARD.encode(&envelope[..length])),
                Err(CryptoError::InvalidEnvelope)
            ),
            "canonical base64 of invalid binary prefix {length}"
        );
    }
    let encoded = STANDARD.encode(&envelope);
    for length in 0..encoded.len() {
        assert!(
            key.decrypt_base64(&encoded[..length]).is_err(),
            "accepted textual prefix with {length} characters"
        );
    }
}

#[test]
fn iv_checksum_rejects_corruption_at_every_header_position() {
    let key = TransportKey::from_secret(SECRET).unwrap();
    let envelope = key.encrypt(b"header boundary fixture").unwrap();
    for position in 0..48 {
        let mut corrupted = envelope.clone();
        corrupted[position] ^= 1;
        assert!(
            matches!(key.decrypt(&corrupted), Err(CryptoError::InvalidEnvelope)),
            "header byte {position} was not checked"
        );
    }
}

#[test]
fn independent_ciphertexts_reject_zero_oversized_and_inconsistent_pkcs7_padding() {
    // Independently encrypted with Python cryptography 48.0.0 and no padder.
    // Every envelope has the correct IV checksum and a complete AES block.
    // The raw plaintexts end in 00, 11, and 01 02 respectively.
    let invalid_padding = [
        "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRpBXuaml8SjE2atNEJ8C8wQ==",
        "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRc/A7DzVOliS4SBuFFTtDNw==",
        "AAECAwQFBgcICQoLDA0OD75FyyYFvza+veaEhBoo8P1DxphQo9zl/tummSjuOomRCxlK72LF+goki8uYie+QZg==",
    ];
    let key = TransportKey::from_secret(SECRET).unwrap();
    for encoded in invalid_padding {
        assert!(matches!(
            key.decrypt_base64(encoded),
            Err(CryptoError::InvalidEnvelope)
        ));
    }
}

#[test]
fn transport_keys_reject_wrong_byte_lengths_and_non_ascii_input() {
    for length in [0, 1, 24, 31, 33, 64] {
        assert!(matches!(
            TransportKey::from_secret(&"A".repeat(length)),
            Err(CryptoError::InvalidKey)
        ));
    }
    for secret in ["é".repeat(16), format!("{}é", "A".repeat(30))] {
        assert_eq!(secret.len(), 32);
        assert!(matches!(
            TransportKey::from_secret(&secret),
            Err(CryptoError::InvalidKey)
        ));
    }
    let key = TransportKey::from_secret(SECRET).unwrap();
    assert_eq!(&*key.export_secret(), SECRET.as_bytes());
}

#[test]
fn ecdh_rejects_invalid_scalar_lengths_zero_and_values_at_or_above_the_order() {
    let order =
        hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141").unwrap();
    let mut invalid_scalars = vec![vec![0; 32], vec![0xff; 32], order.clone()];
    for length in [0, 1, 23, 33, 64] {
        invalid_scalars.push(vec![1; length]);
    }
    for scalar in invalid_scalars {
        assert!(matches!(
            TransportKey::from_ecdh(&scalar, PEER_PUBLIC),
            Err(CryptoError::InvalidPrivateKey)
        ));
    }

    // Independently checked with Python cryptography 48.0.0: 1 * (2G)
    // and (order - 1) * (2G) have the same X coordinate.
    let mut one = [0; 32];
    one[31] = 1;
    let mut order_minus_one = order;
    order_minus_one[31] -= 1;
    for scalar in [one.as_slice(), order_minus_one.as_slice()] {
        let key = TransportKey::from_ecdh(scalar, PEER_PUBLIC).unwrap();
        assert_eq!(&*key.export_secret(), ECDH_SECRET.as_bytes());
    }
}

#[test]
fn both_ecdh_entry_points_reject_malformed_hex_sec1_framing_and_off_curve_points() {
    let mut private = [0; 32];
    private[31] = 1;
    let malformed = [
        String::new(),
        PEER_PUBLIC[..128].to_owned(),
        format!("{PEER_PUBLIC}00"),
        format!(" {}", &PEER_PUBLIC[..129]),
        "é".repeat(33),
        format!("02{}", "g0".repeat(32)),
        format!("06{}", &PEER_PUBLIC[2..66]),
        format!("00{}", &PEER_PUBLIC[2..66]),
        format!("04{}", "00".repeat(64)),
        format!("02{}", "ff".repeat(32)),
        format!("04{}", "ff".repeat(64)),
    ];
    for peer in malformed {
        assert!(
            matches!(
                TransportKey::from_ecdh(&private, &peer),
                Err(CryptoError::InvalidPublicKey)
            ),
            "ECDH accepted malformed peer {peer:?}"
        );
        assert!(
            matches!(Handshake::new(&peer), Err(CryptoError::InvalidPublicKey)),
            "handshake accepted malformed peer {peer:?}"
        );
    }
}

#[test]
fn compressed_and_uncompressed_sec1_encodings_derive_the_same_independent_key() {
    let mut private = [0; 32];
    private[31] = 1;
    for peer in [
        PEER_PUBLIC.to_owned(),
        COMPRESSED_PEER_PUBLIC.to_owned(),
        PEER_PUBLIC.to_uppercase(),
    ] {
        let key = TransportKey::from_ecdh(&private, &peer).unwrap();
        assert_eq!(&*key.export_secret(), ECDH_SECRET.as_bytes());
    }
}

#[test]
fn compressed_peer_handshakes_use_fresh_keys_and_interoperate_in_both_directions() {
    let mut client_private = [0; 32];
    client_private[31] = 2;
    let (first_public, first_key) = Handshake::new(COMPRESSED_PEER_PUBLIC).unwrap();
    let (second_public, second_key) = Handshake::new(COMPRESSED_PEER_PUBLIC).unwrap();
    assert_ne!(first_public, second_public);
    assert_ne!(*first_key.export_secret(), *second_key.export_secret());

    let binary_payload: Vec<u8> = (0..=255).collect();
    for (server_public, server_key) in [(first_public, first_key), (second_public, second_key)] {
        assert_eq!(server_public.len(), 130);
        assert!(server_public.starts_with("04"));
        let client_key = TransportKey::from_ecdh(&client_private, &server_public).unwrap();
        assert_eq!(*client_key.export_secret(), *server_key.export_secret());
        let encrypted = client_key.encrypt(&binary_payload).unwrap();
        assert_eq!(server_key.decrypt(&encrypted).unwrap(), binary_payload);
        let encrypted = server_key
            .encrypt_base64(b"server to compressed peer\0")
            .unwrap();
        assert_eq!(
            client_key.decrypt_base64(&encrypted).unwrap(),
            b"server to compressed peer\0"
        );
    }
}
