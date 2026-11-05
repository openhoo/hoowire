//! Wire spellings observed with the SDK and native desktop location viewers.
use super::*;

pub(crate) async fn dispatch(
    app: &App,
    auth: &Auth,
    method: &Method,
    path: &str,
    value: Value,
) -> Result<Value> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["t", "loc"]) => app.live_locations(auth, None).await,
        ("GET", ["t", chat, "loc"]) => app.live_locations(auth, Some(identifier(chat)?)).await,
        ("GET", ["t", chat, "loc", message]) => {
            app.live_location(auth, identifier(chat)?, identifier(message)?)
                .await
        }
        ("POST", ["u", "loc"]) => app.update_live_locations(auth, &value).await,
        ("DELETE", ["t", chat, "loc"]) => {
            app.stop_live_locations(auth, identifier(chat)?).await?;
            Ok(json!({}))
        }
        _ => Err(Error::Unsupported),
    }
}
