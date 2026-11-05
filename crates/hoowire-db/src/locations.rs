//! Durable, conversation-encrypted live location sharing.
use super::*;

fn wire(row: &PgRow) -> Result<Value> {
    let id: Uuid = row.try_get("message_id").map_err(db_error)?;
    let last: DateTime<Utc> = row.try_get("last_active_at").map_err(db_error)?;
    let expires: DateTime<Utc> = row.try_get("expires_at").map_err(db_error)?;
    let stopped: Option<DateTime<Utc>> = row.try_get("stopped_at").map_err(db_error)?;
    let overlay: Option<String> = row.try_get("map_overlay").map_err(db_error)?;
    let active = stopped.is_none() && expires > Utc::now();
    Ok(
        json!({"id":id,"mid":id,"tid":row.try_get::<Uuid,_>("chat_id").map_err(db_error)?,
        "user":row.try_get::<Uuid,_>("user_id").map_err(db_error)?,"user_id":row.try_get::<Uuid,_>("user_id").map_err(db_error)?,"userId":row.try_get::<Uuid,_>("user_id").map_err(db_error)?,
        "location":row.try_get::<String,_>("location").map_err(db_error)?,"active":active,
        "last_active":last.timestamp_millis(),"lastActive":last.timestamp_millis(),
        "duration":row.try_get::<i64,_>("duration").map_err(db_error)?,"expires_at":expires.to_rfc3339(),
        "map_overlay":overlay,"mapOverlay":overlay}),
    )
}

fn location_string(value: &Value) -> Result<&str> {
    let location = value
        .as_str()
        .ok_or_else(|| Error::Invalid("location must contain encrypted text".into()))?;
    bounded_string(location, 65536, "encrypted location")?;
    if location.is_empty() {
        return Err(Error::Invalid("empty encrypted location".into()));
    }
    Ok(location)
}

/// Invoked after message insertion while the caller holds its chat lock. Any
/// invalid sharing reference rolls back both message and live location writes.
pub(super) async fn attach_message_location(tx: &mut Tx<'_>, message: &Message) -> Result<()> {
    let Some(value) = message
        .attributes
        .get("liveLocation")
        .or_else(|| message.attributes.get("live_location"))
    else {
        return Ok(());
    };
    let parsed = match value {
        Value::String(text) => serde_json::from_str(text)
            .map_err(|_| Error::Invalid("invalid live location JSON".into()))?,
        Value::Object(_) => value.clone(),
        Value::Null => return Ok(()),
        _ => return Err(Error::Invalid("invalid live location attachment".into())),
    };
    let duration = parsed
        .get("duration")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Invalid("missing live location duration".into()))?;
    if !(1..=2_678_400).contains(&duration) {
        return Err(Error::Invalid(
            "live location duration must be 1 second–31 days".into(),
        ));
    }
    let location = location_string(parsed.get("location").unwrap_or(&Value::Null))?;
    let overlay = message
        .attributes
        .get("mapOverlay")
        .or_else(|| message.attributes.get("map_overlay"))
        .and_then(Value::as_str);
    let tenant: Uuid = sqlx::query_scalar("SELECT tenant_id FROM chats WHERE id=$1")
        .bind(message.chat_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    let row=sqlx::query("INSERT INTO live_locations(message_id,tenant_id,chat_id,user_id,location,duration,map_overlay,created_at,expires_at,last_active_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$8) RETURNING *")
        .bind(message.id).bind(tenant).bind(message.chat_id).bind(message.sender_id).bind(location).bind(duration).bind(overlay)
        .bind(message.created_at).bind(message.created_at+chrono::Duration::seconds(duration)).fetch_one(&mut **tx).await.map_err(db_error)?;
    let users = recipients(tx, message.chat_id).await?;
    emit(tx, &users, "live_location", &wire(&row)?).await?;
    Ok(())
}

/// The caller already holds the conversation lock. Stops remain in the outbox
/// so connected viewers can remove a departed user's sharing immediately.
pub(super) async fn stop_member_locations(
    tx: &mut Tx<'_>,
    actor: Uuid,
    chat: Uuid,
) -> Result<Vec<Value>> {
    let rows=sqlx::query("UPDATE live_locations SET stopped_at=clock_timestamp(),last_active_at=clock_timestamp() WHERE chat_id=$1 AND user_id=$2 AND stopped_at IS NULL RETURNING *")
        .bind(chat).bind(actor).fetch_all(&mut **tx).await.map_err(db_error)?;
    let users = recipients(tx, chat).await?;
    let mut result = vec![];
    for row in rows {
        let value = wire(&row)?;
        emit(tx, &users, "live_location", &value).await?;
        result.push(value);
    }
    Ok(result)
}

