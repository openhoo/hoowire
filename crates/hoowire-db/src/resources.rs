//! Durable reactions, polls, pins and read-marker queries.
use super::*;

const RESOURCE_LIMIT: usize = 1000;

fn complete_page<T>(rows: Vec<T>, resource: &str) -> Result<Vec<T>> {
    if rows.len() > RESOURCE_LIMIT {
        return Err(Error::Invalid(format!(
            "{resource} exceeds {RESOURCE_LIMIT} results; complete pagination is not implemented"
        )));
    }
    Ok(rows)
}

fn reaction_wire(row: &PgRow) -> Result<Value> {
    let time: DateTime<Utc> = row.try_get("updated_at").map_err(db_error)?;
    let reaction: String = row.try_get("emoji").map_err(db_error)?;
    Ok(json!({
        "id":row.try_get::<Uuid,_>("id").map_err(db_error)?,
        "mid":row.try_get::<Uuid,_>("message_id").map_err(db_error)?,
        "tid":row.try_get::<Uuid,_>("chat_id").map_err(db_error)?,
        "user_id":row.try_get::<Uuid,_>("user_id").map_err(db_error)?,
        "userId":row.try_get::<Uuid,_>("user_id").map_err(db_error)?,
        "emoji":reaction,"reaction":reaction,
        "action":row.try_get::<String,_>("action").map_err(db_error)?,
        "ts":time.timestamp_millis(),
        "created_at":time.to_rfc3339(),
    }))
}

async fn visible_poll(tx: &mut Tx<'_>, user: Uuid, id: Uuid, lock: bool) -> Result<PgRow> {
    let sql = if lock {
        "SELECT p.* FROM poll_records p WHERE p.id=$2 AND (p.created_by=$1 OR EXISTS(SELECT 1 FROM poll_chats pc JOIN chat_members cm ON cm.chat_id=pc.chat_id WHERE pc.poll_id=p.id AND cm.user_id=$1)) FOR UPDATE"
    } else {
        "SELECT p.* FROM poll_records p WHERE p.id=$2 AND (p.created_by=$1 OR EXISTS(SELECT 1 FROM poll_chats pc JOIN chat_members cm ON cm.chat_id=pc.chat_id WHERE pc.poll_id=p.id AND cm.user_id=$1))"
    };
    sqlx::query(sql)
        .bind(user)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or(Error::Forbidden)
}

