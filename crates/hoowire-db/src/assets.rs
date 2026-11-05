//! Attachment references only grant access after a sender with existing access
//! posts them to a conversation. Guessed IDs never grant access to private blobs.
use super::*;

const ATTACHMENT_KEYS: &[&str] = &[
    "gallery",
    "files",
    "snd",
    "video",
    "mapOverlay",
    "map_overlay",
    "asset",
    "asset_id",
    "attachment",
    "attachments",
    "thumbnail",
];

fn references(value: &Value, depth: usize, ids: &mut BTreeSet<Uuid>) -> Result<()> {
    if depth > 24 {
        return Err(Error::Invalid(
            "attachment nesting exceeds 24 levels".into(),
        ));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                references(value, depth + 1, ids)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "id" | "aid"
                        | "asset"
                        | "asset_id"
                        | "url"
                        | "src"
                        | "thumbnail"
                        | "preview"
                        | "files"
                        | "gallery"
                ) || value.is_array()
                    || value.is_object()
                {
                    references(value, depth + 1, ids)?;
                }
            }
        }
        Value::String(value) => {
            if let Some(path) = value.strip_prefix("asset:///") {
                let id = path.split('/').next().unwrap_or("");
                ids.insert(
                    Uuid::parse_str(id)
                        .map_err(|_| Error::Invalid("invalid attachment asset URL".into()))?,
                );
            } else if let Ok(id) = Uuid::parse_str(value) {
                ids.insert(id);
            } else if value.starts_with('[') || value.starts_with('{') {
                let parsed: Value = serde_json::from_str(value)
                    .map_err(|_| Error::Invalid("invalid attachment JSON".into()))?;
                references(&parsed, depth + 1, ids)?;
            }
        }
        _ => {}
    }
    if ids.len() > 250 {
        return Err(Error::Invalid("more than 250 attachment references".into()));
    }
    Ok(())
}

pub(super) async fn attach_message_assets(
    tx: &mut Tx<'_>,
    sender: Uuid,
    chat: Uuid,
    attributes: &Value,
) -> Result<()> {
    let mut ids = BTreeSet::new();
    for key in ATTACHMENT_KEYS {
        if let Some(value) = attributes.get(*key) {
            references(value, 0, &mut ids)?;
        }
    }
    for id in ids {
        let asset = sqlx::query("SELECT * FROM assets WHERE id=$1 FOR UPDATE")
            .bind(id)
            .try_map(asset_row)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?
            .ok_or(Error::Forbidden)?;
        let accessible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM asset_shares s JOIN chat_members m ON m.chat_id=s.chat_id WHERE s.asset_id=$1 AND m.user_id=$2)")
            .bind(id).bind(sender).fetch_one(&mut **tx).await.map_err(db_error)?;
        if asset.uploaded_by != sender && !accessible {
            return Err(Error::Forbidden);
        }
        // Composite foreign keys independently enforce the conversation tenant.
        sqlx::query("INSERT INTO asset_shares(asset_id,chat_id,tenant_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(id).bind(chat).bind(asset.tenant_id).execute(&mut **tx).await.map_err(db_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wire_attachment_shapes_and_json_encoded_galleries() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let value = json!([{"url":format!("asset:///{a}"),"caption":"private"}, {"id":b,"name":"file.pdf"}]);
        let mut ids = BTreeSet::new();
        references(&json!(value.to_string()), 0, &mut ids).unwrap();
        assert_eq!(ids, BTreeSet::from([a, b]));
    }
}
