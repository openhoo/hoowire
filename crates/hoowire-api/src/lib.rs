//! HTTP and WebSocket adapters for the independently observed Teamwire API20.
//! Unknown operations fail explicitly; route presence is not a compatibility certificate.
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{CloseFrame, Message as WsMessage, WebSocket},
    },
    http::{HeaderMap, Method, Request, Response, StatusCode, header},
    routing::get,
};
use chrono::{DateTime, SecondsFormat, Utc};
use hoowire_domain::{Chat, Error, Message, Result, User, identifier};
use hoowire_service::{App, Auth, uuid_list};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Semaphore, broadcast};
use uuid::Uuid;
mod avatars;
mod cors;
mod locations;
mod resources;
pub use cors::CorsConfig;

const MAX_REQUEST_BYTES: usize = 24 * 1024 * 1024;
const MAX_WEBSOCKETS: u32 = 1024;

pub async fn serve(listener: tokio::net::TcpListener, app: App) -> std::io::Result<()> {
    serve_with_cors(listener, app, CorsConfig::default()).await
}

pub async fn serve_with_cors(
    listener: tokio::net::TcpListener,
    app: App,
    cors: CorsConfig,
) -> std::io::Result<()> {
    let state = ApiState::new(app.clone());
    let websockets = state.websockets.clone();
    let shutdown_app = app.clone();
    let result = axum::serve(listener, router_with_state(state, cors))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                if let Ok(mut term) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{} }
                    shutdown_app.begin_shutdown();
                    return;
                }
            }
            let _ = tokio::signal::ctrl_c().await;
            shutdown_app.begin_shutdown();
        })
        .await;
    app.begin_shutdown();
    // Axum drains HTTP connections, while on_upgrade callbacks are separate tasks.
    // A permit spans authentication, the pending upgrade and the complete WS task.
    // Keep the runtime and database alive until those tasks finish their close path.
    match tokio::time::timeout(
        Duration::from_secs(3),
        websockets.acquire_many_owned(MAX_WEBSOCKETS),
    )
    .await
    {
        Ok(Ok(_)) => result,
        Ok(Err(_)) => Err(std::io::Error::other(
            "WebSocket capacity tracker closed during shutdown",
        )),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out draining WebSocket connections",
        )),
    }
}

#[derive(Clone)]
struct ApiState {
    app: App,
    broadcasts: broadcast::Sender<(Uuid, Uuid, Value)>,
    requests: Arc<Semaphore>,
    websockets: Arc<Semaphore>,
    uploads: Arc<Semaphore>,
}

impl ApiState {
    fn new(app: App) -> Self {
        let (broadcasts, _) = broadcast::channel(256);
        Self {
            app,
            broadcasts,
            requests: Arc::new(Semaphore::new(64)),
            websockets: Arc::new(Semaphore::new(MAX_WEBSOCKETS as usize)),
            uploads: Arc::new(Semaphore::new(4)),
        }
    }
}

pub fn router(app: App) -> Router {
    router_with_cors(app, CorsConfig::default())
}

pub fn router_with_cors(app: App, cors: CorsConfig) -> Router {
    router_with_state(ApiState::new(app), cors)
}

fn router_with_state(state: ApiState, cors: CorsConfig) -> Router {
    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(ready))
        .route("/ws/v20", get(websocket_sdk))
        .route("/ws/v20/", get(websocket_sdk))
        .route("/ws/v20/{token}", get(websocket))
        .fallback(http)
        .with_state(state)
        .layer(cors.layer())
}

async fn ready(State(state): State<ApiState>) -> Response<Body> {
    match state.app.health().await {
        Ok(()) => plain(StatusCode::OK, json!({"status":"ready"})),
        Err(error) => error_response(error),
    }
}

