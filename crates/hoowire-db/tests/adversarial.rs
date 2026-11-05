//! Behavioral failure and concurrency contracts against real PostgreSQL.
//! Every case uses fresh tenants; the opt-in URL must select a disposable DB.
use std::{collections::BTreeSet, future::Future, sync::Arc, time::Duration};

use chrono::Utc;
use hoowire_db::Repository;
use hoowire_domain::{Asset, Chat, Error, NewChat, NewMessage, NewUser};
use serde_json::{Value, json};
use tokio::sync::Barrier;
use uuid::Uuid;

async fn bounded(future: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(45), future)
        .await
        .expect("PostgreSQL scenario exceeded its 45-second deadline");
}

async fn setup() -> Repository {
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL")
        .expect("set HOOWIRE_TEST_DATABASE_URL to a disposable PostgreSQL database");
    let repo = Repository::connect(&url, 16).await.unwrap();
    repo.migrate().await.unwrap();
    repo
}

async fn user(repo: &Repository, tenant: Uuid) -> Uuid {
    repo.create_user(NewUser {
        tenant_id: tenant,
        email_hash: Uuid::new_v4().to_string(),
        password_hash: "adversarial-test-only".into(),
        profile: json!({}),
        is_bot: false,
    })
    .await
    .unwrap()
    .id
}

async fn group(repo: &Repository, tenant: Uuid, owner: Uuid, members: &[Uuid]) -> Chat {
    repo.create_chat(NewChat {
        tenant_id: tenant,
        created_by: owner,
        title: "Adversarial contract".into(),
        kind: "group".into(),
        col: Uuid::new_v4().to_string(),
        attributes: json!({}),
        members: members.to_vec(),
    })
    .await
    .unwrap()
}

fn message(chat: Uuid, sender: Uuid, key: &str, attributes: Value) -> NewMessage {
    NewMessage {
        chat_id: chat,
        sender_id: sender,
        client_id: Some(key.into()),
        body: "original encrypted body".into(),
        attributes,
    }
}

async fn outboxes(repo: &Repository, users: &[Uuid]) -> Value {
    let mut values = Vec::new();
    for user in users {
        values.push(json!([user, repo.events(*user, 0, 1000).await.unwrap()]));
    }
    json!(values)
}

