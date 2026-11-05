//! Multiple real processes share durable sessions, events and one asset store.
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
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WsError, Message, error::ProtocolError},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const PASSWORD: &str = "isolated-multi-instance-process-password!";
const CLIENT_PUBLIC: &str = concat!(
    "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Replicas {
    // Struct fields drop in declaration order: stop the borrowing peer before
    // the owner stops and removes their shared asset directory, even on panic.
    peer: ServerProcess,
    owner: ServerProcess,
    alice: User,
    bob: User,
}

impl Replicas {
    async fn start() -> Self {
        let mut owner = ServerProcess::new();
        assert_eq!(owner.cli(&["migrate"], &[]).await.status.code(), Some(0));
        let output = owner
            .cli(
                &["create-tenant", "--name", "Multi-instance process E2E"],
                &[],
            )
            .await;
        assert_eq!(output.status.code(), Some(0));
        let tenant = Uuid::parse_str(std::str::from_utf8(&output.stdout).unwrap().trim()).unwrap();
        let app = App::new(
            Repository::connect(&owner.database_url, 4).await.unwrap(),
            BlobStore::open(&owner.data).await.unwrap(),
            &MASTER_KEY,
        );
        let mut users = Vec::new();
        for name in ["Alice", "Bob"] {
            users.push(
                app.provision_user(
                    tenant,
                    &format!("{}@example.invalid", Uuid::new_v4()),
                    Zeroizing::new(PASSWORD.into()),
                    name,
                    "Operations E2E",
                    false,
                )
                .await
                .unwrap(),
            );
        }
        app.close().await;
        owner.start().await;
        // Reserve the peer address after the owner binds its listener.
        let mut peer = ServerProcess::new();
        peer.share_data_from(&owner);
        assert_eq!(peer.database_url, owner.database_url);
        assert_eq!(peer.data, owner.data);
        peer.start().await;
        assert_ne!(peer.child().id(), owner.child().id());
        let bob = users.pop().unwrap();
        let alice = users.pop().unwrap();
        Self {
            peer,
            owner,
            alice,
            bob,
        }
    }

    async fn stop(&mut self) {
        self.peer.stop().await;
        self.owner.stop().await;
    }
}

struct Human {
    key: TransportKey,
    from: String,
}

