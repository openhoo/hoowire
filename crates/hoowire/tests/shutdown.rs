//! SIGTERM exercises the actual executable, PostgreSQL and upgraded connections.
#![cfg(unix)]

mod common;

use common::{MASTER_KEY, SECRET, ServerProcess};
use futures_util::{SinkExt, StreamExt, future::join_all};
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_service::{App, DEFAULT_TENANT};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const PEERS: usize = 16;

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn sigterm_closes_authenticated_websockets_before_exiting_successfully() {
    let mut server = ServerProcess::new();
    let repo = Repository::connect(&server.database_url, 4).await.unwrap();
    repo.migrate().await.unwrap();
    let app = App::new(
        repo,
        BlobStore::open(&server.data).await.unwrap(),
        &MASTER_KEY,
    );
    let user = app
        .provision_user(
            DEFAULT_TENANT,
            &format!("{}@example.invalid", Uuid::new_v4()),
            Zeroizing::new("long-test-password!".into()),
            "Shutdown",
            "",
            true,
        )
        .await
        .unwrap();
    let appid = Uuid::new_v4().to_string();
    app.provision_integration(&user, &appid, SECRET)
        .await
        .unwrap();
    app.close().await;

    server.start().await;
    let address = server.address;

    let key = TransportKey::from_secret(SECRET).unwrap();
    let subscription = json!({"action":"subscribe","message_types":["chat_message"]});
    let subscription = key
        .encrypt_base64(&serde_json::to_vec(&subscription).unwrap())
        .unwrap();
    let mut sockets = Vec::with_capacity(PEERS);
    for peer in 0..PEERS {
        let mut request = format!("ws://{address}/ws/v20/")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert("From", appid.parse().unwrap());
        let (mut socket, response) = timeout(Duration::from_secs(3), connect_async(request))
            .await
            .expect("WebSocket upgrade timeout")
            .expect("authenticated WebSocket upgrade");
        assert_eq!(response.status().as_u16(), 101, "peer {peer}");
        timeout(Duration::from_secs(3), async {
            socket
                .send(Message::Binary(subscription.as_bytes().to_vec().into()))
                .await
                .unwrap();
            let ack = socket.next().await.unwrap().unwrap();
            let Message::Binary(bytes) = ack else {
                panic!("peer {peer}: expected encrypted binary subscription ACK");
            };
            let ack: Value = serde_json::from_slice(
                &key.decrypt_base64(std::str::from_utf8(&bytes).unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                ack,
                json!({"type":"subscribed","message_types":["chat_message"]}),
                "peer {peer}"
            );
        })
        .await
        .expect("encrypted subscription ACK timeout");
        sockets.push(socket);
    }

    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    server.terminate();
    timeout_at(
        shutdown_deadline,
        join_all(
            sockets
                .iter_mut()
                .enumerate()
                .map(|(peer, socket)| async move {
                    loop {
                        match socket
                            .next()
                            .await
                            .unwrap_or_else(|| panic!("peer {peer}: EOF before Close1001"))
                            .unwrap_or_else(|error| panic!("peer {peer}: {error}"))
                        {
                            Message::Close(Some(frame)) => {
                                assert_eq!(u16::from(frame.code), 1001, "peer {peer}");
                                break;
                            }
                            Message::Close(None) => panic!("peer {peer}: Close without code"),
                            _ => {}
                        }
                    }
                }),
        ),
    )
    .await
    .expect("all peers must receive Close1001 before the shutdown deadline");

    // Tungstenite queues the Close reply on read, then sends it on the next
    // read/write/flush. Hold those replies so an early runtime exit is observable.
    sleep(Duration::from_millis(200)).await;
    assert!(
        server.child().try_wait().unwrap().is_none(),
        "server exited before peers completed their close handshakes"
    );
    let replies = timeout_at(
        shutdown_deadline,
        join_all(sockets.iter_mut().map(|socket| socket.flush())),
    )
    .await
    .expect("close replies must flush before the shutdown deadline");
    for reply in replies {
        reply.expect("flush WebSocket close reply");
    }
    let status = server.wait_until(shutdown_deadline).await;
    assert_eq!(status.code(), Some(0), "server must exit successfully");
}