async fn poll_wire(tx: &mut Tx<'_>, user: Uuid, row: &PgRow) -> Result<Value> {
    let id: Uuid = row.try_get("id").map_err(db_error)?;
    let created: DateTime<Utc> = row.try_get("created_at").map_err(db_error)?;
    let duration: i64 = row.try_get("duration").map_err(db_error)?;
    let ended: Option<DateTime<Utc>> = row.try_get("ended_at").map_err(db_error)?;
    let expired = duration > 0 && created + chrono::Duration::seconds(duration) <= Utc::now();
    let choices = sqlx::query("SELECT c.choice_id,c.label,count(v.user_id) AS votes FROM poll_choices c LEFT JOIN poll_votes v ON v.poll_id=c.poll_id AND v.choice_id=c.choice_id WHERE c.poll_id=$1 GROUP BY c.choice_id,c.label ORDER BY c.choice_id")
        .bind(id).fetch_all(&mut **tx).await.map_err(db_error)?;
    let choices = choices
        .into_iter()
        .map(|choice| {
            Ok(
                json!({"id":choice.try_get::<i32,_>("choice_id").map_err(db_error)?,
            "text":choice.try_get::<String,_>("label").map_err(db_error)?,
            "count":choice.try_get::<i64,_>("votes").map_err(db_error)?}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let my_choice: Option<i32> =
        sqlx::query_scalar("SELECT choice_id FROM poll_votes WHERE poll_id=$1 AND user_id=$2")
            .bind(id)
            .bind(user)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    let expires =
        (duration > 0).then(|| (created + chrono::Duration::seconds(duration)).to_rfc3339());
    Ok(
        json!({"id":id,"type":"poll","question":row.try_get::<String,_>("question").map_err(db_error)?,
        "created_at":created.to_rfc3339(),"expires_at":expires,"duration":duration,"has_ended":ended.is_some() || expired,
        "my_choice":my_choice,"choices":choices,"creator":row.try_get::<Uuid,_>("created_by").map_err(db_error)?}),
    )
}

/// Associate a poll with a chat in the message's own transaction.
/// Polls remain private to their creator until their first authorised reference.
pub(super) async fn attach_message_resources(
    tx: &mut Tx<'_>,
    actor: Uuid,
    chat: Uuid,
    attributes: &Value,
) -> Result<()> {
    let Some(interactive) = attributes.get("interactive") else {
        return Ok(());
    };
    let entries = match interactive {
        Value::String(text) => serde_json::from_str::<Value>(text)
            .map_err(|_| Error::Invalid("invalid interactive attachment".into()))?,
        Value::Array(_) => interactive.clone(),
        Value::Null => return Ok(()),
        _ => return Err(Error::Invalid("invalid interactive attachment".into())),
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| Error::Invalid("interactive attachments must be a list".into()))?;
    if entries.len() > 20 {
        return Err(Error::Invalid("too many interactive attachments".into()));
    }
    let mut polls = BTreeSet::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) == Some("poll") {
            let id = hoowire_domain::identifier(
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Invalid("missing poll ID".into()))?,
            )?;
            polls.insert(id);
        } else {
            return Err(Error::Unsupported);
        }
    }
    for poll in polls {
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT created_by FROM poll_records WHERE id=$1 FOR UPDATE")
                .bind(poll)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_error)?;
        if owner != Some(actor) {
            return Err(Error::Forbidden);
        }
        sqlx::query("INSERT INTO poll_chats(poll_id,chat_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
            .bind(poll)
            .bind(chat)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

impl Repository {
    /// A member can unsubscribe without administrator privileges. A remaining
    /// conversation must retain an administrator; a sole member can leave.
    pub async fn leave_chat(
        &self,
        actor: Uuid,
        chat: Uuid,
        expected_version: Option<i64>,
    ) -> Result<Chat> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = lock_chat(&mut tx, actor, chat, false, expected_version).await?;
        let before = recipients(&mut tx, chat).await?;
        let another_admin: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_members WHERE chat_id=$1 AND user_id<>$2 AND is_admin)")
            .bind(chat).bind(actor).fetch_one(&mut *tx).await.map_err(db_error)?;
        if before.len() > 1 && !another_admin {
            let is_admin: bool = sqlx::query_scalar(
                "SELECT is_admin FROM chat_members WHERE chat_id=$1 AND user_id=$2",
            )
            .bind(chat)
            .bind(actor)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if is_admin {
                return Err(Error::Invalid(
                    "assign another administrator before leaving".into(),
                ));
            }
        }
        super::locations::stop_member_locations(&mut tx, actor, chat).await?;
        sqlx::query("DELETE FROM chat_members WHERE chat_id=$1 AND user_id=$2")
            .bind(chat)
            .bind(actor)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let mut attributes = current.attributes;
        let actor_id = actor.to_string();
        for field in ["users", "admins"] {
            if let Some(ids) = attributes.get_mut(field).and_then(Value::as_array_mut) {
                ids.retain(|id| id.as_str() != Some(actor_id.as_str()));
            }
        }
        let updated = bump_chat(&mut tx, chat, &attributes, &current.title).await?;
        emit(
            &mut tx,
            &before,
            "thread_updated",
            &json!({"chat_id":chat,"thread_id":chat,"version":updated.version}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(updated)
    }

    pub async fn directory_revision(&self, tenant: Uuid) -> Result<DateTime<Utc>> {
        sqlx::query_scalar("SELECT changed_at FROM directory_revisions WHERE tenant_id=$1")
            .bind(tenant)
            .fetch_optional(&self.0)
            .await
            .map_err(db_error)?
            .ok_or(Error::NotFound)
    }

    pub async fn replace_member_components(
        &self,
        actor: Uuid,
        chat: Uuid,
        members: &[Uuid],
        expected_version: Option<i64>,
        components: Option<Value>,
    ) -> Result<Chat> {
        let desired: BTreeSet<_> = members.iter().copied().collect();
        if desired.is_empty() || desired.len() > 1000 {
            return Err(Error::Invalid("invalid membership count".into()));
        }
        let desired: Vec<_> = desired.into_iter().collect();
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = lock_chat(&mut tx, actor, chat, true, expected_version).await?;
        let before = recipients(&mut tx, chat).await?;
        if one_to_one(&current.kind) && before != desired {
            return Err(Error::Invalid("one-to-one membership is immutable".into()));
        }
        let found: i64 =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE tenant_id=$1 AND id=ANY($2)")
                .bind(current.tenant_id)
                .bind(&desired)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if found as usize != desired.len() {
            return Err(Error::Invalid(
                "members must belong to the same tenant".into(),
            ));
        }
        for removed in before
            .iter()
            .filter(|user| desired.binary_search(user).is_err())
        {
            super::locations::stop_member_locations(&mut tx, *removed, chat).await?;
        }
        sqlx::query("DELETE FROM chat_members WHERE chat_id=$1 AND NOT(user_id=ANY($2))")
            .bind(chat)
            .bind(&desired)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for user in &desired {
            sqlx::query("INSERT INTO chat_members(chat_id,user_id,tenant_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
                .bind(chat).bind(user).bind(current.tenant_id).execute(&mut *tx).await.map_err(db_error)?;
        }
        let admins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM chat_members WHERE chat_id=$1 AND is_admin")
                .bind(chat)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if admins == 0 {
            return Err(Error::Invalid(
                "the last administrator cannot be removed".into(),
            ));
        }
        let mut attributes = current.attributes;
        if let Some(components) = components {
            object(&components)?;
            if components.as_object().is_none_or(|fields| {
                fields
                    .keys()
                    .any(|key| !matches!(key.as_str(), "users" | "groups" | "roles"))
            }) {
                return Err(Error::Invalid("invalid membership component fields".into()));
            }
            merge(&mut attributes, components);
        }
        let updated = bump_chat(&mut tx, chat, &attributes, &current.title).await?;
        emit(
            &mut tx,
            &before.into_iter().chain(desired).collect::<Vec<_>>(),
            "thread_updated",
            &json!({"chat_id":chat,"thread_id":chat,"version":updated.version}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(updated)
    }

    /// Resolve a set of directory IDs inside one tenant without N+1 member reads.
    pub async fn expand_directory_members(
        &self,
        tenant: Uuid,
        kind: &str,
        ids: &[Uuid],
    ) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        if !matches!(kind, "group" | "role") || ids.len() > 1000 {
            return Err(Error::Invalid(
                "invalid directory membership selection".into(),
            ));
        }
        let ids: Vec<_> = ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let row = sqlx::query("SELECT (SELECT count(*) FROM directory_groups WHERE tenant_id=$1 AND kind=$2 AND id=ANY($3)) AS matched, COALESCE((SELECT array_agg(DISTINCT user_id ORDER BY user_id) FROM directory_group_members WHERE tenant_id=$1 AND kind=$2 AND group_id=ANY($3)), ARRAY[]::UUID[]) AS members")
            .bind(tenant).bind(kind).bind(&ids).fetch_one(&self.0).await.map_err(db_error)?;
        if row.try_get::<i64, _>("matched").map_err(db_error)? as usize != ids.len() {
            return Err(Error::NotFound);
        }
        row.try_get("members").map_err(db_error)
    }

    pub async fn resource_directory(
        &self,
        tenant: Uuid,
        kind: &str,
        id: Option<Uuid>,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query_scalar("SELECT dg.attributes || jsonb_build_object('id',dg.id,'title',dg.title,'kind',dg.kind,'members',COALESCE(m.ids,'[]'::jsonb),'user_ids',COALESCE(m.ids,'[]'::jsonb),'users',COALESCE(m.ids,'[]'::jsonb)) FROM directory_groups dg LEFT JOIN LATERAL(SELECT jsonb_agg(user_id ORDER BY user_id) AS ids FROM directory_group_members WHERE tenant_id=dg.tenant_id AND group_id=dg.id AND kind=dg.kind) m ON true WHERE dg.tenant_id=$1 AND dg.kind=$2 AND ($3::UUID IS NULL OR dg.id=$3) ORDER BY dg.id LIMIT 1001")
            .bind(tenant).bind(kind).bind(id).fetch_all(&self.0).await.map_err(db_error)?;
        complete_page(rows, "directory")
    }

    pub async fn directory_members(&self, tenant: Uuid, id: Uuid, kind: &str) -> Result<Vec<Uuid>> {
        self.directory_entry(tenant, id, kind).await?;
        sqlx::query_scalar("SELECT user_id FROM directory_group_members WHERE tenant_id=$1 AND group_id=$2 AND kind=$3 ORDER BY user_id")
            .bind(tenant).bind(id).bind(kind).fetch_all(&self.0).await.map_err(db_error)
    }

    /// Create, replace or remove this user's one reaction to a message.
    pub async fn wire_react(
        &self,
        actor: Uuid,
        chat: Uuid,
        message: Uuid,
        reaction: Option<Uuid>,
        emoji: Option<&str>,
    ) -> Result<Value> {
        if let Some(emoji) = emoji {
            bounded_string(emoji, 128, "emoji")?;
            if emoji.is_empty() {
                return Err(Error::Invalid("empty emoji".into()));
            }
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        lock_chat(&mut tx, actor, chat, false, None).await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND chat_id=$2 AND deleted_at IS NULL)")
            .bind(message).bind(chat).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !exists {
            return Err(Error::NotFound);
        }
        let row = if let Some(id) = reaction {
            let sql = if emoji.is_some() {
                "UPDATE wire_reactions SET emoji=$5,action='React',updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 AND message_id=$3 AND chat_id=$4 RETURNING *"
            } else {
                "UPDATE wire_reactions SET emoji=COALESCE($5,emoji),action='Unreact',updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 AND message_id=$3 AND chat_id=$4 RETURNING *"
            };
            sqlx::query(sql)
                .bind(id)
                .bind(actor)
                .bind(message)
                .bind(chat)
                .bind(emoji)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?
                .ok_or(Error::NotFound)?
        } else {
            let emoji = emoji.ok_or_else(|| Error::Invalid("missing emoji".into()))?;
            sqlx::query("INSERT INTO wire_reactions(id,message_id,chat_id,user_id,emoji,action) VALUES($1,$2,$3,$4,$5,'React') ON CONFLICT(message_id,user_id) DO UPDATE SET emoji=EXCLUDED.emoji,action='React',updated_at=clock_timestamp() RETURNING *")
                .bind(Uuid::new_v4()).bind(message).bind(chat).bind(actor).bind(emoji)
                .fetch_one(&mut *tx).await.map_err(db_error)?
        };
        let value = reaction_wire(&row)?;
        let users = recipients(&mut tx, chat).await?;
        emit(&mut tx, &users, "reaction", &value).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    pub async fn wire_reactions(
        &self,
        actor: Uuid,
        chat: Option<Uuid>,
        message: Option<Uuid>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<Value>> {
        if let Some(chat) = chat {
            self.chat_for(actor, chat).await?;
        }
        if from.zip(to).is_some_and(|(from, to)| from > to) {
            return Err(Error::Invalid("reaction from must not exceed to".into()));
        }
        let rows = sqlx::query("SELECT r.* FROM wire_reactions r JOIN chat_members cm ON cm.chat_id=r.chat_id WHERE cm.user_id=$1 AND ($2::UUID IS NULL OR r.chat_id=$2) AND ($3::UUID IS NULL OR r.message_id=$3) AND ($4::TIMESTAMPTZ IS NULL OR r.updated_at>=$4) AND ($5::TIMESTAMPTZ IS NULL OR r.updated_at<=$5) ORDER BY r.updated_at,r.id LIMIT 1001")
            .bind(actor).bind(chat).bind(message).bind(from).bind(to).fetch_all(&self.0).await.map_err(db_error)?;
        complete_page(rows, "reaction query (narrow the from/to bounds)")?
            .iter()
            .map(reaction_wire)
            .collect()
    }

    pub async fn create_poll(
        &self,
        actor: Uuid,
        question: &str,
        duration: i64,
        choices: &[String],
    ) -> Result<Value> {
        bounded_string(question, 8192, "poll question")?;
        if question.trim().is_empty()
            || !(0..=2_678_400).contains(&duration)
            || !(2..=20).contains(&choices.len())
        {
            return Err(Error::Invalid(
                "poll requires a question, 2–20 choices and duration 0–31 days".into(),
            ));
        }
        for label in choices {
            bounded_string(label, 2048, "poll choice")?;
            if label.trim().is_empty() {
                return Err(Error::Invalid("empty poll choice".into()));
            }
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let tenant: Uuid = sqlx::query_scalar("SELECT tenant_id FROM users WHERE id=$1")
            .bind(actor)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let id = Uuid::new_v4();
        let row = sqlx::query("INSERT INTO poll_records(id,tenant_id,created_by,question,duration) VALUES($1,$2,$3,$4,$5) RETURNING *")
            .bind(id).bind(tenant).bind(actor).bind(question).bind(duration).fetch_one(&mut *tx).await.map_err(db_error)?;
        for (index, label) in choices.iter().enumerate() {
            sqlx::query("INSERT INTO poll_choices(poll_id,choice_id,label) VALUES($1,$2,$3)")
                .bind(id)
                .bind(index as i32)
                .bind(label)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        let value = poll_wire(&mut tx, actor, &row).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    pub async fn poll(&self, actor: Uuid, id: Uuid) -> Result<Value> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let row = visible_poll(&mut tx, actor, id, false).await?;
        let value = poll_wire(&mut tx, actor, &row).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    pub async fn vote_poll(&self, actor: Uuid, id: Uuid, choice: i32) -> Result<Value> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let row = visible_poll(&mut tx, actor, id, true).await?;
        let created: DateTime<Utc> = row.try_get("created_at").map_err(db_error)?;
        let ended: Option<DateTime<Utc>> = row.try_get("ended_at").map_err(db_error)?;
        let duration: i64 = row.try_get("duration").map_err(db_error)?;
        if ended.is_some()
            || (duration > 0 && created + chrono::Duration::seconds(duration) <= Utc::now())
        {
            return Err(Error::Conflict);
        }
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM poll_choices WHERE poll_id=$1 AND choice_id=$2)",
        )
        .bind(id)
        .bind(choice)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if !valid {
            return Err(Error::Invalid("invalid poll choice".into()));
        }
        sqlx::query("INSERT INTO poll_votes(poll_id,user_id,choice_id) VALUES($1,$2,$3) ON CONFLICT(poll_id,user_id) DO UPDATE SET choice_id=EXCLUDED.choice_id,updated_at=clock_timestamp()")
            .bind(id).bind(actor).bind(choice).execute(&mut *tx).await.map_err(db_error)?;
        let value = poll_wire(&mut tx, actor, &row).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    pub async fn end_poll(&self, actor: Uuid, id: Uuid) -> Result<Value> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let row = visible_poll(&mut tx, actor, id, true).await?;
        if row.try_get::<Uuid, _>("created_by").map_err(db_error)? != actor {
            return Err(Error::Forbidden);
        }
        let row = sqlx::query("UPDATE poll_records SET ended_at=COALESCE(ended_at,clock_timestamp()) WHERE id=$1 RETURNING *")
            .bind(id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let value = poll_wire(&mut tx, actor, &row).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    pub async fn pins(&self, actor: Uuid, chat: Uuid) -> Result<Vec<Value>> {
        self.chat_for(actor, chat).await?;
        let rows = sqlx::query("SELECT p.message_id,p.pinned_at FROM chat_pins p JOIN chat_members cm ON cm.chat_id=p.chat_id JOIN messages m ON m.id=p.message_id AND m.chat_id=p.chat_id WHERE p.chat_id=$1 AND cm.user_id=$2 AND m.deleted_at IS NULL ORDER BY p.pinned_at,p.message_id")
            .bind(chat).bind(actor).fetch_all(&self.0).await.map_err(db_error)?;
        rows.into_iter().map(|row| {
            let time: DateTime<Utc> = row.try_get("pinned_at").map_err(db_error)?;
            Ok(json!({"mid":row.try_get::<Uuid,_>("message_id").map_err(db_error)?,"ts":time.timestamp_millis()}))
        }).collect()
    }

    pub async fn set_pin(
        &self,
        actor: Uuid,
        chat: Uuid,
        message: Uuid,
        pinned: bool,
    ) -> Result<()> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        lock_chat(&mut tx, actor, chat, false, None).await?;
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND chat_id=$2 AND deleted_at IS NULL)")
            .bind(message).bind(chat).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Err(Error::NotFound);
        }
        let changed = if pinned {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_pins p JOIN messages m ON m.id=p.message_id AND m.chat_id=p.chat_id WHERE p.chat_id=$1 AND m.deleted_at IS NULL")
                .bind(chat)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM chat_pins WHERE chat_id=$1 AND message_id=$2)",
            )
            .bind(chat)
            .bind(message)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if count >= 5 && !exists {
                return Err(Error::Invalid(
                    "chat already contains five pinned messages".into(),
                ));
            }
            sqlx::query("INSERT INTO chat_pins(chat_id,message_id,pinned_by) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
                .bind(chat).bind(message).bind(actor).execute(&mut *tx).await.map_err(db_error)?.rows_affected()>0
        } else {
            sqlx::query("DELETE FROM chat_pins WHERE chat_id=$1 AND message_id=$2")
                .bind(chat)
                .bind(message)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?
                .rows_affected()
                > 0
        };
        if changed {
            let users = recipients(&mut tx, chat).await?;
            let payload = json!({"mid":Uuid::new_v4(),"tid":chat,"from":"sys","editor":actor,
                "type":if pinned {"msg_pin"} else {"msg_unpin"},"msg_ref":message,"ts":Utc::now().timestamp_millis()});
            emit(
                &mut tx,
                &users,
                if pinned {
                    "message_pinned"
                } else {
                    "message_unpinned"
                },
                &payload,
            )
            .await?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub async fn thread_readers(&self, actor: Uuid, chat: Uuid) -> Result<Vec<Value>> {
        self.chat_for(actor, chat).await?;
        let rows = sqlx::query("SELECT user_id,read_message_id,read_at FROM chat_members WHERE chat_id=$1 AND read_at IS NOT NULL AND EXISTS(SELECT 1 FROM chat_members me WHERE me.chat_id=$1 AND me.user_id=$2) ORDER BY user_id")
            .bind(chat).bind(actor).fetch_all(&self.0).await.map_err(db_error)?;
        rows.into_iter().map(|row| {
            let time: DateTime<Utc> = row.try_get("read_at").map_err(db_error)?;
            let user: Uuid = row.try_get("user_id").map_err(db_error)?;
            Ok(json!({"id":user,"user_id":user,"user":user,"mid":row.try_get::<Option<Uuid>,_>("read_message_id").map_err(db_error)?,"ts":time.timestamp_millis()}))
        }).collect()
    }

    pub async fn read_threads(&self, actor: Uuid) -> Result<Vec<Value>> {
        let rows = sqlx::query("SELECT chat_id,read_at FROM chat_members WHERE user_id=$1 AND read_at IS NOT NULL ORDER BY chat_id LIMIT 1001")
            .bind(actor).fetch_all(&self.0).await.map_err(db_error)?;
        complete_page(rows, "read thread list")?.into_iter().map(|row| {
            let time: DateTime<Utc> = row.try_get("read_at").map_err(db_error)?;
            Ok(json!({"tid":row.try_get::<Uuid,_>("chat_id").map_err(db_error)?,"last_read_ts":time.to_rfc3339()}))
        }).collect()
    }
}
