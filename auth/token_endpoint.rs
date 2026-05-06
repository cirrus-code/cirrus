//! Shared OAuth token-exchange machinery used by every auth flow.
//!
//! Salesforce's `/services/oauth2/token` endpoint accepts several
//! `grant_type` values (JWT bearer, refresh token, authorization code,
//! device code, client credentials). The wire shape is the same for all of
//! them: an `application/x-www-form-urlencoded` POST body, and a JSON
//! response that's either a [`TokenResponse`] on 2xx or
//! `{error, error_description}` on 4xx/5xx. The flow-specific code in
//! [`crate::auth::jwt`], [`crate::auth::refresh`], etc. constructs the form
//! body; [`exchange`] handles the rest.

use crate::error::{CloudburstError, CloudburstResult};
use serde::{Deserialize, Serialize};

/// Successful token-endpoint response.
///
/// Fields not surfaced by any flow yet (`token_type`, `signature`, `id`)
/// are deserialized into oblivion. `refresh_token`, `id_token`, `scope`,
/// and `issued_at` are populated only when the flow + connected-app
/// configuration cause Salesforce to include them.
#[derive(Debug, Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) instance_url: String,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) id_token: Option<String>,
    #[serde(default)]
    pub(super) scope: Option<String>,
    #[serde(default)]
    pub(super) issued_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// POSTs a token-exchange form body to `{login_url}/services/oauth2/token`
/// and parses the response.
///
/// The caller assembles the form body with the flow-specific fields
/// (`grant_type`, `assertion`, `refresh_token`, etc.). On non-2xx, the body
/// is parsed as the OAuth error shape if possible; otherwise the raw body
/// is folded into a generic [`CloudburstError::Auth`] message.
pub(super) async fn exchange<B>(
    http: &reqwest::Client,
    login_url: &str,
    body: &B,
) -> CloudburstResult<TokenResponse>
where
    B: Serialize + ?Sized,
{
    let url = format!("{login_url}/services/oauth2/token");
    let response = http.post(&url).form(body).send().await?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await?;

    if !(200..300).contains(&status) {
        if let Ok(oauth_err) = serde_json::from_slice::<OAuthErrorResponse>(&bytes) {
            return Err(CloudburstError::OAuth {
                error: oauth_err.error,
                error_description: oauth_err.error_description,
            });
        }
        return Err(CloudburstError::Auth(format!(
            "token endpoint returned status {status}: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }

    serde_json::from_slice::<TokenResponse>(&bytes)
        .map_err(|e| CloudburstError::Auth(format!("malformed token response: {e}")))
}

/// Validates that a token response's `instance_url` matches the value the
/// caller configured. A mismatch usually signals a misconfigured Connected
/// App (wrong org), which is more actionable when surfaced at auth time
/// than as a downstream API error.
pub(super) fn check_instance_url(expected: &str, response: &TokenResponse) -> CloudburstResult<()> {
    let returned = response.instance_url.trim_end_matches('/');
    if returned != expected {
        return Err(CloudburstError::Auth(format!(
            "token response instance_url ({returned}) does not match configured instance_url ({expected})"
        )));
    }
    Ok(())
}
