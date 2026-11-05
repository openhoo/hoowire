//! Observable HTTP/WS error paths, budgets and recovery against real PostgreSQL.
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_service::{App, DEFAULT_TENANT};
use reqwest::{Client, Method, Response, header};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
use uuid::Uuid;
use zeroize::Zeroizing;

const SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

struct Server {
    app: App,
    client: Client,
    url: String,
    data: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.app.begin_shutdown();
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

impl Server {
    async fn start() -> Self {
        let repo = Repository::connect(
            &std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL required"),
            4,
        )
        .await
        .unwrap();
        repo.migrate().await.unwrap();
        let data = std::env::temp_dir().join(format!("hoowire-transport-{}", Uuid::new_v4()));
        let app = App::new(repo, BlobStore::open(&data).await.unwrap(), &[0x31; 32]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = hoowire_api::router(app.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            app,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            url,
            data,
            task,
        }
    }

    async fn bot(&self) -> (Uuid, String) {
        let user = self
            .app
            .provision_user(
                DEFAULT_TENANT,
                &format!("{}@example.invalid", Uuid::new_v4()),
                Zeroizing::new("transport-test-password!".into()),
                "Transport",
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

    async fn encrypted(&self, appid: &str, method: Method, path: &str, body: Value) -> Response {
        self.client
            .request(method, format!("{}/v20{path}", self.url))
            .header("From", appid)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(
                TransportKey::from_secret(SECRET)
                    .unwrap()
                    .encrypt_base64(&serde_json::to_vec(&body).unwrap())
                    .unwrap(),
            )
            .send()
            .await
            .unwrap()
    }
}

async fn error(response: Response, status: u16) -> Value {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let value: Value = response.json().await.unwrap();
    assert!(value["error"].is_string());
    value
}

// Receiving 100 Continue proves the server is polling this request's body.
// Keep one byte pending to observe admission limits without sleeps or private state.
async fn pending_body(server: &Server, path: &str, appid: Option<&str>) -> BufReader<TcpStream> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let address = server.url.strip_prefix("http://").unwrap();
        let mut stream = TcpStream::connect(address).await.unwrap();
        let auth = appid.map(|id| format!("From: {id}\r\n")).unwrap_or_default();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Length: 1\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        assert_eq!(status, "HTTP/1.1 100 Continue\r\n");
        let mut bytes = status.len();
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).await.unwrap();
            bytes += read;
            assert!(read > 0 && bytes < 8192, "incomplete interim response");
            if line == "\r\n" {
                break;
            }
        }
        reader
    })
    .await
    .expect("request must reach its body within five seconds")
}

