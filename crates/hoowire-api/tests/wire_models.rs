//! Public wire models must retain opaque content and protect server-owned fields.
use chrono::{DateTime, Utc};
use hoowire_api::{message_wire, user_wire};
use hoowire_domain::{Message, User};
use serde_json::json;
use uuid::Uuid;

fn message() -> Message {
    Message {
        id: Uuid::from_u128(1),
        chat_id: Uuid::from_u128(2),
        sender_id: Uuid::from_u128(3),
        client_id: Some("client-retry-id".into()),
        body: "opaque nested ciphertext".into(),
        attributes: json!({}),
        created_at: DateTime::parse_from_rfc3339("2026-11-04T12:34:56.123456Z")
            .unwrap()
            .with_timezone(&Utc),
        deleted_at: None,
    }
}

#[test]
fn message_attributes_cannot_spoof_server_identity_or_ciphertext() {
    let mut record = message();
    record.attributes = json!({
        "mid":"forged-message", "tid":"forged-chat", "from":"forged-sender",
        "m":"replacement", "ts":0, "cid":"forged-retry", "forwarded":true
    });
    let wire = message_wire(&record);
    assert_eq!(wire["mid"], record.id.to_string());
    assert_eq!(wire["tid"], record.chat_id.to_string());
    assert_eq!(wire["from"], record.sender_id.to_string());
    assert_eq!(wire["m"], record.body);
    assert_eq!(wire["cid"], "client-retry-id");
    assert!(wire["ts"].as_f64().unwrap() > 0.0);
    assert_eq!(wire["forwarded"], true);
}

#[test]
fn deleted_message_never_resurrects_ciphertext_from_extension_attributes() {
    let mut record = message();
    record.deleted_at = Some(record.created_at);
    record.attributes = json!({
        "m":"resurrected body", "deleted":false,
        "attachment":{"url":"asset:///opaque","extension":[true,null,42]}
    });
    let wire = message_wire(&record);
    assert_eq!(wire["m"], "");
    assert_eq!(wire["deleted"], true);
    assert_eq!(wire["mid"], record.id.to_string());
    assert_eq!(wire["cid"], "client-retry-id");
    assert_eq!(wire["attachment"], record.attributes["attachment"]);
    assert!(!wire.to_string().contains(&record.body));
    assert!(!wire.to_string().contains("resurrected body"));
}

fn user() -> User {
    User {
        id: Uuid::from_u128(10),
        tenant_id: Uuid::from_u128(11),
        email_hash: "PRIVATE-EMAIL-HASH-SENTINEL".into(),
        password_hash: "PRIVATE-PASSWORD-HASH-SENTINEL".into(),
        profile: json!({"first_name":"Özlem","last_name":"李","account_email":"display@example.invalid"}),
        is_bot: false,
        created_at: message().created_at,
    }
}

#[test]
fn user_wire_exposes_the_public_profile_without_authentication_hashes() {
    let record = user();
    let wire = user_wire(&record);
    assert_eq!(wire["id"], record.id.to_string());
    assert_eq!(wire["organisation"], record.tenant_id.to_string());
    assert_eq!(wire["first_name"], "Özlem");
    assert_eq!(wire["last_name"], "李");
    assert_eq!(wire["profile"]["account_email"], "display@example.invalid");
    let serialized = wire.to_string();
    assert!(!serialized.contains(&record.password_hash));
    assert!(!serialized.contains(&record.email_hash));
    assert!(wire.get("password_hash").is_none());
    assert!(wire.get("email_hash").is_none());
}
