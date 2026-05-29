//! Preset-credential auth: caller supplies a known-good access token and
//! instance URL. No refresh, no negotiation.
//!
//! Useful for tests, short-lived scripts, or callers that have already
//! performed an OAuth flow out-of-band (e.g. via the `sfdx` CLI) and want to
//! reuse the resulting token. Real OAuth flows live in sibling modules.

use crate::AuthSession;
use crate::error::AuthResult;
use async_trait::async_trait;
use std::borrow::Cow;

/// Authentication backed by a fixed access token and instance URL.
///
/// Once the token expires, requests will start failing with a 401
/// `INVALID_SESSION_ID` from Salesforce. This type does not refresh;
/// pair it with a flow that does (or rebuild the client) to recover.
#[derive(Clone)]
pub struct StaticTokenAuth {
    access_token: String,
    instance_url: String,
}

impl std::fmt::Debug for StaticTokenAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticTokenAuth")
            .field("access_token", &"[redacted]")
            .field("instance_url", &self.instance_url)
            .finish()
    }
}

impl StaticTokenAuth {
    /// Constructs a session from a known token and instance URL.
    ///
    /// `instance_url` is normalized by trimming a trailing slash so that
    /// path concatenation in the client always produces clean URLs.
    ///
    /// Static-token auth doesn't refresh — when the token expires, calls
    /// will surface 401. Use for short-lived scripts, CLI tools, or
    /// tests where you've pasted a token from `sf org display`. For
    /// long-running services prefer [`JwtAuth`](crate::JwtAuth) or
    /// [`RefreshTokenAuth`](crate::RefreshTokenAuth).
    ///
    /// # Example
    ///
    /// ```
    /// use cirrus_auth::{AuthSession, StaticTokenAuth};
    /// use std::sync::Arc;
    ///
    /// let auth: Arc<dyn AuthSession> = Arc::new(StaticTokenAuth::new(
    ///     "00D...!AQ...",
    ///     "https://my-org.my.salesforce.com",
    /// ));
    /// assert_eq!(auth.instance_url(), "https://my-org.my.salesforce.com");
    /// ```
    pub fn new(access_token: impl Into<String>, instance_url: impl Into<String>) -> Self {
        let mut instance_url: String = instance_url.into();
        if instance_url.ends_with('/') {
            instance_url.pop();
        }
        Self {
            access_token: access_token.into(),
            instance_url,
        }
    }
}

#[async_trait]
impl AuthSession for StaticTokenAuth {
    async fn access_token(&self) -> AuthResult<Cow<'_, str>> {
        Ok(Cow::Borrowed(&self.access_token))
    }

    fn instance_url(&self) -> &str {
        &self.instance_url
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_provided_token_and_url() {
        let auth = StaticTokenAuth::new("tok", "https://example.my.salesforce.com");
        assert_eq!(auth.access_token().await.unwrap(), "tok");
        assert_eq!(auth.instance_url(), "https://example.my.salesforce.com");
    }

    #[tokio::test]
    async fn strips_trailing_slash_from_instance_url() {
        let auth = StaticTokenAuth::new("tok", "https://example.my.salesforce.com/");
        assert_eq!(auth.instance_url(), "https://example.my.salesforce.com");
    }
}
