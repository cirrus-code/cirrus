//! OAuth 2.0 Refresh Token grant for long-lived Salesforce sessions.
//!
//! Several Salesforce OAuth flows (Web Server, Device, User-Agent) hand
//! back a `refresh_token` alongside the initial access token. Refresh
//! tokens are long-lived and can be exchanged for fresh access tokens
//! indefinitely (until revoked). This module wraps that grant in an
//! [`AuthSession`] so the rest of the SDK doesn't care which flow
//! originally produced the refresh token.
//!
//! ## Composability
//!
//! `RefreshTokenAuth` is intentionally a standalone auth implementation
//! rather than a wrapper over another `AuthSession`. The natural usage is:
//!
//! 1. The caller (or a future Web Server / Device flow handler) performs
//!    the initial OAuth exchange and obtains a `refresh_token` plus an
//!    `instance_url`.
//! 2. They build a `RefreshTokenAuth` with those values.
//! 3. The SDK uses it for all subsequent requests; new access tokens are
//!    minted on demand by hitting `/services/oauth2/token` with
//!    `grant_type=refresh_token`.
//!
//! ## Confidential vs public clients
//!
//! Connected Apps configured as **confidential clients** require a
//! `client_secret` on every refresh; **public clients** (PKCE-based) do
//! not. The builder treats `consumer_secret` as optional — set it for
//! confidential clients, omit it for public.
//!
//! ## Token rotation
//!
//! Salesforce does not rotate refresh tokens — the same token is reused
//! across refreshes. If a future Salesforce change starts returning a new
//! refresh token in the response, this module currently ignores it; the
//! original is still used. Add rotation handling if/when that changes.

use crate::auth::AuthSession;
use crate::auth::token_endpoint::{check_instance_url, exchange};
use crate::error::{CloudburstError, CloudburstResult};
use async_trait::async_trait;
use std::borrow::Cow;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Salesforce production login URL — also the default token-exchange host.
pub const PRODUCTION_LOGIN_URL: &str = "https://login.salesforce.com";

/// Salesforce sandbox login URL.
pub const SANDBOX_LOGIN_URL: &str = "https://test.salesforce.com";

/// Default cache TTL for an access token after it's issued.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

/// Refresh-token-grant auth session.
///
/// Construct via [`RefreshTokenAuth::builder`].
pub struct RefreshTokenAuth {
    consumer_key: String,
    consumer_secret: Option<String>,
    refresh_token: String,
    login_url: String,
    instance_url: String,
    token_ttl: Duration,
    http: reqwest::Client,
    cached: RwLock<Option<CachedToken>>,
}

impl std::fmt::Debug for RefreshTokenAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Omit consumer_key, consumer_secret, and refresh_token — all secrets.
        f.debug_struct("RefreshTokenAuth")
            .field("login_url", &self.login_url)
            .field("instance_url", &self.instance_url)
            .field("token_ttl", &self.token_ttl)
            .field("confidential", &self.consumer_secret.is_some())
            .finish_non_exhaustive()
    }
}

impl RefreshTokenAuth {
    /// Begins constructing a [`RefreshTokenAuth`].
    pub fn builder() -> RefreshTokenAuthBuilder {
        RefreshTokenAuthBuilder::default()
    }

    async fn mint_token(&self) -> CloudburstResult<CachedToken> {
        // Compose the form body. consumer_secret is conditional on whether
        // the connected app is confidential.
        let mut body: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("client_id", self.consumer_key.as_str()),
            ("refresh_token", self.refresh_token.as_str()),
        ];
        if let Some(secret) = self.consumer_secret.as_deref() {
            body.push(("client_secret", secret));
        }

        let token = exchange(&self.http, &self.login_url, &body).await?;
        check_instance_url(&self.instance_url, &token)?;

        Ok(CachedToken {
            access_token: token.access_token,
            expires_at: Instant::now() + self.token_ttl,
        })
    }
}

#[async_trait]
impl AuthSession for RefreshTokenAuth {
    async fn access_token(&self) -> CloudburstResult<Cow<'_, str>> {
        // Fast path — read lock, return clone of cached token if still valid.
        {
            let guard = self.cached.read().await;
            if let Some(cached) = guard.as_ref()
                && cached.expires_at > Instant::now()
            {
                return Ok(Cow::Owned(cached.access_token.clone()));
            }
        }

        // Slow path — write lock, double-check, mint.
        let mut guard = self.cached.write().await;
        if let Some(cached) = guard.as_ref()
            && cached.expires_at > Instant::now()
        {
            return Ok(Cow::Owned(cached.access_token.clone()));
        }
        let new_token = self.mint_token().await?;
        let token_str = new_token.access_token.clone();
        *guard = Some(new_token);
        Ok(Cow::Owned(token_str))
    }

    fn instance_url(&self) -> &str {
        &self.instance_url
    }

    async fn invalidate(&self, stale_token: &str) {
        // Compare-and-swap: only clear the cached access token if it
        // still matches what the failing request used. The underlying
        // refresh_token isn't affected — we only ever want the
        // *short-lived* access token re-minted.
        let mut guard = self.cached.write().await;
        if let Some(cached) = guard.as_ref()
            && cached.access_token == stale_token
        {
            *guard = None;
        }
    }
}

