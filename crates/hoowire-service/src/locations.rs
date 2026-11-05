//! Access-checked live location use cases.
use super::*;

impl App {
    pub async fn live_locations(&self, auth: &Auth, chat: Option<Uuid>) -> Result<Value> {
        let locations = self
            .0
            .repository
            .live_locations(auth.principal.user_id, chat)
            .await?;
        Ok(
            json!({"activeLiveLocations":locations.len(),"active_live_locations":locations.len(),"liveLocation":locations,"live_location":locations}),
        )
    }

    pub async fn live_location(&self, auth: &Auth, chat: Uuid, message: Uuid) -> Result<Value> {
        self.0
            .repository
            .live_location(auth.principal.user_id, chat, message)
            .await
    }

    pub async fn update_live_locations(&self, auth: &Auth, value: &Value) -> Result<Value> {
        let entries = value
            .get("liveLocation")
            .or_else(|| value.get("live_location"))
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Invalid("missing live location update list".into()))?;
        if entries.len() > 1000 {
            return Err(Error::Invalid("too many live location updates".into()));
        }
        let updates = entries
            .iter()
            .map(|entry| {
                let chat =
                    identifier(entry.get("tid").and_then(Value::as_str).ok_or_else(|| {
                        Error::Invalid("missing live location conversation".into())
                    })?)?;
                let location = entry
                    .get("location")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Invalid("missing encrypted location".into()))?
                    .to_owned();
                Ok((chat, location))
            })
            .collect::<Result<Vec<_>>>()?;
        let locations = self
            .0
            .repository
            .update_live_locations(auth.principal.user_id, &updates)
            .await?;
        Ok(json!({"liveLocation":locations,"live_location":locations}))
    }

    pub async fn stop_live_locations(&self, auth: &Auth, chat: Uuid) -> Result<()> {
        self.0
            .repository
            .stop_live_locations(auth.principal.user_id, chat)
            .await?;
        Ok(())
    }
}
