//! OAuth 2.0 Web Server flow with PKCE for interactive (user-in-the-loop) auth.
//!
//! Two phases driven by the caller:
//!
//! 1. [`WebServerFlow::start`] mints a fresh `code_verifier` + `state`,
//!    derives the `S256` challenge per RFC 7636, and returns the
//!    authorization URL the caller should redirect the user to. The
//!    secrets/state needed for completion are returned alongside as a
//!    [`PendingExchange`] — an opaque, serializable value the caller is
//!    responsible for persisting (signed cookie, server-side session,
//!    Redis, etc.) until the user comes back through the callback.
//!
//! 2. On callback, the caller passes the `code` and `state` query
//!    parameters into [`PendingExchange::complete`]. The state is verified,
//!    the code is exchanged for tokens at `/services/oauth2/token`, and a
//!    [`CompletedSession`] is returned containing the access token,
//!    instance URL, and (if the connected app's scopes include
//!    `refresh_token`) a refresh token.
//!
//! The library is **stateless between the two phases** — it never stores
//! the verifier internally. This composes cleanly with any web framework
//! the caller chooses, and lets multiple in-flight authorizations coexist
//! without a server-side store the SDK has to manage.
//!
//! ## Wiring back into the SDK
//!
//! With a refresh token in hand, build a [`crate::RefreshTokenAuth`]:
//!
//! ```no_run
//! # use cirrus_auth::{RefreshTokenAuth, WebServerFlow};
//! # async fn ex(http: &reqwest::Client) -> Result<(), Box<dyn std::error::Error>> {
//! # let flow = WebServerFlow::builder()
//! #     .consumer_key("k").redirect_uri("https://app/cb").build()?;
//! # let (_url, pending) = flow.start()?;
//! # let session = pending.complete("code", "state", http).await?;
//! let refresh_token = session.refresh_token
//!     .ok_or("connected app didn't return a refresh token")?;
//! let auth = RefreshTokenAuth::builder()
//!     .consumer_key("k")
//!     .refresh_token(refresh_token)
//!     .instance_url(session.instance_url)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! ## Confidential vs public clients
//!
//! Public (PKCE-only) clients omit `consumer_secret` and rely on the
//! `code_verifier` for client authentication. Confidential clients still
//! send `client_secret` on the token exchange — Salesforce permits both.
//! The builder treats `consumer_secret` as optional accordingly.

use crate::error::{AuthError, AuthResult};
use crate::token_endpoint::exchange;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Salesforce production login URL — also the default authorization host.
pub const PRODUCTION_LOGIN_URL: &str = "https://login.salesforce.com";

/// Salesforce sandbox login URL.
pub const SANDBOX_LOGIN_URL: &str = "https://test.salesforce.com";

/// Number of random bytes for the PKCE `code_verifier`. After base64-url
/// (no-pad) encoding, 96 bytes → 128 chars, the maximum allowed by
/// RFC 7636 and the value Salesforce's docs cite verbatim. 32 bytes (256
/// bits) of entropy would already be cryptographically sufficient, but
/// matching the literal Salesforce recommendation avoids audit-time
/// pushback.
const VERIFIER_BYTES: usize = 96;

/// Number of random bytes for the `state` nonce. 16 bytes → 22 chars
/// post-encoding, well over the entropy needed to defeat CSRF.
const STATE_BYTES: usize = 16;

/// Configures the OAuth 2.0 Web Server flow.
///
/// Construct via [`WebServerFlow::builder`].
#[derive(Debug, Clone)]
pub struct WebServerFlow {
    consumer_key: String,
    consumer_secret: Option<String>,
    redirect_uri: String,
    login_url: String,
    scopes: Vec<String>,
    prompt: Option<String>,
    login_hint: Option<String>,
}

impl WebServerFlow {
    /// Begins constructing a [`WebServerFlow`].
    pub fn builder() -> WebServerFlowBuilder {
        WebServerFlowBuilder::default()
    }

