//! Durable mapping from legacy timestamp ETags to per-user outbox positions.
use super::*;

impl Repository {
    pub async fn feed(
        &self,
        user: Uuid,
        token: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<(Vec<Message>, DateTime<Utc>)> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        // Event writers use the same user lock. All committed events up to this
        // high water mark are now visible; later writers wait until we commit.
        let _: i64 = sqlx::query_scalar("SELECT event_sequence FROM users WHERE id=$1 FOR UPDATE")
            .bind(user)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let stored: Option<i64> = if let Some(token) = token {
            sqlx::query_scalar("SELECT position FROM sync_cursors WHERE user_id=$1 AND token=$2 AND created_at>clock_timestamp()-interval '30 days'")
                .bind(user)
                .bind(token)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?
        } else {
            None
        };
        let after = stored.unwrap_or(0);
        let events =
            sqlx::query("SELECT * FROM events WHERE user_id=$1 AND id>$2 ORDER BY id LIMIT $3")
                .bind(user)
                .bind(after)
                .bind(limit.clamp(1, MAX_PAGE))
                .try_map(event_row)
                .fetch_all(&mut *tx)
                .await
                .map_err(db_error)?;
        if events.is_empty() && stored.is_some() {
            // Repeated idle polling must not create a row for every response.
            let existing = token.expect("a stored cursor requires a supplied token");
            tx.commit().await.map_err(db_error)?;
            return Ok((Vec::new(), existing));
        }
        let position = events.last().map_or(after, |event| event.id);
        let ids: Vec<_> = events
            .iter()
            .filter_map(|event| event.payload.get("message_id").and_then(Value::as_str))
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let messages = sqlx::query("SELECT msg.* FROM messages msg JOIN chat_members m ON m.chat_id=msg.chat_id WHERE m.user_id=$1 AND msg.id=ANY($2) ORDER BY msg.created_at,msg.id")
            .bind(user).bind(ids).try_map(message_row).fetch_all(&mut *tx).await.map_err(db_error)?;
        let next: DateTime<Utc> = sqlx::query_scalar("SELECT GREATEST(clock_timestamp(),COALESCE(max(token)+interval '1 microsecond','epoch'::timestamptz)) FROM sync_cursors WHERE user_id=$1")
            .bind(user).fetch_one(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO sync_cursors(user_id,token,position) VALUES($1,$2,$3)")
            .bind(user)
            .bind(next)
            .bind(position)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM sync_cursors WHERE user_id=$1 AND created_at<clock_timestamp()-interval '30 days'")
            .bind(user).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok((messages, next))
    }
}
