//! These tests deliberately need a real PostgreSQL server. They never substitute
//! an in-memory implementation. Run with --ignored and HOOWIRE_TEST_DATABASE_URL.
use std::collections::BTreeSet;

use chrono::Utc;
use hoowire_db::{DEFAULT_TENANT, Repository};
use hoowire_domain::{Asset, Error, Integration, NewChat, NewMessage, NewUser};
use serde_json::json;
use uuid::Uuid;

async fn setup_repository() -> Repository {
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL")
        .expect("set HOOWIRE_TEST_DATABASE_URL to an isolated PostgreSQL database");
    let repository = Repository::connect(&url, 12)
        .await
        .expect("connect PostgreSQL");
    repository.migrate().await.expect("migrate PostgreSQL");
    repository
}

async fn user(repository: &Repository, tenant: Uuid) -> Uuid {
    repository
        .create_user(NewUser {
            tenant_id: tenant,
            email_hash: Uuid::new_v4().to_string(),
            password_hash: "test-argon2-record".into(),
            profile: json!({"name":"Test user"}),
            is_bot: false,
        })
        .await
        .expect("create user")
        .id
}

async fn group(repository: &Repository, creator: Uuid, members: Vec<Uuid>) -> hoowire_domain::Chat {
    repository
        .create_chat(NewChat {
            tenant_id: DEFAULT_TENANT,
            created_by: creator,
            title: "Contract test".into(),
            kind: "group".into(),
            col: Uuid::new_v4().to_string(),
            attributes: json!({}),
            members,
        })
        .await
        .expect("create chat")
}