async fn http(State(state): State<ApiState>, request: Request<Body>) -> Response<Body> {
    let Ok(_permit) = state.requests.clone().try_acquire_owned() else {
        return error_response(Error::Unavailable);
    };
    match tokio::time::timeout(Duration::from_secs(30), handle_http(&state, request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => error_response(error),
        Err(_) => error_response(Error::Unavailable),
    }
}

async fn handle_http(state: &ApiState, request: Request<Body>) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().trim_end_matches('/');
    let method = parts.method;
    if path == "/v20/api_versions" && method == Method::GET {
        return Ok(plain(StatusCode::OK, json!({"versions":["20.0"]})));
    }
    let Some(path) = path.strip_prefix("/v20") else {
        return Err(Error::NotFound);
    };
    let bootstrap = path == "/u/sessions" && method == Method::POST;
    let login = path.starts_with("/u/login/") && method == Method::POST;
    let auth = if bootstrap || login {
        None
    } else {
        let from = parts
            .headers
            .get("from")
            .and_then(|v| v.to_str().ok())
            .ok_or(Error::Unauthorized)?;
        Some(state.app.authenticate(from).await?)
    };
    let upload = method == Method::POST && (path == "/upload" || avatars::target(path).is_some());
    let _upload_permit = if upload {
        Some(
            state
                .uploads
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Unavailable)?,
        )
    } else {
        None
    };
    let limit = if upload {
        MAX_REQUEST_BYTES
    } else if bootstrap || login {
        16 * 1024
    } else {
        6 * 1024 * 1024
    };
    let bytes = to_bytes(body, limit)
        .await
        .map_err(|_| Error::Invalid("request body too large".into()))?;
    if path == "/u/sessions" && method == Method::POST {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Invalid("expected JSON session request".into()))?;
        return Ok(plain(
            StatusCode::OK,
            state
                .app
                .handshake(string(&value, "client_pubkey")?)
                .await?,
        ));
    }
    if let Some(session) = path.strip_prefix("/u/login/") {
        if method != Method::POST {
            return Err(Error::NotFound);
        }
        let key = state.app.session_key(session).await?;
        let data = decode(&key, &bytes)?;
        let response = state.app.login(session, &data).await?;
        return encrypted(&key, &response, None);
    }
    let auth = auth.ok_or(Error::Unauthorized)?;
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();

    if let Some((kind, owner)) = avatars::target(path) {
        return avatars::handle(
            &state.app,
            &auth,
            &method,
            kind,
            identifier(owner)?,
            &parts.headers,
            bytes,
        )
        .await;
    }

    // Upload bytes are already chat encrypted. They must not be decrypted with the session key.
    if path == "/upload" && method == Method::POST {
        let (file, content_type) = multipart_file(&parts.headers, bytes).await?;
        let chat = query.get("tid").map(|v| identifier(v)).transpose()?;
        let asset = state.app.upload(&auth, &file, &content_type, chat).await?;
        return Ok(plain(
            StatusCode::OK,
            json!({"id":asset.id,"url":format!("asset:///{}",asset.id)}),
        ));
    }
    if let Some(id) = path.strip_prefix("/serve/") {
        if method != Method::GET {
            return Err(Error::NotFound);
        }
        let (_, bytes) = state.app.download(&auth, identifier(id)?).await?;
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "private, no-store")
            .body(Body::from(bytes))
            .map_err(|_| Error::Internal);
    }
    let value = decode(&auth.key, &bytes)?;
    if path == "/feed" && method == Method::GET {
        let token = cursor(&parts.headers)?;
        let (messages, token) = state.app.feed(&auth, token).await?;
        let messages = messages.iter().map(message_wire).collect::<Vec<_>>();
        return encrypted(
            &auth.key,
            &json!({"messages":messages,"read_threads":state.app.read_threads(&auth).await?}),
            Some(token),
        );
    }
    let response_cursor = if ["/contacts", "/directory/roles", "/directory/sync"].contains(&path) {
        state.app.directory_revision(&auth).await?
    } else {
        Utc::now()
    };
    let response = dispatch(
        &state.app,
        &auth,
        &method,
        path,
        &query,
        &parts.headers,
        value,
    )
    .await?;
    if path.starts_with("/t/")
        && ((method == Method::PUT && path.ends_with("/members"))
            || (method == Method::DELETE && path.ends_with("/subscription")))
    {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .map_err(|_| Error::Internal);
    }
    encrypted(&auth.key, &response, Some(response_cursor))
}

