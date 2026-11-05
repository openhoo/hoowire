//! Inspect real connection settings without timing sleeps or schema mutations.
use hoowire_db::Repository;
use hoowire_domain::Error;

async fn setting(repository: &Repository, name: &str) -> String {
    sqlx::query_scalar("SELECT current_setting($1)")
        .bind(name)
        .fetch_one(repository.pool())
        .await
        .expect("read PostgreSQL connection setting")
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn migration_timeout_is_separate_from_runtime_and_bounded() {
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL")
        .expect("set HOOWIRE_TEST_DATABASE_URL to an isolated PostgreSQL database");
    let runtime = Repository::connect(&url, 1).await.expect("runtime pool");
    let migration = Repository::connect_for_migrations(&url, 1, 300)
        .await
        .expect("migration pool");

    // SHOW reports PostgreSQL's normalized values, not input string copies.
    let runtime_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(runtime.pool())
        .await
        .unwrap();
    let migration_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(migration.pool())
        .await
        .unwrap();
    assert_eq!(runtime_timeout, "15s");
    assert_eq!(migration_timeout, "5min");
    assert_eq!(setting(&runtime, "application_name").await, "hoowire");
    assert_eq!(
        setting(&migration, "application_name").await,
        "hoowire-migrate"
    );
    for pool in [&runtime, &migration] {
        assert_eq!(setting(pool, "lock_timeout").await, "10s");
        assert_eq!(
            setting(pool, "idle_in_transaction_session_timeout").await,
            "30s"
        );
    }
    for invalid in [0, 3601, u32::MAX] {
        assert!(matches!(
            Repository::connect_for_migrations(&url, 1, invalid).await,
            Err(Error::Invalid(_))
        ));
    }
    runtime.close().await;
    migration.close().await;

    for valid in [1, 3600] {
        let boundary = Repository::connect_for_migrations(&url, 1, valid)
            .await
            .expect("inclusive timeout boundary");
        let timeout_ms: i64 = sqlx::query_scalar(
            "SELECT setting::bigint FROM pg_settings WHERE name='statement_timeout'",
        )
        .fetch_one(boundary.pool())
        .await
        .unwrap();
        assert_eq!(timeout_ms, i64::from(valid) * 1000);
        boundary.close().await;
    }
}
