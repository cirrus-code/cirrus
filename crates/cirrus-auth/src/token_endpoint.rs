//! Shared OAuth token-exchange machinery used by every auth flow.
//!
//! Salesforce's `/services/oauth2/token` endpoint accepts several
//! `grant_type` values (JWT bearer, refresh token, authorization code,
//! device code, client credentials). The wire shape is the same for all of
//! them: an `application/x-www-form-urlencoded` POST body, and a JSON
//! response that's either a [`TokenResponse`] on 2xx or
//! `{error, error_description}` on 4xx/5xx. The flow-specific code in
//! [`crate::jwt`], [`crate::refresh`], etc. constructs the form body;
//! [`exchange`] handles the rest.

use crate::error::{AuthError, AuthResult};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Margin subtracted from a cached token's lifetime when deciding whether
/// it is still usable. A token whose real expiry is within this window is
/// treated as already expired and re-minted proactively, so an in-flight
/// request never lands at Salesforce with a token that expired in transit.
/// This trades a slightly earlier refresh for eliminating a class of
/// avoidable 401 round-trips at every TTL boundary.
pub(super) const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// Successful token-endpoint response.
///
/// Mirrors the documented Salesforce token response shape, which is the
/// standard OAuth 2.0 response (RFC 6749 §5.1: `access_token`, `token_type`,
/// `refresh_token`, `scope`) plus Salesforce-specific extensions
/// (`instance_url`, `id`, `issued_at`, `signature`).
///
/// Field availability depends on the flow + connected-app configuration:
/// - `refresh_token` — only when the connected app's scope set includes
///   `refresh_token` *and* the flow supports issuance (Web Server, Token
///   Exchange). Never present on Client Credentials or JWT Bearer.
/// - `id_token` — only when the requested `scope` includes `openid`
///   (OIDC).
/// - `scope` — present when the granted scope set differs from the
///   requested set, or always on some flows. Treat as best-effort.
/// - `issued_at` — milliseconds since epoch as a *string*, not a number.
/// - `expires_in` — token lifetime in seconds (RFC 6749 §5.1,
///   RECOMMENDED). Salesforce omits it on most flows; when present we
///   prefer it over the configured cache TTL. Modeled as `Option<u64>` +
///   `default` so its absence never breaks parsing.
/// - `signature` / `id` / `token_type` — present on every successful
///   flow except where Salesforce explicitly omits (e.g. some on-behalf-of
///   exchanges).
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) instance_url: String,
    /// Token lifetime in seconds, when the endpoint advertises one
    /// (RFC 6749 §5.1). Preferred over the static cache TTL via
    /// [`TokenResponse::cache_expiry`] when present.
    #[serde(default)]
    pub(super) expires_in: Option<u64>,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) id_token: Option<String>,
    #[serde(default)]
    pub(super) scope: Option<String>,
    #[serde(default)]
    pub(super) issued_at: Option<String>,
    /// Salesforce *user-identity URL*, e.g.
    /// `https://login.salesforce.com/id/00DRO0000004sJ7/005RO0000005V0E`.
    /// Distinct from `id_token` (which is OIDC). Useful for callers that
    /// want to know which user they authenticated as.
    #[serde(default)]
    pub(super) id: Option<String>,
    /// Base64-encoded HMAC-SHA256 of `id + issued_at` using the
    /// connected app's consumer secret as the key. Lets callers verify
    /// the token came from Salesforce, mitigating token-injection
    /// attacks. Absent on flows that don't have a consumer secret
    /// (some public-client variants).
    #[serde(default)]
    pub(super) signature: Option<String>,
    /// Always `"Bearer"` for the OAuth 2.0 flows Salesforce exposes.
    /// Parsed defensively so a future divergence wouldn't break the
    /// deserializer; not propagated onto session structs.
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) token_type: Option<String>,
}

