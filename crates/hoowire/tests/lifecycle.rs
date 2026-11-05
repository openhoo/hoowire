//! Human sessions use the public encrypted HTTP protocol against the real CLI.
#![cfg(unix)]

mod common;

use common::{MASTER_KEY, SECRET, ServerProcess};
use futures_util::{SinkExt, StreamExt};
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_domain::User;
use hoowire_service::App;
use reqwest::{Method, Response, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    net::TcpStream,
    time::{Instant, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use uuid::Uuid;
use zeroize::Zeroizing;

const PASSWORD: &str = "a-long-process-test-password!";
// SEC1 public key for private scalar 2, independently known secp256k1 vector.
const CLIENT_PUBLIC: &str = concat!(
    "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct HumanSession {
    key: TransportKey,
    from: String,
}

impl HumanSession {
    async fn login(server: &ServerProcess, user: &User) -> Self {
        let response = server
            .http
            .post(server.url("/v20/u/sessions/"))
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(&json!({"client_pubkey":CLIENT_PUBLIC})).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let handshake: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        let mut private = [0; 32];
        private[31] = 2;
        let key = TransportKey::from_ecdh(&private, handshake["server_pubkey"].as_str().unwrap())
            .unwrap();
        let credentials = json!({"email_hash":user.email_hash,"password":PASSWORD});
        let response = server
            .http
            .post(server.url(&format!(
                "/v20/u/login/{}/",
                handshake["session_id"].as_str().unwrap()
            )))
            .header("Content-Type", "application/octet-stream")
            .body(
                key.encrypt_base64(&serde_json::to_vec(&credentials).unwrap())
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
        Self {
            key,
            from: login["from_id"].as_str().unwrap().to_owned(),
        }
    }

    async fn request(
        &self,
        server: &ServerProcess,
        method: Method,
        path: &str,
        value: Option<Value>,
    ) -> Response {
        let mut request = server
            .http
            .request(method, server.url(&format!("/v20{path}")))
            .header("From", &self.from);
        if let Some(value) = value {
            request = request
                .header("Content-Type", "application/octet-stream")
                .body(
                    self.key
                        .encrypt_base64(&serde_json::to_vec(&value).unwrap())
                        .unwrap(),
                );
        }
        request.send().await.unwrap()
    }

    async fn call(
        &self,
        server: &ServerProcess,
        method: Method,
        path: &str,
        value: Option<Value>,
    ) -> Value {
        let response = self.request(server, method, path, value).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        serde_json::from_slice(
            &self
                .key
                .decrypt_base64(&response.text().await.unwrap())
                .unwrap(),
        )
        .unwrap()
    }

    async fn subscribe(&self, server: &ServerProcess) -> Socket {
        let ticket = self.call(server, Method::GET, "/u/ws/auth", None).await;
        let (mut socket, response) = timeout(
            Duration::from_secs(3),
            connect_async(format!(
                "ws://{}/ws/v20/{}",
                server.address,
                ticket["token"].as_str().unwrap()
            )),
        )
        .await
        .expect("human WebSocket upgrade timeout")
        .unwrap();
        assert_eq!(response.status().as_u16(), 101);
        let subscription = self
            .key
            .encrypt_base64(br#"{"action":"subscribe","message_types":["chat_message"]}"#)
            .unwrap();
        timeout(
            Duration::from_secs(3),
            socket.send(Message::Text(subscription.into())),
        )
        .await
        .unwrap()
        .unwrap();
        let ack = self.event(&mut socket, "subscribed", None).await;
        assert_eq!(ack["message_types"], json!(["chat_message"]));
        socket
    }

    async fn event(&self, socket: &mut Socket, kind: &str, message_id: Option<&Value>) -> Value {
        timeout(Duration::from_secs(5), async {
            loop {
                let frame = socket
                    .next()
                    .await
                    .expect("human WebSocket closed before event")
                    .unwrap();
                let text = match frame {
                    Message::Text(text) => text,
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => panic!("human WebSocket must deliver encrypted text frames"),
                };
                let value: Value =
                    serde_json::from_slice(&self.key.decrypt_base64(text.as_str()).unwrap())
                        .unwrap();
                if value["type"] == kind
                    && message_id.is_none_or(|id| value["content"]["mid"] == *id)
                {
                    return value;
                }
            }
        })
        .await
        .expect("human WebSocket event timeout")
    }

    async fn upload_private(&self, server: &ServerProcess, encrypted: &[u8]) -> Value {
        let boundary = format!("hoowire-e2e-{}", Uuid::new_v4());
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"proof.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(encrypted);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = server
            .http
            .post(server.url("/v20/upload"))
            .header("From", &self.from)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_str(&response.text().await.unwrap()).unwrap()
    }
}

async fn provision_human(app: &App, tenant: Uuid, name: &str) -> User {
    app.provision_user(
        tenant,
        &format!("{}@example.invalid", Uuid::new_v4()),
        Zeroizing::new(PASSWORD.into()),
        name,
        "Process test",
        false,
    )
    .await
    .unwrap()
}

async fn create_tenant(server: &ServerProcess, name: &str) -> Uuid {
    let output = server.cli(&["create-tenant", "--name", name], &[]).await;
    assert_eq!(output.status.code(), Some(0));
    Uuid::parse_str(std::str::from_utf8(&output.stdout).unwrap().trim()).unwrap()
}

async fn provisioning_app(server: &ServerProcess) -> App {
    let migration = server.cli(&["migrate"], &[]).await;
    assert_eq!(migration.status.code(), Some(0));
    App::new(
        Repository::connect(&server.database_url, 4).await.unwrap(),
        BlobStore::open(&server.data).await.unwrap(),
        &MASTER_KEY,
    )
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn human_chat_text_and_file_survive_process_restart_and_logout_is_persistent() {
    let mut server = ServerProcess::new();
    let app = provisioning_app(&server).await;
    let tenant = create_tenant(&server, "Process E2E").await;
    let alice = provision_human(&app, tenant, "Alice").await;
    let bob = provision_human(&app, tenant, "Bob").await;
    app.close().await;
    server.start().await;
    let alice_session = HumanSession::login(&server, &alice).await;
    let bob_session = HumanSession::login(&server, &bob).await;
    let contacts = alice_session
        .call(&server, Method::GET, "/contacts", None)
        .await;
    assert!(
        contacts["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|user| user["id"] == json!(bob.id))
    );
    let name = bob_session
        .call(&server, Method::GET, "/u/name", None)
        .await;
    assert_eq!(name["first_name"], "Bob");

    let chat = alice_session
        .call(
            &server,
            Method::PUT,
            "/t/?legacy_circle_ids=true",
            Some(json!({"chatType":"group","col":SECRET,"title":"Real process chat","users":[alice.id,bob.id],"groups":[],"roles":[]})),
        )
        .await;
    assert_eq!(chat["thread_info"]["status"], "readwrite");
    assert_eq!(chat["chat_v"], 0);
    let tid = chat["tid"].as_str().unwrap();
    let chat_key = TransportKey::from_secret(chat["col"].as_str().unwrap()).unwrap();
    let mut bob_socket = bob_session.subscribe(&server).await;
    let plaintext = "Hallo Bob — real process, ECDH and nested chat encryption 🦉";
    let encrypted_text = chat_key.encrypt_base64(plaintext.as_bytes()).unwrap();
    let text_message = alice_session
        .call(
            &server,
            Method::POST,
            "/t",
            Some(json!({"tid":tid,"cid":Uuid::new_v4(),"msg":encrypted_text})),
        )
        .await;
    let delivered = bob_session
        .event(&mut bob_socket, "chat_message", Some(&text_message["mid"]))
        .await;
    assert_eq!(delivered["content"]["from"], json!(alice.id));
    assert_eq!(
        chat_key
            .decrypt_base64(delivered["content"]["m"].as_str().unwrap())
            .unwrap(),
        plaintext.as_bytes()
    );

    let file: Vec<u8> = (0..=255).collect();
    let encrypted_file = chat_key.encrypt(&file).unwrap();
    let asset = alice_session.upload_private(&server, &encrypted_file).await;
    let asset_path = format!("/serve/{}", asset["id"].as_str().unwrap());
    assert_eq!(
        bob_session
            .request(&server, Method::GET, &asset_path, None)
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "a private upload is not shared before its message commits"
    );
    let files = serde_json::to_string(&json!([{
        "id":asset["id"],"origin":"asset","v":"2","type":"file","extension":"bin",
        "mimeType":"application/octet-stream","size":file.len(),
        "name":chat_key.encrypt_base64(b"proof.bin").unwrap()
    }]))
    .unwrap();
    let file_message = alice_session
        .call(
            &server,
            Method::POST,
            "/t",
            Some(json!({"tid":tid,"cid":Uuid::new_v4(),"msg":"","files":files})),
        )
        .await;
    let delivered = bob_session
        .event(&mut bob_socket, "chat_message", Some(&file_message["mid"]))
        .await;
    assert_eq!(delivered["content"]["files"], files);
    let download = bob_session
        .request(&server, Method::GET, &asset_path, None)
        .await;
    assert_eq!(download.status(), StatusCode::OK);
    let download = download.bytes().await.unwrap();
    assert_eq!(download.as_ref(), encrypted_file);
    assert_eq!(chat_key.decrypt(&download).unwrap(), file);

    server.terminate();
    timeout(Duration::from_secs(3), async {
        loop {
            if let Message::Close(Some(frame)) = bob_socket.next().await.unwrap().unwrap() {
                assert_eq!(u16::from(frame.code), 1001);
                bob_socket.flush().await.unwrap();
                break;
            }
        }
    })
    .await
    .expect("human WebSocket must close during restart");
    assert_eq!(
        server
            .wait_until(Instant::now() + Duration::from_secs(5))
            .await
            .code(),
        Some(0)
    );
    drop(bob_socket);
    server.start().await;

    // Reuse the original From tokens and ECDH-derived session keys after restart.
    let history = bob_session
        .call(&server, Method::GET, &format!("/t/{tid}/"), None)
        .await;
    let history = history.as_array().unwrap();
    assert_eq!(history.len(), 2);
    let text = history
        .iter()
        .find(|message| message["mid"] == text_message["mid"])
        .unwrap();
    assert_eq!(text["m"], encrypted_text);
    let attachment = history
        .iter()
        .find(|message| message["mid"] == file_message["mid"])
        .unwrap();
    assert_eq!(attachment["files"], files);
    let subscriptions = bob_session
        .call(&server, Method::GET, "/u/subscriptions", None)
        .await;
    let restored_chat = subscriptions
        .as_array()
        .unwrap()
        .iter()
        .find(|chat| chat["tid"] == tid)
        .unwrap();
    assert_eq!(restored_chat["col"], SECRET);
    let download = bob_session
        .request(&server, Method::GET, &asset_path, None)
        .await;
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(
        chat_key.decrypt(&download.bytes().await.unwrap()).unwrap(),
        file
    );
    let mut restored_socket = bob_session.subscribe(&server).await;
    let after_restart = alice_session
        .call(
            &server,
            Method::POST,
            "/t",
            Some(json!({"tid":tid,"cid":Uuid::new_v4(),"msg":chat_key.encrypt_base64(b"after restart").unwrap()})),
        )
        .await;
    let delivered = bob_session
        .event(
            &mut restored_socket,
            "chat_message",
            Some(&after_restart["mid"]),
        )
        .await;
    assert_eq!(
        chat_key
            .decrypt_base64(delivered["content"]["m"].as_str().unwrap())
            .unwrap(),
        b"after restart"
    );

    alice_session
        .call(&server, Method::POST, "/u/logout", None)
        .await;
    for path in ["/u/name", "/u/ws/auth", asset_path.as_str()] {
        assert_eq!(
            alice_session
                .request(&server, Method::GET, path, None)
                .await
                .status(),
            StatusCode::FORBIDDEN,
            "revoked session: {path}"
        );
    }
    assert_eq!(
        bob_session
            .call(&server, Method::GET, "/u/name", None)
            .await["first_name"],
        "Bob"
    );
    restored_socket.close(None).await.unwrap();
    drop(restored_socket);
    server.stop().await;
    server.start().await;
    assert_eq!(
        alice_session
            .request(&server, Method::GET, "/u/name", None)
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "logout revocation must survive another restart"
    );
    bob_session
        .call(&server, Method::GET, "/u/name", None)
        .await;
    server.stop().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn real_process_rejects_other_users_and_tenants_without_sharing_their_data() {
    let mut server = ServerProcess::new();
    let app = provisioning_app(&server).await;
    let tenant = create_tenant(&server, "Process ACL local").await;
    let foreign_tenant = create_tenant(&server, "Process ACL foreign").await;
    let alice = provision_human(&app, tenant, "Alice").await;
    let bob = provision_human(&app, tenant, "Bob").await;
    let charlie = provision_human(&app, tenant, "Charlie").await;
    let mallory = provision_human(&app, foreign_tenant, "Mallory").await;
    app.close().await;
    server.start().await;
    let alice_session = HumanSession::login(&server, &alice).await;
    let bob_session = HumanSession::login(&server, &bob).await;
    let charlie_session = HumanSession::login(&server, &charlie).await;
    let mallory_session = HumanSession::login(&server, &mallory).await;
    let chat = alice_session
        .call(
            &server,
            Method::PUT,
            "/t",
            Some(json!({"chatType":"group","col":SECRET,"title":"Private","users":[alice.id,bob.id]})),
        )
        .await;
    let tid = chat["tid"].as_str().unwrap();
    let chat_key = TransportKey::from_secret(SECRET).unwrap();
    let file = chat_key.encrypt(b"private process fixture").unwrap();
    let asset = alice_session.upload_private(&server, &file).await;
    let asset_path = format!("/serve/{}", asset["id"].as_str().unwrap());
    let files = serde_json::to_string(&json!([{"id":asset["id"],"origin":"asset"}])).unwrap();
    alice_session
        .call(
            &server,
            Method::POST,
            "/t",
            Some(json!({"tid":tid,"cid":Uuid::new_v4(),"msg":chat_key.encrypt_base64(b"private text").unwrap(),"files":files})),
        )
        .await;
    assert_eq!(
        bob_session
            .request(&server, Method::GET, &asset_path, None)
            .await
            .status(),
        StatusCode::OK
    );
    for outsider in [&charlie_session, &mallory_session] {
        for path in [format!("/t/{tid}"), asset_path.clone()] {
            let response = outsider.request(&server, Method::GET, &path, None).await;
            assert!(
                matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
                ),
                "outsider read {path}"
            );
        }
        let response = outsider
            .request(
                &server,
                Method::POST,
                "/t",
                Some(json!({"tid":tid,"msg":"forged"})),
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = outsider
            .request(
                &server,
                Method::PUT,
                &format!("/t/{tid}/settings"),
                Some(json!({"title":"hijacked"})),
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            outsider
                .call(&server, Method::GET, "/u/subscriptions", None)
                .await,
            json!([])
        );
    }
    let response = charlie_session
        .request(
            &server,
            Method::POST,
            &format!("/u/profile/{}", bob.id),
            Some(json!({"first_name":"hijacked"})),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = mallory_session
        .request(
            &server,
            Method::GET,
            &format!("/u/profile/{}", alice.id),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let contacts = mallory_session
        .call(&server, Method::GET, "/contacts", None)
        .await;
    let users = contacts["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["id"], json!(mallory.id));
    let response = alice_session
        .request(&server, Method::PUT, "/t", Some(json!({"chatType":"group","col":SECRET,"title":"Cross tenant","users":[mallory.id]})))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let subscriptions = alice_session
        .call(&server, Method::GET, "/u/subscriptions", None)
        .await;
    assert_eq!(
        subscriptions.as_array().unwrap().len(),
        1,
        "failed cross-tenant creation must not leave a chat"
    );
    assert_eq!(subscriptions[0]["thread_info"]["title"], "Private");
    assert_eq!(
        bob_session
            .call(&server, Method::GET, "/u/name", None)
            .await["first_name"],
        "Bob"
    );

    let own_chat = charlie_session
        .call(
            &server,
            Method::PUT,
            "/t",
            Some(json!({"chatType":"group","col":SECRET,"title":"Outsider own chat","users":[]})),
        )
        .await;
    let own_tid = own_chat["tid"].as_str().unwrap();
    let response = charlie_session
        .request(
            &server,
            Method::POST,
            "/t",
            Some(json!({"tid":own_tid,"msg":"","files":files})),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "guessing an asset ID must not share another user's private attachment"
    );
    assert_eq!(
        charlie_session
            .call(&server, Method::GET, &format!("/t/{own_tid}"), None)
            .await,
        json!([]),
        "rejected forged sharing must not create a message"
    );
    assert_eq!(
        charlie_session
            .request(&server, Method::GET, &asset_path, None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    server.stop().await;
}
