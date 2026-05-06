//! Response parsing and well-known platform types.
//!
//! Salesforce REST returns a small set of envelope shapes that are
//! schema-independent — they describe the platform's behavior, not any
//! org-specific data. Those are hard-coded here. Anything that would carry
//! user-defined fields (records, sObjects, custom payloads) is left generic
//! over a caller-supplied type parameter.
//!
//! ## The success-vs-error split
//!
//! Every REST endpoint returns the same error shape on non-2xx — a JSON array
//! of `{message, errorCode, fields}`. That lets [`parse_response_bytes`] check
//! the status code first and only attempt to deserialize into the caller's `R`
//! on success. Callers never need to model error shapes in their response
//! types.

use crate::error::{CloudburstError, CloudburstResult, SalesforceError};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Envelope returned by SOQL queries (`/query`, `/queryAll`, `/query/{locator}`).
///
/// Generic over the record type `R` — the SDK never assumes a record shape.
/// Use `serde_json::Value` for ad-hoc, or supply a typed struct.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryResult<R> {
    /// Total number of records matched by the query (across all pages).
    #[serde(rename = "totalSize")]
    pub total_size: i64,
    /// `true` if all records have been returned; `false` if more pages exist.
    pub done: bool,
    /// Locator URL for the next batch of records, when `done` is `false`.
    #[serde(rename = "nextRecordsUrl", default)]
    pub next_records_url: Option<String>,
    /// Records returned in this batch.
    #[serde(default = "Vec::new")]
    pub records: Vec<R>,
}

/// Result of a single-record create/upsert via REST sObjects endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct SObjectCreateResult {
    /// ID of the created (or upserted) record.
    pub id: String,
    /// Whether the operation succeeded. Salesforce always sets this to `true`
    /// on a 2xx response — included for completeness with the documented shape.
    pub success: bool,
    /// Error array. Always empty on success, but Salesforce includes the field.
    #[serde(default)]
    pub errors: Vec<SalesforceError>,
    /// `true` if an upsert created a new record, `false` if it updated an
    /// existing one. Absent on plain creates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
}

/// One entry from `GET /services/data` — a Salesforce REST API version.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiVersion {
    /// Human-readable label, e.g. `"Winter '24"`.
    pub label: String,
    /// URL prefix for endpoints in this version, e.g. `"/services/data/v60.0"`.
    pub url: String,
    /// Numeric version string, e.g. `"60.0"`.
    pub version: String,
}

/// Parses a Salesforce response body, branching on the HTTP status.
///
/// On 2xx, the body is deserialized into `R` (use `serde_json::Value` for an
/// untyped response). On 4xx/5xx, the body is parsed as a Salesforce error
/// array; if that fails the raw body is preserved in
/// [`CloudburstError::Api::raw`] for debugging.
pub(crate) fn parse_response_bytes<R: DeserializeOwned>(
    status: u16,
    bytes: &[u8],
) -> CloudburstResult<R> {
    if (200..300).contains(&status) {
        if bytes.is_empty() {
            // Some endpoints return 204 No Content. Try to deserialize an empty
            // JSON null — works for `()` and for `Option<T>`. Anything else
            // produces a serialization error that surfaces the mismatch.
            return serde_json::from_slice(b"null").map_err(CloudburstError::Serialization);
        }
        return serde_json::from_slice(bytes).map_err(CloudburstError::Serialization);
    }

    let errors = serde_json::from_slice::<Vec<SalesforceError>>(bytes).unwrap_or_default();
    let raw = if errors.is_empty() {
        Some(String::from_utf8_lossy(bytes).into_owned())
    } else {
        None
    };

    Err(CloudburstError::Api {
        status,
        errors,
        raw,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::Value;
    use serde_json::json;

    #[test]
    fn parses_success_into_value() {
        let body = json!({"Id": "001xx", "Name": "Acme"}).to_string();
        let parsed: Value = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(parsed["Id"], "001xx");
    }

    #[test]
    fn parses_query_result() {
        let body = json!({
            "totalSize": 2,
            "done": true,
            "records": [
                {"Id": "1", "Name": "A"},
                {"Id": "2", "Name": "B"}
            ]
        })
        .to_string();
        let qr: QueryResult<Value> = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(qr.total_size, 2);
        assert!(qr.done);
        assert_eq!(qr.records.len(), 2);
        assert!(qr.next_records_url.is_none());
    }

    #[test]
    fn parses_paginated_query_result() {
        let body = json!({
            "totalSize": 1500,
            "done": false,
            "nextRecordsUrl": "/services/data/v60.0/query/01g...-2000",
            "records": []
        })
        .to_string();
        let qr: QueryResult<Value> = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(!qr.done);
        assert_eq!(
            qr.next_records_url.as_deref(),
            Some("/services/data/v60.0/query/01g...-2000")
        );
    }

    #[test]
    fn parses_error_array_into_api_error() {
        let body = r#"[{"message":"No such column","errorCode":"INVALID_FIELD","fields":["Foo"]}]"#;
        let err = parse_response_bytes::<Value>(400, body.as_bytes()).unwrap_err();
        match err {
            CloudburstError::Api {
                status,
                errors,
                raw,
            } => {
                assert_eq!(status, 400);
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].error_code, "INVALID_FIELD");
                assert!(raw.is_none());
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_raw_when_error_body_is_unparseable() {
        let body = "<html>Internal Server Error</html>";
        let err = parse_response_bytes::<Value>(500, body.as_bytes()).unwrap_err();
        match err {
            CloudburstError::Api {
                status,
                errors,
                raw,
            } => {
                assert_eq!(status, 500);
                assert!(errors.is_empty());
                assert_eq!(raw.as_deref(), Some(body));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn empty_2xx_body_is_treated_as_null() {
        let parsed: Option<Value> = parse_response_bytes(204, b"").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn parses_sobject_create_result() {
        let body = r#"{"id":"001xx0000000001","success":true,"errors":[]}"#;
        let parsed: SObjectCreateResult = parse_response_bytes(201, body.as_bytes()).unwrap();
        assert_eq!(parsed.id, "001xx0000000001");
        assert!(parsed.success);
        assert!(parsed.errors.is_empty());
        assert!(parsed.created.is_none());
    }
}
