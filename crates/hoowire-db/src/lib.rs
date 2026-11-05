//! PostgreSQL persistence. Every chat mutation and its events commit together.
//! SQL values are always bound parameters; no SDK value becomes SQL syntax.
mod accounts;
mod assets;
mod avatars;
mod chats;
mod directory;
mod feed;
mod locations;
mod messages;
mod preferences;
mod resources;
mod sessions;
pub use avatars::{AvatarInfo, AvatarRecord};
pub use sessions::AuthenticatedSession;

use std::{collections::BTreeSet, time::Duration};

use chrono::{DateTime, Utc};
use hoowire_domain::{
    Asset, Chat, Error, Event, Integration, Message, NewChat, NewMessage, NewUser, Result, User,
    bounded_string,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};
use uuid::Uuid;

pub const DEFAULT_TENANT: Uuid = Uuid::from_u128(1);
const MAX_PAGE: i64 = 1_000;

fn complete_snapshot<T>(rows: Vec<T>, limit: i64) -> Result<Vec<T>> {
    if rows.len() > limit as usize {
        return Err(Error::Invalid("snapshot exceeds supported limit; narrow the query window; complete pagination is not implemented".into()));
    }
    Ok(rows)
}

#[derive(Clone)]
pub struct Repository(PgPool);

type Tx<'a> = Transaction<'a, Postgres>;

fn db_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::RowNotFound => Error::NotFound,
        sqlx::Error::Database(ref db) if db.is_unique_violation() => Error::Conflict,
        sqlx::Error::Database(ref db)
            if db.is_foreign_key_violation() || db.is_check_violation() =>
        {
            Error::Invalid("relationship or constraint violation".into())
        }
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
            Error::Unavailable
        }
        sqlx::Error::Database(ref db)
            if matches!(db.code().as_deref(), Some("40001" | "40P01")) =>
        {
            Error::Conflict
        }
        _ => {
            // Database details can include submitted data. Never put them in logs.
            tracing::error!(code = ?error.as_database_error().and_then(|e| e.code()).as_deref(), "database operation failed");
            Error::Internal
        }
    }
}

macro_rules! row_mapper {
    ($function:ident, $record:ident { $($field:ident),* $(,)? }) => {
        fn $function(row: PgRow) -> std::result::Result<$record, sqlx::Error> {
            Ok($record { $($field: row.try_get(stringify!($field))?),* })
        }
    }
}
row_mapper!(
    user_row,
    User {
        id,
        tenant_id,
        email_hash,
        password_hash,
        profile,
        is_bot,
        created_at
    }
);
row_mapper!(
    chat_row,
    Chat {
        id,
        tenant_id,
        created_by,
        title,
        kind,
        col,
        version,
        attributes,
        created_at,
        updated_at
    }
);
row_mapper!(
    message_row,
    Message {
        id,
        chat_id,
        sender_id,
        client_id,
        body,
        attributes,
        created_at,
        deleted_at
    }
);
row_mapper!(
    asset_row,
    Asset {
        id,
        tenant_id,
        uploaded_by,
        chat_id,
        content_type,
        size,
        digest,
        created_at
    }
);
row_mapper!(
    event_row,
    Event {
        id,
        user_id,
        kind,
        payload,
        created_at
    }
);

fn object(value: &Value) -> Result<()> {
    if !value.is_object() {
        return Err(Error::Invalid("expected a JSON object".into()));
    }
    if value.to_string().len() > 1_048_576 {
        return Err(Error::Invalid("JSON object exceeds 1 MiB".into()));
    }
    Ok(())
}

fn admin_ids(attributes: &Value) -> Result<Option<BTreeSet<Uuid>>> {
    let Some(value) = attributes.get("admins") else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::Invalid("admins must be an array".into()))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| Error::Invalid("invalid administrator UUID".into()))
        })
        .collect::<Result<_>>()
        .map(Some)
}

fn one_to_one(kind: &str) -> bool {
    matches!(kind, "single" | "one_to_one" | "1to1" | "direct")
}

