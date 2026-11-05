//! Real PostgreSQL session recovery and revocation; no SDK or native app is used.
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_domain::{Error, User};
use hoowire_service::{App, DEFAULT_TENANT};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;
use zeroize::Zeroizing;

const MASTER_KEY: [u8; 32] = [0x53; 32];
const PASSWORD: &str = "isolated-session-test-password!";
// SEC1 public key for the independent client private scalar 2.
const CLIENT_PUBLIC: &str = concat!(
    "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
);

struct Fixture {
    url: String,
    data: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

struct Instance {
    app: App,
    repo: Repository,
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.app.begin_shutdown();
    }
}

impl Fixture {
    async fn new() -> Self {
        let url =
            std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL URL required");
        let repo = Repository::connect(&url, 4).await.unwrap();
        repo.migrate().await.unwrap();
        repo.close().await;
        Self {
            url,
            data: std::env::temp_dir().join(format!("hoowire-session-test-{}", Uuid::new_v4())),
        }
    }

    async fn instance(&self, master_key: &[u8; 32]) -> Instance {
        let repo = Repository::connect(&self.url, 4).await.unwrap();
        repo.verify_schema().await.unwrap();
        let app = App::new(
            repo.clone(),
            BlobStore::open(&self.data).await.unwrap(),
            master_key,
        );
        Instance { app, repo }
    }
}

struct Login {
    from: String,
    key: TransportKey,
    user: User,
}

async fn login_user(app: &App) -> Login {
    let email = format!("{}@example.invalid", Uuid::new_v4());
    let user = app
        .provision_user(
            DEFAULT_TENANT,
            &email,
            Zeroizing::new(PASSWORD.into()),
            "Session",
            "Test",
            false,
        )
        .await
        .unwrap();
    let handshake = app.handshake(CLIENT_PUBLIC).await.unwrap();
    let session = handshake["session_id"].as_str().unwrap();
    let mut private = [0; 32];
    private[31] = 2;
    let key =
        TransportKey::from_ecdh(&private, handshake["server_pubkey"].as_str().unwrap()).unwrap();
    let credentials = json!({
        "email_hash": hex::encode(Sha256::digest(email.as_bytes())),
        "password": PASSWORD,
    });
    let response = app.login(session, &credentials).await.unwrap();
    assert_eq!(response["owner_id"], json!(user.id));
    assert_eq!(response["organisation_id"], json!(user.tenant_id));
    assert!(matches!(
        app.session_key(session).await,
        Err(Error::Unauthorized)
    ));
    Login {
        from: response["from_id"].as_str().unwrap().to_owned(),
        key,
        user,
    }
}

