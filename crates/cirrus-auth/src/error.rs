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
///
/// The `error_description` carried by [`AuthError::OAuth`] is
/// server-supplied free text that can include partial token material, so
/// it is redacted from both the `Display` and `Debug` representations of
/// this type — only the machine-readable `error` code is shown. Callers
/// that need the description can read it from the variant field directly.
#[derive(Error)]
#[non_exhaustive]
pub enum AuthError {
    /// A required builder field was not set.
    #[error("missing required builder field: {0}")]
    MissingField(&'static str),

    /// OAuth token endpoint returned an error response (`error` /
    /// `error_description` shape from RFC 6749 §5.2).
    ///
    /// Only the machine-readable `error` code is surfaced via `Display`;
    /// `error_description` is redacted (see the type-level note) but
    /// remains available by matching on the field.
    #[error("OAuth error: {error}")]
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

// Hand-written so `OAuth.error_description` is redacted in `{:?}` output —
// a derived `Debug` would print the raw description verbatim, defeating the
// redaction applied at every other layer. The `error` code and all other
// variants are non-sensitive and printed as usual.
impl std::fmt::Debug for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(field) => f.debug_tuple("MissingField").field(field).finish(),
            Self::OAuth {
                error,
                error_description,
            } => f
                .debug_struct("OAuth")
                .field("error", error)
                .field(
                    "error_description",
                    &error_description.as_ref().map(|_| "[redacted]"),
                )
                .finish(),
            Self::Other(msg) => f.debug_tuple("Other").field(msg).finish(),
            Self::Http(e) => f.debug_tuple("Http").field(e).finish(),
            Self::Serialization(e) => f.debug_tuple("Serialization").field(e).finish(),
            Self::Url(e) => f.debug_tuple("Url").field(e).finish(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn oauth_error_redacts_description_in_display_and_debug() {
        // A description carrying token-like material must not surface in
        // either formatted representation, but must remain readable via
        // the field.
        let err = AuthError::OAuth {
            error: "invalid_grant".to_string(),
            error_description: Some("00Dxx!AQ.SECRET_TOKEN_FRAGMENT".to_string()),
        };

        let display = err.to_string();
        assert!(display.contains("invalid_grant"));
        assert!(
            !display.contains("SECRET_TOKEN_FRAGMENT"),
            "Display leaked error_description: {display}"
        );

        let debug = format!("{err:?}");
        assert!(debug.contains("invalid_grant"));
        assert!(debug.contains("[redacted]"));
        assert!(
            !debug.contains("SECRET_TOKEN_FRAGMENT"),
            "Debug leaked error_description: {debug}"
        );

        // The description is still programmatically accessible.
        match err {
            AuthError::OAuth {
                error_description, ..
            } => assert!(error_description.is_some()),
            other => panic!("expected OAuth, got {other:?}"),
        }
    }

    #[test]
    fn oauth_error_debug_shows_none_description_without_redaction_marker() {
        let err = AuthError::OAuth {
            error: "invalid_client".to_string(),
            error_description: None,
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("invalid_client"));
        assert!(debug.contains("None"));
        assert!(!debug.contains("[redacted]"));
    }
}