/// Builder for [`RefreshTokenAuth`].
#[derive(Default)]
pub struct RefreshTokenAuthBuilder {
    consumer_key: Option<String>,
    consumer_secret: Option<String>,
    refresh_token: Option<String>,
    login_url: Option<String>,
    instance_url: Option<String>,
    token_ttl: Option<Duration>,
    http_client: Option<reqwest::Client>,
}

impl std::fmt::Debug for RefreshTokenAuthBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshTokenAuthBuilder")
            .field("consumer_key", &self.consumer_key.is_some())
            .field("consumer_secret", &self.consumer_secret.is_some())
            .field("refresh_token", &self.refresh_token.is_some())
            .field("login_url", &self.login_url)
            .field("instance_url", &self.instance_url)
            .field("token_ttl", &self.token_ttl)
            .finish_non_exhaustive()
    }
}

impl RefreshTokenAuthBuilder {
    /// Connected App's Consumer Key (Client ID). Required.
    pub fn consumer_key(mut self, key: impl Into<String>) -> Self {
        self.consumer_key = Some(key.into());
        self
    }

    /// Connected App's Consumer Secret (Client Secret). Required for
    /// confidential clients; omit for public/PKCE clients.
    pub fn consumer_secret(mut self, secret: impl Into<String>) -> Self {
        self.consumer_secret = Some(secret.into());
        self
    }

    /// Refresh token issued by a prior OAuth flow (Web Server, Device,
    /// User-Agent). Required.
    pub fn refresh_token(mut self, token: impl Into<String>) -> Self {
        self.refresh_token = Some(token.into());
        self
    }

    /// Login URL — the host that issued the refresh token. Defaults to
    /// [`PRODUCTION_LOGIN_URL`]. Use [`SANDBOX_LOGIN_URL`] for sandboxes,
    /// or your org's My Domain login URL where required.
    pub fn login_url(mut self, url: impl Into<String>) -> Self {
        self.login_url = Some(url.into());
        self
    }

    /// REST instance URL — the org's My Domain. Required. Must match the
    /// `instance_url` returned by the token-exchange response.
    pub fn instance_url(mut self, url: impl Into<String>) -> Self {
        self.instance_url = Some(url.into());
        self
    }

    /// How long to cache an access token before re-minting. Defaults to 30
    /// minutes.
    pub fn token_ttl(mut self, ttl: Duration) -> Self {
        self.token_ttl = Some(ttl);
        self
    }

