//! Durable feed cursor expiry, using only unique fixtures in the selected test DB.
use chrono::{DateTime, Utc};
use hoowire_db::{DEFAULT_TENANT, Repository};
use hoowire_domain::{NewChat, NewMessage, NewUser};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn expired_etag_replays_from_zero_even_when_its_previous_position_is_idle() {
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL URL required");
    let repo = Repository::connect(&url, 4).await.unwrap();
    repo.migrate().await.unwrap();
    let user = repo
        .create_user(NewUser {
            tenant_id: DEFAULT_TENANT,
            email_hash: Uuid::new_v4().to_string(),
            password_hash: "test-only".into(),
            profile: json!({}),
            is_bot: false,
        })
        .await
        .unwrap();
    let chat = repo
        .create_chat(NewChat {
            tenant_id: DEFAULT_TENANT,
            created_by: user.id,
            title: "Cursor lifetime".into(),
            kind: "group".into(),
            col: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            attributes: json!({}),
            members: vec![user.id],
        })
        .await
        .unwrap();
    let message = repo
        .post_message(NewMessage {
            chat_id: chat.id,
            sender_id: user.id,
            client_id: None,
            body: "opaque cursor lifetime fixture".into(),
            attributes: json!({}),
        })
        .await
        .unwrap();
    let (initial, current) = repo.feed(user.id, None, 1000).await.unwrap();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].id, message.id);

    let unexpired: DateTime<Utc> = sqlx::query_scalar("UPDATE sync_cursors SET token=clock_timestamp()-interval '29 days',created_at=clock_timestamp()-interval '29 days' WHERE user_id=$1 AND token=$2 RETURNING token")
        .bind(user.id)
        .bind(current)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let (idle, reused) = repo.feed(user.id, Some(unexpired), 1000).await.unwrap();
    assert!(idle.is_empty());
    assert_eq!(reused, unexpired);

    let expired: DateTime<Utc> = sqlx::query_scalar("UPDATE sync_cursors SET token=clock_timestamp()-interval '30 days',created_at=clock_timestamp()-interval '30 days' WHERE user_id=$1 AND token=$2 RETURNING token")
        .bind(user.id)
        .bind(unexpired)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    // No event was added after the first read. An expired stored cursor must
    // replay the original message instead of taking the idle-poll fast path.
    let (replayed, renewed) = repo.feed(user.id, Some(expired), 1000).await.unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].id, message.id);
    assert!(renewed > expired);
    let old_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sync_cursors WHERE user_id=$1 AND token=$2)",
    )
    .bind(user.id)
    .bind(expired)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(!old_exists);
    repo.close().await;
}
