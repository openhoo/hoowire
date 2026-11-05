//! Domain records. Wire formatting belongs to the API adapter; SQL belongs to the repository.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not authorized")]
    Unauthorized,
    #[error("permission denied")]
    Forbidden,
    #[error("resource not found")]
    NotFound,
    #[error("concurrent modification or duplicate resource")]
    Conflict,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("service temporarily unavailable")]
    Unavailable,
    #[error("operation not yet supported")]
    Unsupported,
    #[error("internal error")]
    Internal,
}

/// Never serialize this record: credentials are intentionally excluded from wire models.
#[derive(Clone)]
pub struct User {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email_hash: String,
    pub password_hash: String,
    pub profile: Value,
    pub is_bot: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chat {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub created_by: Uuid,
    pub title: String,
    pub kind: String,
    /// Client supplied opaque conversation key, retained for the legacy protocol.
    pub col: String,
    pub version: i64,
    pub attributes: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub sender_id: Uuid,
    pub client_id: Option<String>,
    pub body: String,
    pub attributes: Value,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub uploaded_by: Uuid,
    pub chat_id: Option<Uuid>,
    pub content_type: String,
    pub size: i64,
    pub digest: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub user_id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Principal {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub is_bot: bool,
}

#[derive(Clone)]
pub struct Integration {
    pub id: Uuid,
    pub app_id: String,
    pub secret: String,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
}

#[derive(Clone, Debug)]
pub struct NewUser {
    pub tenant_id: Uuid,
    pub email_hash: String,
    pub password_hash: String,
    pub profile: Value,
    pub is_bot: bool,
}

#[derive(Clone, Debug)]
pub struct NewChat {
    pub tenant_id: Uuid,
    pub created_by: Uuid,
    pub title: String,
    pub kind: String,
    pub col: String,
    pub attributes: Value,
    pub members: Vec<Uuid>,
}

#[derive(Clone, Debug)]
pub struct NewMessage {
    pub chat_id: Uuid,
    pub sender_id: Uuid,
    pub client_id: Option<String>,
    pub body: String,
    pub attributes: Value,
}

pub fn identifier(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| Error::Invalid("expected UUID".into()))
}

pub fn bounded_string(value: &str, max: usize, field: &str) -> Result<()> {
    if value.len() > max {
        return Err(Error::Invalid(format!("{field} exceeds {max} bytes")));
    }
    Ok(())
}
