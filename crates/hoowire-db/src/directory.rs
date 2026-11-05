//! PostgreSQL directory repository operations.
use super::*;

impl Repository {
    pub async fn directory(&self, tenant: Uuid, kind: &str) -> Result<Vec<Value>> {
        let rows = sqlx::query_scalar("SELECT attributes || jsonb_build_object('id',id,'title',title,'kind',kind) FROM directory_groups WHERE tenant_id=$1 AND kind=$2 ORDER BY id LIMIT 1001")
            .bind(tenant).bind(kind).fetch_all(&self.0).await.map_err(db_error)?;
        complete_snapshot(rows, MAX_PAGE)
    }

    pub async fn directory_entry(&self, tenant: Uuid, id: Uuid, kind: &str) -> Result<Value> {
        sqlx::query_scalar("SELECT attributes || jsonb_build_object('id',id,'title',title,'kind',kind) FROM directory_groups WHERE tenant_id=$1 AND id=$2 AND kind=$3")
            .bind(tenant).bind(id).bind(kind).fetch_one(&self.0).await.map_err(db_error)
    }

    pub async fn upsert_directory_entry(
        &self,
        tenant: Uuid,
        id: Uuid,
        kind: &str,
        value: Value,
    ) -> Result<()> {
        object(&value)?;
        if !matches!(kind, "group" | "role") {
            return Err(Error::Invalid("invalid directory kind".into()));
        }
        let title = value.get("title").and_then(Value::as_str).unwrap_or("");
        bounded_string(title, 4096, "directory title")?;
        let raw_members = value
            .get("user_ids")
            .or_else(|| value.get("members"))
            .and_then(Value::as_array);
        let members: Vec<Uuid> = raw_members
            .into_iter()
            .flatten()
            .map(|item| {
                item.as_str()
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .ok_or_else(|| Error::Invalid("invalid directory member".into()))
                    .and_then(|id| {
                        Uuid::parse_str(id)
                            .map_err(|_| Error::Invalid("invalid directory member UUID".into()))
                    })
            })
            .collect::<Result<_>>()?;
        if members.len() > 1000 {
            return Err(Error::Invalid("too many directory members".into()));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO directory_groups(tenant_id,id,kind,title,attributes) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,id,kind) DO UPDATE SET title=EXCLUDED.title,attributes=EXCLUDED.attributes")
            .bind(tenant).bind(id).bind(kind).bind(title).bind(&value).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "DELETE FROM directory_group_members WHERE tenant_id=$1 AND group_id=$2 AND kind=$3",
        )
        .bind(tenant)
        .bind(id)
        .bind(kind)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        for user in members {
            sqlx::query("INSERT INTO directory_group_members(tenant_id,group_id,kind,user_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(tenant).bind(id).bind(kind).bind(user).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
