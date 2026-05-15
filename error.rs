//! Error types for the Cirrus SDK.
//!
//! Salesforce REST endpoints return errors as a JSON array of objects with a
//! consistent shape (`message`, `errorCode`, optional `fields`), regardless of
//! the success-response shape. [`SalesforceError`] models that shape, and
//! [`CirrusError::Api`] carries the parsed array along with the HTTP
//! status. OAuth token endpoints use a different shape (`error` /
//! `error_description`), surfaced via [`CirrusError::OAuth`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Specialized `Result` type for Cirrus operations.
pub type CirrusResult<T> = Result<T, CirrusError>;

/// A single Salesforce API error entry.
///
/// Salesforce REST endpoints return errors as a JSON array of these objects.
/// The shape is schema-independent and applies to every REST resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalesforceError {
    /// Human-readable description of the error.
    pub message: String,
    /// Salesforce-defined error code (e.g. `INVALID_FIELD`, `NOT_FOUND`).
    #[serde(rename = "errorCode")]
    pub error_code: String,
    /// Field names involved in the error, when applicable (validation errors).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

/// Errors produced by the Cirrus client.
#[derive(Debug, Error)]
pub enum CirrusError {
    /// A required builder field was not set.
    #[error("missing required builder field: {0}")]
    MissingField(&'static str),

    /// Failed to construct the underlying HTTP client.
    #[error("failed to construct HTTP client: {0}")]
    HttpClient(#[source] reqwest::Error),

    /// Network or transport-level HTTP failure.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// Salesforce returned a non-2xx response. `errors` holds the parsed
    /// Salesforce error array; if the body could not be parsed as the
    /// canonical shape, the raw body is in `raw`.
    #[error("Salesforce API error (status {status}): {}", display_errors(.errors, .raw))]
    Api {
        /// HTTP status code returned by Salesforce.
        status: u16,
        /// Parsed Salesforce error entries. Empty if the body was not parseable
        /// as the canonical error array.
        errors: Vec<SalesforceError>,
        /// Raw response body. Populated when `errors` is empty so callers can
        /// see what came back.
        raw: Option<String>,
    },

    /// OAuth token endpoint returned an error response (`error` /
    /// `error_description` shape).
    #[error("OAuth error: {error}{}", .error_description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default())]
    OAuth {
        error: String,
        error_description: Option<String>,
    },

    /// An auth implementation failed to produce a token (network, expired
    /// credentials, signing failure, etc.). Carries the underlying message.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// JSON serialization or deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// URL parsing failure (instance URL, redirect URI, etc.).
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    /// Header value rejected by reqwest (invalid bytes, etc.).
    #[error("invalid header value: {0}")]
    InvalidHeader(String),

    /// Response could not be interpreted as the requested type or shape.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

fn display_errors(errors: &[SalesforceError], raw: &Option<String>) -> String {
    if errors.is_empty() {
        return raw
            .as_deref()
            .map(|r| format!("<unparsed body: {r}>"))
            .unwrap_or_else(|| "<no body>".to_string());
    }
    errors
        .iter()
        .map(|e| {
            if e.fields.is_empty() {
                format!("[{}] {}", e.error_code, e.message)
            } else {
                format!(
                    "[{}] {} (fields: {})",
                    e.error_code,
                    e.message,
                    e.fields.join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn display_errors_formats_single_error() {
        let err = CirrusError::Api {
            status: 400,
            errors: vec![SalesforceError {
                message: "Required field missing".to_string(),
                error_code: "REQUIRED_FIELD_MISSING".to_string(),
                fields: vec!["Name".to_string()],
            }],
            raw: None,
        };
        let msg = err.to_string();
        assert!(msg.contains("400"));
        assert!(msg.contains("REQUIRED_FIELD_MISSING"));
        assert!(msg.contains("Name"));
    }

    #[test]
    fn display_errors_falls_back_to_raw_body() {
        let err = CirrusError::Api {
            status: 500,
            errors: vec![],
            raw: Some("Internal Server Error".to_string()),
        };
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("Internal Server Error"));
    }

    #[test]
    fn salesforce_error_deserializes_canonical_shape() {
        let json = r#"[{"message":"bad","errorCode":"INVALID_FIELD","fields":["Foo"]}]"#;
        let parsed: Vec<SalesforceError> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].error_code, "INVALID_FIELD");
        assert_eq!(parsed[0].fields, vec!["Foo"]);
    }

    #[test]
    fn salesforce_error_handles_missing_fields() {
        let json = r#"[{"message":"bad","errorCode":"INVALID_SESSION_ID"}]"#;
        let parsed: Vec<SalesforceError> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].fields.is_empty());
    }
}
