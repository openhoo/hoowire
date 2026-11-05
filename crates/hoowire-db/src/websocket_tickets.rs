//! Shared, bounded, single-use WebSocket upgrade credentials.
use super::*;

const MAX_WEBSOCKET_TICKETS: i64 = 4096;
const MAX_CREDENTIAL_BYTES: usize = 4096;

/// Only sealed credential material leaves the repository; omit Debug output.
pub struct WebsocketTicket {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub credential_sealed: Vec<u8>,
}

impl Repository {
    /// Issue one ticket with a fixed 60-second lifetime across all instances.
    pub async fn create_websocket_ticket(
        &self,
        token_hash: &[u8; 32],
        user_id: Uuid,
        tenant_id: Uuid,
        credential_sealed: &[u8],
    ) -> Result<()> {
        if credential_sealed.is_empty() || credential_sealed.len() > MAX_CREDENTIAL_BYTES {
            return Err(Error::Invalid(
                "invalid sealed WebSocket credential size".into(),
            ));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        // Serialize cleanup, capacity checks and issuance between processes.
        // This mode requires only privileges already granted to the runtime
        // role (UPDATE or DELETE); it does not require table ownership.
        sqlx::query("LOCK TABLE websocket_tickets IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM websocket_tickets WHERE expires_at<=clock_timestamp()")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM websocket_tickets")
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if pending >= MAX_WEBSOCKET_TICKETS {
            tx.commit().await.map_err(db_error)?;
            return Err(Error::Unavailable);
        }
        sqlx::query("INSERT INTO websocket_tickets(token_hash,user_id,tenant_id,credential_sealed,expires_at) VALUES($1,$2,$3,$4,clock_timestamp()+interval '60 seconds')")
            .bind(token_hash.as_slice())
            .bind(user_id)
            .bind(tenant_id)
            .bind(credential_sealed)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    /// Atomically remove a ticket. Expired rows are consumed without exposing
    /// their sealed credential to the authentication layer.
    pub async fn consume_websocket_ticket(
        &self,
        token_hash: &[u8; 32],
    ) -> Result<Option<WebsocketTicket>> {
        sqlx::query("WITH consumed AS (DELETE FROM websocket_tickets WHERE token_hash=$1 RETURNING user_id,tenant_id,credential_sealed,expires_at) SELECT user_id,tenant_id,credential_sealed FROM consumed WHERE expires_at>clock_timestamp()")
            .bind(token_hash.as_slice())
            .try_map(|row: PgRow| {
                Ok(WebsocketTicket {
                    user_id: row.try_get("user_id")?,
                    tenant_id: row.try_get("tenant_id")?,
                    credential_sealed: row.try_get("credential_sealed")?,
                })
            })
            .fetch_optional(&self.0)
            .await
            .map_err(db_error)
    }
}
