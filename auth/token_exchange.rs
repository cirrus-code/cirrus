//! OAuth 2.0 Token Exchange flow (RFC 8693) for trading an external
//! identity provider's token for a Salesforce access token.
//!
//! Use this when Salesforce is one component in a federated identity
//! architecture: a central IdP issues access / refresh / id / SAML / JWT
//! tokens, and you want a Salesforce session for the same end user
//! without making them log in to Salesforce separately. The caller's
//! `subject_token` (from the IdP) is exchanged at the Salesforce token
//! endpoint for a fresh Salesforce token.
//!
//! ## Wire shape
//!
//! POST `/services/oauth2/token` with form body:
//!
//! - `grant_type` — either
//!   `urn:ietf:params:oauth:grant-type:token-exchange` (default) or
//!   `urn:ietf:params:oauth:grant-type:hybrid-token-exchange`
//!   for hybrid mobile apps.
//! - `subject_token` — the IdP-issued token (max 10,000 chars per docs).
//! - `subject_token_type` — one of the five well-known URNs in
//!   [`SubjectTokenType`].
//! - `client_id` — connected app consumer key.
//! - `client_secret` — required for confidential clients (the connected
//!   app's `Require Secret for Token Exchange Flow` setting), omitted
//!   for public clients.
//! - `scope` — optional space-separated scopes.
//! - `token_handler` — optional Apex token-exchange-handler name. The
//!   docs strongly recommend setting this; otherwise Salesforce uses the
//!   org's default handler.
//!
//! ## My Domain URL is required
//!
//! Per the Salesforce help docs, examples use
//! `MyDomainName.my.salesforce.com` (or the Experience Cloud
//! `MyDomainName.my.site.com`) as the host. Like Client Credentials,
//! `https://login.salesforce.com` is **not** a valid host for this flow,
//! so the builder requires `login_url`.
//!
//! ## What you get back
//!
//! A [`TokenExchangeSession`] containing the Salesforce `access_token`,
//! `instance_url`, and (depending on the connected app's scopes and the
//! request) optional `refresh_token`, `id_token`, `scope`, `issued_at`.
//! If a `refresh_token` is returned, wire it into a
//! [`crate::auth::RefreshTokenAuth`] for ongoing API access — the same
//! pattern as Web Server PKCE.

use crate::auth::token_endpoint::exchange;
use crate::error::{CloudburstError, CloudburstResult};

/// RFC 8693 grant-type URN for the regular token exchange flow.
pub const GRANT_TYPE_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// Salesforce-specific grant-type URN for the hybrid mobile-app variant.
pub const GRANT_TYPE_HYBRID_TOKEN_EXCHANGE: &str =
    "urn:ietf:params:oauth:grant-type:hybrid-token-exchange";

/// Which `grant_type` URN the request should send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TokenExchangeGrantType {
    /// `urn:ietf:params:oauth:grant-type:token-exchange` — the standard flow.
    #[default]
    TokenExchange,
    /// `urn:ietf:params:oauth:grant-type:hybrid-token-exchange` — for hybrid
    /// mobile apps that need both a UI session and an API session.
    HybridTokenExchange,
}

impl TokenExchangeGrantType {
    /// Returns the URN that should appear in the form body's `grant_type`.
    pub fn as_urn(self) -> &'static str {
        match self {
            Self::TokenExchange => GRANT_TYPE_TOKEN_EXCHANGE,
            Self::HybridTokenExchange => GRANT_TYPE_HYBRID_TOKEN_EXCHANGE,
        }
    }
}

/// RFC 8693 `subject_token_type` URNs supported by Salesforce.
///
/// The connected app's `OauthTokenExchangeHandler` metadata controls
/// which of these are accepted (`isAccessTokenSupported`,
/// `isRefreshTokenSupported`, `isIdTokenSupported`, `isSaml2Supported`,
/// `isJwtSupported`). [`Custom`](Self::Custom) is provided as an escape
/// hatch for token-type URNs Salesforce may add post-cutoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectTokenType {
    /// `urn:ietf:params:oauth:token-type:access_token` — OAuth 2.0 access token.
    AccessToken,
    /// `urn:ietf:params:oauth:token-type:refresh_token` — OAuth 2.0 refresh token.
    RefreshToken,
    /// `urn:ietf:params:oauth:token-type:id_token` — OpenID Connect ID token.
    IdToken,
    /// `urn:ietf:params:oauth:token-type:saml2` — base64 URL-encoded SAML 2.0 assertion.
    Saml2,
    /// `urn:ietf:params:oauth:token-type:jwt` — any token formatted as a JWT.
    Jwt,
    /// Escape hatch for an unrecognized URN.
    Custom(String),
}