impl TokenResponse {
    /// Computes the cache-expiry [`Instant`] for this freshly-issued token.
    ///
    /// Prefers the server-advertised `expires_in` (RFC 6749 §5.1) when
    /// present, falling back to the caller's configured `fallback_ttl`
    /// otherwise. Shared by every caching flow so they stay consistent.
    pub(super) fn cache_expiry(&self, fallback_ttl: Duration) -> Instant {
        let ttl = self
            .expires_in
            .map(Duration::from_secs)
            .unwrap_or(fallback_ttl);
        Instant::now() + ttl
    }
}

/// Whether a cached token minted to expire at `expires_at` is still safe to
/// use, accounting for the [`EXPIRY_MARGIN`] refresh window. The JWT,
/// refresh, and client-credentials flows all call this, so they apply an
/// identical margin.
pub(super) fn token_is_fresh(expires_at: Instant) -> bool {
    expires_at > Instant::now() + EXPIRY_MARGIN
}

// Redact every secret-bearing field. The `id`, `issued_at`, `scope`,
// `token_type`, and `instance_url` fields are non-sensitive — emit them
// verbatim so debug output stays useful.
impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"[redacted]")
            .field("instance_url", &self.instance_url)
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "[redacted]"))
            .field("scope", &self.scope)
            .field("issued_at", &self.issued_at)
            .field("id", &self.id)
            .field("signature", &self.signature.as_ref().map(|_| "[redacted]"))
            .field("token_type", &self.token_type)
            .finish()
    }
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

// `error_description` is server-supplied free text and has historically
// contained partial token material in some Salesforce error paths.
// Redact the description so the OAuth error code is the only thing that
// surfaces in `{:?}`.
impl std::fmt::Debug for OAuthErrorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthErrorResponse")
            .field("error", &self.error)
            .field(
                "error_description",
                &self.error_description.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// POSTs a token-exchange form body to `{login_url}/services/oauth2/token`
/// and parses the response.
///
/// The caller assembles the form body with the flow-specific fields
/// (`grant_type`, `assertion`, `refresh_token`, etc.). On non-2xx, the body
/// is parsed as the OAuth error shape if possible; otherwise the raw body
/// is folded into a generic [`AuthError::Other`] message.
pub(super) async fn exchange<B>(
    http: &reqwest::Client,
    login_url: &str,
    body: &B,
) -> AuthResult<TokenResponse>
where
    B: Serialize + ?Sized,
{
    let url = format!("{login_url}/services/oauth2/token");
    let response = http.post(&url).form(body).send().await?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await?;

    if !(200..300).contains(&status) {
        if let Ok(oauth_err) = serde_json::from_slice::<OAuthErrorResponse>(&bytes) {
            return Err(AuthError::OAuth {
                error: oauth_err.error,
                error_description: oauth_err.error_description,
            });
        }
        // The body didn't match the OAuth error shape. Do NOT fold it into
        // the error message: non-standard token-endpoint bodies (HTML error
        // pages, proxies, reflected request parameters) can echo token
        // material, and the error message flows into logs. Surface only the
        // status; expose the body solely at TRACE, which is off by default
        // and a deliberate per-target opt-in for debugging.
        tracing::trace!(
            target: "cirrus::auth",
            status,
            body = %String::from_utf8_lossy(&bytes),
            "token endpoint returned a non-2xx body that did not parse as an OAuth error",
        );
        return Err(AuthError::Other(format!(
            "token endpoint returned status {status} with an unrecognized error body"
        )));
    }

    serde_json::from_slice::<TokenResponse>(&bytes)
        .map_err(|e| AuthError::Other(format!("malformed token response: {e}")))
}

/// Validates that a token response's `instance_url` matches the value the
/// caller configured. A mismatch usually signals a misconfigured Connected
/// App (wrong org), which is more actionable when surfaced at auth time
/// than as a downstream API error.
pub(super) fn check_instance_url(expected: &str, response: &TokenResponse) -> AuthResult<()> {
    let returned = response.instance_url.trim_end_matches('/');
    if returned != expected {
        return Err(AuthError::Other(format!(
            "token response instance_url ({returned}) does not match configured instance_url ({expected})"
        )));
    }
    Ok(())
}
