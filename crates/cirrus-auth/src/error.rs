//! Error types for `cirrus-auth`.
//!
//! Salesforce OAuth token endpoints return errors in the standard
//! `{error, error_description}` shape (RFC 6749 §5.2), which
//! [`AuthError::OAuth`] models directly. Everything else — missing
//! builder fields, transport failures, malformed responses — is
//! categorized into the remaining variants.
//!
//! The companion `cirrus` crate carries a `From<AuthError> for
//! CirrusError` impl so REST call sites that invoke an
//! [`crate::AuthSession`] can use `?` and surface the auth failure as
//! part of the unified client error.
//!
//! `AuthError` is intentionally `non_exhaustive` so we can grow it
//! without a SemVer break.

use thiserror::Error;

/// Specialized `Result` type for `cirrus-auth` operations.
pub type AuthResult<T> = Result<T, AuthError>;

/// Errors produced while acquiring or refreshing a Salesforce OAuth
/// session.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuthError {
    /// A required builder field was not set.
    #[error("missing required builder field: {0}")]
    MissingField(&'static str),

    /// OAuth token endpoint returned an error response (`error` /
    /// `error_description` shape from RFC 6749 §5.2).
    #[error("OAuth error: {error}{}", .error_description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default())]
    OAuth {
        error: String,
        error_description: Option<String>,
    },

    /// Catch-all for auth failures not modelled by a dedicated variant
    /// (system clock skew, JWT signing failure, CSPRNG failure,
    /// malformed token responses, etc.). Carries the underlying
    /// message.
    #[error("authentication failed: {0}")]
    Other(String),

    /// Network or transport-level HTTP failure while contacting an
    /// OAuth endpoint.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization or deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// URL parsing failure (instance URL, redirect URI, login URL,
    /// etc.).
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
}
