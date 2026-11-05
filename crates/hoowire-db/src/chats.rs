//! PostgreSQL chats repository operations.
use super::*;

fn settings_patch(input: Value) -> Result<Value> {
    object(&input)?;
    let mut patch = serde_json::Map::new();
    for (key, value) in input
        .as_object()
        .ok_or_else(|| Error::Invalid("expected chat settings".into()))?
    {
        let (key, value) = match key.as_str() {
            "title" if value.is_string() => ("title", value.clone()),
            "admins" if value.is_array() => ("admins", value.clone()),
            "allowSending" | "allow_sending" if value.is_boolean() => {
                ("allowSending", value.clone())
            }
            "allowMuteNotifications" | "allow_mute_notifications" if value.is_boolean() => {
                ("allowMuteNotifications", value.clone())
            }
            "isClosed" | "is_closed" | "closed" if value.is_boolean() => {
                ("isClosed", value.clone())
            }
            "open" if value.is_boolean() => ("isClosed", json!(!value.as_bool().unwrap_or(false))),
            "version" if value.as_i64().is_some_and(|version| version > 0) => {
                continue;
            }
            _ => {
                return Err(Error::Invalid(format!(
                    "unsupported or invalid chat setting: {key}"
                )));
            }
        };
        if patch.get(key).is_some_and(|previous| previous != &value) {
            return Err(Error::Invalid(format!(
                "conflicting chat setting aliases: {key}"
            )));
        }
        patch.insert(key.into(), value);
    }
    Ok(Value::Object(patch))
}

