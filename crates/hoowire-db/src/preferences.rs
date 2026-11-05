//! PostgreSQL preferences repository operations.
use super::*;

impl Repository {
    pub async fn patch_preferences(&self, user: Uuid, patch: Value) -> Result<()> {
        self.set_preferences(user, patch).await
    }

    pub async fn set_preference_list(
        &self,
        user: Uuid,
        key: &str,
        chat: Uuid,
        add: bool,
    ) -> Result<()> {
        if !matches!(
            key,
            "favourite_chats" | "unread_chats" | "muted_chats" | "archived_chats"
        ) {
            return Err(Error::Invalid("invalid preference list".into()));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        lock_chat(&mut tx, user, chat, false, None).await?;
        let mut preferences: Value =
            sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1 FOR UPDATE")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let mut ids = preferences
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let target = json!(chat);
        ids.retain(|id| *id != target);
        if add {
            ids.push(target);
        }
        preferences[key] = json!(ids);
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(user)
            .bind(&preferences)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        emit(&mut tx, &[user], "preferences_updated", &preferences).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn preferences(&self, user: Uuid) -> Result<Value> {
        sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&self.0)
            .await
            .map_err(db_error)
    }

    pub async fn set_preferences(&self, user: Uuid, preferences: Value) -> Result<()> {
        object(&preferences)?;
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let changed = sqlx::query("UPDATE users SET preferences=preferences || $2 WHERE id=$1")
            .bind(user)
            .bind(&preferences)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if changed.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        emit(&mut tx, &[user], "preferences_updated", &preferences).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