async fn dispatch(
    app: &App,
    auth: &Auth,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
    value: Value,
) -> Result<Value> {
    match (method.as_str(), path) {
        ("GET", "/u/name") => {
            let user = app.me(auth).await?;
            Ok(
                json!({"user_id":user.id,"owner_id":user.id,"id":user.id,"from_id":auth.from_id,
                "first_name":user.profile["first_name"].as_str().unwrap_or(""),"last_name":user.profile["last_name"].as_str().unwrap_or("")}),
            )
        }
        ("GET", "/u/appsettings") => Ok(appsettings()),
        ("GET", "/u/settings") => {
            let prefs = app.preferences(auth).await?;
            let mut settings = json!({"autosave":false,"read_receipts":true,"readReceipts":true});
            merge(&mut settings, prefs.get("settings").unwrap_or(&Value::Null));
            settings["readReceipts"] = settings["read_receipts"].clone();
            Ok(settings)
        }
        ("PUT", "/u/settings") => {
            let mut value = value;
            if let Some(receipts) = value.get("readReceipts").cloned() {
                value["read_receipts"] = receipts;
            }
            app.patch_preferences(auth, json!({"settings":value}))
                .await?;
            Ok(json!({}))
        }
        ("GET", "/u/preferences") => {
            let mut prefs = json!({"favourite_chats":[],"unread_chats":[],"muted_chats":[],"archived_chats":[]});
            merge(&mut prefs, &app.preferences(auth).await?);
            Ok(prefs)
        }
        ("POST", "/u/logout") => {
            app.logout(auth).await?;
            Ok(json!({}))
        }
        ("GET", "/u/ws/auth") => Ok(json!({"token":app.websocket_token(auth).await?})),
        ("PUT", "/notifications/token") => {
            if value.get("pnstype").and_then(Value::as_str) != Some("ws") {
                return Err(Error::Unsupported);
            }
            // The desktop client selects its existing WebSocket transport here.
            // Accepting APNs/FCM would promise push delivery we do not provide.
            Ok(json!({}))
        }
        ("GET" | "POST", "/contacts") => {
            let users = app
                .users(auth)
                .await?
                .iter()
                .map(user_wire)
                .collect::<Vec<_>>();
            Ok(
                json!({"users":users,"organisations":[{"id":auth.principal.tenant_id,"name":"HooWire","federation_name":""}],
                "managed_groups":app.directory_entries(auth,"group").await?,"deleted_users":[],"deleted_groups":[],"force_resync":""}),
            )
        }
        ("POST", "/u/query") => {
            let text = value
                .get("query_string")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let max = value
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(1, 1000) as usize;
            let users = app
                .users(auth)
                .await?
                .iter()
                .filter(|u| u.profile.to_string().to_lowercase().contains(&text))
                .take(max)
                .map(user_wire)
                .collect::<Vec<_>>();
            Ok(json!({"results":users}))
        }
        ("GET", "/u/subscriptions") => subscriptions(app, auth, None).await,
        ("POST", "/u/subscriptions/matching_ids") => {
            let ids = uuid_list(value.get("thread_ids"))?;
            Ok(json!({"subscriptions":subscriptions(app,auth,Some(&ids)).await?}))
        }
        ("GET", "/feed") => {
            let after = cursor(headers)?;
            let messages = app
                .messages(auth, None, after, 1000)
                .await?
                .iter()
                .map(message_wire)
                .collect::<Vec<_>>();
            Ok(json!({"messages":messages,"read_threads":[]}))
        }
        ("PUT", "/t") => {
            legacy_chat_mode(&value)?;
            let chat = app.create_chat(auth, &value).await?;
            subscription(app, auth, &chat).await
        }
        ("POST", "/t") => {
            let msg = app.post_message(auth, &value).await?;
            Ok(
                json!({"tid":msg.chat_id,"mid":msg.id,"ts":timestamp(msg.created_at),"timestamp":msg.created_at.timestamp_micros() as f64 / 1_000_000.0}),
            )
        }
        ("GET", "/t/loc") => app.live_locations(auth, None).await,
        _ => dispatch_resource(app, auth, method, path, query, headers, value).await,
    }
}