impl Human {
    async fn login(server: &ServerProcess, user: &User) -> Self {
        let response = server
            .http
            .post(server.url("/v20/u/sessions"))
            .header("Content-Type", "application/json")
            .body(json!({"client_pubkey":CLIENT_PUBLIC}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let handshake: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        let mut private = [0; 32];
        private[31] = 2;
        let key = TransportKey::from_ecdh(&private, handshake["server_pubkey"].as_str().unwrap())
            .unwrap();
        let response = server
            .http
            .post(server.url(&format!(
                "/v20/u/login/{}",
                handshake["session_id"].as_str().unwrap()
            )))
            .header("Content-Type", "application/octet-stream")
            .body(
                key.encrypt_base64(
                    &serde_json::to_vec(&json!({"email_hash":user.email_hash,"password":PASSWORD}))
                        .unwrap(),
                )
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
            from: login["from_id"].as_str().unwrap().into(),
        }
    }

    async fn request(
        &self,
        server: &ServerProcess,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let mut request = server
            .http
            .request(method, server.url(&format!("/v20{path}")))
            .header("From", &self.from);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/octet-stream")
                .body(
                    self.key
                        .encrypt_base64(&serde_json::to_vec(&body).unwrap())
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
        body: Option<Value>,
    ) -> Value {
        let response = self.request(server, method, path, body).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        serde_json::from_slice(
            &self
                .key
                .decrypt_base64(&response.text().await.unwrap())
                .unwrap(),
        )
        .unwrap()
    }

    async fn subscribe(&self, issuer: &ServerProcess, acceptor: &ServerProcess) -> Socket {
        let ticket = self.call(issuer, Method::GET, "/u/ws/auth", None).await;
        self.redeem(acceptor, ticket["token"].as_str().unwrap())
            .await
    }

    async fn redeem(&self, acceptor: &ServerProcess, ticket: &str) -> Socket {
        let (mut socket, response) = timeout(
            Duration::from_secs(3),
            connect_async(format!("ws://{}/ws/v20/{ticket}", acceptor.address)),
        )
        .await
        .expect("cross-instance WebSocket upgrade timeout")
        .unwrap();
        assert_eq!(response.status().as_u16(), 101);
        self.acknowledge_subscription(&mut socket).await;
        socket
    }

    async fn acknowledge_subscription(&self, socket: &mut Socket) {
        let subscription = self
            .key
            .encrypt_base64(br#"{"action":"subscribe","message_types":["chat_message"]}"#)
            .unwrap();
        socket
            .send(Message::Text(subscription.into()))
            .await
            .unwrap();
        let ack = self.event(socket, "subscribed", None).await;
        assert_eq!(ack["message_types"], json!(["chat_message"]));
    }

    async fn event(&self, socket: &mut Socket, kind: &str, mid: Option<&Value>) -> Value {
        timeout(Duration::from_secs(5), async {
            loop {
                let frame = socket
                    .next()
                    .await
                    .expect("cross-instance WebSocket closed before event")
                    .unwrap();
                let Message::Text(text) = frame else {
                    if matches!(frame, Message::Ping(_) | Message::Pong(_)) {
                        continue;
                    }
                    panic!("expected encrypted human WebSocket text frame");
                };
                let value: Value =
                    serde_json::from_slice(&self.key.decrypt_base64(text.as_str()).unwrap())
                        .unwrap();
                if value["type"] == kind && mid.is_none_or(|id| value["content"]["mid"] == *id) {
                    return value;
                }
            }
        })
        .await
        .expect("cross-instance WebSocket event timeout")
    }
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn human_sessions_and_encrypted_events_work_between_two_server_processes() {
    let mut replicas = Replicas::start().await;
    let alice = Human::login(&replicas.owner, &replicas.alice).await;
    let bob = Human::login(&replicas.peer, &replicas.bob).await;
    assert_eq!(
        alice
            .call(&replicas.peer, Method::GET, "/u/name", None)
            .await["first_name"],
        "Alice"
    );
    assert_eq!(
        bob.call(&replicas.owner, Method::GET, "/u/name", None)
            .await["first_name"],
        "Bob"
    );
    let chat = alice
        .call(
            &replicas.owner,
            Method::PUT,
            "/t",
            Some(json!({"chatType":"group","col":SECRET,"title":"Across processes","users":[replicas.alice.id,replicas.bob.id]})),
        )
        .await;
    let tid = chat["tid"].as_str().unwrap();
    let chat_key = TransportKey::from_secret(SECRET).unwrap();
    // A ticket issued by either process is usable at the other process.
    let mut alice_socket = alice.subscribe(&replicas.peer, &replicas.owner).await;
    let mut bob_socket = bob.subscribe(&replicas.owner, &replicas.peer).await;
    for (sender, writer, recipient, socket, sender_id, plaintext) in [
        (
            &alice,
            &replicas.owner,
            &bob,
            &mut bob_socket,
            replicas.alice.id,
            "A → B: Hallo 🦉",
        ),
        (
            &bob,
            &replicas.peer,
            &alice,
            &mut alice_socket,
            replicas.bob.id,
            "B → A: سۆرانی\0",
        ),
    ] {
        let posted = sender
            .call(
                writer,
                Method::POST,
                "/t",
                Some(json!({"tid":tid,"cid":Uuid::new_v4(),"msg":chat_key.encrypt_base64(plaintext.as_bytes()).unwrap()})),
            )
            .await;
        let event = recipient
            .event(socket, "chat_message", Some(&posted["mid"]))
            .await;
        assert_eq!(event["content"]["from"], json!(sender_id));
        assert_eq!(event["content"]["tid"], tid);
        assert_eq!(
            chat_key
                .decrypt_base64(event["content"]["m"].as_str().unwrap())
                .unwrap(),
            plaintext.as_bytes()
        );
    }
    let path = format!("/t/{tid}");
    let history_a = alice.call(&replicas.owner, Method::GET, &path, None).await;
    let history_b = bob.call(&replicas.peer, Method::GET, &path, None).await;
    assert_eq!(history_a.as_array().unwrap().len(), 2);
    assert_eq!(history_a, history_b);
    alice_socket.close(None).await.unwrap();
    bob_socket.close(None).await.unwrap();
    drop((alice_socket, bob_socket));
    replicas.stop().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn logout_on_one_process_revokes_http_tickets_and_idle_websocket_on_the_other() {
    let mut replicas = Replicas::start().await;
    let revoked = Human::login(&replicas.owner, &replicas.alice).await;
    // An independent session for the same user must remain valid.
    let surviving = Human::login(&replicas.peer, &replicas.alice).await;
    // Redeem locally to keep remote revocation independent of ticket routing.
    let mut revoked_socket = revoked.subscribe(&replicas.peer, &replicas.peer).await;
    let mut surviving_socket = surviving.subscribe(&replicas.peer, &replicas.peer).await;
    let unused_ticket = revoked
        .call(&replicas.peer, Method::GET, "/u/ws/auth", None)
        .await;
    assert_eq!(
        revoked
            .call(&replicas.peer, Method::GET, "/u/name", None)
            .await["first_name"],
        "Alice"
    );

    revoked
        .call(&replicas.owner, Method::POST, "/u/logout", None)
        .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    for server in [&replicas.owner, &replicas.peer] {
        for path in ["/u/name", "/u/ws/auth"] {
            assert_eq!(
                revoked
                    .request(server, Method::GET, path, None)
                    .await
                    .status(),
                StatusCode::FORBIDDEN,
                "logged-out From token remains valid for {path} on {}",
                server.address
            );
        }
    }
    let rejected = timeout_at(
        deadline,
        connect_async(format!(
            "ws://{}/ws/v20/{}",
            replicas.peer.address,
            unused_ticket["token"].as_str().unwrap()
        )),
    )
    .await
    .expect("revoked-ticket upgrade must finish before the revocation deadline");
    let Err(WsError::Http(response)) = rejected else {
        panic!("an unused ticket for the revoked session must not upgrade");
    };
    assert_eq!(response.status().as_u16(), 403);

    // Send nothing after logout: a successful result must come from remote
    // revocation, rather than a client frame causing another authentication.
    let terminal = timeout_at(deadline, revoked_socket.next())
        .await
        .expect("idle remote WebSocket remained connected after logout");
    match terminal {
        None | Some(Ok(Message::Close(_))) => {}
        Some(Err(WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake))) => {}
        other => panic!("revoked idle WebSocket produced a nonterminal frame: {other:?}"),
    }
    for server in [&replicas.owner, &replicas.peer] {
        assert_eq!(
            surviving.call(server, Method::GET, "/u/name", None).await["first_name"],
            "Alice"
        );
    }
    let ping = surviving.key.encrypt_base64(br#"{"type":"ping"}"#).unwrap();
    surviving_socket
        .send(Message::Text(ping.into()))
        .await
        .unwrap();
    assert_eq!(
        surviving.event(&mut surviving_socket, "pong", None).await,
        json!({"type":"pong"})
    );
    surviving_socket.close(None).await.unwrap();
    drop((revoked_socket, surviving_socket));
    replicas.stop().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn websocket_ticket_outlives_issuer_and_is_consumed_once_across_processes() {
    let mut replicas = Replicas::start().await;
    let alice = Human::login(&replicas.owner, &replicas.alice).await;
    let ticket = alice
        .call(&replicas.owner, Method::GET, "/u/ws/auth", None)
        .await;
    let ticket = ticket["token"].as_str().unwrap();
    replicas.owner.stop().await;
    // The issuer is absent: the peer must retrieve all required auth material.
    let mut restored = alice.redeem(&replicas.peer, ticket).await;
    replicas.owner.start().await;
    assert_eq!(
        alice
            .call(&replicas.owner, Method::GET, "/u/name", None)
            .await["first_name"],
        "Alice"
    );
    let replay = timeout(
        Duration::from_secs(3),
        connect_async(format!("ws://{}/ws/v20/{ticket}", replicas.owner.address)),
    )
    .await
    .expect("ticket replay upgrade timeout");
    let Err(WsError::Http(response)) = replay else {
        panic!("ticket consumed on the peer was reusable after issuer restart");
    };
    assert_eq!(response.status().as_u16(), 403);

    let fresh = alice
        .call(&replicas.peer, Method::GET, "/u/ws/auth", None)
        .await;
    let fresh = fresh["token"].as_str().unwrap();
    let contenders = timeout(Duration::from_secs(5), async {
        tokio::join!(
            connect_async(format!("ws://{}/ws/v20/{fresh}", replicas.owner.address)),
            connect_async(format!("ws://{}/ws/v20/{fresh}", replicas.peer.address))
        )
    })
    .await
    .expect("competing ticket upgrades must finish");
    let mut winner = None;
    let mut rejected = 0;
    for result in [contenders.0, contenders.1] {
        match result {
            Ok((socket, response)) => {
                assert_eq!(response.status().as_u16(), 101);
                assert!(
                    winner.replace(socket).is_none(),
                    "same ticket upgraded both replicas"
                );
            }
            Err(WsError::Http(response)) => {
                assert_eq!(response.status().as_u16(), 403);
                rejected += 1;
            }
            Err(error) => panic!("unexpected competing-upgrade error: {error}"),
        }
    }
    assert_eq!(rejected, 1);
    let mut winner = winner.expect("exactly one replica must consume the fresh ticket");
    alice.acknowledge_subscription(&mut winner).await;
    restored.close(None).await.unwrap();
    winner.close(None).await.unwrap();
    drop((restored, winner));
    replicas.stop().await;
}