    /// Phase 1 — generate a fresh PKCE verifier + state nonce, build the
    /// authorization URL, and return both. The caller redirects the user
    /// to the URL and persists the [`PendingExchange`] until the callback.
    pub fn start(&self) -> AuthResult<(String, PendingExchange)> {
        let code_verifier = random_b64url(VERIFIER_BYTES)?;
        let state = random_b64url(STATE_BYTES)?;
        let code_challenge = pkce_s256_challenge(&code_verifier);

        let mut url = url::Url::parse(&self.login_url)?;
        url.set_path("/services/oauth2/authorize");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code");
            q.append_pair("client_id", &self.consumer_key);
            q.append_pair("redirect_uri", &self.redirect_uri);
            q.append_pair("code_challenge", &code_challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("state", &state);
            if !self.scopes.is_empty() {
                q.append_pair("scope", &self.scopes.join(" "));
            }
            if let Some(p) = self.prompt.as_deref() {
                q.append_pair("prompt", p);
            }
            if let Some(h) = self.login_hint.as_deref() {
                q.append_pair("login_hint", h);
            }
        }

        let pending = PendingExchange {
            consumer_key: self.consumer_key.clone(),
            consumer_secret: self.consumer_secret.clone(),
            redirect_uri: self.redirect_uri.clone(),
            login_url: self.login_url.clone(),
            code_verifier,
            state,
        };

        Ok((url.into(), pending))
    }
}

/// Opaque, serializable handle holding the PKCE verifier + state nonce
/// between the authorize and token-exchange phases. The caller must
/// persist it (signed cookie, session store, etc.) until the OAuth
/// callback fires.
///
/// Treat the contents as a secret — leakage of the `code_verifier` would
/// let an attacker who intercepts the authorization code complete the
/// exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingExchange {
    consumer_key: String,
    consumer_secret: Option<String>,
    redirect_uri: String,
    login_url: String,
    code_verifier: String,
    state: String,
}

impl PendingExchange {
    /// Phase 2 — verify the returned `state`, exchange `code` for tokens.
    ///
    /// Returns a [`CompletedSession`] with the access token, instance URL,
    /// and (if the connected app issued one) refresh token.
    pub async fn complete(
        self,
        code: &str,
        returned_state: &str,
        http: &reqwest::Client,
    ) -> AuthResult<CompletedSession> {
        // CSRF defense: the state we generated in start() must match what
        // the IdP echoed back. A mismatch typically means a forged callback.
        if returned_state != self.state {
            return Err(AuthError::Other(
                "state mismatch in OAuth callback".to_string(),
            ));
        }

        let mut body: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", self.consumer_key.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", self.code_verifier.as_str()),
        ];
        if let Some(secret) = self.consumer_secret.as_deref() {
            body.push(("client_secret", secret));
        }

        let token = exchange(http, &self.login_url, &body).await?;
        Ok(CompletedSession {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            instance_url: token.instance_url,
            id: token.id,
            issued_at: token.issued_at,
            signature: token.signature,
            scope: token.scope,
        })
    }

    /// The `state` nonce sent in the authorization URL. Exposed for tests
    /// and for callers that want to surface it to logs.
    pub fn state(&self) -> &str {
        &self.state
    }
}

/// Result of a successful authorization-code exchange.
#[derive(Debug, Clone)]
pub struct CompletedSession {
    /// Bearer access token for immediate API calls.
    pub access_token: String,
    /// Long-lived refresh token. Present only if the connected app's
    /// scopes include `refresh_token` and the user granted it.
    pub refresh_token: Option<String>,
    /// REST instance URL reported by the token endpoint.
    pub instance_url: String,
    /// Salesforce user-identity URL (e.g.
    /// `https://login.salesforce.com/id/{org_id}/{user_id}`). Identifies
    /// which user was authenticated.
    pub id: Option<String>,
    /// Token-issuance timestamp (milliseconds since epoch as a string).
    pub issued_at: Option<String>,
    /// Base64-encoded HMAC-SHA256 of `id + issued_at` with the
    /// connected-app consumer secret as the key — lets the caller
    /// verify the token came from Salesforce.
    pub signature: Option<String>,
    /// Granted scopes, space-separated.
    pub scope: Option<String>,
}