async fn dispatch_resource(
    app: &App,
    auth: &Auth,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
    value: Value,
) -> Result<Value> {
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    let op = method.as_str();
    match segments.as_slice() {
        ["u", id, "info"] if op == "GET" => Ok(user_wire(&app.user(auth, identifier(id)?).await?)),
        ["u", "profile", id] => {
            let id = identifier(id)?;
            match op {
                "GET" => Ok(user_wire(&app.user(auth, id).await?)),
                "POST" if id == auth.principal.user_id => {
                    app.update_profile(auth, value).await?;
                    Ok(json!({}))
                }
                _ => Err(Error::Forbidden),
            }
        }
        ["t", id] if op == "GET" => {
            let id = identifier(id)?;
            app.chat(auth, id).await?;
            Ok(Value::Array(
                app.messages(auth, Some(id), cursor(headers)?, 1000)
                    .await?
                    .iter()
                    .map(message_wire)
                    .collect(),
            ))
        }
        ["t", id, "settings"] if op == "PUT" => {
            legacy_chat_mode(&value)?;
            app.update_chat(auth, identifier(id)?, value).await?;
            Ok(json!({}))
        }
        ["t", id, "members"] if op == "PUT" => {
            legacy_chat_mode(&value)?;
            let id = identifier(id)?;
            let version = value.get("version").and_then(Value::as_i64);
            app.replace_member_components(auth, id, &value, version)
                .await?;
            Ok(json!({}))
        }
        ["t", id, "subscription"] => {
            let id = identifier(id)?;
            match op {
                "DELETE" => {
                    app.leave_chat(auth, id, None).await?;
                    Ok(json!({}))
                }
                "PUT" => {
                    app.chat(auth, id).await?;
                    preference_value(app, auth, &format!("chat_{id}"), value).await?;
                    Ok(json!({}))
                }
                _ => Err(Error::Unsupported),
            }
        }
        ["t", id, "admin"] if op == "PUT" || op == "DELETE" => {
            let id = identifier(id)?;
            app.chat(auth, id).await?;
            let user = identifier(string(&value, "user")?)?;
            let mut admins = app.administrators(auth, id).await?;
            if op == "PUT" {
                if !admins.contains(&user) {
                    admins.push(user);
                }
            } else {
                admins.retain(|id| *id != user);
            }
            if admins.is_empty() {
                return Err(Error::Invalid("chat requires an administrator".into()));
            }
            app.update_chat(auth, id, json!({"admins":admins})).await?;
            Ok(json!({}))
        }
        ["t", id, "readers"] if op == "GET" || op == "POST" => {
            let id = identifier(id)?;
            app.chat(auth, id).await?;
            if op == "POST" {
                app.read_chat(
                    auth,
                    id,
                    value
                        .get("mid")
                        .and_then(Value::as_str)
                        .map(identifier)
                        .transpose()?,
                )
                .await?;
            }
            let readers = app
                .readers(auth, id)
                .await?
                .iter()
                .filter_map(|reader| reader["user_id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            Ok(json!({"readers":readers,"ts":timestamp(Utc::now())}))
        }
        ["t", tid, "m", mid] if op == "DELETE" => {
            let message = app.message(auth, identifier(mid)?).await?;
            if message.chat_id != identifier(tid)? {
                return Err(Error::NotFound);
            }
            app.delete_message(auth, message.id).await?;
            Ok(json!({}))
        }
        ["u", category] | ["u", category, _]
            if [
                "favouritechats",
                "unreadchats",
                "mutedchats",
                "archivedchats",
            ]
            .contains(category) =>
        {
            let key = match *category {
                "favouritechats" => "favourite_chats",
                "unreadchats" => "unread_chats",
                "mutedchats" => "muted_chats",
                _ => "archived_chats",
            };
            let tid = if segments.len() == 3 {
                identifier(segments[2])?
            } else {
                identifier(string(&value, "tid")?)?
            };
            app.chat(auth, tid).await?;
            if op != "POST" && op != "DELETE" {
                return Err(Error::Unsupported);
            }
            app.preference_list(auth, key, tid, op == "POST").await?;
            Ok(json!({}))
        }
        // Explicitly unsupported until their durable schemas and client contracts are implemented.
        _ => resources::dispatch(app, auth, method, path, query, value).await,
    }
}

pub fn user_wire(user: &User) -> Value {
    let mut profile =
        json!({"about_me":"","department":"","job_title":"","phone":"","account_email":""});
    merge(&mut profile, &user.profile);
    let first = user.profile["first_name"].as_str().unwrap_or("");
    let last = user.profile["last_name"].as_str().unwrap_or("");
    json!({"id":user.id,"uid":user.id,"user_id":user.id,"first_name":first,"last_name":last,
        "firstname":first,"lastname":last,"type":if user.is_bot {"bot"} else {"user"},"is_bot":user.is_bot,
        "organisation":user.tenant_id,"has_avatar":user.profile["has_avatar"].as_bool().unwrap_or(false),"avatar_modified_at":user.profile["avatar_modified_at"].as_i64().unwrap_or(0),"federation_name":"","profile":profile})
}

pub fn message_wire(message: &Message) -> Value {
    let mut value = message.attributes.clone();
    if !value.is_object() {
        value = json!({});
    }
    merge(
        &mut value,
        &json!({"mid":message.id,"tid":message.chat_id,"from":message.sender_id,"m":message.body,"ts":timestamp(message.created_at)}),
    );
    if let Some(cid) = &message.client_id {
        value["cid"] = json!(cid);
    }
    if message.deleted_at.is_some() {
        value["m"] = json!("");
        value["deleted"] = json!(true);
    }
    value
}

fn legacy_chat_mode(value: &Value) -> Result<()> {
    for field in ["chat_v", "chatV"] {
        if let Some(mode) = value.get(field) {
            let mode = mode
                .as_i64()
                .ok_or_else(|| Error::Invalid("chat protocol version must be an integer".into()))?;
            if mode > 0 {
                return Err(Error::Unsupported);
            }
        }
    }
    Ok(())
}

async fn subscription(app: &App, auth: &Auth, chat: &Chat) -> Result<Value> {
    let users = match chat.attributes.get("users") {
        Some(users) => uuid_list(Some(users))?,
        None => app.members(auth, chat.id).await?,
    };
    let groups = uuid_list(chat.attributes.get("groups"))?;
    let roles = uuid_list(chat.attributes.get("roles"))?;
    let admins = app.administrators(auth, chat.id).await?;
    let mut info = json!({"type":chat.kind,"title":chat.title,"admins":admins,"allow_mute_notifications":true,
        "allow_sending":true,"avatar_modified_at":0,"has_avatar":false,"is_closed":false,"is_federated":false,"is_public":false,"status":"readwrite"});
    for (src, dst) in [
        ("allowSending", "allow_sending"),
        ("allowMuteNotifications", "allow_mute_notifications"),
        ("isClosed", "is_closed"),
        ("hasAvatar", "has_avatar"),
        ("avatarModifiedAt", "avatar_modified_at"),
    ] {
        if let Some(value) = chat.attributes.get(src) {
            info[dst] = value.clone();
        }
    }
    let mut camel = info.clone();
    for (snake, name) in [
        ("allow_sending", "allowSending"),
        ("allow_mute_notifications", "allowMuteNotifications"),
        ("is_closed", "isClosed"),
        ("has_avatar", "hasAvatar"),
        ("avatar_modified_at", "avatarModifiedAt"),
        ("is_public", "isPublic"),
        ("is_federated", "isFederated"),
    ] {
        camel[name] = info[snake].clone();
    }
    camel["chatType"] = json!(chat.kind);
    // chatV selects the legacy protocol; the database revision is independent.
    Ok(
        json!({"tid":chat.id,"col":chat.col,"members":{"users":users,"groups":groups,"roles":roles},
        "thread_info":info,"threadInfo":camel,"ts":timestamp(chat.updated_at),"chat_v":0,"chatV":0,"version":chat.version}),
    )
}

async fn subscriptions(app: &App, auth: &Auth, ids: Option<&[Uuid]>) -> Result<Value> {
    let mut list = vec![];
    for chat in app.chats(auth).await? {
        if ids.is_none_or(|ids| ids.contains(&chat.id)) {
            list.push(subscription(app, auth, &chat).await?);
        }
    }
    Ok(Value::Array(list))
}

fn appsettings() -> Value {
    let mut settings = json!({"app_color":"#146EF5","logo":null,"logo_updated_at":0,"max_chat_members":1000,
        "max_allowed_pinned_messages_per_chat":5,"messages_retention_period":0,"roles_enabled":true,
        "show_contact_data":true,"platform":"hoowire","max_file_size":16*1024*1024,"roles_settings":{}});
    for key in [
        "allow_access_calendar",
        "allow_access_camera",
        "allow_access_location",
        "allow_access_microphone",
        "allow_access_photos",
        "allow_access_videos",
        "allow_attachments",
        "allow_copy_paste",
        "allow_file_sharing",
        "allow_close_chats",
        "allow_delete_messages",
        "allow_disable_read_receipts",
        "allow_interactive_attachments",
        "allow_invite_members",
        "allow_leave_conversation",
        "allow_notification_preview",
        "allow_open_in",
        "allow_reply_from_notifications",
        "enable_alerting",
        "enable_edit_user_profile_department",
        "enable_edit_user_profile_email",
        "enable_edit_user_profile_notes",
        "enable_edit_user_profile_phone",
        "enable_edit_user_profile_role",
        "enable_edit_user_profile_status",
        "enable_edit_user_profile_website",
    ] {
        settings[key] = json!(true);
    }
    for key in [
        "allow_voip_calls",
        "allow_voip_group_calls",
        "allow_live_location",
        "allow_push_to_talk",
        "allow_blocking_users",
        "allow_change_password",
        "allow_close_sessions",
        "allow_email_conversation",
        "enable_s_o_s_button",
        "enable_edit_login_password",
        "require_pin",
    ] {
        settings[key] = json!(false);
    }
    settings
}

fn decode(key: &hoowire_crypto::TransportKey, bytes: &[u8]) -> Result<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Invalid("expected base64 transport".into()))?;
    let data = key
        .decrypt_base64(text)
        .map_err(|_| Error::Invalid("invalid encrypted request".into()))?;
    serde_json::from_slice(&data).map_err(|_| Error::Invalid("invalid JSON request".into()))
}

