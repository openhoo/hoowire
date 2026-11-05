//! PostgreSQL messages repository operations.
use super::*;

impl Repository {
    pub async fn post_message(&self, input: NewMessage) -> Result<Message> {
        object(&input.attributes)?;
        bounded_string(&input.body, 1_048_576, "message body")?;
        if let Some(client) = &input.client_id {
            bounded_string(client, 512, "client id")?;
        }
        let fingerprint = Sha256::digest(
            serde_json::to_vec(&(input.body.as_str(), &input.attributes))
                .map_err(|_| Error::Internal)?,
        )
        .to_vec();
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let chat = lock_chat(&mut tx, input.sender_id, input.chat_id, false, None).await?;
        if let Some(client) = &input.client_id {
            let existing = sqlx::query(
                "SELECT * FROM messages WHERE sender_id=$1 AND chat_id=$2 AND client_id=$3",
            )
            .bind(input.sender_id)
            .bind(input.chat_id)
            .bind(client)
            .try_map(|row| {
                let fingerprint: Option<Vec<u8>> = row.try_get("request_hash")?;
                Ok((message_row(row)?, fingerprint))
            })
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
            if let Some((existing, original)) = existing {
                let same = original.as_ref().map_or_else(
                    || existing.body == input.body && existing.attributes == input.attributes,
                    |original| *original == fingerprint,
                );
                if !same {
                    return Err(Error::Conflict);
                }
                return Ok(existing);
            }
        }
        if chat.attributes.get("isClosed").and_then(Value::as_bool) == Some(true)
            || chat.attributes.get("allowSending").and_then(Value::as_bool) == Some(false)
            || chat.attributes.get("closed").and_then(Value::as_bool) == Some(true)
            || chat
                .attributes
                .pointer("/threadInfo/closed")
                .and_then(Value::as_bool)
                == Some(true)
            || chat.attributes.get("open").and_then(Value::as_bool) == Some(false)
        {
            return Err(Error::Forbidden);
        }
        let message = sqlx::query("INSERT INTO messages(id,chat_id,sender_id,client_id,body,attributes,request_hash) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
            .bind(Uuid::new_v4()).bind(input.chat_id).bind(input.sender_id).bind(input.client_id).bind(input.body).bind(input.attributes).bind(fingerprint)
            .try_map(message_row).fetch_one(&mut *tx).await.map_err(db_error)?;
        resources::attach_message_resources(
            &mut tx,
            message.sender_id,
            chat.id,
            &message.attributes,
        )
        .await?;
        assets::attach_message_assets(&mut tx, message.sender_id, chat.id, &message.attributes)
            .await?;
        locations::attach_message_location(&mut tx, &message).await?;
        let updated = bump_chat(&mut tx, chat.id, &chat.attributes, &chat.title).await?;
        let users = recipients(&mut tx, chat.id).await?;
        emit(&mut tx, &users, "message_created", &json!({"chat_id":chat.id,"thread_id":chat.id,"message_id":message.id,"version":updated.version})).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(message)
    }

    pub async fn message_for(&self, user: Uuid, id: Uuid) -> Result<Message> {
        sqlx::query("SELECT msg.* FROM messages msg JOIN chat_members m ON m.chat_id=msg.chat_id WHERE m.user_id=$1 AND msg.id=$2")
            .bind(user).bind(id).try_map(message_row).fetch_optional(&self.0).await.map_err(db_error)?.ok_or(Error::Forbidden)
    }

    pub async fn messages(
        &self,
        user: Uuid,
        chat: Option<Uuid>,
        after: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<Message>> {
        if let Some(chat) = chat {
            self.chat_for(user, chat).await?;
        }
        let limit = limit.clamp(1, MAX_PAGE);
        let rows = sqlx::query("SELECT msg.* FROM messages msg JOIN chat_members m ON m.chat_id=msg.chat_id WHERE m.user_id=$1 AND ($2::UUID IS NULL OR msg.chat_id=$2) AND ($3::TIMESTAMPTZ IS NULL OR msg.created_at > $3) ORDER BY msg.created_at,msg.id LIMIT $4")
            .bind(user).bind(chat).bind(after).bind(limit + 1).try_map(message_row).fetch_all(&self.0).await.map_err(db_error)?;
        complete_snapshot(rows, limit)
    }

    pub async fn delete_message(&self, actor: Uuid, id: Uuid) -> Result<()> {
        let known = self.message_for(actor, id).await?;
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let chat = lock_chat(&mut tx, actor, known.chat_id, false, None).await?;
        if actor != known.sender_id && !member(&mut tx, actor, chat.id).await? {
            return Err(Error::Forbidden);
        }
        let changed = sqlx::query("UPDATE messages SET body='',attributes='{}',deleted_at=clock_timestamp() WHERE id=$1 AND deleted_at IS NULL")
            .bind(id).execute(&mut *tx).await.map_err(db_error)?;
        if changed.rows_affected() != 0 {
            locations::stop_message_location(&mut tx, chat.id, id).await?;
            let users = recipients(&mut tx, chat.id).await?;
            let updated = bump_chat(&mut tx, chat.id, &chat.attributes, &chat.title).await?;
            emit(&mut tx, &users, "message_deleted", &json!({"chat_id":chat.id,"thread_id":chat.id,"message_id":id,"version":updated.version})).await?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn update_message(
        &self,
        actor: Uuid,
        id: Uuid,
        attributes: Value,
    ) -> Result<Message> {
        object(&attributes)?;
        let known = self.message_for(actor, id).await?;
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let chat = lock_chat(&mut tx, actor, known.chat_id, false, None).await?;
        if actor != known.sender_id && !member(&mut tx, actor, chat.id).await? {
            return Err(Error::Forbidden);
        }
        let current = sqlx::query("SELECT * FROM messages WHERE id=$1 AND deleted_at IS NULL")
            .bind(id)
            .try_map(message_row)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let mut merged = current.attributes;
        merge(&mut merged, attributes);
        object(&merged)?;
        let message = sqlx::query(
            "UPDATE messages SET attributes=$2 WHERE id=$1 AND deleted_at IS NULL RETURNING *",
        )
        .bind(id)
        .bind(merged)
        .try_map(message_row)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let users = recipients(&mut tx, chat.id).await?;
        emit(
            &mut tx,
            &users,
            "message_updated",
            &json!({"chat_id":chat.id,"thread_id":chat.id,"message_id":id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(message)
    }

    pub async fn react(&self, user: Uuid, id: Uuid, reaction: Option<String>) -> Result<()> {
        if let Some(ref value) = reaction {
            bounded_string(value, 128, "reaction")?;
        }
        let known = self.message_for(user, id).await?;
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let chat = lock_chat(&mut tx, user, known.chat_id, false, None).await?;
        let mut message = sqlx::query("SELECT * FROM messages WHERE id=$1 AND deleted_at IS NULL")
            .bind(id)
            .try_map(message_row)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if !message
            .attributes
            .get("reactions")
            .is_some_and(Value::is_object)
        {
            message.attributes["reactions"] = json!({});
        }
        if let Some(reaction) = reaction {
            message.attributes["reactions"][user.to_string()] = json!(reaction);
        } else {
            message.attributes["reactions"]
                .as_object_mut()
                .expect("object above")
                .remove(&user.to_string());
        }
        sqlx::query("UPDATE messages SET attributes=$2 WHERE id=$1")
            .bind(id)
            .bind(&message.attributes)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let users = recipients(&mut tx, chat.id).await?;
        emit(
            &mut tx,
            &users,
            "message_updated",
            &json!({"chat_id":chat.id,"thread_id":chat.id,"message_id":id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn read_chat(&self, user: Uuid, chat: Uuid, message: Option<Uuid>) -> Result<()> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        lock_chat(&mut tx, user, chat, false, None).await?;
        if let Some(message) = message {
            let valid: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND chat_id=$2)",
            )
            .bind(message)
            .bind(chat)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if !valid {
                return Err(Error::Invalid(
                    "read marker must reference the same chat".into(),
                ));
            }
        }
        let marker: Option<Uuid> = sqlx::query_scalar("UPDATE chat_members SET read_message_id=CASE WHEN read_message_id IS NULL OR $3::UUID IS NULL THEN COALESCE($3,read_message_id) WHEN (SELECT (created_at,id) FROM messages WHERE id=$3)>=(SELECT (created_at,id) FROM messages WHERE id=read_message_id) THEN $3 ELSE read_message_id END,read_at=clock_timestamp() WHERE chat_id=$1 AND user_id=$2 RETURNING read_message_id")
            .bind(chat).bind(user).bind(message).fetch_one(&mut *tx).await.map_err(db_error)?;
        let users = recipients(&mut tx, chat).await?;
        emit(
            &mut tx,
            &users,
            "thread_read",
            &json!({"chat_id":chat,"thread_id":chat,"user_id":user,"message_id":marker}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn readers(&self, actor: Uuid, chat: Uuid) -> Result<Vec<Value>> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        member(&mut tx, actor, chat).await?;
        let readers = sqlx::query_scalar("SELECT jsonb_build_object('user',user_id,'user_id',user_id,'ts',extract(epoch FROM read_at),'message_id',read_message_id) FROM chat_members WHERE chat_id=$1 AND read_at IS NOT NULL ORDER BY read_at,user_id")
            .bind(chat).fetch_all(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(readers)
    }
}
