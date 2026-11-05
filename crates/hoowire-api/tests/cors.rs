//! Real HTTP CORS behavior on the complete API router, including authentication.
use axum::http::StatusCode;
use hoowire_api::CorsConfig;
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_service::{App, DEFAULT_TENANT};
use reqwest::{Client, Method, Response, header};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::task::JoinHandle;
use uuid::Uuid;
use zeroize::Zeroizing;

struct Server {
    app: App,
    url: String,
    task: JoinHandle<()>,
    data: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

impl Server {
    async fn start(cors: CorsConfig) -> Self {
        let repo = Repository::connect(
            &std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL URL required"),
            4,
        )
        .await
        .unwrap();
        repo.migrate().await.unwrap();
        let data = std::env::temp_dir().join(format!("hoowire-cors-test-{}", Uuid::new_v4()));
        let app = App::new(repo, BlobStore::open(&data).await.unwrap(), &[0x39; 32]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = hoowire_api::router_with_cors(app.clone(), cors);
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            app,
            url,
            task,
            data,
        }
    }

    async fn preflight(&self, origin: &str, method: &str, headers: &str) -> Response {
        client()
            .request(Method::OPTIONS, format!("{}/v20/u/sessions/", self.url))
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, method)
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, headers)
            .send()
            .await
            .unwrap()
    }

    async fn discovery(&self, origin: &str) -> Response {
        client()
            .get(format!("{}/v20/api_versions/?format=json", self.url))
            .header(header::ORIGIN, origin)
            .header("X-Request-ID", Uuid::new_v4().to_string())
            .send()
            .await
            .unwrap()
    }
}

fn client() -> Client {
    Client::builder().no_proxy().build().unwrap()
}