fn merge(target: &mut Value, patch: Value) {
    if let (Value::Object(target), Value::Object(patch)) = (target, patch) {
        for (key, value) in patch {
            if value.is_object() && target.get(&key).is_some_and(Value::is_object) {
                merge(target.get_mut(&key).expect("checked object key"), value);
            } else {
                target.insert(key, value);
            }
        }
    }
}

async fn member(tx: &mut Tx<'_>, user: Uuid, chat: Uuid) -> Result<bool> {
    sqlx::query_scalar("SELECT is_admin FROM chat_members WHERE user_id=$1 AND chat_id=$2")
        .bind(user)
        .bind(chat)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or(Error::Forbidden)
}

async fn lock_chat(
    tx: &mut Tx<'_>,
    actor: Uuid,
    chat: Uuid,
    admin: bool,
    version: Option<i64>,
) -> Result<Chat> {
    let current = sqlx::query("SELECT * FROM chats WHERE id=$1 FOR UPDATE")
        .bind(chat)
        .try_map(chat_row)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    let is_admin = member(tx, actor, chat).await?;
    if admin && !is_admin {
        return Err(Error::Forbidden);
    }
    if version.is_some_and(|v| v != current.version) {
        return Err(Error::Conflict);
    }
    Ok(current)
}

async fn recipients(tx: &mut Tx<'_>, chat: Uuid) -> Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT user_id FROM chat_members WHERE chat_id=$1 ORDER BY user_id")
        .bind(chat)
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)
}

