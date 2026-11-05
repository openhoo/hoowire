//! Avatar uploads are plain multipart; downloads use the requesting session key.
use super::*;

pub(super) fn target(path: &str) -> Option<(&'static str, &str)> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["u", "profile", id, "image"] | ["u", "profile", id, "image", "upload"] => {
            Some(("user", id))
        }
        ["t", id, "avatar"] => Some(("chat", id)),
        _ => None,
    }
}

pub(super) async fn handle(
    app: &App,
    auth: &Auth,
    method: &Method,
    kind: &str,
    owner: Uuid,
    headers: &HeaderMap,
    bytes: Bytes,
) -> Result<Response<Body>> {
    match method.as_str() {
        "POST" => {
            let (bytes, mime) = multipart_file(headers, bytes).await?;
            app.set_avatar(auth, kind, owner, &bytes, &mime).await?;
            encrypted(&auth.key, &json!({}), None)
        }
        "GET" => {
            let bytes = app.avatar(auth, kind, owner).await?;
            let envelope = auth.key.encrypt(&bytes).map_err(|_| Error::Internal)?;
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(Body::from(envelope))
                .map_err(|_| Error::Internal)
        }
        "HEAD" => {
            app.avatar_info(auth, kind, owner)
                .await?
                .ok_or(Error::NotFound)?;
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(Body::empty())
                .map_err(|_| Error::Internal)
        }
        "DELETE" => {
            app.remove_avatar(auth, kind, owner).await?;
            encrypted(&auth.key, &json!({}), None)
        }
        _ => Err(Error::Unsupported),
    }
}
