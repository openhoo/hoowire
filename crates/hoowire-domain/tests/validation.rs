//! Public validation and record boundaries; no repository or transport internals.
use hoowire_domain::{Error, Message, bounded_string, identifier};
use serde_json::{Value, json};
use uuid::Uuid;

fn message_fixture() -> Value {
    json!({
        "id": "123e4567-e89b-42d3-a456-426614174000",
        "chat_id": "223e4567-e89b-42d3-a456-426614174000",
        "sender_id": "323e4567-e89b-42d3-a456-426614174000",
        "client_id": "native/opaque:🦀\0",
        "body": "Hallo 👋\nسۆرانی\0\"quoted\"",
        "attributes": {
            "vendor_extension": {"values": [null, true, {"nested": "\0"}]},
            "large_integer": u64::MAX
        },
        "created_at": "2026-11-04T23:59:59.123456789+02:00",
        "deleted_at": "2026-11-05T01:02:03.000000004+01:00"
    })
}

#[test]
fn string_limits_count_utf8_bytes_at_empty_exact_and_one_byte_over_boundaries() {
    for (name, value, bytes) in [
        ("empty", "", 0),
        ("ASCII", "hello", 5),
        ("accent", "é", 2),
        ("emoji", "🦀", 4),
        ("combining", "e\u{301}", 3),
        ("embedded NUL", "a\0b", 3),
    ] {
        assert!(
            bounded_string(value, bytes, "message").is_ok(),
            "{name}: exact limit"
        );
        assert!(
            bounded_string(value, bytes + 1, "message").is_ok(),
            "{name}: below limit"
        );
        if bytes > 0 {
            let limit = bytes - 1;
            let result = bounded_string(value, limit, "message");
            assert!(
                matches!(result, Err(Error::Invalid(ref reason)) if reason == &format!("message exceeds {limit} bytes")),
                "{name}: one byte over limit"
            );
        }
    }
    assert!(matches!(
        bounded_string("x", 0, "name"),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn identifiers_normalize_hex_case_and_report_malformed_values_consistently() {
    let canonical = "123e4567-e89b-42d3-a456-426614174000";
    let expected = Uuid::from_u128(0x123e4567_e89b_42d3_a456_426614174000);
    for value in [
        canonical.to_owned(),
        canonical.to_uppercase(),
        canonical.replace('-', ""),
    ] {
        let parsed = identifier(&value).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), canonical);
    }
    for value in [
        "",
        " ",
        "123e4567-e89b-42d3-a456-42661417400",
        "123e4567-e89b-42d3-a456-4266141740000",
        "123e4567_e89b-42d3-a456-426614174000",
        "123e4567-e89b-42d3-a456-42661417400g",
        "123e4567-e89b-42d3-a456-42661417400é",
        "123e4567-e89b-42d3-a456-426614174000\0",
        " 123e4567-e89b-42d3-a456-426614174000",
        "123e4567-e89b-42d3-a456-426614174000\n",
    ] {
        assert!(
            matches!(identifier(value), Err(Error::Invalid(ref reason)) if reason == "expected UUID"),
            "accepted malformed UUID {value:?}"
        );
    }
}

#[test]
fn message_serialization_preserves_opaque_data_optional_fields_and_timestamp_precision() {
    let fixture = message_fixture();
    let message: Message = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(message.created_at.timestamp_subsec_nanos(), 123_456_789);
    assert_eq!(message.deleted_at.unwrap().timestamp_subsec_nanos(), 4);

    let mut expected = fixture.clone();
    expected["created_at"] = json!("2026-11-04T21:59:59.123456789Z");
    expected["deleted_at"] = json!("2026-11-05T00:02:03.000000004Z");
    assert_eq!(serde_json::to_value(&message).unwrap(), expected);

    for explicit_null in [false, true] {
        let mut optional = fixture.clone();
        if explicit_null {
            optional["client_id"] = Value::Null;
            optional["deleted_at"] = Value::Null;
        } else {
            let fields = optional.as_object_mut().unwrap();
            fields.remove("client_id");
            fields.remove("deleted_at");
        }
        let message: Message = serde_json::from_value(optional).unwrap();
        assert!(message.client_id.is_none());
        assert!(message.deleted_at.is_none());
        let serialized = serde_json::to_value(message).unwrap();
        assert_eq!(serialized["client_id"], Value::Null);
        assert_eq!(serialized["deleted_at"], Value::Null);
        assert_eq!(serialized["body"], fixture["body"]);
        assert_eq!(serialized["attributes"], fixture["attributes"]);
    }
}

#[test]
fn message_deserialization_rejects_missing_identity_invalid_timestamps_and_wrong_types() {
    for missing in [
        "id",
        "chat_id",
        "sender_id",
        "body",
        "attributes",
        "created_at",
    ] {
        let mut fixture = message_fixture();
        fixture.as_object_mut().unwrap().remove(missing);
        assert!(
            serde_json::from_value::<Message>(fixture).is_err(),
            "accepted missing required field {missing}"
        );
    }
    for (field, invalid) in [
        ("id", json!("not-a-uuid")),
        ("chat_id", json!("not-a-uuid")),
        ("sender_id", json!("not-a-uuid")),
        ("id", json!(1)),
        ("client_id", json!(1)),
        ("body", Value::Null),
        ("body", json!({"text": "must be a string"})),
        ("created_at", json!("2026-02-30T00:00:00Z")),
        ("created_at", json!("2026-11-05T00:00:00")),
        ("created_at", json!(0)),
        ("deleted_at", json!("not-a-timestamp")),
    ] {
        let mut fixture = message_fixture();
        fixture[field] = invalid;
        assert!(
            serde_json::from_value::<Message>(fixture).is_err(),
            "accepted invalid value for {field}"
        );
    }
    for invalid_record in [Value::Null, json!(false), json!([]), json!("message")] {
        assert!(serde_json::from_value::<Message>(invalid_record).is_err());
    }
}