async fn finish_invalid_body(mut reader: BufReader<TcpStream>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        reader.get_mut().write_all(b"!").await.unwrap();
        let mut response = Vec::new();
        reader.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
    })
    .await
    .expect("finishing the request must release admission within five seconds");
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn protected_routes_reject_missing_invalid_and_non_utf8_credentials_without_reflecting_them()
{
    let server = Server::start().await;
    for path in [
        "/u/name",
        "/contacts",
        "/feed",
        "/u/settings",
        "/u/subscriptions",
    ] {
        let response = server
            .client
            .get(format!("{}/v20{path}", server.url))
            .send()
            .await
            .unwrap();
        assert_eq!(
            error(response, 403).await,
            json!({"error":"not authorized"})
        );
    }
    for token in [
        header::HeaderValue::from_static(""),
        header::HeaderValue::from_static("unknown-sensitive-credential"),
        header::HeaderValue::from_str(&"x".repeat(257)).unwrap(),
        header::HeaderValue::from_bytes(&[0xff]).unwrap(),
    ] {
        let response = server
            .client
            .get(format!("{}/v20/u/name", server.url))
            .header("From", token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            error(response, 403).await,
            json!({"error":"not authorized"})
        );
    }
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn malformed_encrypted_requests_fail_closed_and_do_not_poison_the_credential() {
    let server = Server::start().await;
    let (user, appid) = server.bot().await;
    let key = TransportKey::from_secret(SECRET).unwrap();
    let bodies = [
        vec![0xff, 0xfe],
        b"not-base64-request".to_vec(),
        b"AAAA".to_vec(),
        key.encrypt_base64(b"not JSON and not reflected")
            .unwrap()
            .into_bytes(),
        TransportKey::from_secret("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
            .unwrap()
            .encrypt_base64(br#"{"read_receipts":false}"#)
            .unwrap()
            .into_bytes(),
    ];
    for body in bodies {
        let response = server
            .client
            .put(format!("{}/v20/u/settings", server.url))
            .header("From", &appid)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .unwrap();
        let failure = error(response, 400).await;
        let text = failure.to_string();
        assert!(!text.contains(SECRET));
        assert!(!text.contains(&appid));
        assert!(!text.contains("not reflected"));
    }
    let response = server
        .encrypted(&appid, Method::GET, "/u/name", Value::Null)
        .await;
    assert_eq!(response.status().as_u16(), 200);
    let identity: Value =
        serde_json::from_slice(&key.decrypt_base64(&response.text().await.unwrap()).unwrap())
            .unwrap();
    assert_eq!(identity["user_id"], user.to_string());
    let response = server
        .encrypted(&appid, Method::GET, "/u/settings", Value::Null)
        .await;
    let settings: Value =
        serde_json::from_slice(&key.decrypt_base64(&response.text().await.unwrap()).unwrap())
            .unwrap();
    assert_eq!(settings["read_receipts"], true);
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn invalid_feed_cursors_are_rejected_and_a_valid_cursor_still_round_trips() {
    let server = Server::start().await;
    let (_, appid) = server.bot().await;
    for cursor in [
        "not-hex".to_owned(),
        "fffe".to_owned(),
        hex::encode("2026-99-99T99:99:99Z"),
        format!("\"{}\"", hex::encode("2026-11-05T00:00:00Z")),
        hex::encode("private-invalid-cursor"),
    ] {
        let response = server
            .client
            .get(format!("{}/v20/feed", server.url))
            .header("From", &appid)
            .header(header::ETAG, cursor)
            .send()
            .await
            .unwrap();
        let failure = error(response, 400).await;
        assert!(!failure.to_string().contains("private-invalid-cursor"));
    }
    let response = server
        .client
        .get(format!("{}/v20/feed", server.url))
        .header("From", &appid)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let cursor = response.headers()[header::ETAG].clone();
    let response = server
        .client
        .get(format!("{}/v20/feed", server.url))
        .header("From", &appid)
        .header(header::ETAG, cursor)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let feed: Value = serde_json::from_slice(
        &TransportKey::from_secret(SECRET)
            .unwrap()
            .decrypt_base64(&response.text().await.unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(feed["messages"], json!([]));
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn request_admission_is_bounded_keeps_liveness_available_and_recovers_after_completion() {
    let server = Server::start().await;
    let scenario = async {
        let mut pending = Vec::new();
        for _ in 0..64 {
            pending.push(pending_body(&server, "/v20/u/sessions", None).await);
        }
        let response = server
            .client
            .get(format!("{}/v20/api_versions", server.url))
            .send()
            .await
            .unwrap();
        error(response, 503).await;
        let response = server
            .client
            .get(format!("{}/health/live", server.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.text().await.unwrap(), "ok");
        for reader in pending {
            finish_invalid_body(reader).await;
        }
        let response = server
            .client
            .get(format!("{}/v20/api_versions", server.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"versions":["20.0"]})
        );
    };
    tokio::time::timeout(Duration::from_secs(20), scenario)
        .await
        .expect("admission scenario must finish before the server's 30-second request deadline");
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn upload_admission_is_bounded_without_blocking_normal_requests_and_recovers() {
    let server = Server::start().await;
    let (_, appid) = server.bot().await;
    let scenario = async {
        let mut pending = Vec::new();
        for _ in 0..4 {
            pending.push(pending_body(&server, "/v20/upload", Some(&appid)).await);
        }
        let response = server
            .client
            .post(format!("{}/v20/upload", server.url))
            .header("From", &appid)
            .body("!")
            .send()
            .await
            .unwrap();
        error(response, 503).await;
        let response = server
            .encrypted(&appid, Method::GET, "/u/name", Value::Null)
            .await;
        assert_eq!(response.status().as_u16(), 200);
        for reader in pending {
            finish_invalid_body(reader).await;
        }
        let bytes = vec![0, 1, 0xff, 0x80, b'\r', b'\n'];
        let response = server
            .client
            .post(format!("{}/v20/upload", server.url))
            .header("From", &appid)
            .multipart(reqwest::multipart::Form::new().part(
                "f",
                reqwest::multipart::Part::bytes(bytes.clone()).file_name("opaque.bin"),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let asset: Value = response.json().await.unwrap();
        let response = server
            .client
            .get(format!(
                "{}/v20/serve/{}",
                server.url,
                asset["id"].as_str().unwrap()
            ))
            .header("From", &appid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
    };
    tokio::time::timeout(Duration::from_secs(20), scenario)
        .await
        .expect("upload admission scenario must finish before the 30-second request deadline");
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn json_body_limits_accept_valid_boundary_requests_and_reject_one_extra_byte() {
    use k256::elliptic_curve::sec1::ToEncodedPoint;

    let server = Server::start().await;
    let public = k256::SecretKey::from_slice(&[2; 32])
        .unwrap()
        .public_key()
        .to_encoded_point(false);
    let mut body =
        serde_json::to_vec(&json!({"client_pubkey":hex::encode(public.as_bytes())})).unwrap();
    body.resize(16 * 1024, b' ');
    let response = server
        .client
        .post(format!("{}/v20/u/sessions", server.url))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let handshake: Value = response.json().await.unwrap();
    assert!(handshake["session_id"].as_str().is_some());
    body.push(b' ');
    let response = server
        .client
        .post(format!("{}/v20/u/sessions", server.url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(
        error(response, 400).await["error"]
            .as_str()
            .unwrap()
            .contains("body too large")
    );

    let (_, appid) = server.bot().await;
    // A canonical envelope of exactly 6 MiB: 48 header bytes, AES blocks and
    // one padding byte. JSON permits whitespace inside the encrypted payload.
    let mut plaintext = br#"{"read_receipts":false}"#.to_vec();
    plaintext.resize(4_718_543, b' ');
    let mut body = TransportKey::from_secret(SECRET)
        .unwrap()
        .encrypt_base64(&plaintext)
        .unwrap()
        .into_bytes();
    assert_eq!(body.len(), 6 * 1024 * 1024);
    let response = server
        .client
        .put(format!("{}/v20/u/settings", server.url))
        .header("From", &appid)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    body.push(b' ');
    let response = server
        .client
        .put(format!("{}/v20/u/settings", server.url))
        .header("From", &appid)
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(
        error(response, 400).await["error"]
            .as_str()
            .unwrap()
            .contains("body too large")
    );
    let response = server
        .encrypted(&appid, Method::GET, "/u/settings", Value::Null)
        .await;
    assert_eq!(response.status().as_u16(), 200);
    let settings: Value = serde_json::from_slice(
        &TransportKey::from_secret(SECRET)
            .unwrap()
            .decrypt_base64(&response.text().await.unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(settings["read_receipts"], false);
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn multipart_rejects_missing_ambiguous_and_truncated_files_then_accepts_a_valid_upload() {
    use reqwest::multipart::{Form, Part};

    let server = Server::start().await;
    let (_, appid) = server.bot().await;
    let endpoint = format!("{}/v20/upload", server.url);
    let requests = [
        server.client.post(&endpoint)
            .header(header::CONTENT_TYPE, "multipart/form-data")
            .body("missing boundary"),
        server.client.post(&endpoint)
            .multipart(Form::new().part("other", Part::bytes(vec![1, 2, 3]))),
        server.client.post(&endpoint)
            .multipart(Form::new().part("f", Part::bytes(vec![1]))
                .part("f", Part::bytes(vec![2]))),
        server.client.post(&endpoint)
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=cut")
            .body("--cut\r\nContent-Disposition: form-data; name=\"f\"; filename=\"test\"\r\n\r\ntruncated"),
    ];
    for request in requests {
        let response = request.header("From", &appid).send().await.unwrap();
        error(response, 400).await;
    }
    let response = server
        .client
        .post(&endpoint)
        .header("From", &appid)
        .multipart(Form::new().part("f", Part::bytes(Vec::new()).file_name("empty.bin")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let asset: Value = response.json().await.unwrap();
    let id = Uuid::parse_str(asset["id"].as_str().unwrap()).unwrap();
    let response = server
        .client
        .get(format!("{}/v20/serve/{id}", server.url))
        .header("From", &appid)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.bytes().await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn upload_limits_preserve_exact_size_bytes_and_reject_oversized_files_and_requests() {
    use reqwest::multipart::{Form, Part};
    use sha2::{Digest, Sha256};

    let server = Server::start().await;
    let (_, appid) = server.bot().await;
    let endpoint = format!("{}/v20/upload", server.url);
    let mut bytes: Vec<u8> = (0..16 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let response = server
        .client
        .post(&endpoint)
        .header("From", &appid)
        .multipart(Form::new().part("f", Part::bytes(bytes.clone()).file_name("limit.bin")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let asset: Value = response.json().await.unwrap();
    let response = server
        .client
        .get(format!(
            "{}/v20/serve/{}",
            server.url,
            asset["id"].as_str().unwrap()
        ))
        .header("From", &appid)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let downloaded = response.bytes().await.unwrap();
    assert_eq!(downloaded.len(), bytes.len());
    assert_eq!(Sha256::digest(&downloaded), Sha256::digest(&bytes));

    bytes.push(0x7f);
    let response = server
        .client
        .post(&endpoint)
        .header("From", &appid)
        .multipart(Form::new().part("f", Part::bytes(bytes).file_name("too-large.bin")))
        .send()
        .await
        .unwrap();
    assert!(
        error(response, 400).await["error"]
            .as_str()
            .unwrap()
            .contains("asset exceeds 16 MiB")
    );
    let response = server
        .client
        .post(&endpoint)
        .header("From", &appid)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(vec![0; 24 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert!(
        error(response, 400).await["error"]
            .as_str()
            .unwrap()
            .contains("body too large")
    );
    let response = server
        .encrypted(&appid, Method::GET, "/u/name", Value::Null)
        .await;
    assert_eq!(response.status().as_u16(), 200);
}