fn message(chat: Uuid, sender: Uuid, client: Option<String>) -> NewMessage {
    NewMessage {
        chat_id: chat,
        sender_id: sender,
        client_id: client,
        body: "hello".into(),
        attributes: json!({"format":"plain"}),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_integrity_permissions_and_transactional_events() {
    let repository = setup_repository().await;
    repository.health().await.expect("health");
    let creator = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let outsider = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, creator, vec![member, member]).await;
    for invalid in [
        json!({"col":"plaintext-secret"}),
        json!({"users":["invalid"]}),
        json!({"groups":[]}),
        json!({"allowSending":"true"}),
    ] {
        assert!(matches!(
            repository
                .update_chat(creator, chat.id, None, invalid)
                .await,
            Err(Error::Invalid(_))
        ));
    }
    assert_eq!(
        repository.chat_for(creator, chat.id).await.unwrap().version,
        chat.version
    );
    assert_eq!(repository.events(creator, 0, 1000).await.unwrap().len(), 1);
    assert_eq!(repository.members(creator, chat.id).await.unwrap().len(), 2);
    assert!(matches!(
        repository.chat_for(outsider, chat.id).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repository
            .update_chat(member, chat.id, None, json!({"title":"forbidden"}))
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repository
            .update_chat(creator, chat.id, Some(0), json!({"title":"stale"}))
            .await,
        Err(Error::Conflict)
    ));
    let count_before = repository.events(creator, 0, 1000).await.unwrap().len();
    assert!(matches!(
        repository
            .post_message(message(chat.id, outsider, None))
            .await,
        Err(Error::Forbidden)
    ));
    assert_eq!(
        repository.events(creator, 0, 1000).await.unwrap().len(),
        count_before
    );
    assert!(
        repository
            .messages(creator, Some(chat.id), None, 1000)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repository
            .remove_members(creator, chat.id, &[creator], None)
            .await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        repository.members(creator, chat.id).await.unwrap().len(),
        2,
        "removal rolled back"
    );
    let updated = repository
        .add_members(creator, chat.id, &[outsider], Some(chat.version))
        .await
        .unwrap();
    assert_eq!(updated.version, 2);
    repository
        .post_message(message(chat.id, member, None))
        .await
        .unwrap();
    repository
        .remove_members(creator, chat.id, &[outsider], None)
        .await
        .unwrap();
    assert!(matches!(
        repository
            .messages(outsider, Some(chat.id), None, 1000)
            .await,
        Err(Error::Forbidden)
    ));
    let removed_events = repository.events(outsider, 0, 1000).await.unwrap();
    assert_eq!(removed_events.last().unwrap().kind, "thread_updated");
    assert_eq!(
        removed_events.len(),
        3,
        "add, message, removal all persisted"
    );

    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants(id,name) VALUES($1,'isolated')")
        .bind(tenant)
        .execute(repository.pool())
        .await
        .unwrap();
    let cross_tenant = user(&repository, tenant).await;
    assert!(matches!(
        repository
            .add_members(creator, chat.id, &[cross_tenant], None)
            .await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        repository
            .create_chat(NewChat {
                tenant_id: DEFAULT_TENANT,
                created_by: creator,
                title: "invalid".into(),
                kind: "group".into(),
                col: "invalid".into(),
                attributes: json!({}),
                members: vec![cross_tenant]
            })
            .await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(repository.chats(cross_tenant).await.unwrap().len(), 0);
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_concurrent_idempotency_replay_and_restart() {
    let repository = setup_repository().await;
    let creator = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, creator, vec![member]).await;
    let client_id = Uuid::new_v4().to_string();
    let mut requests = Vec::new();
    for _ in 0..16 {
        let repository = repository.clone();
        let client_id = client_id.clone();
        requests.push(tokio::spawn(async move {
            repository
                .post_message(message(chat.id, creator, Some(client_id)))
                .await
        }));
    }
    let mut ids = BTreeSet::new();
    for request in requests {
        ids.insert(request.await.unwrap().unwrap().id);
    }
    assert_eq!(ids.len(), 1, "idempotent writes return the same message");
    assert_eq!(
        repository
            .messages(member, Some(chat.id), None, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let events = repository.events(member, 0, 100).await.unwrap();
    assert_eq!(events.len(), 2, "one create event and one message event");
    assert!(events.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert_eq!(
        repository
            .events(member, events[0].id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let mut conflicting = message(chat.id, creator, Some(client_id));
    conflicting.body = "different content".into();
    assert!(matches!(
        repository.post_message(conflicting).await,
        Err(Error::Conflict)
    ));
    let message_id = *ids.first().unwrap();
    repository.close().await;
    let restarted = setup_repository().await;
    assert_eq!(
        restarted
            .message_for(member, message_id)
            .await
            .unwrap()
            .body,
        "hello"
    );
    assert_eq!(restarted.events(member, 0, 100).await.unwrap().len(), 2);
    restarted.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_message_ownership_reads_assets_and_directory() {
    let repository = setup_repository().await;
    let admin = user(&repository, DEFAULT_TENANT).await;
    let author = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let outsider = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, admin, vec![author, member]).await;
    let posted = repository
        .post_message(message(chat.id, author, None))
        .await
        .unwrap();
    assert!(matches!(
        repository.delete_message(member, posted.id).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repository
            .update_message(member, posted.id, json!({"changed":true}))
            .await,
        Err(Error::Forbidden)
    ));
    repository
        .react(member, posted.id, Some("👍".into()))
        .await
        .unwrap();
    assert_eq!(
        repository
            .message_for(author, posted.id)
            .await
            .unwrap()
            .attributes["reactions"][member.to_string()],
        "👍"
    );
    repository.react(member, posted.id, None).await.unwrap();
    repository
        .update_message(author, posted.id, json!({"location":{"lat":12}}))
        .await
        .unwrap();
    repository
        .read_chat(member, chat.id, Some(posted.id))
        .await
        .unwrap();
    let other = group(&repository, admin, vec![member]).await;
    let other_message = repository
        .post_message(message(other.id, admin, None))
        .await
        .unwrap();
    assert!(matches!(
        repository
            .read_chat(member, chat.id, Some(other_message.id))
            .await,
        Err(Error::Invalid(_))
    ));
    repository
        .update_chat(admin, chat.id, None, json!({"closed":true}))
        .await
        .unwrap();
    assert!(matches!(
        repository
            .post_message(message(chat.id, author, None))
            .await,
        Err(Error::Forbidden)
    ));
    let asset = Asset {
        id: Uuid::new_v4(),
        tenant_id: DEFAULT_TENANT,
        uploaded_by: author,
        chat_id: Some(chat.id),
        content_type: "text/plain".into(),
        size: 4,
        digest: "test-sha256".into(),
        created_at: Utc::now(),
    };
    repository.create_asset(asset.clone()).await.unwrap();
    assert_eq!(
        repository.asset_for(member, asset.id).await.unwrap().size,
        4
    );
    assert!(matches!(
        repository.asset_for(outsider, asset.id).await,
        Err(Error::Forbidden)
    ));
    repository.delete_message(admin, posted.id).await.unwrap();
    let deleted = repository.message_for(author, posted.id).await.unwrap();
    assert!(deleted.deleted_at.is_some());
    assert_eq!(deleted.body, "");
    assert_eq!(deleted.attributes, json!({}));
    let before = repository.events(author, 0, 1000).await.unwrap().len();
    repository.delete_message(author, posted.id).await.unwrap();
    assert_eq!(
        repository.events(author, 0, 1000).await.unwrap().len(),
        before
    );

    let entry = Uuid::new_v4();
    repository
        .upsert_directory_entry(
            DEFAULT_TENANT,
            entry,
            "group",
            json!({"title":"Team","user_ids":[author,member]}),
        )
        .await
        .unwrap();
    let directory = repository
        .directory_entry(DEFAULT_TENANT, entry, "group")
        .await
        .unwrap();
    assert_eq!(directory["title"], "Team");
    assert_eq!(directory["user_ids"].as_array().unwrap().len(), 2);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM directory_group_members WHERE group_id=$1")
            .bind(entry)
            .fetch_one(repository.pool())
            .await
            .unwrap();
    assert_eq!(count, 2);
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_profiles_preferences_sealed_integrations_and_direct_chat() {
    let repository = setup_repository().await;
    let bot = user(&repository, DEFAULT_TENANT).await;
    let human = user(&repository, DEFAULT_TENANT).await;
    repository
        .update_profile(bot, json!({"department":"Engineering"}))
        .await
        .unwrap();
    assert_eq!(
        repository.user(bot).await.unwrap().profile["department"],
        "Engineering"
    );
    repository
        .set_preferences(human, json!({"language":"de"}))
        .await
        .unwrap();
    repository
        .set_preferences(human, json!({"mute":true}))
        .await
        .unwrap();
    assert_eq!(
        repository.preferences(human).await.unwrap(),
        json!({"language":"de","mute":true})
    );
    let app_id = Uuid::new_v4().to_string();
    let secret = "v1:AES256GCM-test-envelope";
    repository
        .create_integration(Integration {
            id: Uuid::new_v4(),
            app_id: app_id.clone(),
            secret: secret.into(),
            user_id: bot,
            tenant_id: DEFAULT_TENANT,
        })
        .await
        .unwrap();
    assert_eq!(
        repository.integration(&app_id).await.unwrap().secret,
        secret
    );
    let chat = repository
        .create_chat(NewChat {
            tenant_id: DEFAULT_TENANT,
            created_by: bot,
            title: "Direct".into(),
            kind: "one_to_one".into(),
            col: Uuid::new_v4().to_string(),
            attributes: json!({}),
            members: vec![human],
        })
        .await
        .unwrap();
    assert!(matches!(
        repository
            .remove_members(bot, chat.id, &[human], None)
            .await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        repository.add_members(bot, chat.id, &[human], None).await,
        Err(Error::Invalid(_))
    ));
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_atomic_members_preferences_and_timestamp_etag_feed() {
    let repository = setup_repository().await;
    let admin = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let added = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, admin, vec![member]).await;
    assert!(matches!(
        repository
            .replace_members(admin, chat.id, &[added], None)
            .await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        repository.members(admin, chat.id).await.unwrap().len(),
        2,
        "membership rollback is atomic"
    );
    repository
        .update_chat(admin, chat.id, None, json!({"admins":[admin,member]}))
        .await
        .unwrap();
    assert_eq!(
        repository
            .administrators(member, chat.id)
            .await
            .unwrap()
            .len(),
        2
    );
    let replaced = repository
        .replace_members(member, chat.id, &[member, added], None)
        .await
        .unwrap();
    assert_eq!(repository.members(member, chat.id).await.unwrap().len(), 2);
    assert!(matches!(
        repository.chat_for(admin, chat.id).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repository
            .update_chat(
                member,
                chat.id,
                Some(replaced.version),
                json!({"admins":[]})
            )
            .await,
        Err(Error::Invalid(_))
    ));
    let other = group(&repository, member, vec![added]).await;
    let left = repository.clone();
    let right = repository.clone();
    let (a, b) = tokio::join!(
        left.set_preference_list(member, "favourite_chats", chat.id, true),
        right.set_preference_list(member, "favourite_chats", other.id, true)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        repository.preferences(member).await.unwrap()["favourite_chats"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (_, cursor) = repository.feed(added, None, 1000).await.unwrap();
    let first = repository
        .post_message(message(chat.id, member, Some("feed-1".into())))
        .await
        .unwrap();
    let second = repository
        .post_message(message(chat.id, member, Some("feed-2".into())))
        .await
        .unwrap();
    // Deliberately equal history timestamps cannot lose a message in ETag replay.
    sqlx::query("UPDATE messages SET created_at='2026-11-04T00:00:00Z' WHERE id=ANY($1)")
        .bind(vec![first.id, second.id])
        .execute(repository.pool())
        .await
        .unwrap();
    let (batch, token) = repository.feed(added, Some(cursor), 1).await.unwrap();
    assert_eq!(batch.len(), 1);
    let (next, token) = repository.feed(added, Some(token), 1).await.unwrap();
    assert_eq!(next.len(), 1);
    assert_ne!(batch[0].id, next[0].id);
    let idle = repository.feed(added, Some(token), 1000).await.unwrap();
    assert!(idle.0.is_empty());
    assert_eq!(idle.1, token, "idle polls reuse a durable token");
    let (replayed, _) = repository.feed(added, Some(cursor), 1000).await.unwrap();
    assert_eq!(
        replayed.len(),
        2,
        "old tokens map to exact durable positions"
    );
    repository
        .read_chat(added, chat.id, Some(first.id))
        .await
        .unwrap();
    assert!(
        repository
            .readers(member, chat.id)
            .await
            .unwrap()
            .iter()
            .any(|reader| reader["user"] == json!(added))
    );
    repository
        .react(added, first.id, Some("test".into()))
        .await
        .unwrap();
    let repeated = repository
        .post_message(message(chat.id, member, Some("feed-1".into())))
        .await
        .unwrap();
    assert_eq!(
        repeated.id, first.id,
        "idempotency survives mutable metadata"
    );
    repository.delete_message(member, first.id).await.unwrap();
    let repeated = repository
        .post_message(message(chat.id, member, Some("feed-1".into())))
        .await
        .unwrap();
    assert!(
        repeated.deleted_at.is_some(),
        "retry never resurrects a tombstone"
    );
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_private_attachment_hop_forwarding_and_rollback() {
    let repository = setup_repository().await;
    let sender = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let other = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, sender, vec![member]).await;
    let forward = group(&repository, member, vec![other]).await;
    let private = Asset {
        id: Uuid::new_v4(),
        tenant_id: DEFAULT_TENANT,
        uploaded_by: sender,
        chat_id: None,
        content_type: "image/png".into(),
        size: 4,
        digest: "abcd".into(),
        created_at: Utc::now(),
    };
    repository.create_asset(private.clone()).await.unwrap();
    assert!(matches!(
        repository.asset_for(member, private.id).await,
        Err(Error::Forbidden)
    ));
    let mut post = message(chat.id, sender, None);
    post.attributes =
        json!({"gallery":json!([{ "url":format!("asset:///{}",private.id) }]).to_string()});
    repository.post_message(post).await.unwrap();
    assert_eq!(
        repository.asset_for(member, private.id).await.unwrap().id,
        private.id,
        "posting shares a private upload"
    );
    assert!(matches!(
        repository.asset_for(other, private.id).await,
        Err(Error::Forbidden)
    ));
    let mut post = message(forward.id, member, None);
    post.attributes = json!({"files":[{"id":private.id,"name":"forward.png"}]});
    repository.post_message(post).await.unwrap();
    assert_eq!(
        repository.asset_for(other, private.id).await.unwrap().id,
        private.id,
        "authorized forwarding creates another share"
    );
    let hidden = Asset {
        id: Uuid::new_v4(),
        tenant_id: DEFAULT_TENANT,
        uploaded_by: sender,
        chat_id: None,
        content_type: "application/octet-stream".into(),
        size: 2,
        digest: "efgh".into(),
        created_at: Utc::now(),
    };
    repository.create_asset(hidden.clone()).await.unwrap();
    let before = repository
        .messages(member, Some(forward.id), None, 1000)
        .await
        .unwrap()
        .len();
    let mut stolen = message(forward.id, member, None);
    stolen.attributes = json!({"files":[{"id":hidden.id}]});
    assert!(matches!(
        repository.post_message(stolen).await,
        Err(Error::Forbidden)
    ));
    assert_eq!(
        repository
            .messages(member, Some(forward.id), None, 1000)
            .await
            .unwrap()
            .len(),
        before,
        "failed attachment sharing rolls message back"
    );
    let shares: i64 = sqlx::query_scalar("SELECT count(*) FROM asset_shares WHERE asset_id=$1")
        .bind(hidden.id)
        .fetch_one(repository.pool())
        .await
        .unwrap();
    assert_eq!(shares, 0);
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_notify_is_a_committed_outbox_hint() {
    let repository = setup_repository().await;
    let sender = user(&repository, DEFAULT_TENANT).await;
    let member = user(&repository, DEFAULT_TENANT).await;
    let outsider = user(&repository, DEFAULT_TENANT).await;
    let chat = group(&repository, sender, vec![member]).await;
    let mut listener = repository.listener().await.unwrap();
    repository
        .post_message(message(chat.id, sender, None))
        .await
        .unwrap();
    let mut recipients = BTreeSet::new();
    while recipients.len() < 2 {
        let notification = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv())
            .await
            .unwrap()
            .unwrap();
        let user = Uuid::parse_str(notification.payload()).unwrap();
        // Other independent integration fixtures share this notification channel.
        if user != sender && user != member {
            continue;
        }
        recipients.insert(user);
        assert_eq!(
            repository
                .events(user, 0, 100)
                .await
                .unwrap()
                .last()
                .unwrap()
                .kind,
            "message_created"
        );
    }
    assert_eq!(recipients, BTreeSet::from([sender, member]));
    assert!(
        repository
            .post_message(message(chat.id, outsider, None))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), async {
            loop {
                let notification = listener.recv().await.unwrap();
                let id = Uuid::parse_str(notification.payload()).unwrap();
                if id == sender || id == member {
                    return;
                }
            }
        })
        .await
        .is_err(),
        "failed mutation emits no notification"
    );
    drop(listener);
    repository.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_overlapping_conversations_keep_recipient_sequences_contiguous() {
    let repository = setup_repository().await;
    let alice = user(&repository, DEFAULT_TENANT).await;
    let bob = user(&repository, DEFAULT_TENANT).await;
    let shared = user(&repository, DEFAULT_TENANT).await;
    let first = group(&repository, alice, vec![bob, shared]).await;
    let second = group(&repository, bob, vec![alice, shared]).await;
    let mut pending = Vec::new();
    for index in 0..24 {
        let repo = repository.clone();
        let (chat, sender) = if index % 2 == 0 {
            (first.id, alice)
        } else {
            (second.id, bob)
        };
        pending.push(tokio::spawn(async move {
            repo.post_message(message(chat, sender, Some(Uuid::new_v4().to_string())))
                .await
        }));
    }
    for write in pending {
        write.await.unwrap().unwrap();
    }
    for recipient in [alice, bob, shared] {
        let events = repository.events(recipient, 0, 1000).await.unwrap();
        assert_eq!(events.len(), 26);
        assert_eq!(
            events.iter().map(|event| event.id).collect::<Vec<_>>(),
            (1..=26).collect::<Vec<_>>()
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "message_created")
                .count(),
            24
        );
    }
    assert!(
        matches!(
            repository.messages(shared, Some(first.id), None, 10).await,
            Err(Error::Invalid(_))
        ),
        "a truncated history must not masquerade as a complete snapshot"
    );
    repository.verify_schema().await.unwrap();
    repository.close().await;
}
