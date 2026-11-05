//! Real HTTP, WebSocket, encrypted transports and PostgreSQL, with isolated users.
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_service::{App, DEFAULT_TENANT};
use k256::{SecretKey, elliptic_curve::sec1::ToEncodedPoint};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::PathBuf, time::Duration};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

struct Server {
    app: App,
    repo: Repository,
    url: String,
    task: tokio::task::JoinHandle<()>,
    data: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}
impl Server {
    async fn start() -> Self {
        let url =
            std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL URL required");
        let repo = Repository::connect(&url, 12).await.unwrap();
        repo.migrate().await.unwrap();
        let data = std::env::temp_dir().join(format!("hoowire-api-test-{}", Uuid::new_v4()));
        let app = App::new(
            repo.clone(),
            BlobStore::open(&data).await.unwrap(),
            &[0x42; 32],
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = hoowire_api::router(app.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            app,
            repo,
            url,
            task,
            data,
        }
    }
    async fn bot(&self, name: &str) -> (Uuid, String) {
        let email = format!("{}@example.invalid", Uuid::new_v4());
        let user = self
            .app
            .provision_user(
                DEFAULT_TENANT,
                &email,
                Zeroizing::new("long-test-password!".into()),
                name,
                "",
                true,
            )
            .await
            .unwrap();
        let appid = Uuid::new_v4().to_string();
        self.app
            .provision_integration(&user, &appid, SECRET)
            .await
            .unwrap();
        (user.id, appid)
    }
    async fn request(
        &self,
        appid: &str,
        method: Method,
        path: &str,
        value: Option<Value>,
    ) -> reqwest::Response {
        let key = TransportKey::from_secret(SECRET).unwrap();
        let mut req = Client::new()
            .request(method, format!("{}/v20{path}", self.url))
            .header("From", appid);
        if let Some(value) = value {
            req = req.header("Content-Type", "application/octet-stream").body(
                key.encrypt_base64(&serde_json::to_vec(&value).unwrap())
                    .unwrap(),
            );
        }
        req.send().await.unwrap()
    }
    async fn call(&self, appid: &str, method: Method, path: &str, value: Option<Value>) -> Value {
        let response = self.request(appid, method, path, value).await;
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        serde_json::from_slice(
            &TransportKey::from_secret(SECRET)
                .unwrap()
                .decrypt_base64(&body)
                .unwrap(),
        )
        .unwrap()
    }
    async fn chat(&self, owner: &str, users: &[Uuid]) -> Uuid {
        let response=self.call(owner,Method::PUT,"/t/?legacy_circle_ids=true",Some(json!({"chatType":"group","col":SECRET,"title":"Interop","users":users,"groups":[],"roles":[]}))).await;
        // Desktop 5.0.1 disables the composer for every other status literal.
        assert_eq!(response["thread_info"]["status"], "readwrite");
        assert_eq!(response["threadInfo"]["status"], "readwrite");
        assert_eq!(response["chat_v"], 0);
        assert_eq!(response["chatV"], 0);
        assert_eq!(response["version"], 1);
        Uuid::parse_str(response["tid"].as_str().unwrap()).unwrap()
    }
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn encrypted_chat_persistence_permissions_idempotency_and_feed_cursor() {
    let server = Server::start().await;
    let (alice, a) = server.bot("Alice").await;
    let (bob, b) = server.bot("Bob").await;
    let (_, c) = server.bot("Outsider").await;
    let tid = server.chat(&a, &[alice, bob]).await;
    let key = TransportKey::from_secret(SECRET).unwrap();
    let encrypted = key.encrypt_base64("Hallo 🌍".as_bytes()).unwrap();
    let cid = Uuid::new_v4().to_string();
    let payload = json!({"tid":tid,"cid":cid,"msg":encrypted,"isAlert":false});
    let sent = server
        .call(&a, Method::POST, "/t", Some(payload.clone()))
        .await;
    assert!(sent["ts"].as_f64().unwrap() > 1_700_000_000_000.0);
    assert!(
        (sent["ts"].as_f64().unwrap() / 1000.0 - sent["timestamp"].as_f64().unwrap()).abs() < 0.001
    );
    let retry = server.call(&a, Method::POST, "/t", Some(payload)).await;
    assert_eq!(sent["mid"], retry["mid"]);
    let conflict = server
        .request(
            &a,
            Method::POST,
            "/t",
            Some(json!({"tid":tid,"cid":cid,"msg":"other"})),
        )
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let history = server
        .call(&b, Method::GET, &format!("/t/{tid}/"), None)
        .await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["m"], encrypted);
    assert_eq!(
        key.decrypt_base64(history[0]["m"].as_str().unwrap())
            .unwrap(),
        "Hallo 🌍".as_bytes()
    );
    let unauthorized = server
        .request(&c, Method::GET, &format!("/t/{tid}"), None)
        .await;
    assert!(matches!(
        unauthorized.status(),
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    let feed = server.request(&b, Method::GET, "/feed", None).await;
    assert_eq!(feed.status(), StatusCode::OK);
    let cursor = feed
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let feed: Value =
        serde_json::from_slice(&key.decrypt_base64(&feed.text().await.unwrap()).unwrap()).unwrap();
    assert!(
        feed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["mid"] == sent["mid"])
    );
    let response = Client::new()
        .get(format!("{}/v20/feed", server.url))
        .header("From", &b)
        .header("ETag", cursor)
        .send()
        .await
        .unwrap();
    let next: Value =
        serde_json::from_slice(&key.decrypt_base64(&response.text().await.unwrap()).unwrap())
            .unwrap();
    assert_eq!(next["messages"], json!([]));
    let stored = server.repo.chat_for(alice, tid).await.unwrap();
    assert!(stored.col.starts_with("v1:"));
    assert_ne!(stored.col, SECRET);
    let creds = server.repo.integration(&a).await.unwrap();
    assert!(creds.secret.starts_with("v1:"));
    assert_ne!(creds.secret, SECRET);
    let reconnected = Repository::connect(&std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap(), 2)
        .await
        .unwrap();
    assert_eq!(
        reconnected
            .messages(bob, Some(tid), None, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    reconnected.close().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn native_chat_mode_is_separate_from_optimistic_revision() {
    let server = Server::start().await;
    let (alice, appid) = server.bot("Chat mode Alice").await;
    let (bob, _) = server.bot("Chat mode Bob").await;
    let tid = server.chat(&appid, &[alice, bob]).await;
    server
        .call(
            &appid,
            Method::PUT,
            &format!("/t/{tid}/settings"),
            Some(json!({"chatV":0,"version":1,"title":"Changed title"})),
        )
        .await;
    let subscriptions = server
        .call(&appid, Method::GET, "/u/subscriptions", None)
        .await;
    let chat = subscriptions
        .as_array()
        .unwrap()
        .iter()
        .find(|chat| chat["tid"] == json!(tid))
        .unwrap();
    assert_eq!(chat["chatV"], 0);
    assert_eq!(chat["chat_v"], 0);
    assert_eq!(chat["version"], 2);
    let replacement = json!({"chatV":0,"version":2,"users":[alice,bob],"groups":[],"roles":[]});
    let response = server
        .request(
            &appid,
            Method::PUT,
            &format!("/t/{tid}/members"),
            Some(replacement.clone()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let stale = server
        .request(
            &appid,
            Method::PUT,
            &format!("/t/{tid}/members"),
            Some(replacement),
        )
        .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(server.repo.chat_for(alice, tid).await.unwrap().version, 3);
    let unsupported = server
        .request(
            &appid,
            Method::PUT,
            &format!("/t/{tid}/settings"),
            Some(json!({"chatV":1,"title":"Unsupported mode"})),
        )
        .await;
    assert_eq!(unsupported.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(server.repo.chat_for(alice, tid).await.unwrap().version, 3);
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn human_ecdh_login_and_logout_over_actual_http() {
    let server = Server::start().await;
    let email = format!("{}@example.invalid", Uuid::new_v4());
    let user = server
        .app
        .provision_user(
            DEFAULT_TENANT,
            &email,
            Zeroizing::new("a-long-human-password!".into()),
            "Human",
            "Account",
            false,
        )
        .await
        .unwrap();
    let private = SecretKey::random(&mut rand::rngs::OsRng);
    let public = hex::encode(private.public_key().to_encoded_point(false).as_bytes());
    let client = Client::new();
    let handshake: Value = client
        .post(format!("{}/v20/u/sessions/", server.url))
        .json(&json!({"client_pubkey":public}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key = TransportKey::from_ecdh(
        private.to_bytes().as_ref(),
        handshake["server_pubkey"].as_str().unwrap(),
    )
    .unwrap();
    let login = json!({"email_hash":hex::encode(Sha256::digest(email.as_bytes())),"password":"a-long-human-password!"});
    let response = client
        .post(format!(
            "{}/v20/u/login/{}/",
            server.url,
            handshake["session_id"].as_str().unwrap()
        ))
        .body(
            key.encrypt_base64(&serde_json::to_vec(&login).unwrap())
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let login: Value =
        serde_json::from_slice(&key.decrypt_base64(&response.text().await.unwrap()).unwrap())
            .unwrap();
    assert_eq!(login["owner_id"], json!(user.id));
    assert_eq!(login["owner_type"], "user");
    let from = login["from_id"].as_str().unwrap();
    let settings = client
        .get(format!("{}/v20/u/appsettings", server.url))
        .header("From", from)
        .send()
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);
    let response = client
        .post(format!("{}/v20/u/logout", server.url))
        .header("From", from)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = client
        .get(format!("{}/v20/u/name", server.url))
        .header("From", from)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn websocket_tickets_work_as_raw_url_segments_and_are_single_use() {
    let server = Server::start().await;
    let (_, appid) = server.bot("Native ticket").await;
    let mut tickets = HashSet::new();
    // Multiple draws cover the intermittent slash/plus failure of standard
    // Base64 tickets when a native client constructs an unescaped path.
    for _ in 0..32 {
        let response = server.call(&appid, Method::GET, "/u/ws/auth", None).await;
        let token = response["token"].as_str().unwrap();
        assert_eq!(token.len(), 43);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "ticket must be an unescaped URL segment"
        );
        assert!(tickets.insert(token.to_owned()));
        let url = format!("{}/ws/v20/{token}", server.url.replace("http://", "ws://"));
        let (mut socket, response) = connect_async(&url).await.unwrap();
        assert_eq!(response.status().as_u16(), 101);
        socket.close(None).await.unwrap();
        let error = connect_async(&url).await.unwrap_err();
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), 403);
            }
            other => panic!("expected an authentication rejection, got {other}"),
        }
    }
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn websocket_sdk_binary_subscription_and_real_message_delivery() {
    let server = Server::start().await;
    let (alice, a) = server.bot("WS Alice").await;
    let (bob, b) = server.bot("WS Bob").await;
    let tid = server.chat(&a, &[alice, bob]).await;
    let mut request = format!("{}/ws/v20/", server.url.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert("From", b.parse().unwrap());
    let (mut ws, _) = connect_async(request).await.unwrap();
    let key = TransportKey::from_secret(SECRET).unwrap();
    let subscribe = key
        .encrypt_base64(br#"{"action":"subscribe","message_types":["chat_message","chat_update"]}"#)
        .unwrap();
    ws.send(Message::Binary(subscribe.into_bytes().into()))
        .await
        .unwrap();
    let sent=server.call(&a,Method::POST,"/t",Some(json!({"tid":tid,"msg":key.encrypt_base64(b"live message").unwrap(),"cid":Uuid::new_v4()}))).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = ws.next().await.unwrap().unwrap();
            let bytes = frame.into_data();
            let value: Value = serde_json::from_slice(
                &key.decrypt_base64(std::str::from_utf8(&bytes).unwrap())
                    .unwrap(),
            )
            .unwrap();
            if value["type"] == "chat_message" && value["content"]["mid"] == sent["mid"] {
                break;
            }
        }
    })
    .await
    .expect("durable outbox event delivered to real WebSocket peer");
    ws.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn opaque_asset_upload_download_and_access_control() {
    let server = Server::start().await;
    let (alice, a) = server.bot("Asset Alice").await;
    let (bob, b) = server.bot("Asset Bob").await;
    let (_, c) = server.bot("No access").await;
    let tid = server.chat(&a, &[alice, bob]).await;
    let key = TransportKey::from_secret(SECRET).unwrap();
    let bytes = key.encrypt(&[0, 1, 2, 3, 255]).unwrap();
    let form = reqwest::multipart::Form::new().part(
        "f",
        reqwest::multipart::Part::bytes(bytes.clone()).file_name("test.bin"),
    );
    let response = Client::new()
        .post(format!("{}/v20/upload?tid={tid}", server.url))
        .header("From", &a)
        .multipart(form)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let asset: Value = serde_json::from_str(&body).unwrap();
    let id = asset["id"].as_str().unwrap();
    let response = server
        .request(&b, Method::GET, &format!("/serve/{id}"), None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    let response = server
        .request(&c, Method::GET, &format!("/serve/{id}"), None)
        .await;
    assert!(matches!(
        response.status(),
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    assert_eq!(STANDARD.encode(key.decrypt(&bytes).unwrap()), "AAECA/8=");
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn avatar_multipart_raw_envelope_metadata_and_access_control() {
    let server = Server::start().await;
    let (alice, a) = server.bot("Avatar Alice").await;
    let (bob, b) = server.bot("Avatar Bob").await;
    let (_, outsider) = server.bot("Avatar Outsider").await;
    let chat = server.chat(&a, &[alice, bob]).await;
    let png = include_bytes!("../../../tests/fixtures/pixel.png");
    for (upload, download) in [
        (
            format!("/u/profile/{alice}/image/upload"),
            format!("/u/profile/{alice}/image"),
        ),
        (format!("/t/{chat}/avatar"), format!("/t/{chat}/avatar")),
    ] {
        let form = reqwest::multipart::Form::new().part(
            "f",
            reqwest::multipart::Part::bytes(png.to_vec())
                .file_name("f")
                .mime_str("image/png")
                .unwrap(),
        );
        let response = Client::new()
            .post(format!("{}/v20{upload}", server.url))
            .header("From", &a)
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{upload}");
        let response = server.request(&b, Method::HEAD, &download, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.bytes().await.unwrap().is_empty());
        let response = server.request(&b, Method::GET, &download, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let envelope = response.bytes().await.unwrap();
        assert_eq!(
            TransportKey::from_secret(SECRET)
                .unwrap()
                .decrypt(&envelope)
                .unwrap(),
            png
        );
        assert_eq!(
            server
                .request(&b, Method::DELETE, &download, None)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        server
            .request(&outsider, Method::GET, &format!("/t/{chat}/avatar"), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    server
        .call(
            &a,
            Method::POST,
            &format!("/u/profile/{alice}"),
            Some(json!({"has_avatar":false,"avatar_modified_at":-1,"about_me":"kept"})),
        )
        .await;
    let profile = server
        .call(&b, Method::GET, &format!("/u/profile/{alice}"), None)
        .await;
    assert_eq!(profile["has_avatar"], true);
    assert!(profile["avatar_modified_at"].as_i64().unwrap() > 0);
    assert_eq!(profile["profile"]["about_me"], "kept");
    server
        .call(
            &a,
            Method::DELETE,
            &format!("/u/profile/{alice}/image"),
            None,
        )
        .await;
    assert_eq!(
        server
            .request(&a, Method::HEAD, &format!("/u/profile/{alice}/image"), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let profile = server
        .call(&a, Method::GET, &format!("/u/profile/{alice}"), None)
        .await;
    assert_eq!(profile["has_avatar"], false);
    server.app.close().await;
}
