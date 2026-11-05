//! Real PostgreSQL resource tests. The opt-in URL must select a disposable DB.
use hoowire_db::{DEFAULT_TENANT, Repository};
use hoowire_domain::{Error, NewChat, NewMessage, NewUser};
use serde_json::{Value, json};
use uuid::Uuid;

async fn setup() -> Repository {
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL").expect("isolated PostgreSQL URL required");
    let repo = Repository::connect(&url, 12).await.unwrap();
    repo.migrate().await.unwrap();
    repo
}

async fn user(repo: &Repository) -> Uuid {
    user_in_tenant(repo, DEFAULT_TENANT).await
}

async fn user_in_tenant(repo: &Repository, tenant_id: Uuid) -> Uuid {
    repo.create_user(NewUser {
        tenant_id,
        email_hash: Uuid::new_v4().to_string(),
        password_hash: "test-only".into(),
        profile: json!({}),
        is_bot: false,
    })
    .await
    .unwrap()
    .id
}

async fn chat(repo: &Repository, creator: Uuid, member: Uuid) -> Uuid {
    repo.create_chat(NewChat {
        tenant_id: DEFAULT_TENANT,
        created_by: creator,
        title: "resources".into(),
        kind: "group".into(),
        col: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        attributes: json!({}),
        members: vec![creator, member],
    })
    .await
    .unwrap()
    .id
}

async fn post(
    repo: &Repository,
    sender: Uuid,
    chat: Uuid,
    attributes: Value,
) -> hoowire_domain::Message {
    repo.post_message(NewMessage {
        chat_id: chat,
        sender_id: sender,
        body: "opaque encrypted message".into(),
        client_id: None,
        attributes,
    })
    .await
    .unwrap()
}