fn encrypted(
    key: &hoowire_crypto::TransportKey,
    value: &Value,
    cursor: Option<DateTime<Utc>>,
) -> Result<Response<Body>> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Internal)?;
    let data = key.encrypt_base64(&bytes).map_err(|_| Error::Internal)?;
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CACHE_CONTROL, "private, no-store");
    if let Some(cursor) = cursor {
        builder = builder.header(
            header::ETAG,
            hex::encode(cursor.to_rfc3339_opts(SecondsFormat::Micros, true)),
        );
    }
    builder.body(Body::from(data)).map_err(|_| Error::Internal)
}

fn plain(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(value.to_string()))
        .expect("static HTTP headers")
}
fn error_response(error: Error) -> Response<Body> {
    let status = match &error {
        Error::Unauthorized | Error::Forbidden => StatusCode::FORBIDDEN,
        Error::NotFound => StatusCode::NOT_FOUND,
        Error::Conflict => StatusCode::CONFLICT,
        Error::Invalid(_) => StatusCode::BAD_REQUEST,
        Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        Error::Unsupported => StatusCode::NOT_IMPLEMENTED,
        Error::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    plain(status, json!({"error":error.to_string()}))
}
fn cursor(headers: &HeaderMap) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = headers.get(header::ETAG) else {
        return Ok(None);
    };
    let bytes = hex::decode(
        value
            .to_str()
            .map_err(|_| Error::Invalid("invalid ETag".into()))?,
    )
    .map_err(|_| Error::Invalid("invalid ETag".into()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid("invalid ETag".into()))?;
    Ok(Some(
        DateTime::parse_from_rfc3339(text)
            .map_err(|_| Error::Invalid("invalid cursor".into()))?
            .with_timezone(&Utc),
    ))
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing {key}")))
}
fn merge(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}
fn timestamp(time: DateTime<Utc>) -> f64 {
    time.timestamp_micros() as f64 / 1_000.0
}

async fn preference_value(app: &App, auth: &Auth, key: &str, value: Value) -> Result<()> {
    let mut patch = json!({});
    patch[key] = value;
    app.patch_preferences(auth, patch).await
}

async fn multipart_file(headers: &HeaderMap, bytes: Bytes) -> Result<(Vec<u8>, String)> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| Error::Invalid("expected multipart upload".into()))?;
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| Error::Invalid("missing multipart boundary".into()))?;
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut result = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| Error::Invalid("invalid multipart field".into()))?
    {
        if field.name() == Some("f") {
            if result.is_some() {
                return Err(Error::Invalid("duplicate file field".into()));
            }
            let mime = field
                .content_type()
                .map(ToString::to_string)
                .unwrap_or_else(|| "application/octet-stream".into());
            let bytes = field
                .bytes()
                .await
                .map_err(|_| Error::Invalid("invalid file".into()))?;
            if bytes.len() > hoowire_blob::MAX_ASSET_BYTES {
                return Err(Error::Invalid("asset exceeds 16 MiB".into()));
            }
            result = Some((bytes.to_vec(), mime));
        }
    }
    result.ok_or_else(|| Error::Invalid("missing multipart field f".into()))
}

