//! Resource use cases. The transport layer cannot bypass tenant or chat checks.
use super::*;

impl App {
    pub async fn directory_revision(&self, auth: &Auth) -> Result<DateTime<Utc>> {
        self.0
            .repository
            .directory_revision(auth.principal.tenant_id)
            .await
    }

    /// Materialize current circle and role members for the conversation ACL.
    /// Direct membership components remain separate in the wire subscription.
    pub async fn expanded_members(&self, auth: &Auth, value: &Value) -> Result<Vec<Uuid>> {
        let mut members = uuid_list(value.get("users"))?;
        for (field, kind) in [("groups", "group"), ("roles", "role")] {
            let ids = uuid_list(value.get(field))?;
            members.extend(
                self.0
                    .repository
                    .expand_directory_members(auth.principal.tenant_id, kind, &ids)
                    .await?,
            );
        }
        members.sort_unstable();
        members.dedup();
        if members.len() > 1000 {
            return Err(Error::Invalid(
                "too many expanded conversation members".into(),
            ));
        }
        Ok(members)
    }

    pub async fn replace_member_components(
        &self,
        auth: &Auth,
        chat: Uuid,
        value: &Value,
        version: Option<i64>,
    ) -> Result<Chat> {
        let mut members = self.expanded_members(auth, value).await?;
        if !members.contains(&auth.principal.user_id) {
            members.push(auth.principal.user_id);
        }
        let mut direct_users = uuid_list(value.get("users"))?;
        if !direct_users.contains(&auth.principal.user_id) {
            direct_users.push(auth.principal.user_id);
        }
        let components = json!({
            "users":direct_users,
            "groups":uuid_list(value.get("groups"))?,
            "roles":uuid_list(value.get("roles"))?,
        });
        self.unseal_chat(
            self.0
                .repository
                .replace_member_components(
                    auth.principal.user_id,
                    chat,
                    &members,
                    version,
                    Some(components),
                )
                .await?,
        )
    }

    pub async fn leave_chat(&self, auth: &Auth, chat: Uuid, version: Option<i64>) -> Result<()> {
        self.0
            .repository
            .leave_chat(auth.principal.user_id, chat, version)
            .await?;
        Ok(())
    }

    pub async fn directory_entries(&self, auth: &Auth, kind: &str) -> Result<Vec<Value>> {
        if !matches!(kind, "group" | "role") {
            return Err(Error::Invalid("invalid directory kind".into()));
        }
        let mut entries = self
            .0
            .repository
            .resource_directory(auth.principal.tenant_id, kind, None)
            .await?;
        for entry in &mut entries {
            if entry.get("name").is_none() {
                entry["name"] = entry["title"].clone();
            }
        }
        Ok(entries)
    }

    pub async fn directory_item(&self, auth: &Auth, id: Uuid, kind: &str) -> Result<Value> {
        if !matches!(kind, "group" | "role") {
            return Err(Error::Invalid("invalid directory kind".into()));
        }
        let mut item = self
            .0
            .repository
            .resource_directory(auth.principal.tenant_id, kind, Some(id))
            .await?
            .pop()
            .ok_or(Error::NotFound)?;
        if item.get("name").is_none() {
            item["name"] = item["title"].clone();
        }
        Ok(item)
    }

    pub async fn reactions(
        &self,
        auth: &Auth,
        chat: Option<Uuid>,
        message: Option<Uuid>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<Value>> {
        self.0
            .repository
            .wire_reactions(auth.principal.user_id, chat, message, from, to)
            .await
    }

    pub async fn set_reaction(
        &self,
        auth: &Auth,
        chat: Uuid,
        message: Uuid,
        reaction: Option<Uuid>,
        emoji: Option<&str>,
    ) -> Result<Value> {
        self.0
            .repository
            .wire_react(auth.principal.user_id, chat, message, reaction, emoji)
            .await
    }

    pub async fn create_poll(&self, auth: &Auth, value: &Value) -> Result<Value> {
        if value.get("type").and_then(Value::as_str) != Some("poll") {
            return Err(Error::Unsupported);
        }
        let data = value
            .get("data")
            .ok_or_else(|| Error::Invalid("missing poll data".into()))?;
        let question = data
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Invalid("missing poll question".into()))?;
        let duration = match data.get("duration") {
            None | Some(Value::Null) => 0,
            Some(value) => value
                .as_i64()
                .ok_or_else(|| Error::Invalid("invalid poll duration".into()))?,
        };
        let choices = data
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Invalid("missing poll choices".into()))?;
        let choices = choices
            .iter()
            .map(|choice| {
                choice
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Invalid("poll choices must be strings".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        self.0
            .repository
            .create_poll(auth.principal.user_id, question, duration, &choices)
            .await
    }

    pub async fn poll(&self, auth: &Auth, id: Uuid) -> Result<Value> {
        self.0.repository.poll(auth.principal.user_id, id).await
    }

    pub async fn update_poll(&self, auth: &Auth, value: &Value) -> Result<Value> {
        let id = identifier(
            value
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("missing poll ID".into()))?,
        )?;
        match value.get("type").and_then(Value::as_str) {
            Some("poll_response") => {
                let raw = value
                    .pointer("/response/choice")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::Invalid("missing poll choice".into()))?;
                let choice =
                    i32::try_from(raw).map_err(|_| Error::Invalid("invalid poll choice".into()))?;
                self.0
                    .repository
                    .vote_poll(auth.principal.user_id, id, choice)
                    .await
            }
            Some("poll_end") => self.0.repository.end_poll(auth.principal.user_id, id).await,
            _ => Err(Error::Unsupported),
        }
    }

    pub async fn pinned_messages(&self, auth: &Auth, chat: Uuid) -> Result<Vec<Value>> {
        self.0.repository.pins(auth.principal.user_id, chat).await
    }

    pub async fn pin_message(
        &self,
        auth: &Auth,
        chat: Uuid,
        message: Uuid,
        pinned: bool,
    ) -> Result<()> {
        self.0
            .repository
            .set_pin(auth.principal.user_id, chat, message, pinned)
            .await
    }

    pub async fn read_markers(&self, auth: &Auth, chat: Uuid) -> Result<Vec<Value>> {
        self.0
            .repository
            .thread_readers(auth.principal.user_id, chat)
            .await
    }

    pub async fn read_threads(&self, auth: &Auth) -> Result<Vec<Value>> {
        self.0.repository.read_threads(auth.principal.user_id).await
    }
}