fn id(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_reaction_ownership_tombstones_pins_and_read_markers() {
    let repo = setup().await;
    let creator = user(&repo).await;
    let member = user(&repo).await;
    let outsider = user(&repo).await;
    let thread = chat(&repo, creator, member).await;
    let message = post(&repo, creator, thread, json!({})).await;
    let first = repo
        .wire_react(member, thread, message.id, None, Some("👍"))
        .await
        .unwrap();
    let reaction = id(&first);
    assert_eq!(first["action"], "React");
    assert_eq!(first["userId"], json!(member));
    assert_eq!(
        repo.wire_reactions(creator, Some(thread), Some(message.id), None, None)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        repo.wire_react(outsider, thread, message.id, None, Some("👍"))
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repo.wire_react(creator, thread, message.id, Some(reaction), Some("👎"))
            .await,
        Err(Error::NotFound)
    ));
    let updated = repo
        .wire_react(member, thread, message.id, Some(reaction), Some("👋"))
        .await
        .unwrap();
    assert_eq!(updated["id"], first["id"]);
    assert_eq!(updated["emoji"], "👋");
    let tombstone = repo
        .wire_react(member, thread, message.id, Some(reaction), None)
        .await
        .unwrap();
    assert_eq!(tombstone["action"], "Unreact");
    assert_eq!(
        repo.wire_reactions(member, None, None, None, None)
            .await
            .unwrap()[0]["action"],
        "Unreact"
    );
    assert_eq!(
        repo.events(member, 0, 100)
            .await
            .unwrap()
            .last()
            .unwrap()
            .kind,
        "reaction"
    );

    repo.set_pin(member, thread, message.id, true)
        .await
        .unwrap();
    let before = repo.events(member, 0, 100).await.unwrap().len();
    repo.set_pin(member, thread, message.id, true)
        .await
        .unwrap();
    assert_eq!(
        repo.events(member, 0, 100).await.unwrap().len(),
        before,
        "pin retry is idempotent"
    );
    assert_eq!(repo.pins(creator, thread).await.unwrap().len(), 1);
    assert!(matches!(
        repo.set_pin(outsider, thread, message.id, false).await,
        Err(Error::Forbidden)
    ));
    for _ in 0..4 {
        let msg = post(&repo, creator, thread, json!({})).await;
        repo.set_pin(member, thread, msg.id, true).await.unwrap();
    }
    let sixth = post(&repo, creator, thread, json!({})).await;
    assert!(matches!(
        repo.set_pin(member, thread, sixth.id, true).await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(repo.pins(creator, thread).await.unwrap().len(), 5);
    repo.delete_message(creator, message.id).await.unwrap();
    assert_eq!(
        repo.pins(creator, thread).await.unwrap().len(),
        4,
        "a deleted message is no longer pinned"
    );
    repo.set_pin(member, thread, sixth.id, true).await.unwrap();
    assert_eq!(repo.pins(creator, thread).await.unwrap().len(), 5);
    repo.set_pin(member, thread, sixth.id, false).await.unwrap();
    assert_eq!(repo.pins(creator, thread).await.unwrap().len(), 4);

    repo.read_chat(member, thread, Some(sixth.id))
        .await
        .unwrap();
    let readers = repo.thread_readers(creator, thread).await.unwrap();
    assert_eq!(readers.len(), 1);
    assert_eq!(readers[0]["user_id"], json!(member));
    assert_eq!(
        repo.read_threads(member).await.unwrap()[0]["tid"],
        json!(thread)
    );
    assert!(matches!(
        repo.thread_readers(outsider, thread).await,
        Err(Error::Forbidden)
    ));
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_poll_sharing_atomicity_voting_and_revocation() {
    let repo = setup().await;
    let creator = user(&repo).await;
    let member = user(&repo).await;
    let outsider = user(&repo).await;
    let thread = chat(&repo, creator, member).await;
    let poll = repo
        .create_poll(creator, "Which?", 3600, &["Yes".into(), "No".into()])
        .await
        .unwrap();
    let poll_id = id(&poll);
    assert_eq!(poll["choices"][0]["count"], 0);
    assert!(poll["expires_at"].is_string());
    assert!(matches!(
        repo.poll(member, poll_id).await,
        Err(Error::Forbidden)
    ));
    let attrs = json!({"interactive":json!([{"id":poll_id,"type":"poll","v":1}]).to_string()});
    let rejected = repo
        .post_message(NewMessage {
            chat_id: thread,
            sender_id: member,
            body: "not my poll".into(),
            client_id: None,
            attributes: attrs.clone(),
        })
        .await;
    assert!(matches!(rejected, Err(Error::Forbidden)));
    assert!(
        repo.messages(creator, Some(thread), None, 100)
            .await
            .unwrap()
            .is_empty(),
        "rejected attachment rolls back message"
    );
    post(&repo, creator, thread, attrs).await;
    assert_eq!(
        repo.poll(member, poll_id).await.unwrap()["question"],
        "Which?"
    );
    assert!(matches!(
        repo.poll(outsider, poll_id).await,
        Err(Error::Forbidden)
    ));

    let mut tasks = Vec::new();
    for index in 0..8 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.vote_poll(member, poll_id, index % 2).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let result = repo.poll(member, poll_id).await.unwrap();
    let sum: i64 = result["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["count"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, 1, "concurrent voting retains one vote per person");
    assert!(matches!(
        repo.vote_poll(member, poll_id, 99).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        repo.end_poll(member, poll_id).await,
        Err(Error::Forbidden)
    ));
    assert_eq!(
        repo.end_poll(creator, poll_id).await.unwrap()["has_ended"],
        true
    );
    assert!(matches!(
        repo.vote_poll(member, poll_id, 0).await,
        Err(Error::Conflict)
    ));
    repo.remove_members(creator, thread, &[member], None)
        .await
        .unwrap();
    assert!(matches!(
        repo.poll(member, poll_id).await,
        Err(Error::Forbidden)
    ));
    assert!(
        repo.create_poll(creator, "", 0, &["A".into(), "B".into()])
            .await
            .is_err()
    );
    assert!(
        repo.create_poll(creator, "?", 0, &["A".into()])
            .await
            .is_err()
    );
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_directory_members_are_persisted_and_tenant_scoped() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let pool = sqlx::PgPool::connect(&std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants(id,name) VALUES($1,'isolated-directory-test')")
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let member = user_in_tenant(&repo, tenant).await;
    let group = Uuid::new_v4();
    let before = repo.directory_revision(tenant).await.unwrap();
    repo.upsert_directory_entry(
        tenant,
        group,
        "group",
        json!({"title":"Support","user_ids":[member]}),
    )
    .await
    .unwrap();
    let after_group = repo.directory_revision(tenant).await.unwrap();
    assert!(after_group > before);
    assert_eq!(after_group.timestamp_subsec_micros() % 1000, 0);
    repo.update_profile(member, json!({"first_name":"Updated"}))
        .await
        .unwrap();
    let after_profile = repo.directory_revision(tenant).await.unwrap();
    assert!(after_profile > after_group);
    repo.set_preferences(member, json!({"muted_chats":[]}))
        .await
        .unwrap();
    assert_eq!(
        repo.directory_revision(tenant).await.unwrap(),
        after_profile,
        "outbox counters and preferences do not masquerade as directory changes"
    );
    let result = repo
        .resource_directory(tenant, "group", Some(group))
        .await
        .unwrap();
    assert_eq!(result[0]["title"], "Support");
    assert_eq!(result[0]["members"], json!([member]));
    assert_eq!(result[0]["users"], json!([member]));
    assert_eq!(
        repo.expand_directory_members(tenant, "group", &[group, group])
            .await
            .unwrap(),
        vec![member]
    );
    assert!(matches!(
        repo.expand_directory_members(Uuid::new_v4(), "group", &[group])
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        repo.expand_directory_members(tenant, "role", &[group])
            .await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.resource_directory(Uuid::new_v4(), "group", Some(group))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.resource_directory(tenant, "role", Some(group))
            .await
            .unwrap()
            .is_empty()
    );
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_membership_components_and_acl_replace_atomically() {
    let repo = setup().await;
    let creator = user(&repo).await;
    let old_member = user(&repo).await;
    let circle_member = user(&repo).await;
    let role_member = user(&repo).await;
    let group = Uuid::new_v4();
    let role = Uuid::new_v4();
    repo.upsert_directory_entry(
        DEFAULT_TENANT,
        group,
        "group",
        json!({"title":"Circle","users":[circle_member],"members":[circle_member]}),
    )
    .await
    .unwrap();
    repo.upsert_directory_entry(
        DEFAULT_TENANT,
        role,
        "role",
        json!({"title":"Role","user_ids":[role_member]}),
    )
    .await
    .unwrap();
    let mut desired = vec![creator];
    desired.extend(
        repo.expand_directory_members(DEFAULT_TENANT, "group", &[group])
            .await
            .unwrap(),
    );
    desired.extend(
        repo.expand_directory_members(DEFAULT_TENANT, "role", &[role])
            .await
            .unwrap(),
    );
    let thread = chat(&repo, creator, old_member).await;
    let current = repo.chat_for(creator, thread).await.unwrap();
    let components = json!({"users":[creator],"groups":[group],"roles":[role]});
    let updated = repo
        .replace_member_components(
            creator,
            thread,
            &desired,
            Some(current.version),
            Some(components.clone()),
        )
        .await
        .unwrap();
    assert_eq!(updated.attributes["groups"], components["groups"]);
    assert_eq!(updated.attributes["roles"], components["roles"]);
    assert_eq!(updated.attributes["users"], components["users"]);
    assert!(repo.chat_for(circle_member, thread).await.is_ok());
    assert!(repo.chat_for(role_member, thread).await.is_ok());
    assert!(matches!(
        repo.chat_for(old_member, thread).await,
        Err(Error::Forbidden)
    ));
    let mut members = repo.members(creator, thread).await.unwrap();
    members.sort_unstable();
    desired.sort_unstable();
    assert_eq!(members, desired);
    assert!(
        matches!(
            repo.replace_member_components(
                creator,
                thread,
                &[old_member],
                None,
                Some(json!({"users":[old_member],"groups":[],"roles":[]}))
            )
            .await,
            Err(Error::Invalid(_))
        ),
        "removing the last administrator rolls back ACL and component changes"
    );
    let unchanged = repo.chat_for(creator, thread).await.unwrap();
    assert_eq!(unchanged.version, updated.version);
    assert_eq!(unchanged.attributes, updated.attributes);
    assert!(matches!(
        repo.chat_for(old_member, thread).await,
        Err(Error::Forbidden)
    ));
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_resource_overflow_never_returns_a_false_complete_snapshot() {
    let repo = setup().await;
    let actor = user(&repo).await;
    let member = user(&repo).await;
    let thread = chat(&repo, actor, member).await;
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("WITH created AS (INSERT INTO messages(id,chat_id,sender_id,body,attributes) SELECT gen_random_uuid(),$1,$2,'opaque','{}'::jsonb FROM generate_series(1,1001) RETURNING id) INSERT INTO wire_reactions(id,message_id,chat_id,user_id,emoji,action) SELECT gen_random_uuid(),id,$1,$2,'ok','React' FROM created")
        .bind(thread).bind(actor).execute(&pool).await.unwrap();
    assert!(matches!(
        repo.wire_reactions(actor, Some(thread), None, None, None)
            .await,
        Err(Error::Invalid(_))
    ));
    let message: Uuid = sqlx::query_scalar("SELECT id FROM messages WHERE chat_id=$1 LIMIT 1")
        .bind(thread)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.wire_reactions(actor, Some(thread), Some(message), None, None)
            .await
            .unwrap()
            .len(),
        1
    );
    let now = chrono::Utc::now();
    assert!(matches!(
        repo.wire_reactions(
            actor,
            None,
            None,
            Some(now),
            Some(now - chrono::Duration::seconds(1))
        )
        .await,
        Err(Error::Invalid(_))
    ));
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants(id,name) VALUES($1,'resource-overflow-test')")
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO directory_groups(tenant_id,id,kind,title,attributes) SELECT $1,gen_random_uuid(),'group','overflow','{}'::jsonb FROM generate_series(1,1001)")
        .bind(tenant).execute(&pool).await.unwrap();
    assert!(matches!(
        repo.resource_directory(tenant, "group", None).await,
        Err(Error::Invalid(_))
    ));
    pool.close().await;
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_avatar_permissions_metadata_and_persistence() {
    let repo = setup().await;
    let owner = user(&repo).await;
    let member = user(&repo).await;
    let outsider = user(&repo).await;
    let thread = chat(&repo, owner, member).await;
    let first = vec![7u8; 128];
    let second = vec![9u8; 128];
    repo.set_avatar(owner, "user", owner, &first, "image/png")
        .await
        .unwrap();
    assert_eq!(
        repo.avatar(member, "user", owner)
            .await
            .unwrap()
            .bytes_sealed,
        first
    );
    assert_eq!(repo.user(owner).await.unwrap().profile["has_avatar"], true);
    assert!(matches!(
        repo.set_avatar(member, "user", owner, &second, "image/png")
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repo.remove_avatar(member, "user", owner).await,
        Err(Error::Forbidden)
    ));
    repo.set_avatar(owner, "chat", thread, &first, "image/png")
        .await
        .unwrap();
    let metadata = repo.chat_for(member, thread).await.unwrap();
    assert_eq!(metadata.attributes["hasAvatar"], true);
    assert_eq!(
        repo.avatar(member, "chat", thread)
            .await
            .unwrap()
            .bytes_sealed,
        first
    );
    assert_eq!(
        repo.avatar_infos(member, "chat", &[thread])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        repo.set_avatar(member, "chat", thread, &second, "image/png")
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repo.remove_avatar(member, "chat", thread).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        repo.avatar(outsider, "chat", thread).await,
        Err(Error::NotFound)
    ));
    repo.set_avatar(owner, "chat", thread, &second, "image/jpeg")
        .await
        .unwrap();
    assert_eq!(
        repo.avatar(member, "chat", thread)
            .await
            .unwrap()
            .bytes_sealed,
        second
    );
    assert!(
        repo.set_avatar(owner, "chat", thread, &first, "text/html")
            .await
            .is_err()
    );
    assert_eq!(
        repo.avatar(member, "chat", thread)
            .await
            .unwrap()
            .bytes_sealed,
        second,
        "rejected metadata rolls back byte replacement"
    );
    repo.remove_members(owner, thread, &[member], None)
        .await
        .unwrap();
    assert!(matches!(
        repo.avatar(member, "chat", thread).await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.avatar_infos(member, "chat", &[thread])
            .await
            .unwrap()
            .is_empty()
    );
    let url = std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let other_tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants(id,name) VALUES($1,'avatar-other-tenant')")
        .bind(other_tenant)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = repo
        .create_user(NewUser {
            tenant_id: other_tenant,
            email_hash: Uuid::new_v4().to_string(),
            password_hash: "test-only".into(),
            profile: json!({}),
            is_bot: false,
        })
        .await
        .unwrap()
        .id;
    assert!(matches!(
        repo.avatar(foreign, "user", owner).await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.avatar_infos(foreign, "user", &[owner])
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
    repo.close().await;
    let reopened = setup().await;
    assert_eq!(
        reopened
            .avatar(owner, "chat", thread)
            .await
            .unwrap()
            .bytes_sealed,
        second
    );
    reopened.remove_avatar(owner, "chat", thread).await.unwrap();
    reopened.remove_avatar(owner, "chat", thread).await.unwrap();
    assert_eq!(
        reopened.chat_for(owner, thread).await.unwrap().attributes["hasAvatar"],
        false
    );
    assert!(matches!(
        reopened.avatar(owner, "chat", thread).await,
        Err(Error::NotFound)
    ));
    reopened.remove_avatar(owner, "user", owner).await.unwrap();
    assert_eq!(
        reopened.user(owner).await.unwrap().profile["has_avatar"],
        false
    );
    reopened.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_voluntary_leave_does_not_require_admin_and_sole_member_can_leave() {
    let repo = setup().await;
    let admin = user(&repo).await;
    let member = user(&repo).await;
    let thread = chat(&repo, admin, member).await;
    let current = repo.chat_for(admin, thread).await.unwrap();
    assert!(matches!(
        repo.leave_chat(member, thread, Some(current.version + 1))
            .await,
        Err(Error::Conflict)
    ));
    assert!(
        matches!(
            repo.leave_chat(admin, thread, None).await,
            Err(Error::Invalid(_))
        ),
        "remaining members need an administrator"
    );
    let departing = post(&repo,member,thread,json!({"liveLocation":json!({"duration":900,"location":"encrypted-departing"}).to_string()})).await;
    repo.leave_chat(member, thread, None).await.unwrap();
    assert_eq!(
        repo.live_location(admin, thread, departing.id)
            .await
            .unwrap()["active"],
        false
    );
    assert!(matches!(
        repo.chat_for(member, thread).await,
        Err(Error::Forbidden)
    ));
    assert!(repo.chat_for(admin, thread).await.is_ok());
    assert_eq!(
        repo.events(member, 0, 100)
            .await
            .unwrap()
            .last()
            .unwrap()
            .kind,
        "thread_updated"
    );
    repo.leave_chat(admin, thread, None).await.unwrap();
    assert!(matches!(
        repo.chat_for(admin, thread).await,
        Err(Error::Forbidden)
    ));
    assert!(repo.chats(admin).await.unwrap().is_empty());
    repo.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_live_locations_ownership_atomicity_expiry_deletion_and_revocation() {
    let repo = setup().await;
    let owner = user(&repo).await;
    let member = user(&repo).await;
    let outsider = user(&repo).await;
    let thread = chat(&repo, owner, member).await;
    let attrs =
        json!({"liveLocation":json!({"duration":900,"location":"encrypted-initial"}).to_string()});
    let message = post(&repo, owner, thread, attrs.clone()).await;
    let list = repo.live_locations(member, Some(thread)).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], json!(message.id));
    assert_eq!(list[0]["active"], true);
    assert!(list[0]["last_active"].as_i64().unwrap() > 1_000_000_000_000);
    assert_eq!(
        repo.live_location(member, thread, message.id)
            .await
            .unwrap()["location"],
        "encrypted-initial"
    );
    assert!(matches!(
        repo.live_location(outsider, thread, message.id).await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.live_locations(outsider, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repo.update_live_locations(member, &[(thread, "not-my-share".into())])
            .await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.stop_live_locations(member, thread)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.live_locations(member, Some(thread))
            .await
            .unwrap()
            .len(),
        1
    );
    repo.update_live_locations(owner, &[(thread, "encrypted-updated".into())])
        .await
        .unwrap();
    assert_eq!(
        repo.live_location(member, thread, message.id)
            .await
            .unwrap()["location"],
        "encrypted-updated"
    );
    let missing_share = chat(&repo, owner, outsider).await;
    assert!(matches!(
        repo.update_live_locations(
            owner,
            &[
                (thread, "must-roll-back".into()),
                (missing_share, "missing-share".into())
            ]
        )
        .await,
        Err(Error::NotFound)
    ));
    assert_eq!(
        repo.live_location(member, thread, message.id)
            .await
            .unwrap()["location"],
        "encrypted-updated"
    );
    let rejected = repo
        .post_message(NewMessage {
            chat_id: thread,
            sender_id: owner,
            client_id: None,
            body: "invalid duration".into(),
            attributes: json!({"liveLocation":json!({"duration":0,"location":"no"}).to_string()}),
        })
        .await;
    assert!(matches!(rejected, Err(Error::Invalid(_))));
    assert_eq!(
        repo.messages(owner, Some(thread), None, 100)
            .await
            .unwrap()
            .len(),
        1,
        "invalid sharing rolls back containing message"
    );
    let stopped = repo.stop_live_locations(owner, thread).await.unwrap();
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["active"], false);
    assert!(
        repo.stop_live_locations(owner, thread)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.live_locations(member, Some(thread))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.live_location(member, thread, message.id)
            .await
            .unwrap()["active"],
        false
    );
    let expired = post(&repo, owner, thread, attrs.clone()).await;
    let pool = sqlx::PgPool::connect(&std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE live_locations SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE message_id=$1").bind(expired.id).execute(&pool).await.unwrap();
    pool.close().await;
    assert_eq!(
        repo.live_location(member, thread, expired.id)
            .await
            .unwrap()["active"],
        false
    );
    assert!(matches!(
        repo.update_live_locations(owner, &[(thread, "expired-update".into())])
            .await,
        Err(Error::NotFound)
    ));
    let deleted = post(&repo, owner, thread, attrs.clone()).await;
    repo.delete_message(owner, deleted.id).await.unwrap();
    assert!(matches!(
        repo.live_location(member, thread, deleted.id).await,
        Err(Error::NotFound)
    ));
    assert!(
        repo.live_locations(member, Some(thread))
            .await
            .unwrap()
            .is_empty()
    );
    let removal_thread = chat(&repo, owner, member).await;
    let removed_share = post(&repo, member, removal_thread, attrs.clone()).await;
    repo.remove_members(owner, removal_thread, &[member], None)
        .await
        .unwrap();
    assert_eq!(
        repo.live_location(owner, removal_thread, removed_share.id)
            .await
            .unwrap()["active"],
        false,
        "administrator removal must stop the removed member's share"
    );
    assert!(matches!(
        repo.live_location(member, removal_thread, removed_share.id)
            .await,
        Err(Error::NotFound)
    ));
    let check_pool = sqlx::PgPool::connect(&std::env::var("HOOWIRE_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT stopped_at IS NOT NULL FROM live_locations WHERE message_id=$1"
        )
        .bind(removed_share.id)
        .fetch_one(&check_pool)
        .await
        .unwrap(),
        "member removal must persist the stop, not only hide the share through ACL"
    );
    check_pool.close().await;
    repo.add_members(owner, removal_thread, &[member], None)
        .await
        .unwrap();
    assert_eq!(
        repo.live_location(member, removal_thread, removed_share.id)
            .await
            .unwrap()["active"],
        false,
        "rejoining must not revive an old location share"
    );
    let persistent = post(&repo, owner, thread, attrs).await;
    repo.leave_chat(member, thread, None).await.unwrap();
    assert!(matches!(
        repo.live_location(member, thread, persistent.id).await,
        Err(Error::NotFound)
    ));
    assert!(repo.live_locations(member, None).await.unwrap().is_empty());
    assert!(
        repo.events(owner, 0, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "live_location")
    );
    repo.close().await;
    let reopened = setup().await;
    assert_eq!(
        reopened
            .live_location(owner, thread, persistent.id)
            .await
            .unwrap()["active"],
        true
    );
    assert_eq!(
        reopened
            .live_location(member, removal_thread, removed_share.id)
            .await
            .unwrap()["active"],
        false,
        "the member-removal stop must survive a repository restart"
    );
    reopened.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL and HOOWIRE_TEST_DATABASE_URL"]
async fn postgres_live_batch_chat_lock_order_is_consistent() {
    let repo = setup().await;
    let owner = user(&repo).await;
    let member = user(&repo).await;
    let a = chat(&repo, owner, member).await;
    let b = chat(&repo, owner, member).await;
    let attrs =
        json!({"liveLocation":json!({"duration":900,"location":"encrypted-start"}).to_string()});
    let ma = post(&repo, owner, a, attrs.clone()).await;
    let mb = post(&repo, owner, b, attrs).await;
    let mut tasks = vec![];
    for index in 0..8 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            let encrypted = format!("encrypted-update-{index}");
            let updates = if index % 2 == 0 {
                vec![(a, encrypted.clone()), (b, encrypted)]
            } else {
                vec![(b, encrypted.clone()), (a, encrypted)]
            };
            repo.update_live_locations(owner, &updates).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let first = repo.live_location(member, a, ma.id).await.unwrap();
    let second = repo.live_location(member, b, mb.id).await.unwrap();
    assert_eq!(
        first["location"], second["location"],
        "a concurrent batch cannot leave mixed coordinates across chats"
    );
    repo.close().await;
}