impl SubjectTokenType {
    /// Returns the URN as it should appear in the form body.
    pub fn as_urn(&self) -> &str {
        match self {
            Self::AccessToken => "urn:ietf:params:oauth:token-type:access_token",
            Self::RefreshToken => "urn:ietf:params:oauth:token-type:refresh_token",
            Self::IdToken => "urn:ietf:params:oauth:token-type:id_token",
            Self::Saml2 => "urn:ietf:params:oauth:token-type:saml2",
            Self::Jwt => "urn:ietf:params:oauth:token-type:jwt",
            Self::Custom(s) => s,
        }
    }
}

/// One-shot RFC 8693 token-exchange request.
///
/// Construct via [`TokenExchangeFlow::builder`].
#[derive(Debug)]
pub struct TokenExchangeFlow {
    consumer_key: String,
    consumer_secret: Option<String>,
    login_url: String,
    subject_token: String,
    subject_token_type: SubjectTokenType,
    grant_type: TokenExchangeGrantType,
    scopes: Vec<String>,
    token_handler: Option<String>,
    http: reqwest::Client,
}

impl TokenExchangeFlow {
    /// Begins constructing a [`TokenExchangeFlow`].
    pub fn builder() -> TokenExchangeFlowBuilder {
        TokenExchangeFlowBuilder::default()
    }

    /// Performs the token exchange and returns the resulting Salesforce
    /// session. This consumes `self` because each invocation uses a
    /// specific `subject_token` that may already have been consumed at the
    /// IdP — re-running with the same builder would risk a double-spend
    /// of the IdP's token.
    pub async fn exchange(self) -> CloudburstResult<TokenExchangeSession> {
        let scope_joined;
        let mut body: Vec<(&str, &str)> = vec![
            ("grant_type", self.grant_type.as_urn()),
            ("subject_token", self.subject_token.as_str()),
            ("subject_token_type", self.subject_token_type.as_urn()),
            ("client_id", self.consumer_key.as_str()),
        ];
        if let Some(secret) = self.consumer_secret.as_deref() {
            body.push(("client_secret", secret));
        }
        if !self.scopes.is_empty() {
            scope_joined = self.scopes.join(" ");
            body.push(("scope", scope_joined.as_str()));
        }
        if let Some(handler) = self.token_handler.as_deref() {
            body.push(("token_handler", handler));
        }

        let token = exchange(&self.http, &self.login_url, &body).await?;
        Ok(TokenExchangeSession {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            id_token: token.id_token,
            instance_url: token.instance_url,
            issued_at: token.issued_at,
            scope: token.scope,
        })
    }
}

/// Result of a successful Token Exchange.
#[derive(Debug, Clone)]
pub struct TokenExchangeSession {
    /// Bearer access token for immediate Salesforce API calls.
    pub access_token: String,
    /// Refresh token, if the connected app + requested scopes caused one
    /// to be issued.
    pub refresh_token: Option<String>,
    /// OpenID Connect ID token, if `openid` was in the requested scopes.
    pub id_token: Option<String>,
    /// REST instance URL for subsequent API calls.
    pub instance_url: String,
    /// `issued_at` timestamp from the response (milliseconds-since-epoch as
    /// a string per Salesforce's wire format).
    pub issued_at: Option<String>,
    /// Granted scopes, space-separated.
    pub scope: Option<String>,
}

/// Builder for [`TokenExchangeFlow`].
#[derive(Default)]
pub struct TokenExchangeFlowBuilder {
    consumer_key: Option<String>,
    consumer_secret: Option<String>,
    login_url: Option<String>,
    subject_token: Option<String>,
    subject_token_type: Option<SubjectTokenType>,
    grant_type: Option<TokenExchangeGrantType>,
    scopes: Vec<String>,
    token_handler: Option<String>,
    http_client: Option<reqwest::Client>,
}

impl std::fmt::Debug for TokenExchangeFlowBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenExchangeFlowBuilder")
            .field("consumer_key", &self.consumer_key.is_some())
            .field("consumer_secret", &self.consumer_secret.is_some())
            .field("login_url", &self.login_url)
            .field("subject_token", &self.subject_token.is_some())
            .field("subject_token_type", &self.subject_token_type)
            .field("grant_type", &self.grant_type)
            .field("scopes", &self.scopes)
            .field("token_handler", &self.token_handler)
            .finish_non_exhaustive()
    }
}