async fn private_asset(repo: &Repository, tenant: Uuid, owner: Uuid, id: Uuid) {
    repo.create_asset(Asset {
        id,
        tenant_id: tenant,
        uploaded_by: owner,
        chat_id: None,
        content_type: "application/octet-stream".into(),
        size: 3,
        digest: "test-metadata-only".into(),
        created_at: Utc::now(),
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn mixed_resources_roll_back_earlier_grants_outbox_and_idempotency() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("mixed-resource-rollback").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let hidden_owner = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[peer]).await;
        let mut asset_ids = [Uuid::new_v4(), Uuid::new_v4()];
        asset_ids.sort_unstable();
        let [allowed, hidden] = asset_ids;
        // Deterministically visit the allowed asset first, before rejecting the
        // hidden one. Poll attachment also precedes asset attachment.
        private_asset(&repo, tenant, owner, allowed).await;
        private_asset(&repo, tenant, hidden_owner, hidden).await;
        let poll = repo
            .create_poll(
                owner,
                "Private until committed?",
                0,
                &["Yes".into(), "No".into()],
            )
            .await
            .unwrap();
        let poll_id = Uuid::parse_str(poll["id"].as_str().unwrap()).unwrap();
        let before = outboxes(&repo, &[owner, peer, hidden_owner]).await;
        let key = Uuid::new_v4().to_string();
        let rejected = message(
            chat.id,
            owner,
            &key,
            json!({"interactive":[{"type":"poll","id":poll_id}],
                "files":[{"id":allowed},{"id":hidden}]}),
        );
        assert!(matches!(
            repo.post_message(rejected).await,
            Err(Error::Forbidden)
        ));
        assert!(
            repo.messages(peer, Some(chat.id), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            repo.poll(peer, poll_id).await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.asset_for(peer, allowed).await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.asset_for(peer, hidden).await,
            Err(Error::Forbidden)
        ));
        assert_eq!(
            json!(repo.chat_for(owner, chat.id).await.unwrap()),
            json!(chat)
        );
        assert_eq!(outboxes(&repo, &[owner, peer, hidden_owner]).await, before);

        // A failed request must not reserve its idempotency key or fingerprint.
        let accepted = repo
            .post_message(message(
                chat.id,
                owner,
                &key,
                json!({"interactive":[{"type":"poll","id":poll_id}],"files":[{"id":allowed}]}),
            ))
            .await
            .unwrap();
        assert_eq!(
            repo.message_for(peer, accepted.id).await.unwrap().id,
            accepted.id
        );
        assert_eq!(repo.asset_for(peer, allowed).await.unwrap().id, allowed);
        assert_eq!(repo.poll(peer, poll_id).await.unwrap()["id"], poll["id"]);
        assert!(matches!(
            repo.asset_for(peer, hidden).await,
            Err(Error::Forbidden)
        ));
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn invalid_late_location_attachment_cannot_publish_polls_or_assets() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("late-resource-rollback").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[peer]).await;
        let asset = Uuid::new_v4();
        private_asset(&repo, tenant, owner, asset).await;
        let poll = repo
            .create_poll(owner, "Commit?", 0, &["A".into(), "B".into()])
            .await
            .unwrap();
        let poll_id = Uuid::parse_str(poll["id"].as_str().unwrap()).unwrap();
        let before = outboxes(&repo, &[owner, peer]).await;
        let rejected = message(
            chat.id,
            owner,
            "invalid-live-location",
            json!({
                "interactive":[{"type":"poll","id":poll_id}],
                "files":[{"id":asset}],
                "liveLocation":{"duration":0,"location":"opaque"}
            }),
        );
        assert!(matches!(
            repo.post_message(rejected).await,
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            repo.poll(peer, poll_id).await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.asset_for(peer, asset).await,
            Err(Error::Forbidden)
        ));
        assert!(
            repo.live_locations(peer, Some(chat.id))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.messages(peer, Some(chat.id), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            json!(repo.chat_for(owner, chat.id).await.unwrap()),
            json!(chat)
        );
        assert_eq!(outboxes(&repo, &[owner, peer]).await, before);
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn stale_versions_cannot_change_acl_components_or_emit_events() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("stale-version-rollback").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let added = user(&repo, tenant).await;
        let initial = group(&repo, tenant, owner, &[peer]).await;
        let current = repo
            .update_chat(
                owner,
                initial.id,
                Some(initial.version),
                json!({"title":"Committed title","admins":[owner,peer]}),
            )
            .await
            .unwrap();
        let stale = Some(initial.version);
        let before = outboxes(&repo, &[owner, peer, added]).await;
        let members = repo.members(owner, current.id).await.unwrap();
        let admins = repo.administrators(owner, current.id).await.unwrap();
        let results = [
            repo.update_chat(
                owner,
                current.id,
                stale,
                json!({"title":"stale","admins":[owner]}),
            )
            .await,
            repo.add_members(owner, current.id, &[added], stale).await,
            repo.remove_members(owner, current.id, &[peer], stale).await,
            repo.replace_members(owner, current.id, &[owner, added], stale)
                .await,
            repo.replace_member_components(
                owner,
                current.id,
                &[owner, added],
                stale,
                Some(json!({"users":[owner,added],"groups":[],"roles":[]})),
            )
            .await,
            repo.leave_chat(owner, current.id, stale).await,
        ];
        for result in results {
            assert!(
                matches!(result, Err(Error::Conflict)),
                "stale mutation: {result:?}"
            );
        }
        assert_eq!(
            json!(repo.chat_for(owner, current.id).await.unwrap()),
            json!(current)
        );
        assert_eq!(repo.members(owner, current.id).await.unwrap(), members);
        assert_eq!(
            repo.administrators(owner, current.id).await.unwrap(),
            admins
        );
        assert!(matches!(
            repo.chat_for(added, current.id).await,
            Err(Error::Forbidden)
        ));
        assert_eq!(outboxes(&repo, &[owner, peer, added]).await, before);
        repo.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn concurrent_acl_removal_orders_all_member_mutations_before_revocation() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("acl-mutation-race").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[peer]).await;
        let original = repo
            .post_message(message(chat.id, peer, "original", json!({})))
            .await
            .unwrap();
        let cursor = repo.events(peer, 0, 100).await.unwrap().last().unwrap().id;
        let barrier = Arc::new(Barrier::new(7));
        let mut tasks = Vec::new();
        for operation in 0..5 {
            let repo = repo.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                match operation {
                    0 => repo
                        .update_message(peer, original.id, json!({"edited":true}))
                        .await
                        .map(|_| ()),
                    1 => repo
                        .post_message(message(chat.id, peer, "racing-post", json!({})))
                        .await
                        .map(|_| ()),
                    2 => repo.read_chat(peer, chat.id, Some(original.id)).await,
                    3 => repo
                        .wire_react(peer, chat.id, original.id, None, Some("yes"))
                        .await
                        .map(|_| ()),
                    _ => repo.set_pin(peer, chat.id, original.id, true).await,
                }
            }));
        }
        let removal_repo = repo.clone();
        let removal_barrier = barrier.clone();
        let removal = tokio::spawn(async move {
            removal_barrier.wait().await;
            removal_repo
                .remove_members(owner, chat.id, &[peer], None)
                .await
        });
        barrier.wait().await;
        let mut committed = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(()) => committed += 1,
                Err(Error::Forbidden) => {}
                other => {
                    panic!("mutation must commit before removal or fail authorization: {other:?}")
                }
            }
        }
        removal.await.unwrap().unwrap();
        for actor in [owner, peer] {
            let events = repo.events(actor, cursor, 100).await.unwrap();
            assert_eq!(events.len(), committed + 1);
            assert_eq!(
                events.last().unwrap().kind,
                "thread_updated",
                "no member mutation may be emitted after the removal event"
            );
            assert_eq!(events.last().unwrap().payload["chat_id"], json!(chat.id));
        }
        assert!(matches!(
            repo.chat_for(peer, chat.id).await,
            Err(Error::Forbidden)
        ));
        let before = outboxes(&repo, &[owner, peer]).await;
        assert!(matches!(
            repo.update_message(peer, original.id, json!({"late":true}))
                .await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.post_message(message(chat.id, peer, "racing-post", json!({})))
                .await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.read_chat(peer, chat.id, Some(original.id)).await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.wire_react(peer, chat.id, original.id, None, Some("late"))
                .await,
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            repo.set_pin(peer, chat.id, original.id, true).await,
            Err(Error::Forbidden)
        ));
        assert_eq!(outboxes(&repo, &[owner, peer]).await, before);
        repo.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn competing_idempotent_payloads_have_one_winner_and_one_outbox_record() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo
            .create_tenant("idempotency-content-race")
            .await
            .unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[peer]).await;
        let key = Uuid::new_v4().to_string();
        let barrier = Arc::new(Barrier::new(13));
        let mut tasks = Vec::new();
        for index in 0..12 {
            let repo = repo.clone();
            let barrier = barrier.clone();
            let key = key.clone();
            tasks.push(tokio::spawn(async move {
                let mut request = message(chat.id, owner, &key, json!({}));
                request.body = format!("encrypted-payload-{}", index % 2);
                let submitted = request.body.clone();
                barrier.wait().await;
                (submitted, repo.post_message(request).await)
            }));
        }
        barrier.wait().await;
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for task in tasks {
            let (body, result) = task.await.unwrap();
            match result {
                Ok(posted) => {
                    assert_eq!(posted.body, body);
                    accepted.push(posted);
                }
                Err(Error::Conflict) => rejected.push(body),
                other => panic!("unexpected idempotency outcome: {other:?}"),
            }
        }
        assert_eq!(accepted.len(), 6);
        assert_eq!(rejected.len(), 6);
        let winner = &accepted[0];
        assert!(
            accepted
                .iter()
                .all(|value| value.id == winner.id && value.body == winner.body)
        );
        assert!(rejected.iter().all(|body| body != &winner.body));
        let history = repo.messages(peer, Some(chat.id), None, 100).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, winner.id);
        assert_eq!(
            repo.chat_for(owner, chat.id).await.unwrap().version,
            chat.version + 1
        );
        for actor in [owner, peer] {
            let events = repo.events(actor, 0, 100).await.unwrap();
            assert_eq!(events.len(), 2);
            assert_eq!(events[1].kind, "message_created");
            assert_eq!(events[1].payload["message_id"], json!(winner.id));
        }
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn idempotency_keys_are_scoped_by_sender_and_chat() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("idempotency-scope").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let first = group(&repo, tenant, owner, &[peer]).await;
        let second = group(&repo, tenant, owner, &[peer]).await;
        let key = Uuid::new_v4().to_string();
        let mut posted = Vec::new();
        for chat in [first.id, second.id] {
            for sender in [owner, peer] {
                posted.push(
                    repo.post_message(message(chat, sender, &key, json!({})))
                        .await
                        .unwrap(),
                );
            }
        }
        assert_eq!(
            posted
                .iter()
                .map(|value| value.id)
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        for chat in [first.id, second.id] {
            assert_eq!(
                repo.messages(peer, Some(chat), None, 100)
                    .await
                    .unwrap()
                    .len(),
                2
            );
        }
        let before = outboxes(&repo, &[owner, peer]).await;
        for original in posted {
            let retried = repo
                .post_message(message(
                    original.chat_id,
                    original.sender_id,
                    &key,
                    json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(retried.id, original.id);
        }
        assert_eq!(outboxes(&repo, &[owner, peer]).await, before);
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn revoked_sender_cannot_retry_and_rejoining_never_resurrects_a_tombstone() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("revoked-idempotency").await.unwrap();
        let owner = user(&repo, tenant).await;
        let peer = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[peer]).await;
        let key = Uuid::new_v4().to_string();
        let posted = repo
            .post_message(message(chat.id, peer, &key, json!({"original":true})))
            .await
            .unwrap();
        repo.update_message(peer, posted.id, json!({"changed":true}))
            .await
            .unwrap();
        repo.delete_message(owner, posted.id).await.unwrap();
        repo.remove_members(owner, chat.id, &[peer], None)
            .await
            .unwrap();
        let before = outboxes(&repo, &[owner, peer]).await;
        assert!(matches!(
            repo.post_message(message(chat.id, peer, &key, json!({"original":true})))
                .await,
            Err(Error::Forbidden)
        ));
        assert_eq!(outboxes(&repo, &[owner, peer]).await, before);
        repo.add_members(owner, chat.id, &[peer], None)
            .await
            .unwrap();
        let before = outboxes(&repo, &[owner, peer]).await;
        let current = repo.chat_for(owner, chat.id).await.unwrap();
        let retried = repo
            .post_message(message(chat.id, peer, &key, json!({"original":true})))
            .await
            .unwrap();
        assert_eq!(retried.id, posted.id);
        assert!(retried.deleted_at.is_some());
        assert!(retried.body.is_empty());
        assert_eq!(retried.attributes, json!({}));
        assert!(matches!(
            repo.post_message(message(chat.id, peer, &key, json!({"original":false})))
                .await,
            Err(Error::Conflict)
        ));
        assert_eq!(
            json!(repo.chat_for(owner, chat.id).await.unwrap()),
            json!(current)
        );
        assert_eq!(outboxes(&repo, &[owner, peer]).await, before);
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn cross_tenant_directory_replacement_restores_title_members_and_revision() {
    bounded(async {
        let repo = setup().await;
        let left = repo.create_tenant("directory-left").await.unwrap();
        let right = repo.create_tenant("directory-right").await.unwrap();
        let alice = user(&repo, left).await;
        let bob = user(&repo, right).await;
        let shared_id = Uuid::new_v4();
        for (tenant, member, title) in [(left, alice, "Left"), (right, bob, "Right")] {
            repo.upsert_directory_entry(
                tenant,
                shared_id,
                "group",
                json!({"title":title,"user_ids":[member]}),
            )
            .await
            .unwrap();
        }
        let left_before = repo
            .resource_directory(left, "group", Some(shared_id))
            .await
            .unwrap();
        let right_before = repo
            .resource_directory(right, "group", Some(shared_id))
            .await
            .unwrap();
        let left_revision = repo.directory_revision(left).await.unwrap();
        let right_revision = repo.directory_revision(right).await.unwrap();
        assert!(matches!(
            repo.upsert_directory_entry(
                left,
                shared_id,
                "group",
                json!({"title":"Must roll back","user_ids":[alice,bob]})
            )
            .await,
            Err(Error::Invalid(_))
        ));
        assert_eq!(
            repo.resource_directory(left, "group", Some(shared_id))
                .await
                .unwrap(),
            left_before
        );
        assert_eq!(
            repo.resource_directory(right, "group", Some(shared_id))
                .await
                .unwrap(),
            right_before
        );
        assert_eq!(
            repo.directory_members(left, shared_id, "group")
                .await
                .unwrap(),
            vec![alice]
        );
        assert_eq!(
            repo.directory_members(right, shared_id, "group")
                .await
                .unwrap(),
            vec![bob]
        );
        assert_eq!(repo.directory_revision(left).await.unwrap(), left_revision);
        assert_eq!(
            repo.directory_revision(right).await.unwrap(),
            right_revision
        );
        repo.close().await;
    })
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn expired_polls_remain_readable_but_reject_new_votes_and_vote_changes() {
    bounded(async {
        let repo = setup().await;
        let tenant = repo.create_tenant("poll-expiry").await.unwrap();
        let owner = user(&repo, tenant).await;
        let voter = user(&repo, tenant).await;
        let nonvoter = user(&repo, tenant).await;
        let chat = group(&repo, tenant, owner, &[voter, nonvoter]).await;
        let poll = repo
            .create_poll(owner, "Expiry contract", 3600, &["A".into(), "B".into()])
            .await
            .unwrap();
        let poll_id = Uuid::parse_str(poll["id"].as_str().unwrap()).unwrap();
        repo.post_message(message(
            chat.id,
            owner,
            "share-expiring-poll",
            json!({"interactive":[{"type":"poll","id":poll_id}]}),
        ))
        .await
        .unwrap();
        repo.vote_poll(voter, poll_id, 0).await.unwrap();
        // Move only the fixture clock; assertions use the public resource API.
        sqlx::query(
            "UPDATE poll_records SET created_at=clock_timestamp()-INTERVAL '2 hours' WHERE id=$1",
        )
        .bind(poll_id)
        .execute(repo.pool())
        .await
        .unwrap();
        let before = repo.poll(voter, poll_id).await.unwrap();
        let events = outboxes(&repo, &[owner, voter, nonvoter]).await;
        assert_eq!(before["has_ended"], true);
        assert_eq!(before["my_choice"], 0);
        assert!(matches!(
            repo.vote_poll(voter, poll_id, 1).await,
            Err(Error::Conflict)
        ));
        assert!(matches!(
            repo.vote_poll(nonvoter, poll_id, 1).await,
            Err(Error::Conflict)
        ));
        assert_eq!(repo.poll(voter, poll_id).await.unwrap(), before);
        assert_eq!(
            repo.poll(nonvoter, poll_id).await.unwrap()["my_choice"],
            Value::Null
        );
        assert_eq!(outboxes(&repo, &[owner, voter, nonvoter]).await, events);
        repo.close().await;
    })
    .await;
}
