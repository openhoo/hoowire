//! Authenticated-encrypted avatar bytes with transactional owner metadata.
use super::*;

#[derive(Clone, Debug)]
pub struct AvatarInfo {
    pub owner_id: Uuid,
    pub mime_type: String,
    pub updated_at: DateTime<Utc>,
}

/// Ciphertext is intentionally absent from Debug and serializable wire models.
pub struct AvatarRecord {
    pub info: AvatarInfo,
    pub bytes_sealed: Vec<u8>,
}

fn avatar_row(row: PgRow) -> std::result::Result<AvatarRecord, sqlx::Error> {
    Ok(AvatarRecord {
        info: AvatarInfo {
            owner_id: row.try_get("owner_id")?,
            mime_type: row.try_get("mime_type")?,
            updated_at: row.try_get("updated_at")?,
        },
        bytes_sealed: row.try_get("bytes_sealed")?,
    })
}

impl Repository {
    pub async fn avatar(&self, actor: Uuid, kind: &str, owner: Uuid) -> Result<AvatarRecord> {
        let query = match kind {
            "user" => {
                "SELECT a.*,a.user_id AS owner_id FROM avatars a JOIN users actor ON actor.id=$1 AND actor.tenant_id=a.tenant_id WHERE a.user_id=$2"
            }
            "chat" => {
                "SELECT a.*,a.chat_id AS owner_id FROM avatars a JOIN chat_members cm ON cm.chat_id=a.chat_id AND cm.user_id=$1 WHERE a.chat_id=$2"
            }
            _ => return Err(Error::Invalid("invalid avatar owner type".into())),
        };
        sqlx::query(query)
            .bind(actor)
            .bind(owner)
            .try_map(avatar_row)
            .fetch_optional(&self.0)
            .await
            .map_err(db_error)?
            .ok_or(Error::NotFound)
    }

    pub async fn avatar_infos(
        &self,
        actor: Uuid,
        kind: &str,
        owners: &[Uuid],
    ) -> Result<Vec<AvatarInfo>> {
        if owners.len() > 1000 {
            return Err(Error::Invalid("too many avatar owners".into()));
        }
        let query = match kind {
            "user" => {
                "SELECT a.user_id AS owner_id,a.mime_type,a.updated_at FROM avatars a JOIN users actor ON actor.id=$1 AND actor.tenant_id=a.tenant_id WHERE a.user_id=ANY($2)"
            }
            "chat" => {
                "SELECT a.chat_id AS owner_id,a.mime_type,a.updated_at FROM avatars a JOIN chat_members cm ON cm.chat_id=a.chat_id AND cm.user_id=$1 WHERE a.chat_id=ANY($2)"
            }
            _ => return Err(Error::Invalid("invalid avatar owner type".into())),
        };
        sqlx::query(query)
            .bind(actor)
            .bind(owners)
            .try_map(|row: PgRow| {
                Ok(AvatarInfo {
                    owner_id: row.try_get("owner_id")?,
                    mime_type: row.try_get("mime_type")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .fetch_all(&self.0)
            .await
            .map_err(db_error)
    }

    pub async fn set_avatar(
        &self,
        actor: Uuid,
        kind: &str,
        owner: Uuid,
        bytes_sealed: &[u8],
        mime_type: &str,
    ) -> Result<()> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = match kind {
            "user" if actor == owner => None,
            "user" => return Err(Error::Forbidden),
            "chat" => Some(lock_chat(&mut tx, actor, owner, true, None).await?),
            _ => return Err(Error::Invalid("invalid avatar owner type".into())),
        };
        let tenant = if let Some(chat) = &current {
            chat.tenant_id
        } else {
            sqlx::query_scalar("SELECT tenant_id FROM users WHERE id=$1")
                .bind(actor)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?
        };
        let query = if kind == "user" {
            "INSERT INTO avatars(tenant_id,user_id,mime_type,bytes_sealed) VALUES($1,$2,$3,$4) ON CONFLICT(user_id) DO UPDATE SET mime_type=EXCLUDED.mime_type,bytes_sealed=EXCLUDED.bytes_sealed,updated_at=clock_timestamp() RETURNING updated_at"
        } else {
            "INSERT INTO avatars(tenant_id,chat_id,mime_type,bytes_sealed) VALUES($1,$2,$3,$4) ON CONFLICT(chat_id) DO UPDATE SET mime_type=EXCLUDED.mime_type,bytes_sealed=EXCLUDED.bytes_sealed,updated_at=clock_timestamp() RETURNING updated_at"
        };
        let changed: DateTime<Utc> = sqlx::query_scalar(query)
            .bind(tenant)
            .bind(owner)
            .bind(mime_type)
            .bind(bytes_sealed)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if let Some(chat) = current {
            let mut attributes = chat.attributes;
            attributes["hasAvatar"] = json!(true);
            attributes["avatarModifiedAt"] = json!(changed.timestamp_millis());
            let updated = bump_chat(&mut tx, owner, &attributes, &chat.title).await?;
            let users = recipients(&mut tx, owner).await?;
            emit(
                &mut tx,
                &users,
                "thread_updated",
                &json!({"chat_id":owner,"thread_id":owner,"version":updated.version}),
            )
            .await?;
        } else {
            sqlx::query("UPDATE users SET profile=profile || jsonb_build_object('has_avatar',true,'avatar_modified_at',$2::BIGINT) WHERE id=$1")
                .bind(owner).bind(changed.timestamp_millis()).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn remove_avatar(&self, actor: Uuid, kind: &str, owner: Uuid) -> Result<()> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = match kind {
            "user" if actor == owner => None,
            "user" => return Err(Error::Forbidden),
            "chat" => Some(lock_chat(&mut tx, actor, owner, true, None).await?),
            _ => return Err(Error::Invalid("invalid avatar owner type".into())),
        };
        let query = if kind == "user" {
            "DELETE FROM avatars WHERE user_id=$1"
        } else {
            "DELETE FROM avatars WHERE chat_id=$1"
        };
        if sqlx::query(query)
            .bind(owner)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected()
            > 0
        {
            if let Some(chat) = current {
                let mut attributes = chat.attributes;
                attributes["hasAvatar"] = json!(false);
                attributes["avatarModifiedAt"] = json!(0);
                let updated = bump_chat(&mut tx, owner, &attributes, &chat.title).await?;
                let users = recipients(&mut tx, owner).await?;
                emit(
                    &mut tx,
                    &users,
                    "thread_updated",
                    &json!({"chat_id":owner,"thread_id":owner,"version":updated.version}),
                )
                .await?;
            } else {
                sqlx::query("UPDATE users SET profile=profile || jsonb_build_object('has_avatar',false,'avatar_modified_at',0) WHERE id=$1")
                    .bind(owner).execute(&mut *tx).await.map_err(db_error)?;
            }
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