impl TokenExchangeFlowBuilder {
    /// Connected App's Consumer Key (Client ID). Required.
    pub fn consumer_key(mut self, key: impl Into<String>) -> Self {
        self.consumer_key = Some(key.into());
        self
    }

    /// Connected App's Consumer Secret. Send only when the connected app
    /// has `Require Secret for Token Exchange Flow` enabled (or
    /// `isSecretRequiredForTokenExchange = true` for an external client
    /// app). Public clients (mobile, SPA) should omit this.
    pub fn consumer_secret(mut self, secret: impl Into<String>) -> Self {
        self.consumer_secret = Some(secret.into());
        self
    }

    /// Login URL — must be the org's My Domain URL (or Experience Cloud
    /// site URL). Required. `login.salesforce.com` is not supported for
    /// this flow.
    pub fn login_url(mut self, url: impl Into<String>) -> Self {
        self.login_url = Some(url.into());
        self
    }

    /// The IdP-issued token to exchange. Required. Salesforce documents a
    /// 10,000-char ceiling; we don't enforce it client-side, but the
    /// endpoint will reject anything larger.
    pub fn subject_token(mut self, token: impl Into<String>) -> Self {
        self.subject_token = Some(token.into());
        self
    }

    /// The type of the IdP token. Required.
    pub fn subject_token_type(mut self, ty: SubjectTokenType) -> Self {
        self.subject_token_type = Some(ty);
        self
    }

    /// Override the grant type. Defaults to
    /// [`TokenExchangeGrantType::TokenExchange`]; switch to
    /// [`TokenExchangeGrantType::HybridTokenExchange`] for hybrid mobile
    /// apps that need a UI session in addition to API access.
    pub fn grant_type(mut self, gt: TokenExchangeGrantType) -> Self {
        self.grant_type = Some(gt);
        self
    }

    /// Adds a scope. Multiple calls accumulate. The final set must be a
    /// subset of the connected app's assigned scopes.
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    /// Replaces the entire scope set.
    pub fn scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Apex token-exchange handler name. Strongly recommended per docs:
    /// without it, Salesforce uses the org's default handler (you must
    /// have at least one).
    pub fn token_handler(mut self, name: impl Into<String>) -> Self {
        self.token_handler = Some(name.into());
        self
    }

