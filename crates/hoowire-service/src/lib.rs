//! Application use cases. All durable writes go through the PostgreSQL repository.
mod avatars;
mod locations;
mod resources;
mod sessions;
use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, OsRng, rand_core::RngCore},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
pub use avatars::MAX_AVATAR_BYTES;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, Utc};
use hoowire_blob::BlobStore;
use hoowire_crypto::{Handshake, TransportKey};
use hoowire_db::Repository;
use hoowire_domain::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{RwLock, Semaphore, broadcast, watch};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const DEFAULT_TENANT: Uuid = Uuid::from_u128(1);
const SESSION_TTL_SECONDS: u32 = 3600;
const MAX_PENDING_HANDSHAKES: usize = 4096;

#[derive(Clone)]
pub struct Auth {
    pub principal: Principal,
    pub key: TransportKey,
    pub from_id: String,
}

struct PendingHandshake {
    key: TransportKey,
    expires: Instant,
    attempts: u8,
}

struct Inner {
    repository: Repository,
    blobs: BlobStore,
    secrets: Aes256Gcm,
    handshakes: RwLock<HashMap<String, PendingHandshake>>,
    password_slots: Arc<Semaphore>,
    changes: broadcast::Sender<Uuid>,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct App(Arc<Inner>);

impl App {
    pub fn new(repository: Repository, blobs: BlobStore, master_key: &[u8; 32]) -> Self {
        let (changes, _) = broadcast::channel(1024);
        let (shutdown, mut stop) = watch::channel(false);
        let listener_repository = repository.clone();
        let notifications = changes.clone();
        tokio::spawn(async move {
            loop {
                let listener = tokio::select! {
                    _=stop.changed()=>return,
                    result=listener_repository.listener()=>result,
                };
                match listener {
                    Ok(mut listener) => loop {
                        tokio::select! {
                            _=stop.changed()=>return,
                            result=listener.recv()=>match result {
                                Ok(notification)=>if let Ok(user)=Uuid::parse_str(notification.payload()){let _=notifications.send(user);},
                                Err(_)=>break,
                            }
                        }
                    },
                    Err(_) => tracing::warn!(
                        "database notification listener unavailable; durable catch-up remains active"
                    ),
                }
                tokio::select! {_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
            }
        });
        Self(Arc::new(Inner {
            repository,
            blobs,
            secrets: Aes256Gcm::new(master_key.into()),
            handshakes: RwLock::new(HashMap::new()),
            password_slots: Arc::new(Semaphore::new(4)),
            changes,
            shutdown,
        }))
    }

    pub async fn health(&self) -> Result<()> {
        self.0.repository.health().await
    }
    pub async fn close(&self) {
        self.begin_shutdown();
        self.0.repository.close().await;
    }
    pub fn begin_shutdown(&self) {
        let _ = self.0.shutdown.send(true);
    }
    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.0.shutdown.subscribe()
    }
    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.0.changes.subscribe()
    }

    pub async fn handshake(&self, client_pubkey: &str) -> Result<Value> {
        let (server_pubkey, key) = Handshake::new(client_pubkey)
            .map_err(|_| Error::Invalid("invalid client public key".into()))?;
        let id = Uuid::new_v4().to_string();
        let mut handshakes = self.0.handshakes.write().await;
        handshakes.retain(|_, s| s.expires > Instant::now());
        if handshakes.len() >= MAX_PENDING_HANDSHAKES {
            return Err(Error::Unavailable);
        }
        handshakes.insert(
            id.clone(),
            PendingHandshake {
                key,
                expires: Instant::now() + Duration::from_secs(120),
                attempts: 0,
            },
        );
        Ok(json!({"server_pubkey": server_pubkey, "session_id": id}))
    }

    pub async fn session_key(&self, id: &str) -> Result<TransportKey> {
        let handshakes = self.0.handshakes.read().await;
        let session = handshakes
            .get(id)
            .filter(|s| s.expires > Instant::now())
            .ok_or(Error::Unauthorized)?;
        Ok(session.key.clone())
    }