impl Repository {
    pub async fn create_chat(&self, input: NewChat) -> Result<Chat> {
        object(&input.attributes)?;
        bounded_string(&input.title, 4096, "title")?;
        bounded_string(&input.col, 16384, "conversation key")?;
        bounded_string(&input.kind, 32, "chat kind")?;
        let mut members = input.members.into_iter().collect::<BTreeSet<_>>();
        members.insert(input.created_by);
        if members.len() > 1000 || (one_to_one(&input.kind) && members.len() != 2) {
            return Err(Error::Invalid("invalid membership count".into()));
        }
        let members: Vec<_> = members.into_iter().collect();
        let mut admins = admin_ids(&input.attributes)?.unwrap_or_default();
        admins.insert(input.created_by);
        if !admins.iter().all(|id| members.contains(id)) {
            return Err(Error::Invalid(
                "administrators must be conversation members".into(),
            ));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE tenant_id=$1 AND id=ANY($2)")
                .bind(input.tenant_id)
                .bind(&members)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if count as usize != members.len() {
            return Err(Error::Invalid(
                "members must belong to the same tenant".into(),
            ));
        }
        let chat = sqlx::query("INSERT INTO chats(id,tenant_id,created_by,title,kind,col,attributes) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
            .bind(Uuid::new_v4()).bind(input.tenant_id).bind(input.created_by).bind(input.title).bind(input.kind).bind(input.col)
            .bind(input.attributes).try_map(chat_row).fetch_one(&mut *tx).await.map_err(db_error)?;
        for user in &members {
            sqlx::query(
                "INSERT INTO chat_members(chat_id,user_id,tenant_id,is_admin) VALUES($1,$2,$3,$4)",
            )
            .bind(chat.id)
            .bind(user)
            .bind(chat.tenant_id)
            .bind(admins.contains(user))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        emit(
            &mut tx,
            &members,
            "thread_created",
            &json!({"chat_id":chat.id,"thread_id":chat.id,"version":chat.version}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(chat)
    }

    pub async fn chat_for(&self, user: Uuid, chat: Uuid) -> Result<Chat> {
        sqlx::query("SELECT c.* FROM chats c JOIN chat_members m ON m.chat_id=c.id WHERE m.user_id=$1 AND c.id=$2")
            .bind(user).bind(chat).try_map(chat_row).fetch_optional(&self.0).await.map_err(db_error)?.ok_or(Error::Forbidden)
    }

    pub async fn chats(&self, user: Uuid) -> Result<Vec<Chat>> {
        let rows = sqlx::query("SELECT c.* FROM chats c JOIN chat_members m ON m.chat_id=c.id WHERE m.user_id=$1 ORDER BY c.updated_at DESC,c.id LIMIT 1001")
            .bind(user).try_map(chat_row).fetch_all(&self.0).await.map_err(db_error)?;
        complete_snapshot(rows, MAX_PAGE)
    }

    pub async fn members(&self, user: Uuid, chat: Uuid) -> Result<Vec<Uuid>> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        member(&mut tx, user, chat).await?;
        let result = recipients(&mut tx, chat).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    pub async fn administrators(&self, user: Uuid, chat: Uuid) -> Result<Vec<Uuid>> {
        let mut tx = self.0.begin().await.map_err(db_error)?;
        member(&mut tx, user, chat).await?;
        let admins = sqlx::query_scalar(
            "SELECT user_id FROM chat_members WHERE chat_id=$1 AND is_admin ORDER BY user_id",
        )
        .bind(chat)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(admins)
    }

    pub async fn update_chat(
        &self,
        actor: Uuid,
        chat: Uuid,
        expected_version: Option<i64>,
        attributes: Value,
    ) -> Result<Chat> {
        let attributes = settings_patch(attributes)?;
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = lock_chat(&mut tx, actor, chat, true, expected_version).await?;
        let title = attributes
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&current.title)
            .to_owned();
        bounded_string(&title, 4096, "title")?;
        if let Some(admins) = admin_ids(&attributes)? {
            if admins.is_empty() {
                return Err(Error::Invalid(
                    "a conversation needs an administrator".into(),
                ));
            }
            let users = recipients(&mut tx, chat).await?;
            if !admins.iter().all(|id| users.contains(id)) {
                return Err(Error::Invalid(
                    "administrators must be conversation members".into(),
                ));
            }
            let admins: Vec<_> = admins.into_iter().collect();
            sqlx::query("UPDATE chat_members SET is_admin=(user_id=ANY($2)) WHERE chat_id=$1")
                .bind(chat)
                .bind(admins)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        let mut merged = current.attributes;
        merge(&mut merged, attributes);
        object(&merged)?;
        let updated = bump_chat(&mut tx, chat, &merged, &title).await?;
        let users = recipients(&mut tx, chat).await?;
        emit(
            &mut tx,
            &users,
            "thread_updated",
            &json!({"chat_id":chat,"thread_id":chat,"version":updated.version}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(updated)
    }

    pub async fn add_members(
        &self,
        actor: Uuid,
        chat: Uuid,
        members: &[Uuid],
        expected_version: Option<i64>,
    ) -> Result<Chat> {
        self.change_members(actor, chat, members, expected_version, true)
            .await
    }

    pub async fn remove_members(
        &self,
        actor: Uuid,
        chat: Uuid,
        members: &[Uuid],
        expected_version: Option<i64>,
    ) -> Result<Chat> {
        self.change_members(actor, chat, members, expected_version, false)
            .await
    }

    pub async fn replace_members(
        &self,
        actor: Uuid,
        chat: Uuid,
        members: &[Uuid],
        expected_version: Option<i64>,
    ) -> Result<Chat> {
        self.replace_member_components(actor, chat, members, expected_version, None)
            .await
    }

    async fn change_members(
        &self,
        actor: Uuid,
        chat: Uuid,
        members: &[Uuid],
        expected_version: Option<i64>,
        add: bool,
    ) -> Result<Chat> {
        if members.len() > 1000 {
            return Err(Error::Invalid("too many members".into()));
        }
        let mut tx = self.0.begin().await.map_err(db_error)?;
        let current = lock_chat(&mut tx, actor, chat, true, expected_version).await?;
        if one_to_one(&current.kind) {
            return Err(Error::Invalid("one-to-one membership is immutable".into()));
        }
        let before = recipients(&mut tx, chat).await?;
        if add {
            let found: i64 =
                sqlx::query_scalar("SELECT count(*) FROM users WHERE tenant_id=$1 AND id=ANY($2)")
                    .bind(current.tenant_id)
                    .bind(members)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_error)?;
            if found as usize != members.iter().copied().collect::<BTreeSet<_>>().len() {
                return Err(Error::Invalid(
                    "members must belong to the same tenant".into(),
                ));
            }
            for user in members {
                sqlx::query("INSERT INTO chat_members(chat_id,user_id,tenant_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
                    .bind(chat).bind(user).bind(current.tenant_id).execute(&mut *tx).await.map_err(db_error)?;
            }
        } else {
            for user in members.iter().copied().collect::<BTreeSet<_>>() {
                locations::stop_member_locations(&mut tx, user, chat).await?;
            }
            sqlx::query("DELETE FROM chat_members WHERE chat_id=$1 AND user_id=ANY($2)")
                .bind(chat)
                .bind(members)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            let admins: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM chat_members WHERE chat_id=$1 AND is_admin",
            )
            .bind(chat)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if admins == 0 {
                return Err(Error::Invalid(
                    "the last administrator cannot be removed".into(),
                ));
            }
        }
        let after = recipients(&mut tx, chat).await?;
        if after.len() > 1000 {
            return Err(Error::Invalid("too many members".into()));
        }
        let updated = bump_chat(&mut tx, chat, &current.attributes, &current.title).await?;
        let all = before.into_iter().chain(after).collect::<Vec<_>>();
        emit(
            &mut tx,
            &all,
            "thread_updated",
            &json!({"chat_id":chat,"thread_id":chat,"version":updated.version}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(updated)
    }
}