/// Builder for [`WebServerFlow`].
#[derive(Default)]
pub struct WebServerFlowBuilder {
    consumer_key: Option<String>,
    consumer_secret: Option<String>,
    redirect_uri: Option<String>,
    login_url: Option<String>,
    scopes: Vec<String>,
    prompt: Option<String>,
    login_hint: Option<String>,
}

impl std::fmt::Debug for WebServerFlowBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebServerFlowBuilder")
            .field("consumer_key", &self.consumer_key.is_some())
            .field("consumer_secret", &self.consumer_secret.is_some())
            .field("redirect_uri", &self.redirect_uri)
            .field("login_url", &self.login_url)
            .field("scopes", &self.scopes)
            .field("prompt", &self.prompt)
            .field("login_hint", &self.login_hint)
            .finish_non_exhaustive()
    }
}

impl WebServerFlowBuilder {
    /// Connected App's Consumer Key (Client ID). Required.
    pub fn consumer_key(mut self, key: impl Into<String>) -> Self {
        self.consumer_key = Some(key.into());
        self
    }

    /// Connected App's Consumer Secret. Optional — set only for
    /// confidential clients. Public (PKCE-only) clients omit it.
    pub fn consumer_secret(mut self, secret: impl Into<String>) -> Self {
        self.consumer_secret = Some(secret.into());
        self
    }

    /// Redirect URI registered on the Connected App. Required. Must match
    /// exactly (Salesforce compares string-for-string).
    pub fn redirect_uri(mut self, uri: impl Into<String>) -> Self {
        self.redirect_uri = Some(uri.into());
        self
    }

    /// Authorization host. Defaults to [`PRODUCTION_LOGIN_URL`]. Use
    /// [`SANDBOX_LOGIN_URL`] for sandboxes or your org's My Domain login URL.
    pub fn login_url(mut self, url: impl Into<String>) -> Self {
        self.login_url = Some(url.into());
        self
    }