    pub async fn login(&self, id: &str, credentials: &Value) -> Result<Value> {
        {
            let mut handshakes = self.0.handshakes.write().await;
            let session = handshakes
                .get_mut(id)
                .filter(|s| s.expires > Instant::now())
                .ok_or(Error::Unauthorized)?;
            session.attempts += 1;
            if session.attempts > 5 {
                handshakes.remove(id);
                return Err(Error::Unauthorized);
            }
        }
        let email_hash = credentials
            .get("email_hash")
            .and_then(Value::as_str)
            .ok_or(Error::Unauthorized)?;
        let password = Zeroizing::new(
            credentials
                .get("password")
                .and_then(Value::as_str)
                .ok_or(Error::Unauthorized)?
                .to_owned(),
        );
        if password.len() > 1024 {
            return Err(Error::Unauthorized);
        }
        // Do the same expensive verification for unknown accounts to limit enumeration.
        let user = match self.0.repository.user_by_email_hash(email_hash).await {
            Ok(user) => Some(user),
            Err(Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        let expected = user.as_ref().map(|u| u.password_hash.clone());
        let permit = self
            .0
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Unavailable)?;
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Some(expected) = expected {
                PasswordHash::new(&expected).is_ok_and(|hash| {
                    Argon2::default()
                        .verify_password(password.as_bytes(), &hash)
                        .is_ok()
                })
            } else {
                let salt = SaltString::generate(&mut OsRng);
                let _ = Argon2::default().hash_password(password.as_bytes(), &salt);
                false
            }
        })
        .await
        .map_err(|_| Error::Internal)?;
        if !valid {
            return Err(Error::Unauthorized);
        }
        let user = user.ok_or(Error::Unauthorized)?;
        let principal = Principal {
            user_id: user.id,
            tenant_id: user.tenant_id,
            is_bot: user.is_bot,
        };
        let from_id = Uuid::new_v4().to_string();
        let handshake = self
            .0
            .handshakes
            .write()
            .await
            .remove(id)
            .filter(|s| s.expires > Instant::now())
            .ok_or(Error::Unauthorized)?;
        let token_hash = sessions::token_hash(&from_id);
        let key_sealed = self.seal_session_key(&token_hash, &principal, &handshake.key)?;
        self.0
            .repository
            .create_session(
                &token_hash,
                principal.user_id,
                principal.tenant_id,
                &key_sealed,
                SESSION_TTL_SECONDS,
            )
            .await?;
        Ok(
            json!({"from_id":from_id,"owner_id":user.id,"user_id":user.id,"id":user.id,
            "first_name":user.profile["first_name"].as_str().unwrap_or(""),"last_name":user.profile["last_name"].as_str().unwrap_or(""),
            "organisation":user.tenant_id,"organisation_id":user.tenant_id,"owner_type":"user","send_contacts":true,"session_id":id}),
        )
    }

    pub async fn authenticate(&self, from: &str) -> Result<Auth> {
        if from.is_empty() || from.len() > 256 {
            return Err(Error::Unauthorized);
        }
        let token_hash = sessions::token_hash(from);
        if let Some(session) = self.0.repository.session(&token_hash).await? {
            if !session.active {
                return Err(Error::Unauthorized);
            }
            let principal = Principal {
                user_id: session.user_id,
                tenant_id: session.tenant_id,
                is_bot: session.is_bot,
            };
            return Ok(Auth {
                key: self.open_session_key(&token_hash, &principal, &session.key_sealed)?,
                principal,
                from_id: from.into(),
            });
        }
        let integration = self
            .0
            .repository
            .integration(from)
            .await
            .map_err(|e| match e {
                Error::NotFound => Error::Unauthorized,
                other => other,
            })?;
        let secret = self.open_secret(&integration.secret)?;
        let key = TransportKey::from_secret(&secret).map_err(|_| Error::Internal)?;
        Ok(Auth {
            principal: Principal {
                user_id: integration.user_id,
                tenant_id: integration.tenant_id,
                is_bot: true,
            },
            key,
            from_id: from.into(),
        })
    }

    pub async fn logout(&self, auth: &Auth) -> Result<()> {
        self.0
            .repository
            .revoke_session(
                &sessions::token_hash(&auth.from_id),
                auth.principal.user_id,
                auth.principal.tenant_id,
            )
            .await?;
        let _ = self.0.changes.send(auth.principal.user_id);
        Ok(())
    }

    pub async fn websocket_token(&self, auth: &Auth) -> Result<String> {
        let auth = self.authenticate(&auth.from_id).await?;
        // Native clients append this ticket directly as one URL path segment.
        let mut random = Zeroizing::new(vec![0u8; 32]);
        OsRng.fill_bytes(&mut random);
        let token = URL_SAFE_NO_PAD.encode(&random);
        let token_hash = sessions::token_hash(&token);
        let sealed = self.seal_websocket_credential(&token_hash, &auth)?;
        self.0
            .repository
            .create_websocket_ticket(
                &token_hash,
                auth.principal.user_id,
                auth.principal.tenant_id,
                &sealed,
            )
            .await?;
        Ok(token)
    }

    pub async fn websocket_auth(&self, token: &str) -> Result<Auth> {
        if token.len() != 43
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(Error::Unauthorized);
        }
        let token_hash = sessions::token_hash(token);
        let ticket = self
            .0
            .repository
            .consume_websocket_ticket(&token_hash)
            .await?
            .ok_or(Error::Unauthorized)?;
        let from = self.open_websocket_credential(
            &token_hash,
            ticket.user_id,
            ticket.tenant_id,
            &ticket.credential_sealed,
        )?;
        let auth = self.authenticate(&from).await?;
        if auth.principal.user_id != ticket.user_id || auth.principal.tenant_id != ticket.tenant_id
        {
            return Err(Error::Unauthorized);
        }
        Ok(auth)
    }

    pub async fn provision_user(
        &self,
        tenant: Uuid,
        email: &str,
        password: Zeroizing<String>,
        first_name: &str,
        last_name: &str,
        is_bot: bool,
    ) -> Result<User> {
        bounded_string(email, 320, "email")?;
        bounded_string(first_name, 128, "first_name")?;
        bounded_string(last_name, 128, "last_name")?;
        if password.len() < 12 || password.len() > 1024 {
            return Err(Error::Invalid("password must contain 12–1024 bytes".into()));
        }
        let password_hash = tokio::task::spawn_blocking(move || {
            let salt = SaltString::generate(&mut OsRng);
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map(|h| h.to_string())
                .map_err(|_| Error::Internal)
        })
        .await
        .map_err(|_| Error::Internal)??;
        self.0.repository.create_user(NewUser {
            tenant_id:tenant, email_hash:hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes())), password_hash,
            profile:json!({"first_name":first_name,"last_name":last_name,"account_email":email.trim().to_lowercase()}), is_bot,
        }).await
    }

    pub async fn provision_integration(
        &self,
        user: &User,
        app_id: &str,
        secret: &str,
    ) -> Result<()> {
        if !user.is_bot {
            return Err(Error::Invalid("integration owner must be a bot".into()));
        }
        TransportKey::from_secret(secret)
            .map_err(|_| Error::Invalid("SDK secret must be exactly 32 ASCII bytes".into()))?;
        self.0
            .repository
            .create_integration(Integration {
                id: Uuid::new_v4(),
                user_id: user.id,
                tenant_id: user.tenant_id,
                app_id: app_id.into(),
                secret: self.seal_secret(secret)?,
            })
            .await
    }

    pub async fn me(&self, auth: &Auth) -> Result<User> {
        self.0.repository.user(auth.principal.user_id).await
    }
    pub async fn user(&self, auth: &Auth, id: Uuid) -> Result<User> {
        let user = self.0.repository.user(id).await?;
        if user.tenant_id != auth.principal.tenant_id {
            return Err(Error::NotFound);
        }
        Ok(user)
    }
    pub async fn users(&self, auth: &Auth) -> Result<Vec<User>> {
        self.0
            .repository
            .users(auth.principal.tenant_id, 1000)
            .await
    }
    pub async fn update_profile(&self, auth: &Auth, mut profile: Value) -> Result<()> {
        if !profile.is_object() {
            return Err(Error::Invalid("profile must be an object".into()));
        }
        bounded_string(&profile.to_string(), 16384, "profile")?;
        // Only avatar mutations can advance these persisted metadata fields.
        if let Some(fields) = profile.as_object_mut() {
            fields.remove("has_avatar");
            fields.remove("avatar_modified_at");
        }
        self.0
            .repository
            .update_profile(auth.principal.user_id, profile)
            .await
    }
    pub async fn preferences(&self, auth: &Auth) -> Result<Value> {
        self.0.repository.preferences(auth.principal.user_id).await
    }
    pub async fn set_preferences(&self, auth: &Auth, value: Value) -> Result<()> {
        self.0
            .repository
            .set_preferences(auth.principal.user_id, value)
            .await
    }
    pub async fn patch_preferences(&self, auth: &Auth, value: Value) -> Result<()> {
        self.0
            .repository
            .patch_preferences(auth.principal.user_id, value)
            .await
    }
    pub async fn preference_list(
        &self,
        auth: &Auth,
        key: &str,
        chat: Uuid,
        add: bool,
    ) -> Result<()> {
        self.0
            .repository
            .set_preference_list(auth.principal.user_id, key, chat, add)
            .await
    }
    pub async fn feed(
        &self,
        auth: &Auth,
        token: Option<DateTime<Utc>>,
    ) -> Result<(Vec<Message>, DateTime<Utc>)> {
        self.0
            .repository
            .feed(auth.principal.user_id, token, 1000)
            .await
    }

    pub async fn create_chat(&self, auth: &Auth, value: &Value) -> Result<Chat> {
        let title = value.get("title").and_then(Value::as_str).unwrap_or("");
        bounded_string(title, 512, "title")?;
        let kind = value
            .get("chatType")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("group");
        if !["group", "1to1"].contains(&kind) {
            return Err(Error::Invalid("invalid chat type".into()));
        }
        let col = value
            .get("col")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Invalid("missing conversation key".into()))?;
        TransportKey::from_secret(col)
            .map_err(|_| Error::Invalid("invalid conversation key".into()))?;
        let mut members = self.expanded_members(auth, value).await?;
        if !members.contains(&auth.principal.user_id) {
            members.push(auth.principal.user_id);
        }
        members.sort_unstable();
        members.dedup();
        if members.len() > 1000 || (kind == "1to1" && members.len() != 2) {
            return Err(Error::Invalid("invalid chat membership".into()));
        }
        let mut attributes = value.clone();
        let mut direct_users = uuid_list(value.get("users"))?;
        if !direct_users.contains(&auth.principal.user_id) {
            direct_users.push(auth.principal.user_id);
        }
        attributes["users"] = json!(direct_users);
        attributes
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("chat must be an object".into()))?
            .remove("col");
        if let Some(fields) = attributes.as_object_mut() {
            fields.remove("hasAvatar");
            fields.remove("avatarModifiedAt");
        }
        self.unseal_chat(
            self.0
                .repository
                .create_chat(NewChat {
                    tenant_id: auth.principal.tenant_id,
                    created_by: auth.principal.user_id,
                    title: title.into(),
                    kind: kind.into(),
                    col: self.seal_secret(col)?,
                    attributes,
                    members,
                })
                .await?,
        )
    }
    pub async fn chat(&self, auth: &Auth, id: Uuid) -> Result<Chat> {
        self.unseal_chat(
            self.0
                .repository
                .chat_for(auth.principal.user_id, id)
                .await?,
        )
    }
    pub async fn chats(&self, auth: &Auth) -> Result<Vec<Chat>> {
        self.0
            .repository
            .chats(auth.principal.user_id)
            .await?
            .into_iter()
            .map(|chat| self.unseal_chat(chat))
            .collect()
    }
    pub async fn members(&self, auth: &Auth, id: Uuid) -> Result<Vec<Uuid>> {
        self.0.repository.members(auth.principal.user_id, id).await
    }
    pub async fn administrators(&self, auth: &Auth, id: Uuid) -> Result<Vec<Uuid>> {
        self.0
            .repository
            .administrators(auth.principal.user_id, id)
            .await
    }
    pub async fn replace_members(
        &self,
        auth: &Auth,
        id: Uuid,
        members: &[Uuid],
        version: Option<i64>,
    ) -> Result<Chat> {
        self.unseal_chat(
            self.0
                .repository
                .replace_members(auth.principal.user_id, id, members, version)
                .await?,
        )
    }
    pub async fn update_chat(&self, auth: &Auth, id: Uuid, mut value: Value) -> Result<Chat> {
        let version = value.get("version").and_then(Value::as_i64);
        if let Some(fields) = value.as_object_mut() {
            fields.remove("hasAvatar");
            fields.remove("avatarModifiedAt");
            fields.remove("chatV");
            fields.remove("chat_v");
        }
        self.unseal_chat(
            self.0
                .repository
                .update_chat(auth.principal.user_id, id, version, value)
                .await?,
        )
    }
    pub async fn add_members(
        &self,
        auth: &Auth,
        id: Uuid,
        members: &[Uuid],
        version: Option<i64>,
    ) -> Result<Chat> {
        if members.len() > 2500 {
            return Err(Error::Invalid("too many members".into()));
        }
        self.unseal_chat(
            self.0
                .repository
                .add_members(auth.principal.user_id, id, members, version)
                .await?,
        )
    }
    pub async fn remove_members(
        &self,
        auth: &Auth,
        id: Uuid,
        members: &[Uuid],
        version: Option<i64>,
    ) -> Result<Chat> {
        self.unseal_chat(
            self.0
                .repository
                .remove_members(auth.principal.user_id, id, members, version)
                .await?,
        )
    }

    pub async fn post_message(&self, auth: &Auth, value: &Value) -> Result<Message> {
        let chat_id = identifier(
            value
                .get("tid")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("missing tid".into()))?,
        )?;
        let body = value
            .get("msg")
            .or_else(|| value.get("m"))
            .and_then(Value::as_str)
            .unwrap_or("");
        bounded_string(body, 1024 * 1024, "message")?;
        let client_id = value.get("cid").and_then(Value::as_str).map(str::to_owned);
        if let Some(cid) = &client_id {
            bounded_string(cid, 128, "cid")?;
        }
        let mut attrs = value.clone();
        let map = attrs
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("message must be an object".into()))?;
        for field in ["msg", "m", "tid", "from", "ts", "mid", "cid"] {
            map.remove(field);
        }
        bounded_string(&attrs.to_string(), 2 * 1024 * 1024, "message metadata")?;
        self.0
            .repository
            .post_message(NewMessage {
                chat_id,
                sender_id: auth.principal.user_id,
                client_id,
                body: body.into(),
                attributes: attrs,
            })
            .await
    }
    pub async fn message(&self, auth: &Auth, id: Uuid) -> Result<Message> {
        self.0
            .repository
            .message_for(auth.principal.user_id, id)
            .await
    }
    pub async fn messages(
        &self,
        auth: &Auth,
        chat: Option<Uuid>,
        after: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<Message>> {
        self.0
            .repository
            .messages(auth.principal.user_id, chat, after, limit.clamp(1, 1000))
            .await
    }
    pub async fn delete_message(&self, auth: &Auth, id: Uuid) -> Result<()> {
        self.0
            .repository
            .delete_message(auth.principal.user_id, id)
            .await
    }
    pub async fn read_chat(&self, auth: &Auth, id: Uuid, message: Option<Uuid>) -> Result<()> {
        self.0
            .repository
            .read_chat(auth.principal.user_id, id, message)
            .await
    }
    pub async fn readers(&self, auth: &Auth, id: Uuid) -> Result<Vec<Value>> {
        self.0.repository.readers(auth.principal.user_id, id).await
    }
    pub async fn events(&self, auth: &Auth, after: i64) -> Result<Vec<Event>> {
        self.authenticate(&auth.from_id).await?;
        self.0
            .repository
            .events(auth.principal.user_id, after, 1000)
            .await
    }

    pub async fn upload(
        &self,
        auth: &Auth,
        bytes: &[u8],
        content_type: &str,
        chat_id: Option<Uuid>,
    ) -> Result<Asset> {
        if let Some(chat_id) = chat_id {
            self.chat(auth, chat_id).await?;
        }
        let id = Uuid::new_v4();
        let digest = self.0.blobs.put(id, bytes).await?;
        let asset = Asset {
            id,
            tenant_id: auth.principal.tenant_id,
            uploaded_by: auth.principal.user_id,
            chat_id,
            content_type: content_type.into(),
            size: bytes.len() as i64,
            digest,
            created_at: Utc::now(),
        };
        if let Err(error) = self.0.repository.create_asset(asset.clone()).await {
            if let Err(cleanup) = self.0.blobs.remove(id).await {
                tracing::error!(%cleanup, asset_id = %id, "failed to remove uncommitted asset");
            }
            return Err(error);
        }
        Ok(asset)
    }
    pub async fn download(&self, auth: &Auth, id: Uuid) -> Result<(Asset, Vec<u8>)> {
        let asset = self
            .0
            .repository
            .asset_for(auth.principal.user_id, id)
            .await?;
        let bytes = self.0.blobs.get(asset.id, &asset.digest).await?;
        Ok((asset, bytes))
    }

    fn unseal_chat(&self, mut chat: Chat) -> Result<Chat> {
        chat.col = self.open_secret(&chat.col)?.to_string();
        Ok(chat)
    }

    fn seal_secret(&self, secret: &str) -> Result<String> {
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let cipher = self
            .0
            .secrets
            .encrypt((&nonce).into(), secret.as_bytes())
            .map_err(|_| Error::Internal)?;
        Ok(format!(
            "v1:{}",
            STANDARD.encode([nonce.as_slice(), &cipher].concat())
        ))
    }
    fn open_secret(&self, envelope: &str) -> Result<Zeroizing<String>> {
        let bytes = STANDARD
            .decode(envelope.strip_prefix("v1:").ok_or(Error::Internal)?)
            .map_err(|_| Error::Internal)?;
        if bytes.len() < 28 {
            return Err(Error::Internal);
        }
        let plain = Zeroizing::new(
            self.0
                .secrets
                .decrypt(bytes[..12].into(), &bytes[12..])
                .map_err(|_| Error::Internal)?,
        );
        String::from_utf8(plain.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| Error::Internal)
    }
}

pub fn random_secret(bytes: usize) -> String {
    let mut buffer = Zeroizing::new(vec![0u8; bytes]);
    OsRng.fill_bytes(&mut buffer);
    STANDARD.encode(&buffer)
}

pub fn uuid_list(value: Option<&Value>) -> Result<Vec<Uuid>> {
    match value {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| {
                identifier(
                    v.as_str()
                        .ok_or_else(|| Error::Invalid("expected UUID list".into()))?,
                )
            })
            .collect(),
        _ => Err(Error::Invalid("expected UUID list".into())),
    }
}
