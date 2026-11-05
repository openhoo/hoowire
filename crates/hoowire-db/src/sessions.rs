//! Durable human sessions. Raw From tokens and transport keys never enter SQL.
use super::*;

/// Encrypted session material is not a wire model and has no Debug output.
pub struct AuthenticatedSession {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub is_bot: bool,
    pub key_sealed: Vec<u8>,
    pub active: bool,
}

impl Repository {
    pub async fn create_session(
        &self,
        token_hash: &[u8; 32],
        user_id: Uuid,
        tenant_id: Uuid,
        key_sealed: &[u8],
        ttl_seconds: u32,
    ) -> Result<()> {
        if ttl_seconds == 0 {
            return Err(Error::Invalid("session lifetime must be positive".into()));
        }
        sqlx::query("INSERT INTO authenticated_sessions(token_hash,user_id,tenant_id,key_sealed,expires_at) VALUES($1,$2,$3,$4,clock_timestamp()+$5::BIGINT*interval '1 second')")
            .bind(token_hash.as_slice())
            .bind(user_id)
            .bind(tenant_id)
            .bind(key_sealed)
            .bind(i64::from(ttl_seconds))
            .execute(&self.0)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Read revocation and expiry from PostgreSQL on every authentication.
    /// An inactive known token remains distinct from an unknown integration ID.
    pub async fn session(&self, token_hash: &[u8; 32]) -> Result<Option<AuthenticatedSession>> {
        sqlx::query("SELECT s.user_id,s.tenant_id,u.is_bot,s.key_sealed,(s.revoked_at IS NULL AND s.expires_at>clock_timestamp()) AS active FROM authenticated_sessions s JOIN users u ON u.id=s.user_id AND u.tenant_id=s.tenant_id WHERE s.token_hash=$1")
            .bind(token_hash.as_slice())
            .try_map(|row: PgRow| {
                Ok(AuthenticatedSession {
                    user_id: row.try_get("user_id")?,
                    tenant_id: row.try_get("tenant_id")?,
                    is_bot: row.try_get("is_bot")?,
                    key_sealed: row.try_get("key_sealed")?,
                    active: row.try_get("active")?,
                })
            })
            .fetch_optional(&self.0)
            .await
            .map_err(db_error)
    }

    /// Revocation and the cross-instance WebSocket wake-up commit together.
    pub async fn revoke_session(
        &self,
        token_hash: &[u8; 32],
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<()> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let revoked = sqlx::query("UPDATE authenticated_sessions SET revoked_at=clock_timestamp() WHERE token_hash=$1 AND user_id=$2 AND tenant_id=$3 AND revoked_at IS NULL")
            .bind(token_hash.as_slice())
            .bind(user_id)
            .bind(tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if revoked > 0 {
            sqlx::query("SELECT pg_notify('hoowire_events',$1)")
                .bind(user_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
