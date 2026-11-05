//! Shared ticket consumption, expiry and global issuance bounds in real SQL.
//! Each case owns a schema so its capacity fixture cannot affect other tests.
use chrono::{DateTime, Utc};
use hoowire_db::Repository;
use hoowire_domain::{Error, NewUser};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

struct Fixture {
    admin: Repository,
    first: Repository,
    second: Repository,
    schema: String,
    user: Uuid,
    tenant: Uuid,
}

impl Fixture {
    async fn open() -> Self {
        let url = std::env::var("HOOWIRE_TEST_DATABASE_URL")
            .expect("set HOOWIRE_TEST_DATABASE_URL to a disposable PostgreSQL database");
        let admin = Repository::connect(&url, 1).await.unwrap();
        // Identifiers contain only this fixed prefix and generated hex digits.
        let schema = format!("hoowire_ticket_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        // One physical connection per repository retains its private search
        // path while the two repositories still exercise independent sessions.
        let first = Repository::connect(&url, 1).await.unwrap();
        let second = Repository::connect(&url, 1).await.unwrap();
        for repo in [&first, &second] {
            sqlx::query("SELECT set_config('search_path',$1,false)")
                .bind(format!("{schema},public"))
                .execute(repo.pool())
                .await
                .unwrap();
        }
        first.migrate().await.unwrap();
        let tenant = first
            .create_tenant("WebSocket ticket contract")
            .await
            .unwrap();
        let user = first
            .create_user(NewUser {
                tenant_id: tenant,
                email_hash: Uuid::new_v4().to_string(),
                password_hash: "ticket-test-only".into(),
                profile: json!({}),
                is_bot: false,
            })
            .await
            .unwrap()
            .id;
        Self {
            admin,
            first,
            second,
            schema,
            user,
            tenant,
        }
    }

    async fn close(self) {
        self.first.close().await;
        self.second.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(self.admin.pool())
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn token_hash() -> [u8; 32] {
    Sha256::digest(Uuid::new_v4().as_bytes()).into()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn independent_consumers_have_exactly_one_winner_and_cannot_replace_a_ticket() {
    timeout(Duration::from_secs(45), async {
        let fixture = Fixture::open().await;
        let hash = token_hash();
        let sealed = [0x5a; 64];
        fixture
            .first
            .create_websocket_ticket(&hash, fixture.user, fixture.tenant, &sealed)
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .second
                .create_websocket_ticket(&hash, fixture.user, fixture.tenant, &[0x33; 64])
                .await,
            Err(Error::Conflict)
        ));
        let (first, second) = tokio::join!(
            fixture.first.consume_websocket_ticket(&hash),
            fixture.second.consume_websocket_ticket(&hash)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        let ticket = first.or(second).unwrap();
        assert_eq!(ticket.user_id, fixture.user);
        assert_eq!(ticket.tenant_id, fixture.tenant);
        assert_eq!(ticket.credential_sealed, sealed);
        assert!(
            fixture
                .first
                .consume_websocket_ticket(&hash)
                .await
                .unwrap()
                .is_none()
        );
        fixture.close().await;
    })
    .await
    .expect("single-use ticket contract exceeded its deadline");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn fixed_lifetime_expires_without_revealing_credentials_and_tenant_binding_is_enforced() {
    timeout(Duration::from_secs(45), async {
        let fixture = Fixture::open().await;
        let hash = token_hash();
        let before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(fixture.first.pool())
            .await
            .unwrap();
        fixture
            .first
            .create_websocket_ticket(&hash, fixture.user, fixture.tenant, &[0x5a; 64])
            .await
            .unwrap();
        let row = sqlx::query("SELECT expires_at,clock_timestamp() AS observed_at FROM websocket_tickets WHERE token_hash=$1")
            .bind(hash.as_slice())
            .fetch_one(fixture.first.pool())
            .await
            .unwrap();
        let expiry: DateTime<Utc> = row.get("expires_at");
        let observed: DateTime<Utc> = row.get("observed_at");
        assert!(expiry >= before + chrono::Duration::seconds(60));
        assert!(expiry <= observed + chrono::Duration::seconds(60));
        sqlx::query("UPDATE websocket_tickets SET expires_at=clock_timestamp()-interval '1 microsecond' WHERE token_hash=$1")
            .bind(hash.as_slice())
            .execute(fixture.first.pool())
            .await
            .unwrap();
        assert!(
            fixture
                .second
                .consume_websocket_ticket(&hash)
                .await
                .unwrap()
                .is_none()
        );
        let retained: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM websocket_tickets WHERE token_hash=$1)",
        )
        .bind(hash.as_slice())
        .fetch_one(fixture.first.pool())
        .await
        .unwrap();
        assert!(!retained, "expired credentials must be removed atomically");
        let other_tenant = fixture.first.create_tenant("Other ticket tenant").await.unwrap();
        assert!(matches!(
            fixture
                .first
                .create_websocket_ticket(&token_hash(), fixture.user, other_tenant, &[0x5a; 64])
                .await,
            Err(Error::Invalid(_))
        ));
        for sealed in [Vec::new(), vec![0x5a; 4097]] {
            assert!(matches!(
                fixture
                    .first
                    .create_websocket_ticket(&token_hash(), fixture.user, fixture.tenant, &sealed)
                    .await,
                Err(Error::Invalid(_))
            ));
        }
        fixture.close().await;
    })
    .await
    .expect("ticket expiry contract exceeded its deadline");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn competing_issuers_cannot_exceed_capacity_and_expired_rows_release_space() {
    timeout(Duration::from_secs(45), async {
        let fixture = Fixture::open().await;
        sqlx::query("INSERT INTO websocket_tickets(token_hash,user_id,tenant_id,credential_sealed,expires_at) SELECT decode(lpad(to_hex(n),64,'0'),'hex'),$1,$2,$3,clock_timestamp()+interval '60 seconds' FROM generate_series(1,4095) AS seed(n)")
            .bind(fixture.user)
            .bind(fixture.tenant)
            .bind(&[0x5a_u8; 64][..])
            .execute(fixture.first.pool())
            .await
            .unwrap();
        let first_hash = token_hash();
        let second_hash = token_hash();
        let (first, second) = tokio::join!(
            fixture.first.create_websocket_ticket(&first_hash, fixture.user, fixture.tenant, &[0x5a; 64]),
            fixture.second.create_websocket_ticket(&second_hash, fixture.user, fixture.tenant, &[0x5a; 64])
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let rejected = if first.is_ok() { second } else { first };
        assert!(matches!(rejected, Err(Error::Unavailable)));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM websocket_tickets")
            .fetch_one(fixture.first.pool())
            .await
            .unwrap();
        assert_eq!(count, 4096);
        sqlx::query("UPDATE websocket_tickets SET expires_at=clock_timestamp()-interval '1 microsecond' WHERE token_hash=decode(lpad('1',64,'0'),'hex')")
            .execute(fixture.first.pool())
            .await
            .unwrap();
        fixture
            .second
            .create_websocket_ticket(&token_hash(), fixture.user, fixture.tenant, &[0x5a; 64])
            .await
            .unwrap();
        let row = sqlx::query("SELECT count(*) AS pending,count(*) FILTER(WHERE expires_at<=clock_timestamp()) AS expired FROM websocket_tickets")
            .fetch_one(fixture.first.pool())
            .await
            .unwrap();
        assert_eq!(row.get::<i64, _>("pending"), 4096);
        assert_eq!(row.get::<i64, _>("expired"), 0);
        fixture.close().await;
    })
    .await
    .expect("ticket capacity contract exceeded its deadline");
}