fn token_hash(from: &str) -> Vec<u8> {
    Sha256::digest(from.as_bytes()).to_vec()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn authenticated_session_and_key_survive_a_fresh_app_instance() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let login = login_user(&first.app).await;
    let (stored_hash, sealed, lifetime): (Vec<u8>, Vec<u8>, f64) = sqlx::query_as(
        "SELECT token_hash,key_sealed,extract(epoch FROM expires_at-created_at)::FLOAT8 FROM authenticated_sessions WHERE token_hash=$1",
    )
    .bind(token_hash(&login.from))
    .fetch_one(first.repo.pool())
    .await
    .unwrap();
    assert_eq!(stored_hash, token_hash(&login.from));
    assert_eq!(sealed.len(), 64);
    assert!(sealed.starts_with(b"HWS1"));
    let secret = login.key.export_secret();
    assert!(
        !sealed
            .windows(secret.len())
            .any(|part| part == secret.as_slice())
    );
    assert!((lifetime - 3600.0).abs() < 1.0);
    first.app.close().await;
    drop(first);

    let recovered = fixture.instance(&MASTER_KEY).await;
    let auth = recovered.app.authenticate(&login.from).await.unwrap();
    assert_eq!(auth.principal.user_id, login.user.id);
    assert_eq!(auth.principal.tenant_id, login.user.tenant_id);
    assert!(!auth.principal.is_bot);
    let ciphertext = auth.key.encrypt_base64(b"response after restart").unwrap();
    assert_eq!(
        login.key.decrypt_base64(&ciphertext).unwrap(),
        b"response after restart"
    );
    assert!(recovered.app.events(&auth, 0).await.is_ok());
    recovered.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn logout_revokes_other_instances_websocket_tickets_and_restart() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let login = login_user(&first.app).await;
    let first_auth = first.app.authenticate(&login.from).await.unwrap();
    let second_auth = second.app.authenticate(&login.from).await.unwrap();
    let ticket = first.app.websocket_token(&first_auth).await.unwrap();
    let mut listener = first.repo.listener().await.unwrap();

    second.app.logout(&second_auth).await.unwrap();
    let expected_user = login.user.id.to_string();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if listener.recv().await.unwrap().payload() == expected_user {
                break;
            }
        }
    })
    .await
    .expect("cross-instance revocation notification");
    assert!(matches!(
        first.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        first.app.websocket_auth(&ticket).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        first.app.events(&first_auth, 0).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        second.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    let revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM authenticated_sessions WHERE token_hash=$1",
    )
    .bind(token_hash(&login.from))
    .fetch_one(first.repo.pool())
    .await
    .unwrap();
    assert!(revoked);
    drop(listener);
    first.app.close().await;
    second.app.close().await;

    let recovered = fixture.instance(&MASTER_KEY).await;
    assert!(matches!(
        recovered.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    recovered.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn expiry_is_checked_against_postgresql_for_existing_auth_and_tickets() {
    let fixture = Fixture::new().await;
    let server = fixture.instance(&MASTER_KEY).await;
    let login = login_user(&server.app).await;
    let auth = server.app.authenticate(&login.from).await.unwrap();
    let ticket = server.app.websocket_token(&auth).await.unwrap();
    sqlx::query("UPDATE authenticated_sessions SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 second' WHERE token_hash=$1")
        .bind(token_hash(&login.from))
        .execute(server.repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        server.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        server.app.events(&auth, 0).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        server.app.websocket_auth(&ticket).await,
        Err(Error::Unauthorized)
    ));
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn sealed_key_tampering_and_a_different_master_key_are_rejected() {
    let fixture = Fixture::new().await;
    let server = fixture.instance(&MASTER_KEY).await;
    let login = login_user(&server.app).await;
    let different_master = fixture.instance(&[0x54; 32]).await;
    assert!(matches!(
        different_master.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    different_master.app.close().await;

    let original: Vec<u8> =
        sqlx::query_scalar("SELECT key_sealed FROM authenticated_sessions WHERE token_hash=$1")
            .bind(token_hash(&login.from))
            .fetch_one(server.repo.pool())
            .await
            .unwrap();
    let mut corrupted = original.clone();
    corrupted[20] ^= 1;
    sqlx::query("UPDATE authenticated_sessions SET key_sealed=$2 WHERE token_hash=$1")
        .bind(token_hash(&login.from))
        .bind(corrupted)
        .execute(server.repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        server.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    sqlx::query("UPDATE authenticated_sessions SET key_sealed=$2 WHERE token_hash=$1")
        .bind(token_hash(&login.from))
        .bind(original)
        .execute(server.repo.pool())
        .await
        .unwrap();
    assert!(server.app.authenticate(&login.from).await.is_ok());
    server.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn sealed_keys_cannot_be_reassigned_to_other_tokens_or_owners() {
    let fixture = Fixture::new().await;
    let server = fixture.instance(&MASTER_KEY).await;
    let login = login_user(&server.app).await;
    let other = login_user(&server.app).await;
    let substituted_from = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO authenticated_sessions(token_hash,user_id,tenant_id,key_sealed,expires_at) SELECT $2,user_id,tenant_id,key_sealed,expires_at FROM authenticated_sessions WHERE token_hash=$1")
        .bind(token_hash(&login.from))
        .bind(token_hash(&substituted_from))
        .execute(server.repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        server.app.authenticate(&substituted_from).await,
        Err(Error::Unauthorized)
    ));
    sqlx::query("UPDATE authenticated_sessions SET user_id=$2 WHERE token_hash=$1")
        .bind(token_hash(&login.from))
        .bind(other.user.id)
        .execute(server.repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        server.app.authenticate(&login.from).await,
        Err(Error::Unauthorized)
    ));
    assert!(server.app.authenticate(&other.from).await.is_ok());
    server.app.close().await;
}