    /// Supplies a pre-configured `reqwest::Client`. Useful for sharing a
    /// connection pool.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalizes the builder.
    pub fn build(self) -> CloudburstResult<RefreshTokenAuth> {
        let consumer_key = self
            .consumer_key
            .ok_or(CloudburstError::MissingField("consumer_key"))?;
        let refresh_token = self
            .refresh_token
            .ok_or(CloudburstError::MissingField("refresh_token"))?;
        let mut instance_url = self
            .instance_url
            .ok_or(CloudburstError::MissingField("instance_url"))?;
        if instance_url.ends_with('/') {
            instance_url.pop();
        }
        let mut login_url = self
            .login_url
            .unwrap_or_else(|| PRODUCTION_LOGIN_URL.to_string());
        if login_url.ends_with('/') {
            login_url.pop();
        }
        let token_ttl = self.token_ttl.unwrap_or(DEFAULT_TOKEN_TTL);
        let http = self.http_client.unwrap_or_default();

        Ok(RefreshTokenAuth {
            consumer_key,
            consumer_secret: self.consumer_secret,
            refresh_token,
            login_url,
            instance_url,
            token_ttl,
            http,
            cached: RwLock::new(None),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn builder_with_required_fields() -> RefreshTokenAuthBuilder {
        RefreshTokenAuth::builder()
            .consumer_key("consumer-key-123")
            .refresh_token("5Aep861KIwKdekr...refresh")
            .instance_url("https://my-org.my.salesforce.com")
    }

    #[test]
    fn builder_requires_consumer_key() {
        let err = RefreshTokenAuth::builder()
            .refresh_token("r")
            .instance_url("https://x")
            .build()
            .unwrap_err();
        assert!(matches!(err, CloudburstError::MissingField("consumer_key")));
    }

    #[test]
    fn builder_requires_refresh_token() {
        let err = RefreshTokenAuth::builder()
            .consumer_key("k")
            .instance_url("https://x")
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            CloudburstError::MissingField("refresh_token")
        ));
    }

    #[test]
    fn builder_requires_instance_url() {
        let err = RefreshTokenAuth::builder()
            .consumer_key("k")
            .refresh_token("r")
            .build()
            .unwrap_err();
        assert!(matches!(err, CloudburstError::MissingField("instance_url")));
    }

    #[test]
    fn builder_strips_trailing_slashes_and_defaults_login_url() {
        let auth = builder_with_required_fields()
            .instance_url("https://my-org.my.salesforce.com/")
            .build()
            .unwrap();
        assert_eq!(auth.instance_url(), "https://my-org.my.salesforce.com");
        assert_eq!(auth.login_url, PRODUCTION_LOGIN_URL);
    }

    #[tokio::test]
    async fn refresh_succeeds_and_caches() {
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));

        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("client_id=consumer-key-123"))
            .and(body_string_contains("refresh_token=5Aep861KIwKdekr"))
            .respond_with(CountingResponder {
                hits: hits.clone(),
                response: ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "00DXX!ACCESS",
                    "instance_url": "https://my-org.my.salesforce.com",
                    "token_type": "Bearer",
                    "id": "https://login.salesforce.com/id/00DXX/005XX",
                })),
            })
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();

        let t1 = auth.access_token().await.unwrap();
        assert_eq!(&*t1, "00DXX!ACCESS");
        let t2 = auth.access_token().await.unwrap();
        assert_eq!(&*t2, "00DXX!ACCESS");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn confidential_client_includes_consumer_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains("client_secret=top-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "instance_url": "https://my-org.my.salesforce.com"
            })))
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .consumer_secret("top-secret")
            .login_url(server.uri())
            .build()
            .unwrap();

        // The body matcher above asserts client_secret is present. If it
        // weren't, the mock would 404 and this would error.
        auth.access_token().await.unwrap();
    }

    #[tokio::test]
    async fn public_client_omits_consumer_secret() {
        let server = MockServer::start().await;
        // Match a body that does NOT include client_secret. wiremock has no
        // direct "does not contain" matcher, so we rely on the structure:
        // assert presence of grant_type and absence is verified by total
        // body inspection in the responder.
        let received_body = Arc::new(tokio::sync::Mutex::new(String::new()));
        let captured = received_body.clone();

        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(BodyCapturingResponder {
                captured,
                response: ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "tok",
                    "instance_url": "https://my-org.my.salesforce.com"
                })),
            })
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();
        auth.access_token().await.unwrap();

        let body = received_body.lock().await;
        assert!(
            !body.contains("client_secret"),
            "public client should not send client_secret, got: {body}"
        );
    }

    #[tokio::test]
    async fn expired_cache_remints_token() {
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));

        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(CountingResponder {
                hits: hits.clone(),
                response: ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "tok",
                    "instance_url": "https://my-org.my.salesforce.com"
                })),
            })
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .login_url(server.uri())
            .token_ttl(Duration::ZERO)
            .build()
            .unwrap();

        let _ = auth.access_token().await.unwrap();
        let _ = auth.access_token().await.unwrap();
        let _ = auth.access_token().await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn revoked_refresh_token_surfaces_oauth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "expired authorization code"
            })))
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();

        let err = auth.access_token().await.unwrap_err();
        match err {
            CloudburstError::OAuth {
                error,
                error_description,
            } => {
                assert_eq!(error, "invalid_grant");
                assert!(error_description.is_some());
            }
            other => panic!("expected OAuth error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn instance_url_mismatch_is_an_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "instance_url": "https://wrong-org.my.salesforce.com"
            })))
            .mount(&server)
            .await;

        let auth = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();

        let err = auth.access_token().await.unwrap_err();
        assert!(matches!(err, CloudburstError::Auth(_)));
    }

    /// Counts invocations and returns a fixed response. Same as the JWT
    /// tests' helper — duplicated rather than shared to keep test modules
    /// self-contained.
    struct CountingResponder {
        hits: Arc<AtomicUsize>,
        response: ResponseTemplate,
    }

    impl Respond for CountingResponder {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            self.hits.fetch_add(1, Ordering::SeqCst);
            self.response.clone()
        }
    }

    /// Captures the request body for inspection. Used to assert that
    /// `client_secret` is absent in the public-client case.
    struct BodyCapturingResponder {
        captured: Arc<tokio::sync::Mutex<String>>,
        response: ResponseTemplate,
    }

    impl Respond for BodyCapturingResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = String::from_utf8_lossy(&request.body).into_owned();
            // try_lock works because this responder is invoked in the
            // request-handling task; the test reads after access_token returns.
            if let Ok(mut guard) = self.captured.try_lock() {
                *guard = body;
            }
            self.response.clone()
        }
    }
}
