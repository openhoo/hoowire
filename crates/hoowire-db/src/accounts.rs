//! PostgreSQL accounts repository operations.
use super::*;

impl Repository {
    pub async fn create_tenant(&self, name: &str) -> Result<Uuid> {
        bounded_string(name, 256, "tenant name")?;
        if name.trim().is_empty() {
            return Err(Error::Invalid("tenant name is required".into()));
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants(id,name) VALUES($1,$2)")
            .bind(id)
            .bind(name.trim())
            .execute(&self.0)
            .await
            .map_err(db_error)?;
        Ok(id)
    }

    pub async fn create_user(&self, input: NewUser) -> Result<User> {
        object(&input.profile)?;
        bounded_string(&input.email_hash, 512, "email hash")?;
        if input.email_hash.is_empty() {
            return Err(Error::Invalid("email hash is required".into()));
        }
        sqlx::query("INSERT INTO users(id,tenant_id,email_hash,password_hash,profile,is_bot) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
            .bind(Uuid::new_v4()).bind(input.tenant_id).bind(input.email_hash).bind(input.password_hash)
            .bind(input.profile).bind(input.is_bot).try_map(user_row).fetch_one(&self.0).await.map_err(db_error)
    }

    pub async fn user(&self, id: Uuid) -> Result<User> {
        sqlx::query("SELECT * FROM users WHERE id=$1")
            .bind(id)
            .try_map(user_row)
            .fetch_one(&self.0)
            .await
            .map_err(db_error)
    }

    pub async fn user_by_email_hash(&self, hash: &str) -> Result<User> {
        sqlx::query("SELECT * FROM users WHERE email_hash=$1")
            .bind(hash)
            .try_map(user_row)
            .fetch_one(&self.0)
            .await
            .map_err(db_error)
    }

    pub async fn users(&self, tenant: Uuid, limit: i64) -> Result<Vec<User>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let rows = sqlx::query("SELECT * FROM users WHERE tenant_id=$1 ORDER BY id LIMIT $2")
            .bind(tenant)
            .bind(limit + 1)
            .try_map(user_row)
            .fetch_all(&self.0)
            .await
            .map_err(db_error)?;
        complete_snapshot(rows, limit)
    }

    pub async fn update_profile(&self, user: Uuid, profile: Value) -> Result<()> {
        object(&profile)?;
        let changed = sqlx::query("UPDATE users SET profile=profile || $2 WHERE id=$1")
            .bind(user)
            .bind(profile)
            .execute(&self.0)
            .await
            .map_err(db_error)?;
        if changed.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    pub async fn create_integration(&self, integration: Integration) -> Result<()> {
        if integration.secret.is_empty() {
            return Err(Error::Invalid(
                "sealed integration secret is required".into(),
            ));
        }
        sqlx::query("INSERT INTO integrations(id,app_id,secret_sealed,user_id,tenant_id) VALUES($1,$2,$3,$4,$5)")
            .bind(integration.id).bind(integration.app_id).bind(integration.secret).bind(integration.user_id).bind(integration.tenant_id)
            .execute(&self.0).await.map_err(db_error)?;
        Ok(())
    }

    pub async fn integration(&self, appid: &str) -> Result<Integration> {
        let row = sqlx::query(
            "SELECT id,app_id,secret_sealed,user_id,tenant_id FROM integrations WHERE app_id=$1",
        )
        .bind(appid)
        .fetch_one(&self.0)
        .await
        .map_err(db_error)?;
        Ok(Integration {
            id: row.try_get("id").map_err(db_error)?,
            app_id: row.try_get("app_id").map_err(db_error)?,
            secret: row.try_get("secret_sealed").map_err(db_error)?,
            user_id: row.try_get("user_id").map_err(db_error)?,
            tenant_id: row.try_get("tenant_id").map_err(db_error)?,
        })
    }
}