async fn websocket(
    State(state): State<ApiState>,
    Path(token): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response<Body> {
    let Ok(permit) = state.websockets.clone().try_acquire_owned() else {
        return error_response(Error::Unavailable);
    };
    match state.app.websocket_auth(&token).await {
        Ok(auth) => {
            upgrade
                .max_message_size(2 * 1024 * 1024)
                .on_upgrade(move |socket| async move {
                    let _permit = permit;
                    websocket_loop(state, auth, socket, false).await;
                })
        }
        Err(error) => error_response(error),
    }
}

async fn websocket_sdk(
    State(state): State<ApiState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response<Body> {
    let Ok(permit) = state.websockets.clone().try_acquire_owned() else {
        return error_response(Error::Unavailable);
    };
    let Some(from) = headers.get("from").and_then(|v| v.to_str().ok()) else {
        return error_response(Error::Unauthorized);
    };
    match state.app.authenticate(from).await {
        Ok(auth) => {
            upgrade
                .max_message_size(2 * 1024 * 1024)
                .on_upgrade(move |socket| async move {
                    let _permit = permit;
                    websocket_loop(state, auth, socket, true).await;
                })
        }
        Err(error) => error_response(error),
    }
}

async fn websocket_loop(state: ApiState, auth: Auth, mut socket: WebSocket, binary: bool) {
    let mut shutdown = state.app.subscribe_shutdown();
    if !*shutdown.borrow() {
        // Cancel the whole session, including an in-flight DB query or event batch,
        // before borrowing the socket for the bounded close handshake.
        tokio::select! {
            biased;
            _ = shutdown.changed() => {}
            _ = websocket_session(state, auth, &mut socket, binary) => return,
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        socket
            .send(WsMessage::Close(Some(CloseFrame {
                code: 1001,
                reason: "Server shutdown".into(),
            })))
            .await?;
        while let Some(frame) = socket.recv().await {
            match frame {
                Ok(WsMessage::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        Ok::<(), axum::Error>(())
    })
    .await;
}

async fn websocket_session(state: ApiState, auth: Auth, socket: &mut WebSocket, binary: bool) {
    let mut after = 0;
    let mut catch_up = true;
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut broadcasts = state.broadcasts.subscribe();
    let mut changes = state.app.subscribe_changes();
    let mut topics = HashSet::new();
    let mut live_topics = HashSet::new();
    let mut message_types: HashSet<String> = HashSet::new();
    let mut last_seen = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { break; };
                if state.app.authenticate(&auth.from_id).await.is_err() { break; }
                last_seen = tokio::time::Instant::now();
                let value = match frame {
                    WsMessage::Close(_) => break,
                    WsMessage::Ping(bytes) => {
                        if socket.send(WsMessage::Pong(bytes)).await.is_err() { break; }
                        continue;
                    }
                    WsMessage::Text(text) => decode(&auth.key, text.as_bytes()),
                    WsMessage::Binary(bytes) => decode(&auth.key, &bytes),
                    _ => continue,
                };
                let Ok(value) = value else { break; };
                if value["type"] == "ping" {
                    if ws_send(socket, &auth, &json!({"type":"pong"}), binary).await.is_err() { break; }
                } else if value["type"] == "live_location" || value["type"] == "broadcast" {
                    let Some(tid) = value["tid"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else { continue; };
                    if state.app.chat(&auth, tid).await.is_err() { continue; }
                    if value["type"] == "live_location" {
                        match value["action"].as_str() {
                            Some("subscribe") => { live_topics.insert(tid); }
                            Some("unsubscribe") => { live_topics.remove(&tid); }
                            _ => {}
                        }
                    } else {
                        match value["action"].as_str() {
                            Some("subscribe") => { topics.insert(tid); }
                            Some("unsubscribe") => { topics.remove(&tid); }
                            Some("send") if ["user_typing", "user_recording_audio"].iter().any(|event| value["event"] == *event) => {
                                let _ = state.broadcasts.send((tid, auth.principal.user_id, json!({"type":"broadcast","content":{"tid":tid,"user_id":auth.principal.user_id,"event":value["event"]}})));
                            }
                            _ => {}
                        }
                    }
                } else if (value["action"] == "subscribe" || value["action"] == "unsubscribe")
                    && let Some(types) = value["message_types"].as_array() {
                    if types.len() > 64 || types.iter().any(|name| name.as_str().is_none_or(|name| name.len()>64)) {break;}
                    let subscribe = value["action"] == "subscribe";
                    for name in types.iter().filter_map(Value::as_str) {
                        if subscribe { message_types.insert(name.to_owned()); }
                        else { message_types.remove(name); }
                    }
                    if ws_send(socket, &auth, &json!({"type":if subscribe {"subscribed"} else {"unsubscribed"},"message_types":types}), binary).await.is_err() { break; }
                }
            }
            _ = async {
                if catch_up { return; }
                loop {
                    tokio::select!{
                        _=tick.tick()=>break,
                        result=changes.recv()=>match result{
                            Ok(user) if user==auth.principal.user_id=>break,
                            Err(broadcast::error::RecvError::Lagged(_))=>break,
                            Err(broadcast::error::RecvError::Closed)=>{tick.tick().await;break;},
                            _=>{},
                        }
                    }
                }
            }=>{
                if last_seen.elapsed()>Duration::from_secs(60){break;}
                match state.app.events(&auth,after).await {
                    Ok(events)=>{
                        catch_up=events.len()==1000;
                        for event in events {
                        after=event.id;
                        let frame = match event.kind.as_str() {
                            "message_created"|"message_updated"|"message_deleted"|"message"|"chat_message"=>{
                                let Some(id)=event.payload["message_id"].as_str().and_then(|id|Uuid::parse_str(id).ok()) else {continue;};
                                let Ok(message)=state.app.message(&auth,id).await else {continue;};
                                let content=message_wire(&message);
                                json!({"type":"chat_message","content":content})
                            }
                            "reaction"|"message_pinned"|"message_unpinned"|"live_location"=>{
                                let Some(id)=event.payload["tid"].as_str().and_then(|id|Uuid::parse_str(id).ok()) else {continue;};
                                if state.app.chat(&auth,id).await.is_err(){continue;}
                                if event.kind == "live_location" && !live_topics.contains(&id) {continue;}
                                json!({"type":match event.kind.as_str(){"reaction" if message_types.contains("chat_reaction")=>"chat_reaction","reaction"=>"reaction","live_location"=>"live_location",_=>"chat_message"},"content":event.payload})
                            }
                            "preferences_updated"=>json!({"type":"chat_preferences","content":event.payload}),
                            "thread_created"|"thread_updated"=>{
                                let Some(id)=event.payload["chat_id"].as_str().and_then(|id|Uuid::parse_str(id).ok()) else {continue;};
                                match state.app.chat(&auth,id).await {
                                    Ok(chat)=>match subscription(&state.app,&auth,&chat).await{Ok(content)=>json!({"type":"chat_update","content":content}),Err(_)=>continue},
                                    Err(_)=>json!({"type":"chat_update","content":{"tid":id,"deleted":true}}),
                                }
                            }
                            _=>json!({"type":"chat_update","content":event.payload}),
                        };
                        if (frame["type"] == "live_location" || frame["type"].as_str().is_some_and(|name|message_types.contains(name))) && ws_send(socket,&auth,&frame,binary).await.is_err(){return;}
                        }
                    },
                    Err(_)=>break,
                }
            }
            result=broadcasts.recv()=>{
                if let Ok((tid,sender,frame))=result
                    && sender!=auth.principal.user_id && topics.contains(&tid) {
                    if state.app.authenticate(&auth.from_id).await.is_err(){break;}
                    if state.app.chat(&auth,tid).await.is_ok() && ws_send(socket,&auth,&frame,binary).await.is_err(){break;}
                }
            }
        }
    }
}

async fn ws_send(socket: &mut WebSocket, auth: &Auth, value: &Value, binary: bool) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Internal)?;
    let data = auth
        .key
        .encrypt_base64(&bytes)
        .map_err(|_| Error::Internal)?;
    let message = if binary {
        WsMessage::Binary(data.into_bytes().into())
    } else {
        WsMessage::Text(data.into())
    };
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(|_| Error::Unavailable)
}
