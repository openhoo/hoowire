//! A terminated database operation must remain distinguishable from a code bug.
use std::time::Duration;

use hoowire_db::Repository;
use hoowire_domain::{Error, NewUser};
use serde_json::json;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn terminated_query_is_unavailable_and_reconnect_preserves_the_original_record() {
    timeout(Duration::from_secs(20), async {
        let url = std::env::var("HOOWIRE_TEST_DATABASE_URL")
            .expect("select a disposable PostgreSQL database");
        let repository = Repository::connect(&url, 1).await.unwrap();
        repository.migrate().await.unwrap();
        let operator = Repository::connect(&url, 2).await.unwrap();
        let tenant = repository
            .create_tenant("Termination smoke test")
            .await
            .unwrap();
        let user = repository
            .create_user(NewUser {
                tenant_id: tenant,
                email_hash: Uuid::new_v4().to_string(),
                password_hash: "synthetic-test-only".into(),
                profile: json!({"first_name":"Original"}),
                is_bot: false,
            })
            .await
            .unwrap();

        // Lock only this fixture's row. Observe the real waiting backend rather
        // than relying on a sleep or interrupting another test's connection.
        let mut lock = operator.pool().begin().await.unwrap();
        let locker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *lock)
            .await
            .unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(user.id)
            .execute(&mut *lock)
            .await
            .unwrap();

        let interrupt = async {
            let victim = timeout(Duration::from_secs(5), async {
                loop {
                    let waiting: Option<i32> = sqlx::query_scalar(
                        "SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))",
                    )
                    .bind(locker)
                    .fetch_optional(operator.pool())
                    .await
                    .unwrap();
                    if let Some(pid) = waiting {
                        break pid;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("own profile update must be observed waiting on its row lock");
            let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                .bind(victim)
                .fetch_one(operator.pool())
                .await
                .unwrap();
            assert!(terminated, "terminate only the observed waiting backend");
        };
        let (result, ()) = tokio::join!(
            repository.update_profile(user.id, json!({"first_name":"Interrupted"})),
            interrupt,
        );
        lock.rollback().await.unwrap();
        assert!(
            matches!(result, Err(Error::Unavailable)),
            "a terminated PostgreSQL query must be retryable, got {result:?}"
        );

        repository.health().await.expect("pool reconnects");
        assert_eq!(
            repository.user(user.id).await.unwrap().profile,
            user.profile
        );
        repository
            .update_profile(user.id, json!({"first_name":"Recovered"}))
            .await
            .expect("subsequent writes succeed without restarting the application");
        assert_eq!(
            repository.user(user.id).await.unwrap().profile["first_name"],
            "Recovered"
        );
        repository.close().await;
        operator.close().await;
    })
    .await
    .expect("database interruption smoke exceeded its deadline");
}