pub(super) async fn stop_message_location(
    tx: &mut Tx<'_>,
    chat: Uuid,
    message: Uuid,
) -> Result<()> {
    let row=sqlx::query("UPDATE live_locations SET stopped_at=clock_timestamp(),last_active_at=clock_timestamp() WHERE chat_id=$1 AND message_id=$2 AND stopped_at IS NULL RETURNING *")
        .bind(chat).bind(message).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if let Some(row) = row {
        let users = recipients(tx, chat).await?;
        emit(tx, &users, "live_location", &wire(&row)?).await?;
    }
    Ok(())
}

impl Repository {
    pub async fn live_locations(&self, actor: Uuid, chat: Option<Uuid>) -> Result<Vec<Value>> {
        if let Some(chat) = chat {
            self.chat_for(actor, chat).await?;
        }
        let rows=sqlx::query("SELECT l.* FROM live_locations l JOIN chat_members cm ON cm.chat_id=l.chat_id AND cm.user_id=$1 JOIN messages m ON m.id=l.message_id AND m.deleted_at IS NULL WHERE ($2::UUID IS NULL OR l.chat_id=$2) AND l.stopped_at IS NULL AND l.expires_at>clock_timestamp() ORDER BY l.created_at,l.message_id LIMIT 1001")
            .bind(actor).bind(chat).fetch_all(&self.0).await.map_err(db_error)?;
        if rows.len() > 1000 {
            return Err(Error::Invalid(
                "more than 1000 active live locations; complete pagination is not implemented"
                    .into(),
            ));
        }
        rows.iter().map(wire).collect()
    }

    pub async fn live_location(&self, actor: Uuid, chat: Uuid, message: Uuid) -> Result<Value> {
        let row=sqlx::query("SELECT l.* FROM live_locations l JOIN chat_members cm ON cm.chat_id=l.chat_id AND cm.user_id=$1 JOIN messages m ON m.id=l.message_id AND m.deleted_at IS NULL WHERE l.chat_id=$2 AND l.message_id=$3")
            .bind(actor).bind(chat).bind(message).fetch_optional(&self.0).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        wire(&row)
    }

    pub async fn update_live_locations(
        &self,
        actor: Uuid,
        updates: &[(Uuid, String)],
    ) -> Result<Vec<Value>> {
        if updates.is_empty() || updates.len() > 1000 {
            return Err(Error::Invalid(
                "live location update requires 1–1000 conversations".into(),
            ));
        }
        let mut ordered = std::collections::BTreeMap::new();
        for (chat, location) in updates {
            location_string(&json!(location))?;
            if ordered.insert(*chat, location).is_some() {
                return Err(Error::Invalid(
                    "duplicate live location conversation".into(),
                ));
            }
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let mut result = vec![];
        for chat in ordered.keys() {
            lock_chat(&mut tx, actor, *chat, false, None).await?;
        }
        for (chat, location) in ordered {
            let rows=sqlx::query("UPDATE live_locations l SET location=$3,last_active_at=clock_timestamp() WHERE l.chat_id=$1 AND l.user_id=$2 AND l.stopped_at IS NULL AND l.expires_at>clock_timestamp() AND EXISTS(SELECT 1 FROM messages m WHERE m.id=l.message_id AND m.deleted_at IS NULL) RETURNING l.*")
                .bind(chat).bind(actor).bind(location).fetch_all(&mut *tx).await.map_err(db_error)?;
            if rows.is_empty() {
                return Err(Error::NotFound);
            }
            let users = recipients(&mut tx, chat).await?;
            for row in rows {
                let value = wire(&row)?;
                emit(&mut tx, &users, "live_location", &value).await?;
                result.push(value);
            }
        }
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    pub async fn stop_live_locations(&self, actor: Uuid, chat: Uuid) -> Result<Vec<Value>> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        lock_chat(&mut tx, actor, chat, false, None).await?;
        let result = stop_member_locations(&mut tx, actor, chat).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
}
