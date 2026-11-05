//! Shared single-use upgrade credentials against real PostgreSQL and public App APIs.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hoowire_blob::BlobStore;
use hoowire_crypto::TransportKey;
use hoowire_db::Repository;
use hoowire_domain::{Error, User};
use hoowire_service::{App, Auth};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::Barrier, task::JoinSet};
use uuid::Uuid;
use zeroize::Zeroizing;

const MASTER_KEY: [u8; 32] = [0x74; 32];
const PASSWORD: &str = "isolated-websocket-ticket-password!";
const SDK_SECRET: &str = "TTTTTTTTTTTTTTTTTTTTTTTTTTTTTTTT";
// SEC1 public key for the independent client private scalar 2.
const CLIENT_PUBLIC: &str = concat!(
    "04c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "1ae168fea63dc339a3c58419466ceaeef7f632653266d0e1236431a950cfe52a"
);

struct Fixture {
    url: String,
    data: PathBuf,
    tenant: Uuid,
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
        let tenant = repo
            .create_tenant("WebSocket ticket contracts")
            .await
            .unwrap();
        repo.close().await;
        Self {
            url,
            data: std::env::temp_dir().join(format!("hoowire-ticket-test-{}", Uuid::new_v4())),
            tenant,
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

async fn human(app: &App, tenant: Uuid) -> Login {
    let email = format!("{}@example.invalid", Uuid::new_v4());
    let user = app
        .provision_user(
            tenant,
            &email,
            Zeroizing::new(PASSWORD.into()),
            "WebSocket",
            "Human",
            false,
        )
        .await
        .unwrap();
    let handshake = app.handshake(CLIENT_PUBLIC).await.unwrap();
    let mut private = [0; 32];
    private[31] = 2;
    let key =
        TransportKey::from_ecdh(&private, handshake["server_pubkey"].as_str().unwrap()).unwrap();
    let credentials = json!({
        "email_hash": hex::encode(Sha256::digest(email.as_bytes())),
        "password": PASSWORD,
    });
    let response = app
        .login(handshake["session_id"].as_str().unwrap(), &credentials)
        .await
        .unwrap();
    Login {
        from: response["from_id"].as_str().unwrap().to_owned(),
        key,
        user,
    }
}

async fn bot(app: &App, tenant: Uuid) -> Login {
    let user = app
        .provision_user(
            tenant,
            &format!("{}@example.invalid", Uuid::new_v4()),
            Zeroizing::new(PASSWORD.into()),
            "WebSocket",
            "Bot",
            true,
        )
        .await
        .unwrap();
    let from = Uuid::new_v4().to_string();
    app.provision_integration(&user, &from, SDK_SECRET)
        .await
        .unwrap();
    Login {
        from,
        key: TransportKey::from_secret(SDK_SECRET).unwrap(),
        user,
    }
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn assert_auth(auth: &Auth, login: &Login) {
    assert_eq!(auth.from_id, login.from);
    assert_eq!(auth.principal.user_id, login.user.id);
    assert_eq!(auth.principal.tenant_id, login.user.tenant_id);
    assert_eq!(auth.principal.is_bot, login.user.is_bot);
    let ciphertext = auth.key.encrypt_base64(b"recovered ticket key").unwrap();
    assert_eq!(
        login.key.decrypt_base64(&ciphertext).unwrap(),
        b"recovered ticket key"
    );
}

async fn assert_consumed(repo: &Repository, ticket: &str) {
    let present: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM websocket_tickets WHERE token_hash=$1)")
            .bind(token_hash(ticket))
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        !present,
        "a consumed ticket must not remain reusable in SQL"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn human_and_sdk_tickets_are_sealed_and_consumed_by_an_independent_app() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    for login in [
        human(&first.app, fixture.tenant).await,
        bot(&first.app, fixture.tenant).await,
    ] {
        let auth = first.app.authenticate(&login.from).await.unwrap();
        let before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(first.repo.pool())
            .await
            .unwrap();
        let ticket = first.app.websocket_token(&auth).await.unwrap();
        assert_eq!(ticket.len(), 43);
        assert!(
            ticket
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        let (stored_hash, sealed, expires, now): (
            Vec<u8>,
            Vec<u8>,
            DateTime<Utc>,
            DateTime<Utc>,
        ) = sqlx::query_as(
            "SELECT token_hash,credential_sealed,expires_at,clock_timestamp() FROM websocket_tickets WHERE token_hash=$1",
        )
        .bind(token_hash(&ticket))
        .fetch_one(first.repo.pool())
        .await
        .unwrap();
        assert_eq!(stored_hash, token_hash(&ticket));
        let transport_secret = login.key.export_secret();
        for plaintext in [
            ticket.as_bytes(),
            login.from.as_bytes(),
            SDK_SECRET.as_bytes(),
            transport_secret.as_slice(),
        ] {
            assert!(
                !sealed
                    .windows(plaintext.len())
                    .any(|part| part == plaintext)
            );
        }
        assert!(expires >= before + chrono::Duration::seconds(60));
        assert!(expires <= now + chrono::Duration::seconds(60));
        assert_auth(&second.app.websocket_auth(&ticket).await.unwrap(), &login);
        assert_consumed(&first.repo, &ticket).await;
        for app in [&first.app, &second.app] {
            assert!(matches!(
                app.websocket_auth(&ticket).await,
                Err(Error::Unauthorized)
            ));
        }
    }
    first.app.close().await;
    second.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn concurrent_consumers_on_two_apps_have_exactly_one_winner() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let login = bot(&first.app, fixture.tenant).await;
    let auth = first.app.authenticate(&login.from).await.unwrap();
    let ticket = first.app.websocket_token(&auth).await.unwrap();
    let gate = Arc::new(Barrier::new(17));
    let mut consumers = JoinSet::new();
    for index in 0..16 {
        let app = if index % 2 == 0 {
            first.app.clone()
        } else {
            second.app.clone()
        };
        let ticket = ticket.clone();
        let gate = gate.clone();
        consumers.spawn(async move {
            gate.wait().await;
            app.websocket_auth(&ticket).await
        });
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        gate.wait().await;
        let mut winners = 0;
        let mut denied = 0;
        while let Some(result) = consumers.join_next().await {
            match result.unwrap() {
                Ok(auth) => {
                    winners += 1;
                    assert_auth(&auth, &login);
                }
                Err(Error::Unauthorized) => denied += 1,
                Err(error) => panic!("unexpected ticket-consumption error: {error}"),
            }
        }
        assert_eq!(winners, 1);
        assert_eq!(denied, 15);
    })
    .await
    .expect("bounded concurrent ticket consumption");
    assert_consumed(&first.repo, &ticket).await;
    first.app.close().await;
    second.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn unused_human_and_sdk_tickets_survive_the_issuing_app_shutdown() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let mut pending = Vec::new();
    for login in [
        human(&first.app, fixture.tenant).await,
        bot(&first.app, fixture.tenant).await,
    ] {
        let auth = first.app.authenticate(&login.from).await.unwrap();
        pending.push((first.app.websocket_token(&auth).await.unwrap(), login));
    }
    first.app.close().await;
    drop(first);

    let recovered = fixture.instance(&MASTER_KEY).await;
    for (ticket, login) in pending {
        assert_auth(
            &recovered.app.websocket_auth(&ticket).await.unwrap(),
            &login,
        );
        assert_consumed(&recovered.repo, &ticket).await;
        assert!(matches!(
            recovered.app.websocket_auth(&ticket).await,
            Err(Error::Unauthorized)
        ));
    }
    recovered.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn ticket_expiry_is_checked_against_postgresql_without_waiting_for_ttl() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let login = human(&first.app, fixture.tenant).await;
    let auth = first.app.authenticate(&login.from).await.unwrap();
    let ticket = first.app.websocket_token(&auth).await.unwrap();
    let expired = sqlx::query(
        "UPDATE websocket_tickets SET expires_at=clock_timestamp()-interval '1 second' WHERE token_hash=$1",
    )
    .bind(token_hash(&ticket))
    .execute(first.repo.pool())
    .await
    .unwrap();
    assert_eq!(expired.rows_affected(), 1);
    assert!(matches!(
        second.app.websocket_auth(&ticket).await,
        Err(Error::Unauthorized)
    ));
    assert_consumed(&first.repo, &ticket).await;
    assert!(first.app.authenticate(&login.from).await.is_ok());
    let fresh = first.app.websocket_token(&auth).await.unwrap();
    assert_auth(&second.app.websocket_auth(&fresh).await.unwrap(), &login);
    first.app.close().await;
    second.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn wrong_master_and_malformed_sealed_credentials_fail_closed() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let wrong_master = fixture.instance(&[0x75; 32]).await;
    let login = human(&first.app, fixture.tenant).await;
    let auth = first.app.authenticate(&login.from).await.unwrap();
    let ticket = first.app.websocket_token(&auth).await.unwrap();
    assert!(matches!(
        wrong_master.app.websocket_auth(&ticket).await,
        Err(Error::Unauthorized)
    ));
    assert_consumed(&first.repo, &ticket).await;
    assert!(matches!(
        second.app.websocket_auth(&ticket).await,
        Err(Error::Unauthorized)
    ));
    wrong_master.app.close().await;

    for corruption in 0..6 {
        let ticket = first.app.websocket_token(&auth).await.unwrap();
        let mut sealed: Vec<u8> = sqlx::query_scalar(
            "SELECT credential_sealed FROM websocket_tickets WHERE token_hash=$1",
        )
        .bind(token_hash(&ticket))
        .fetch_one(first.repo.pool())
        .await
        .unwrap();
        match corruption {
            0 => sealed[0] ^= 1,
            1 => sealed[8] ^= 1,
            2 => *sealed.last_mut().unwrap() ^= 1,
            3 => sealed.truncate(1),
            4 => {
                sealed.pop();
            }
            5 => sealed.resize(289, 0),
            _ => unreachable!(),
        }
        sqlx::query("UPDATE websocket_tickets SET credential_sealed=$2 WHERE token_hash=$1")
            .bind(token_hash(&ticket))
            .bind(sealed)
            .execute(first.repo.pool())
            .await
            .unwrap();
        assert!(matches!(
            second.app.websocket_auth(&ticket).await,
            Err(Error::Unauthorized)
        ));
        assert_consumed(&first.repo, &ticket).await;
    }
    let control = first.app.websocket_token(&auth).await.unwrap();
    assert_auth(&second.app.websocket_auth(&control).await.unwrap(), &login);
    first.app.close().await;
    second.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn sealed_tickets_cannot_be_transplanted_to_other_tokens_or_owners() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let login = human(&first.app, fixture.tenant).await;
    let other = human(&first.app, fixture.tenant).await;
    let foreign_tenant = first
        .repo
        .create_tenant("Foreign ticket owner")
        .await
        .unwrap();
    let foreign = human(&first.app, foreign_tenant).await;
    let auth = first.app.authenticate(&login.from).await.unwrap();

    let source = first.app.websocket_token(&auth).await.unwrap();
    let substituted = URL_SAFE_NO_PAD.encode(
        [
            Uuid::new_v4().as_bytes().as_slice(),
            Uuid::new_v4().as_bytes().as_slice(),
        ]
        .concat(),
    );
    sqlx::query("INSERT INTO websocket_tickets(token_hash,user_id,tenant_id,credential_sealed,expires_at) SELECT $2,user_id,tenant_id,credential_sealed,expires_at FROM websocket_tickets WHERE token_hash=$1")
        .bind(token_hash(&source))
        .bind(token_hash(&substituted))
        .execute(first.repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        second.app.websocket_auth(&substituted).await,
        Err(Error::Unauthorized)
    ));
    assert_consumed(&first.repo, &substituted).await;
    assert_auth(&second.app.websocket_auth(&source).await.unwrap(), &login);

    for replacement in [&other.user, &foreign.user] {
        let ticket = first.app.websocket_token(&auth).await.unwrap();
        sqlx::query("UPDATE websocket_tickets SET user_id=$2,tenant_id=$3 WHERE token_hash=$1")
            .bind(token_hash(&ticket))
            .bind(replacement.id)
            .bind(replacement.tenant_id)
            .execute(first.repo.pool())
            .await
            .unwrap();
        assert!(matches!(
            second.app.websocket_auth(&ticket).await,
            Err(Error::Unauthorized)
        ));
        assert_consumed(&first.repo, &ticket).await;
    }
    assert!(first.app.authenticate(&other.from).await.is_ok());
    assert!(second.app.authenticate(&foreign.from).await.is_ok());
    let control = first.app.websocket_token(&auth).await.unwrap();
    assert_auth(&second.app.websocket_auth(&control).await.unwrap(), &login);
    first.app.close().await;
    second.app.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn issuance_and_consumption_reauthenticate_revocation_and_current_owner() {
    let fixture = Fixture::new().await;
    let first = fixture.instance(&MASTER_KEY).await;
    let second = fixture.instance(&MASTER_KEY).await;
    let login = human(&first.app, fixture.tenant).await;
    let cached = first.app.authenticate(&login.from).await.unwrap();
    let pending = first.app.websocket_token(&cached).await.unwrap();
    let remote_auth = second.app.authenticate(&login.from).await.unwrap();
    second.app.logout(&remote_auth).await.unwrap();
    assert!(matches!(
        first.app.websocket_token(&cached).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        first.app.websocket_auth(&pending).await,
        Err(Error::Unauthorized)
    ));
    assert_consumed(&first.repo, &pending).await;

    let original = bot(&first.app, fixture.tenant).await;
    let replacement = bot(&first.app, fixture.tenant).await;
    let mut cached = first.app.authenticate(&original.from).await.unwrap();
    // Public Auth values are caller-owned. Issuance must recover the identity
    // and key from the From credential instead of trusting these copied fields.
    cached.principal.user_id = replacement.user.id;
    cached.key = TransportKey::from_secret("XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX").unwrap();
    let verified = first.app.websocket_token(&cached).await.unwrap();
    assert_auth(
        &second.app.websocket_auth(&verified).await.unwrap(),
        &original,
    );

    let pending = first.app.websocket_token(&cached).await.unwrap();
    sqlx::query("UPDATE integrations SET user_id=$2 WHERE app_id=$1")
        .bind(&original.from)
        .bind(replacement.user.id)
        .execute(first.repo.pool())
        .await
        .unwrap();
    let changed_owner = second.app.authenticate(&original.from).await.unwrap();
    assert_eq!(changed_owner.principal.user_id, replacement.user.id);
    assert!(matches!(
        second.app.websocket_auth(&pending).await,
        Err(Error::Unauthorized)
    ));
    assert_consumed(&first.repo, &pending).await;
    first.app.close().await;
    second.app.close().await;
}