    /// Adds a scope to the authorization request. Multiple calls accumulate.
    /// Common values include `api`, `refresh_token`, `id`, `openid`.
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    /// Replaces the entire scope set. Convenient when scopes are computed
    /// elsewhere as a slice.
    pub fn scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Adds the OAuth `prompt` parameter (e.g. `login`, `consent`,
    /// `select_account`).
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Pre-fills the username field on the authorization page.
    pub fn login_hint(mut self, hint: impl Into<String>) -> Self {
        self.login_hint = Some(hint.into());
        self
    }

    /// Finalizes the builder.
    pub fn build(self) -> AuthResult<WebServerFlow> {
        let consumer_key = self
            .consumer_key
            .ok_or(AuthError::MissingField("consumer_key"))?;
        let redirect_uri = self
            .redirect_uri
            .ok_or(AuthError::MissingField("redirect_uri"))?;
        let mut login_url = self
            .login_url
            .unwrap_or_else(|| PRODUCTION_LOGIN_URL.to_string());
        if login_url.ends_with('/') {
            login_url.pop();
        }
        Ok(WebServerFlow {
            consumer_key,
            consumer_secret: self.consumer_secret,
            redirect_uri,
            login_url,
            scopes: self.scopes,
            prompt: self.prompt,
            login_hint: self.login_hint,
        })
    }
}

/// Returns `len` cryptographically random bytes encoded as URL-safe base64
/// without padding — the format RFC 7636 requires for `code_verifier` and
/// the format we use for `state` to keep it URL-safe.
fn random_b64url(len: usize) -> AuthResult<String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| AuthError::Other(format!("CSPRNG failure: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

/// Computes the RFC 7636 `S256` PKCE challenge: SHA-256 of the verifier
/// bytes, base64-url-no-pad encoded.
fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn flow_with_required_fields() -> WebServerFlowBuilder {
        WebServerFlow::builder()
            .consumer_key("consumer-key-123")
            .redirect_uri("https://app.example.com/oauth/callback")
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc_7636_test_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(pkce_s256_challenge(verifier), expected);
    }

    #[test]
    fn random_b64url_returns_distinct_values() {
        let a = random_b64url(VERIFIER_BYTES).unwrap();
        let b = random_b64url(VERIFIER_BYTES).unwrap();
        assert_ne!(a, b);
        // 96 bytes encodes to exactly 128 base64-url chars (no padding) —
        // the RFC 7636 maximum and Salesforce's documented recommendation.
        assert_eq!(a.len(), 128);
    }

    #[test]
    fn random_b64url_is_url_safe() {
        for _ in 0..10 {
            let s = random_b64url(32).unwrap();
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "non-url-safe char in: {s}"
            );
        }
    }

    #[test]
    fn builder_requires_consumer_key() {
        let err = WebServerFlow::builder()
            .redirect_uri("https://x")
            .build()
            .unwrap_err();
        assert!(matches!(err, AuthError::MissingField("consumer_key")));
    }

    #[test]
    fn builder_requires_redirect_uri() {
        let err = WebServerFlow::builder()
            .consumer_key("k")
            .build()
            .unwrap_err();
        assert!(matches!(err, AuthError::MissingField("redirect_uri")));
    }

    #[test]
    fn start_builds_authorization_url_with_all_required_params() {
        let flow = flow_with_required_fields()
            .login_url("https://login.salesforce.com")
            .scope("api")
            .scope("refresh_token")
            .build()
            .unwrap();
        let (url, pending) = flow.start().unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("login.salesforce.com"));
        assert_eq!(parsed.path(), "/services/oauth2/authorize");

        let q: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(q.get("response_type").map(|s| s.as_ref()), Some("code"));
        assert_eq!(
            q.get("client_id").map(|s| s.as_ref()),
            Some("consumer-key-123")
        );
        assert_eq!(
            q.get("redirect_uri").map(|s| s.as_ref()),
            Some("https://app.example.com/oauth/callback")
        );
        assert_eq!(
            q.get("code_challenge_method").map(|s| s.as_ref()),
            Some("S256")
        );
        assert_eq!(
            q.get("scope").map(|s| s.as_ref()),
            Some("api refresh_token")
        );
        assert!(q.contains_key("code_challenge"));
        assert!(q.contains_key("state"));

        // Challenge in the URL must match a fresh hash of the verifier
        // we held back in the PendingExchange.
        let expected_challenge = pkce_s256_challenge(&pending.code_verifier);
        assert_eq!(
            q.get("code_challenge").map(|s| s.as_ref()),
            Some(expected_challenge.as_str())
        );
        assert_eq!(
            q.get("state").map(|s| s.as_ref()),
            Some(pending.state.as_str())
        );
    }

    #[test]
    fn start_includes_optional_params_when_set() {
        let flow = flow_with_required_fields()
            .prompt("login")
            .login_hint("user@example.com")
            .build()
            .unwrap();
        let (url, _) = flow.start().unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(q.get("prompt").map(|s| s.as_ref()), Some("login"));
        assert_eq!(
            q.get("login_hint").map(|s| s.as_ref()),
            Some("user@example.com")
        );
    }

    #[test]
    fn start_omits_scope_when_empty() {
        let flow = flow_with_required_fields().build().unwrap();
        let (url, _) = flow.start().unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        assert!(!q.contains_key("scope"));
    }