fn tokens(response: &Response, name: header::HeaderName) -> Vec<String> {
    response
        .headers()
        .get_all(name)
        .iter()
        .flat_map(|value| value.to_str().unwrap().split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect()
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn desktop_preflight_is_unauthenticated_but_data_and_etags_still_require_auth() {
    let server = Server::start(CorsConfig::default()).await;
    let preflight = server
        .preflight("null", "POST", "content-type,x-request-id")
        .await;
    assert_eq!(preflight.status(), StatusCode::OK);
    assert_eq!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "null"
    );
    let headers = tokens(&preflight, header::ACCESS_CONTROL_ALLOW_HEADERS);
    for expected in ["from", "content-type", "x-request-id", "etag"] {
        assert!(headers.iter().any(|value| value == expected));
    }
    assert!(
        !preflight
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );
    assert!(preflight.text().await.unwrap().is_empty());

    let discovery = server.discovery("null").await;
    assert_eq!(discovery.status(), StatusCode::OK);
    assert_eq!(
        discovery.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "null"
    );
    assert_eq!(
        discovery.json::<Value>().await.unwrap(),
        json!({"versions":["20.0"]})
    );

    let denied = client()
        .get(format!("{}/v20/u/preferences", server.url))
        .header(header::ORIGIN, "null")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        denied.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "null"
    );

    let user = server
        .app
        .provision_user(
            DEFAULT_TENANT,
            &format!("{}@example.invalid", Uuid::new_v4()),
            Zeroizing::new("cors-test-password-42!".into()),
            "CORS fixture",
            "",
            true,
        )
        .await
        .unwrap();
    let app_id = Uuid::new_v4().to_string();
    let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    server
        .app
        .provision_integration(&user, &app_id, secret)
        .await
        .unwrap();
    let response = client()
        .get(format!("{}/v20/u/preferences", server.url))
        .header(header::ORIGIN, "null")
        .header(header::FROM, app_id)
        .header("X-Request-ID", Uuid::new_v4().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "null"
    );
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );
    assert!(response.headers().contains_key(header::ETAG));
    assert!(tokens(&response, header::ACCESS_CONTROL_EXPOSE_HEADERS).contains(&"etag".into()));
    let vary = tokens(&response, header::VARY);
    for expected in [
        "origin",
        "access-control-request-method",
        "access-control-request-headers",
    ] {
        assert!(vary.iter().any(|value| value == expected));
    }
    let key = TransportKey::from_secret(secret).unwrap();
    let body = key.decrypt_base64(&response.text().await.unwrap()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["favourite_chats"],
        json!([])
    );
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn notification_registration_accepts_only_the_existing_websocket_transport() {
    let server = Server::start(CorsConfig::default()).await;
    let user = server
        .app
        .provision_user(
            DEFAULT_TENANT,
            &format!("{}@example.invalid", Uuid::new_v4()),
            Zeroizing::new("notification-test-password-42!".into()),
            "Notification fixture",
            "",
            true,
        )
        .await
        .unwrap();
    let app_id = Uuid::new_v4().to_string();
    let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    server
        .app
        .provision_integration(&user, &app_id, secret)
        .await
        .unwrap();
    let key = TransportKey::from_secret(secret).unwrap();
    for (body, expected) in [
        (
            json!({"pnstype":"ws","devicetoken":"desktop-fixture"}),
            StatusCode::OK,
        ),
        (
            json!({"pnstype":"apns","devicetoken":"desktop-fixture"}),
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            json!({"pnstype":"fcm","devicetoken":"desktop-fixture"}),
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            json!({"pnstype":"unknown","devicetoken":"desktop-fixture"}),
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            json!({"devicetoken":"desktop-fixture"}),
            StatusCode::NOT_IMPLEMENTED,
        ),
    ] {
        let response = client()
            .put(format!("{}/v20/notifications/token", server.url))
            .header(header::ORIGIN, "null")
            .header(header::FROM, &app_id)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(key.encrypt_base64(body.to_string().as_bytes()).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{body}");
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "null"
        );
        if expected == StatusCode::OK {
            let plaintext = key.decrypt_base64(&response.text().await.unwrap()).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&plaintext).unwrap(),
                json!({})
            );
        } else {
            assert!(response.json::<Value>().await.unwrap()["error"].is_string());
        }
    }
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL"]
async fn web_origins_are_exact_and_browser_preflight_cannot_expand_headers_or_methods() {
    let origin = "https://chat.example.invalid";
    let server = Server::start(CorsConfig::new(false, [origin]).unwrap()).await;
    let allowed = server
        .preflight(origin, "PUT", "from,content-type,x-request-id,etag")
        .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        origin
    );
    assert!(
        !allowed
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );

    for rejected in [
        "null",
        "https://sub.chat.example.invalid",
        "https://chat.example.invalid:8443",
        "http://chat.example.invalid",
        "https://chat.example.invalid.evil.invalid",
    ] {
        let preflight = server.preflight(rejected, "POST", "content-type").await;
        assert_eq!(preflight.status(), StatusCode::OK);
        assert!(
            !preflight
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "{rejected}"
        );
        let response = server.discovery(rejected).await;
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "{rejected}"
        );
    }
    let bad_method = server.preflight(origin, "TRACE", "content-type").await;
    assert!(!tokens(&bad_method, header::ACCESS_CONTROL_ALLOW_METHODS).contains(&"trace".into()));
    let bad_header = server
        .preflight(origin, "POST", "authorization,x-unexpected")
        .await;
    let allowed_headers = tokens(&bad_header, header::ACCESS_CONTROL_ALLOW_HEADERS);
    assert!(!allowed_headers.contains(&"authorization".into()));
    assert!(!allowed_headers.contains(&"x-unexpected".into()));
    // CORS omits permission headers; browsers enforce them. It is not an HTTP
    // authorization mechanism, so even the public discovery path remains public.
    let response = server.discovery(origin).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        origin
    );
    let without_origin = client()
        .get(format!("{}/v20/api_versions", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(without_origin.status(), StatusCode::OK);
    assert!(
        !without_origin
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
    server.app.close().await;
}