    /// Supplies a pre-configured `reqwest::Client` for the exchange.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalizes the builder.
    pub fn build(self) -> CloudburstResult<TokenExchangeFlow> {
        let consumer_key = self
            .consumer_key
            .ok_or(CloudburstError::MissingField("consumer_key"))?;
        let subject_token = self
            .subject_token
            .ok_or(CloudburstError::MissingField("subject_token"))?;
        let subject_token_type = self
            .subject_token_type
            .ok_or(CloudburstError::MissingField("subject_token_type"))?;
        let mut login_url = self
            .login_url
            .ok_or(CloudburstError::MissingField("login_url"))?;
        if login_url.ends_with('/') {
            login_url.pop();
        }
        let http = self.http_client.unwrap_or_default();
        Ok(TokenExchangeFlow {
            consumer_key,
            consumer_secret: self.consumer_secret,
            login_url,
            subject_token,
            subject_token_type,
            grant_type: self.grant_type.unwrap_or_default(),
            scopes: self.scopes,
            token_handler: self.token_handler,
            http,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn builder_with_required_fields() -> TokenExchangeFlowBuilder {
        TokenExchangeFlow::builder()
            .consumer_key("consumer-key-123")
            .login_url("https://my-org.my.salesforce.com")
            .subject_token("idp-issued-token-xyz")
            .subject_token_type(SubjectTokenType::AccessToken)
    }

    #[test]
    fn subject_token_type_urns_match_rfc_8693() {
        assert_eq!(
            SubjectTokenType::AccessToken.as_urn(),
            "urn:ietf:params:oauth:token-type:access_token"
        );
        assert_eq!(
            SubjectTokenType::RefreshToken.as_urn(),
            "urn:ietf:params:oauth:token-type:refresh_token"
        );
        assert_eq!(
            SubjectTokenType::IdToken.as_urn(),
            "urn:ietf:params:oauth:token-type:id_token"
        );
        assert_eq!(
            SubjectTokenType::Saml2.as_urn(),
            "urn:ietf:params:oauth:token-type:saml2"
        );
        assert_eq!(
            SubjectTokenType::Jwt.as_urn(),
            "urn:ietf:params:oauth:token-type:jwt"
        );
        assert_eq!(
            SubjectTokenType::Custom("urn:custom:foo".into()).as_urn(),
            "urn:custom:foo"
        );
    }

    #[test]
    fn grant_type_urns_match_spec() {
        assert_eq!(
            TokenExchangeGrantType::TokenExchange.as_urn(),
            "urn:ietf:params:oauth:grant-type:token-exchange"
        );
        assert_eq!(
            TokenExchangeGrantType::HybridTokenExchange.as_urn(),
            "urn:ietf:params:oauth:grant-type:hybrid-token-exchange"
        );
    }

    #[test]
    fn builder_requires_consumer_key() {
        let err = TokenExchangeFlow::builder()
            .login_url("https://x")
            .subject_token("t")
            .subject_token_type(SubjectTokenType::Jwt)
            .build()
            .unwrap_err();
        assert!(matches!(err, CloudburstError::MissingField("consumer_key")));
    }

    #[test]
    fn builder_requires_login_url() {
        let err = TokenExchangeFlow::builder()
            .consumer_key("k")
            .subject_token("t")
            .subject_token_type(SubjectTokenType::Jwt)
            .build()
            .unwrap_err();
        assert!(matches!(err, CloudburstError::MissingField("login_url")));
    }

    #[test]
    fn builder_requires_subject_token() {
        let err = TokenExchangeFlow::builder()
            .consumer_key("k")
            .login_url("https://x")
            .subject_token_type(SubjectTokenType::Jwt)
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            CloudburstError::MissingField("subject_token")
        ));
    }

    #[test]
    fn builder_requires_subject_token_type() {
        let err = TokenExchangeFlow::builder()
            .consumer_key("k")
            .login_url("https://x")
            .subject_token("t")
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            CloudburstError::MissingField("subject_token_type")
        ));
    }

    #[test]
    fn builder_strips_trailing_slash_on_login_url() {
        let flow = builder_with_required_fields()
            .login_url("https://my-org.my.salesforce.com/")
            .build()
            .unwrap();
        assert_eq!(flow.login_url, "https://my-org.my.salesforce.com");
    }

    #[tokio::test]
    async fn exchange_sends_required_params() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange",
            ))
            .and(body_string_contains("subject_token=idp-issued-token-xyz"))
            .and(body_string_contains(
                "subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token",
            ))
            .and(body_string_contains("client_id=consumer-key-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "00DXX!ACCESS",
                "instance_url": "https://my-org.my.salesforce.com",
                "token_type": "Bearer",
                "scope": "api refresh_token",
                "issued_at": "1700000000000",
            })))
            .mount(&server)
            .await;

        let session = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap()
            .exchange()
            .await
            .unwrap();
        assert_eq!(session.access_token, "00DXX!ACCESS");
        assert_eq!(session.instance_url, "https://my-org.my.salesforce.com");
        assert_eq!(session.scope.as_deref(), Some("api refresh_token"));
        assert_eq!(session.issued_at.as_deref(), Some("1700000000000"));
    }

    #[tokio::test]
    async fn exchange_includes_optional_params_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains("client_secret=hunter2"))
            .and(body_string_contains("scope=api+refresh_token"))
            .and(body_string_contains("token_handler=MyHandler"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "instance_url": "https://my-org.my.salesforce.com",
                "id_token": "eyJ...",
                "refresh_token": "5Aep861...",
            })))
            .mount(&server)
            .await;

        let session = builder_with_required_fields()
            .login_url(server.uri())
            .consumer_secret("hunter2")
            .scope("api")
            .scope("refresh_token")
            .token_handler("MyHandler")
            .build()
            .unwrap()
            .exchange()
            .await
            .unwrap();
        assert_eq!(session.id_token.as_deref(), Some("eyJ..."));
        assert_eq!(session.refresh_token.as_deref(), Some("5Aep861..."));
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

        builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap()
            .exchange()
            .await
            .unwrap();

        let body = captured.lock().await;
        assert!(
            !body.contains("client_secret"),
            "public client should not send client_secret, got: {body}"
        );
    }

    #[tokio::test]
    async fn hybrid_grant_type_sets_correct_urn() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ahybrid-token-exchange",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok",
                "instance_url": "https://my-org.my.salesforce.com"
            })))
            .mount(&server)
            .await;

        builder_with_required_fields()
            .login_url(server.uri())
            .grant_type(TokenExchangeGrantType::HybridTokenExchange)
            .build()
            .unwrap()
            .exchange()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_subject_token_surfaces_oauth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "subject_token validation failed"
            })))
            .mount(&server)
            .await;

        let err = builder_with_required_fields()
            .login_url(server.uri())
            .build()
            .unwrap()
            .exchange()
            .await
            .unwrap_err();
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