async fn emit(tx: &mut Tx<'_>, users: &[Uuid], kind: &str, payload: &Value) -> Result<()> {
    let users: Vec<_> = users
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if users.is_empty() {
        return Ok(());
    }
    // Lock every recipient in UUID order before the bulk update. Four round
    // trips serve the whole fan-out, while sequence allocation still serializes
    // concurrent conversations sharing a recipient.
    // Foreign-key checks on concurrently inserted messages retain KEY SHARE on
    // their sender. NO KEY UPDATE serializes counters without conflicting with
    // those checks; FOR UPDATE here creates a lock-upgrade deadlock.
    let locked = sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE")
        .bind(&users)
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
    if locked.len() != users.len() {
        return Err(Error::NotFound);
    }
    sqlx::query("UPDATE users SET event_sequence=event_sequence+1 WHERE id=ANY($1)")
        .bind(&users)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO events(user_id,id,kind,payload) SELECT id,event_sequence,$2,$3 FROM users WHERE id=ANY($1)")
        .bind(&users).bind(kind).bind(payload).execute(&mut **tx).await.map_err(db_error)?;
    // NOTIFY is committed with the outbox and carries only an opaque user ID.
    sqlx::query("SELECT pg_notify('hoowire_events',id::text) FROM users WHERE id=ANY($1)")
        .bind(&users)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

async fn bump_chat(tx: &mut Tx<'_>, chat: Uuid, attributes: &Value, title: &str) -> Result<Chat> {
    sqlx::query("UPDATE chats SET attributes=$2,title=$3,version=version+1,updated_at=clock_timestamp() WHERE id=$1 RETURNING *")
        .bind(chat).bind(attributes).bind(title).try_map(chat_row).fetch_one(&mut **tx).await.map_err(db_error)
}

impl Repository {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        Self::connect_with_settings(url, max_connections, 15, "hoowire").await
    }

    /// Migration connections have an explicit, bounded statement time budget.
    /// Serving connections always retain their shorter runtime limit.
    pub async fn connect_for_migrations(
        url: &str,
        max_connections: u32,
        statement_timeout_secs: u32,
    ) -> Result<Self> {
        if !(1..=3600).contains(&statement_timeout_secs) {
            return Err(Error::Invalid(
                "migration statement timeout must be 1..3600 seconds".into(),
            ));
        }
        Self::connect_with_settings(
            url,
            max_connections,
            statement_timeout_secs,
            "hoowire-migrate",
        )
        .await
    }

    async fn connect_with_settings(
        url: &str,
        max_connections: u32,
        statement_timeout_secs: u32,
        application_name: &'static str,
    ) -> Result<Self> {
        if !(1..=512).contains(&max_connections) {
            return Err(Error::Invalid("pool size must be 1..512".into()));
        }
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(300))
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    sqlx::query("SELECT set_config('statement_timeout',$1,false), set_config('lock_timeout','10s',false), set_config('idle_in_transaction_session_timeout','30s',false), set_config('application_name',$2,false)")
                        .bind(format!("{statement_timeout_secs}s"))
                        .bind(application_name)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(db_error)?;
        Ok(Self(pool))
    }

    pub fn pool(&self) -> &PgPool {
        &self.0
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.0)
            .await
            .map_err(|_| {
                tracing::error!("database migration failed");
                Error::Internal
            })
    }

    pub async fn health(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.0)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Refuse to serve a missing, stale or modified schema without running DDL.
    pub async fn verify_schema(&self) -> Result<()> {
        let applied =
            sqlx::query("SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&self.0)
                .await
                .map_err(|_| Error::Unavailable)?;
        let migrations = sqlx::migrate!("./migrations");
        for migration in migrations.iter() {
            let valid = applied.iter().any(|row| {
                row.try_get::<i64, _>("version").ok() == Some(migration.version)
                    && row.try_get::<bool, _>("success").ok() == Some(true)
                    && row
                        .try_get::<Vec<u8>, _>("checksum")
                        .is_ok_and(|checksum| checksum.as_slice() == migration.checksum.as_ref())
            });
            if !valid {
                tracing::error!(
                    "database schema does not match executable; run hoowire migrate with the migration role"
                );
                return Err(Error::Unavailable);
            }
        }
        if applied.len() != migrations.iter().count() {
            return Err(Error::Unavailable);
        }
        Ok(())
    }

    /// Reserve one pool connection for the server-wide notification listener.
    /// The serving process needs at least two configured pool connections.
    pub async fn listener(&self) -> Result<sqlx::postgres::PgListener> {
        let mut listener = sqlx::postgres::PgListener::connect_with(&self.0)
            .await
            .map_err(db_error)?;
        listener.listen("hoowire_events").await.map_err(db_error)?;
        Ok(listener)
    }

    pub async fn close(&self) {
        self.0.close().await;
    }

    pub async fn events(&self, user: Uuid, after: i64, limit: i64) -> Result<Vec<Event>> {
        sqlx::query("SELECT * FROM events WHERE user_id=$1 AND id>$2 ORDER BY id LIMIT $3")
            .bind(user)
            .bind(after.max(0))
            .bind(limit.clamp(1, MAX_PAGE))
            .try_map(event_row)
            .fetch_all(&self.0)
            .await
            .map_err(db_error)
    }

    pub async fn create_asset(&self, asset: Asset) -> Result<()> {
        if asset.size < 0 {
            return Err(Error::Invalid("negative asset size".into()));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        if let Some(chat) = asset.chat_id {
            lock_chat(&mut tx, asset.uploaded_by, chat, false, None).await?;
        }
        sqlx::query("INSERT INTO assets(id,tenant_id,uploaded_by,chat_id,content_type,size,digest,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(asset.id).bind(asset.tenant_id).bind(asset.uploaded_by).bind(asset.chat_id).bind(asset.content_type)
            .bind(asset.size).bind(asset.digest).bind(asset.created_at).execute(&mut *tx).await.map_err(db_error)?;
        if let Some(chat) = asset.chat_id {
            sqlx::query("INSERT INTO asset_shares(asset_id,chat_id,tenant_id) VALUES($1,$2,$3)")
                .bind(asset.id)
                .bind(chat)
                .bind(asset.tenant_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn asset_for(&self, user: Uuid, id: Uuid) -> Result<Asset> {
        sqlx::query("SELECT a.* FROM assets a WHERE a.id=$2 AND (a.uploaded_by=$1 OR EXISTS(SELECT 1 FROM asset_shares s JOIN chat_members m ON m.chat_id=s.chat_id WHERE s.asset_id=a.id AND m.user_id=$1))")
            .bind(user).bind(id).try_map(asset_row).fetch_optional(&self.0).await.map_err(db_error)?.ok_or(Error::Forbidden)
    }
}