    #[test]
    fn pending_exchange_round_trips_through_serde() {
        let flow = flow_with_required_fields().build().unwrap();
        let (_, pending) = flow.start().unwrap();
        let json = serde_json::to_string(&pending).unwrap();
        let restored: PendingExchange = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.state(), pending.state());
        assert_eq!(restored.code_verifier, pending.code_verifier);
    }

    #[tokio::test]
    async fn complete_exchanges_code_for_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=auth-code-xyz"))
            .and(body_string_contains("client_id=consumer-key-123"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "00DXX!ACCESS",
                "refresh_token": "5Aep861KIwKdekr",
                "instance_url": "https://my-org.my.salesforce.com",
                "token_type": "Bearer",
                "id": "https://login.salesforce.com/id/00DXX/005XX",
                "issued_at": "1278448384422",
                "signature": "2wG3D9w1PzUlP/BEwa0u3D2C/D54p4Nz6tH5e9d0E5Q=",
                "scope": "api refresh_token",
            })))
            .mount(&server)
            .await;

        let flow = flow_with_required_fields()
            .login_url(server.uri())
            .scope("api")
            .scope("refresh_token")
            .build()
            .unwrap();
        let (_url, pending) = flow.start().unwrap();
        let state = pending.state().to_string();

        let session = pending
            .complete("auth-code-xyz", &state, &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(session.access_token, "00DXX!ACCESS");
        assert_eq!(session.refresh_token.as_deref(), Some("5Aep861KIwKdekr"));
        assert_eq!(session.instance_url, "https://my-org.my.salesforce.com");
        // New audit-driven assertions: surface the documented Salesforce
        // identity fields so callers can verify which user authenticated.
        assert_eq!(
            session.id.as_deref(),
            Some("https://login.salesforce.com/id/00DXX/005XX")
        );
        assert_eq!(session.issued_at.as_deref(), Some("1278448384422"));
        assert!(session.signature.is_some());
        assert_eq!(session.scope.as_deref(), Some("api refresh_token"));
    }

    #[tokio::test]
    async fn complete_rejects_state_mismatch_without_calling_endpoint() {
        // No mock mounted — if we hit the network, the test will fail loudly.
        let server = MockServer::start().await;
        let flow = flow_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();
        let (_url, pending) = flow.start().unwrap();

        let err = pending
            .complete("code", "wrong-state", &reqwest::Client::new())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::Other(_)));
    }

    #[tokio::test]
    async fn confidential_client_includes_client_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains("client_secret=hunter2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "instance_url": "https://my-org.my.salesforce.com"
            })))
            .mount(&server)
            .await;

        let flow = flow_with_required_fields()
            .login_url(server.uri())
            .consumer_secret("hunter2")
            .build()
            .unwrap();
        let (_, pending) = flow.start().unwrap();
        let state = pending.state().to_string();
        pending
            .complete("c", &state, &reqwest::Client::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn public_client_omits_client_secret() {
        let server = MockServer::start().await;
        let captured = Arc::new(tokio::sync::Mutex::new(String::new()));
        let captured_clone = captured.clone();

        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(BodyCapturingResponder {
                captured: captured_clone,
                response: ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "tok",
                    "instance_url": "https://my-org.my.salesforce.com"
                })),
            })
            .mount(&server)
            .await;

        let flow = flow_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();
        let (_, pending) = flow.start().unwrap();
        let state = pending.state().to_string();
        pending
            .complete("c", &state, &reqwest::Client::new())
            .await
            .unwrap();

        let body = captured.lock().await;
        assert!(
            !body.contains("client_secret"),
            "public client should not send client_secret, got: {body}"
        );
    }

    #[tokio::test]
    async fn user_denied_surfaces_oauth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "user denied access"
            })))
            .mount(&server)
            .await;

        let flow = flow_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap();
        let (_, pending) = flow.start().unwrap();
        let state = pending.state().to_string();
        let err = pending
            .complete("c", &state, &reqwest::Client::new())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth { .. }));
    }

    struct BodyCapturingResponder {
        captured: Arc<tokio::sync::Mutex<String>>,
        response: ResponseTemplate,
    }

    impl Respond for BodyCapturingResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = String::from_utf8_lossy(&request.body).into_owned();
            if let Ok(mut guard) = self.captured.try_lock() {
                *guard = body;
            }
            self.response.clone()
        }
    }
}
