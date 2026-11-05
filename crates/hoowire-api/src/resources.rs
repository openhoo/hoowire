//! Wire adapters for persisted auxiliary chat resources.
use super::*;

pub(crate) async fn dispatch(
    app: &App,
    auth: &Auth,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    value: Value,
) -> Result<Value> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let op = method.as_str();
    match parts.as_slice() {
        ["directory", "sync"] if op == "GET" => {
            let revision = app.directory_revision(auth).await?;
            Ok(
                json!({"events":[],"force_resync":{"roles":revision.timestamp_millis()},"snapshot_required":true}),
            )
        }
        ["directory", "roles"] if op == "GET" => {
            Ok(json!({"roles":app.directory_entries(auth,"role").await?}))
        }
        ["directory", "roles", "matching_ids"] if op == "POST" => {
            let ids = uuid_list(value.get("role_ids").or_else(|| value.get("roleIds")))?;
            let mut roles = Vec::with_capacity(ids.len());
            for id in ids {
                roles.push(app.directory_item(auth, id, "role").await?);
            }
            Ok(json!({"roles":roles}))
        }
        ["g", id] if op == "GET" => app.directory_item(auth, identifier(id)?, "group").await,
        ["u", "reactions"] if op == "GET" => Ok(
            json!({"reactions":app.reactions(auth,None,None,query_time(query,"from")?,query_time(query,"to")?).await?}),
        ),
        ["t", tid, "reactions"] if op == "GET" => Ok(
            json!({"reactions":app.reactions(auth,Some(identifier(tid)?),None,query_time(query,"from")?,query_time(query,"to")?).await?}),
        ),
        ["t", tid, "reactions", "m", mid] if op == "GET" => Ok(
            json!({"reactions":app.reactions(auth,Some(identifier(tid)?),Some(identifier(mid)?),None,None).await?}),
        ),
        ["t", tid, "reactions", "m", mid] if op == "POST" => {
            app.set_reaction(
                auth,
                identifier(tid)?,
                identifier(mid)?,
                None,
                Some(string(&value, "reaction")?),
            )
            .await
        }
        ["t", tid, "reactions", "m", mid, rid] if op == "PUT" || op == "DELETE" => {
            app.set_reaction(
                auth,
                identifier(tid)?,
                identifier(mid)?,
                Some(identifier(rid)?),
                if op == "PUT" {
                    Some(string(&value, "reaction")?)
                } else {
                    None
                },
            )
            .await
        }
        ["t", tid, "m", mid, "reactions"] if op == "POST" => {
            app.set_reaction(
                auth,
                identifier(tid)?,
                identifier(mid)?,
                None,
                Some(string(&value, "emoji")?),
            )
            .await
        }
        ["t", tid, "m", mid, "reactions", rid] if op == "PUT" || op == "DELETE" => {
            app.set_reaction(
                auth,
                identifier(tid)?,
                identifier(mid)?,
                Some(identifier(rid)?),
                if op == "PUT" {
                    Some(string(&value, "emoji")?)
                } else {
                    None
                },
            )
            .await
        }
        ["interactive"] if op == "PUT" => app.create_poll(auth, &value).await,
        ["interactive"] if op == "POST" => app.update_poll(auth, &value).await,
        ["interactive", id] if op == "GET" => app.poll(auth, identifier(id)?).await,
        ["t", tid, "pinnedmessages"] if op == "GET" => {
            Ok(json!({"pinnedmessages":app.pinned_messages(auth,identifier(tid)?).await?}))
        }
        ["t", tid, "pinnedmessages"] if op == "POST" => {
            let message = value
                .get("message_id")
                .or_else(|| value.get("messageId"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("missing message ID".into()))?;
            app.pin_message(auth, identifier(tid)?, identifier(message)?, true)
                .await?;
            Ok(json!({}))
        }
        ["t", tid, "pinnedmessages", mid] if op == "DELETE" => {
            app.pin_message(auth, identifier(tid)?, identifier(mid)?, false)
                .await?;
            Ok(json!({}))
        }
        ["t", tid, "readers"] if op == "GET" || op == "POST" => {
            let chat = identifier(tid)?;
            if op == "POST" {
                let mid = value
                    .get("mid")
                    .and_then(Value::as_str)
                    .map(identifier)
                    .transpose()?;
                app.read_chat(auth, chat, mid).await?;
            }
            Ok(
                json!({"readers":app.read_markers(auth,chat).await?,"ts":Utc::now().timestamp_millis()}),
            )
        }
        _ => super::locations::dispatch(app, auth, method, path, value).await,
    }
}

fn query_time(query: &HashMap<String, String>, key: &str) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = query.get(key) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(value)
        .map(|v| Some(v.with_timezone(&Utc)))
        .map_err(|_| Error::Invalid(format!("{key} must be an ISO timestamp")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_time_bounds_are_checked() {
        let query = HashMap::from([("from".into(), "2026-11-04T20:00:00.123456Z".into())]);
        assert_eq!(
            query_time(&query, "from")
                .unwrap()
                .unwrap()
                .timestamp_subsec_micros(),
            123456
        );
        assert!(query_time(&query, "to").unwrap().is_none());
        assert!(query_time(&HashMap::from([("from".into(), "invalid".into())]), "from").is_err());
    }
}
