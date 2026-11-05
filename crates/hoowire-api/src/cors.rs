//! Explicit browser origins for the packaged desktop and separately hosted web UI.
use axum::http::{HeaderValue, Method, header};
use hoowire_domain::{Error, Result};
use std::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// A validated exact-origin list. Cookies and HTTP authentication are not
/// enabled: clients authenticate using their encrypted transport and `From`.
#[derive(Clone, Debug)]
pub struct CorsConfig {
    origins: Vec<HeaderValue>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            origins: vec![HeaderValue::from_static("null")],
        }
    }
}

impl CorsConfig {
    /// Web origins must be serialized HTTP(S) origins, without paths, user info,
    /// wildcards, queries or fragments. `null` is controlled only by the separate
    /// desktop flag. Empty lists allow no web origins.
    pub fn new<I, S>(allow_desktop: bool, web_origins: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut origins = Vec::new();
        if allow_desktop {
            origins.push(HeaderValue::from_static("null"));
        }
        for (index, origin) in web_origins.into_iter().enumerate() {
            if index >= 64 {
                return Err(Error::Invalid(
                    "at most 64 CORS web origins are allowed".into(),
                ));
            }
            let origin = origin.as_ref();
            let invalid = || {
                Error::Invalid(
                    "CORS web origins must be exact serialized http(s) origins (scheme://host[:port])"
                        .into(),
                )
            };
            if origin.len() > 2048 {
                return Err(invalid());
            }
            let url = url::Url::parse(origin).map_err(|_| invalid())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.host_str().is_none_or(|host| host.contains('*'))
                || url.origin().ascii_serialization() != origin
            {
                return Err(invalid());
            }
            let value = HeaderValue::from_str(origin).map_err(|_| invalid())?;
            if !origins.contains(&value) {
                origins.push(value);
            }
        }
        Ok(Self { origins })
    }

    pub(crate) fn layer(&self) -> CorsLayer {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(self.origins.clone()))
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([
                header::FROM,
                header::CONTENT_TYPE,
                header::HeaderName::from_static("x-request-id"),
                header::ETAG,
            ])
            .expose_headers([header::ETAG])
            .max_age(Duration::from_secs(600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_web_origins_cannot_widen_to_patterns_or_non_origins() {
        for origin in [
            "*",
            "null",
            "file://",
            "https://*.example.invalid",
            "https://example.invalid/",
            "https://example.invalid/path",
            "https://example.invalid?query",
            "https://example.invalid#fragment",
            "https://user:password@example.invalid",
            "https://example.invalid https://other.invalid",
            "https://example.invalid\r\nX-Injected: true",
        ] {
            assert!(CorsConfig::new(false, [origin]).is_err(), "{origin:?}");
        }
        assert!(
            CorsConfig::new(false, std::iter::repeat_n("https://example.invalid", 65)).is_err()
        );
        assert!(
            CorsConfig::new(
                false,
                [
                    "https://chat.example.invalid",
                    "http://localhost:3000",
                    "http://[::1]:3000"
                ]
            )
            .is_ok()
        );
    }
}
