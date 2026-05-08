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
use std::collections::HashMap;

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

/// Envelope returned by SOSL search endpoints (`/search`,
/// `/parameterizedSearch`).
///
/// Generic over the record type `R`. Every record carries a Salesforce
/// `attributes` object identifying the object type and self-URL — surface
/// it on your `R` if you need it (e.g. `#[serde(flatten)] attributes:
/// HashMap<String, Value>`).
///
/// `metadata` is populated only when the caller explicitly requests it
/// (`metadata=LABELS` on the search call). Its shape is flexible enough
/// across versions that we surface it as raw JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult<R> {
    /// Hit records, in Salesforce-defined relevance order.
    #[serde(rename = "searchRecords", default = "Vec::new")]
    pub search_records: Vec<R>,
    /// Field-label metadata, present only when the request asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
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

/// One limit entry from `GET /services/data/vXX.X/limits`.
///
/// Most limits are flat `{Max, Remaining}` pairs. A few (notably
/// `PermissionSets`) embed sub-limits with the same shape — those are
/// captured in [`Limit::nested`].
#[derive(Debug, Clone, Deserialize)]
pub struct Limit {
    /// Maximum allocation for the org.
    #[serde(rename = "Max")]
    pub max: i64,
    /// Remaining allocation, accurate to within five minutes per the docs.
    #[serde(rename = "Remaining")]
    pub remaining: i64,
    /// Sub-limits keyed by name. Empty for flat limits; populated for
    /// composite ones such as `PermissionSets.CreateCustom`.
    #[serde(flatten)]
    pub nested: HashMap<String, Limit>,
}

/// Top-level response from `GET /limits` — keys are limit names.
pub type OrgLimits = HashMap<String, Limit>;

/// Response from `GET /sobjects` (describe global). Schema-independent
/// platform metadata — concrete because every org returns the same shape.
#[derive(Debug, Clone, Deserialize)]
pub struct DescribeGlobal {
    /// Org's character encoding (typically `"UTF-8"`).
    pub encoding: String,
    /// Maximum batch size permitted in queries against this org.
    #[serde(rename = "maxBatchSize")]
    pub max_batch_size: i32,
    /// One entry per object visible to the authenticated user.
    pub sobjects: Vec<SObjectMetadata>,
}

/// Per-object metadata returned in [`DescribeGlobal::sobjects`]. Mirrors the
/// flags Salesforce documents for the describe-global response.
#[derive(Debug, Clone, Deserialize)]
pub struct SObjectMetadata {
    pub activateable: bool,
    pub createable: bool,
    pub custom: bool,
    #[serde(rename = "customSetting")]
    pub custom_setting: bool,
    pub deletable: bool,
    #[serde(rename = "deprecatedAndHidden")]
    pub deprecated_and_hidden: bool,
    #[serde(rename = "feedEnabled")]
    pub feed_enabled: bool,
    /// Three-character record-ID prefix (e.g. `"001"` for Account). `None`
    /// for objects without a stable prefix.
    #[serde(rename = "keyPrefix", default)]
    pub key_prefix: Option<String>,
    pub label: String,
    #[serde(rename = "labelPlural")]
    pub label_plural: String,
    pub layoutable: bool,
    pub mergeable: bool,
    #[serde(rename = "mruEnabled")]
    pub mru_enabled: bool,
    /// API name of the object, e.g. `"Account"`, `"My_Object__c"`.
    pub name: String,
    pub queryable: bool,
    pub replicateable: bool,
    pub retrieveable: bool,
    pub searchable: bool,
    pub triggerable: bool,
    pub undeletable: bool,
    pub updateable: bool,
    /// Map of related URL slugs (`sobject`, `describe`, `rowTemplate`,
    /// plus per-feature URLs that vary by object). Kept as a generic map
    /// because Salesforce adds keys here across API versions.
    #[serde(default)]
    pub urls: HashMap<String, String>,
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

/// Top-level response from `POST /composite/batch`.
///
/// Salesforce always returns HTTP 200 for a well-formed batch even when
/// individual sub-requests fail — per-subrequest failures are surfaced via
/// [`has_errors`](Self::has_errors) and the `statusCode` on each
/// [`BatchSubresult`]. Translating sub-failures into transport errors would
/// drop the partial successes in the same response, so callers inspect
/// results directly.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchResponse {
    /// `true` when at least one sub-request returned a 4xx/5xx status.
    #[serde(rename = "hasErrors")]
    pub has_errors: bool,
    /// One entry per sub-request, in the order submitted.
    #[serde(default = "Vec::new")]
    pub results: Vec<BatchSubresult>,
}

/// One sub-request result inside [`BatchResponse::results`].
///
/// `result` is the body returned by the sub-request — a record on success,
/// `null` for a 204 No Content (e.g. PATCH/DELETE), or a Salesforce error
/// array on failure. Its shape is intentionally untyped because batch
/// sub-requests are heterogeneous.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchSubresult {
    /// HTTP status code returned by this sub-request.
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    /// Sub-request response body, or `Value::Null` for 204 responses.
    #[serde(default)]
    pub result: serde_json::Value,
}

impl BatchSubresult {
    /// `true` if this sub-request succeeded (`status_code` in 200..300).
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status_code)
    }
}

/// Top-level response from `POST /composite/tree/{SObject}`.
///
/// **All-or-nothing semantics.** Unlike [`BatchResponse`], a tree request
/// with any failing record rolls back *every* record in the request — no
/// partial commits. If [`has_errors`](Self::has_errors) is `true`, none of
/// the records in `results` were created; fix the listed errors and resend
/// the entire tree.
///
/// The `results` collection therefore behaves differently in the two cases:
/// on success it contains every record's `referenceId` → `id` mapping; on
/// failure it contains *only* the records whose validation/save errored.
#[derive(Debug, Clone, Deserialize)]
pub struct CompositeTreeResponse {
    /// `true` when the request rolled back due to one or more record errors.
    #[serde(rename = "hasErrors")]
    pub has_errors: bool,
    /// On success: one entry per record with [`CompositeTreeResult::id`]
    /// populated. On failure: only entries for the records that failed,
    /// with [`CompositeTreeResult::errors`] populated instead.
    #[serde(default = "Vec::new")]
    pub results: Vec<CompositeTreeResult>,
}

/// One per-record entry in [`CompositeTreeResponse::results`].
///
/// `id` and `errors` form a soft union — exactly one is populated:
/// - `Some(id)` / `None` on a successful create
/// - `None` / `Some(errors)` on a failure
///
/// Modeled as two `Option`s rather than a tagged enum because the wire
/// shape doesn't carry a discriminator and callers usually inspect by
/// field presence rather than matching variants.
#[derive(Debug, Clone, Deserialize)]
pub struct CompositeTreeResult {
    /// Caller-supplied reference ID echoed back from the request.
    #[serde(rename = "referenceId")]
    pub reference_id: String,
    /// Salesforce ID of the created record. Populated only on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Validation/save errors for this record. Populated only when this
    /// record's create failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<CompositeTreeError>>,
}

impl CompositeTreeResult {
    /// `true` if this record was created (`id` is set, `errors` is not).
    pub fn is_success(&self) -> bool {
        self.id.is_some() && self.errors.is_none()
    }
}

/// Per-record error entry inside [`CompositeTreeResult::errors`].
///
/// Salesforce uses a **different shape** here from the standard REST error
/// array surfaced as [`crate::SalesforceError`]: the field is `statusCode`
/// (a string enum like `"INVALID_EMAIL_ADDRESS"`, not an HTTP code) rather
/// than `errorCode`. Don't try to deserialize this from the standard error
/// array shape and vice versa.
#[derive(Debug, Clone, Deserialize)]
pub struct CompositeTreeError {
    /// String enum identifying the error (e.g. `"INVALID_EMAIL_ADDRESS"`).
    #[serde(rename = "statusCode")]
    pub status_code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Field API names contributing to the error, when applicable.
    #[serde(default)]
    pub fields: Vec<String>,
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
    fn parses_flat_limit() {
        let body = r#"{"Max": 5000, "Remaining": 4937}"#;
        let limit: Limit = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(limit.max, 5000);
        assert_eq!(limit.remaining, 4937);
        assert!(limit.nested.is_empty());
    }

    #[test]
    fn parses_nested_limit() {
        // PermissionSets is the canonical nested case from the docs.
        let body = r#"{
            "Max": 1500,
            "Remaining": 1499,
            "CreateCustom": {"Max": 1000, "Remaining": 999}
        }"#;
        let limit: Limit = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(limit.max, 1500);
        assert_eq!(limit.remaining, 1499);
        assert_eq!(limit.nested.len(), 1);
        let nested = limit.nested.get("CreateCustom").unwrap();
        assert_eq!(nested.max, 1000);
        assert_eq!(nested.remaining, 999);
        assert!(nested.nested.is_empty());
    }

    #[test]
    fn parses_org_limits_envelope() {
        let body = json!({
            "DailyApiRequests": {"Max": 5000, "Remaining": 4937},
            "PermissionSets": {
                "Max": 1500,
                "Remaining": 1499,
                "CreateCustom": {"Max": 1000, "Remaining": 999}
            }
        })
        .to_string();
        let limits: OrgLimits = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(limits.len(), 2);
        assert_eq!(limits.get("DailyApiRequests").unwrap().remaining, 4937);
        assert_eq!(
            limits
                .get("PermissionSets")
                .unwrap()
                .nested
                .get("CreateCustom")
                .unwrap()
                .max,
            1000
        );
    }

    #[test]
    fn parses_describe_global() {
        let body = json!({
            "encoding": "UTF-8",
            "maxBatchSize": 200,
            "sobjects": [{
                "activateable": false,
                "custom": false,
                "customSetting": false,
                "createable": true,
                "deletable": true,
                "deprecatedAndHidden": false,
                "feedEnabled": true,
                "keyPrefix": "001",
                "label": "Account",
                "labelPlural": "Accounts",
                "layoutable": true,
                "mergeable": true,
                "mruEnabled": true,
                "name": "Account",
                "queryable": true,
                "replicateable": true,
                "retrieveable": true,
                "searchable": true,
                "triggerable": true,
                "undeletable": true,
                "updateable": true,
                "urls": {
                    "sobject": "/services/data/v60.0/sobjects/Account",
                    "describe": "/services/data/v60.0/sobjects/Account/describe",
                    "rowTemplate": "/services/data/v60.0/sobjects/Account/{ID}"
                }
            }]
        })
        .to_string();
        let dg: DescribeGlobal = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(dg.encoding, "UTF-8");
        assert_eq!(dg.max_batch_size, 200);
        assert_eq!(dg.sobjects.len(), 1);
        assert_eq!(dg.sobjects[0].name, "Account");
        assert_eq!(dg.sobjects[0].key_prefix.as_deref(), Some("001"));
        assert_eq!(
            dg.sobjects[0].urls.get("describe").map(String::as_str),
            Some("/services/data/v60.0/sobjects/Account/describe")
        );
    }

    #[test]
    fn describe_global_handles_missing_key_prefix() {
        // Some objects (junction objects, certain settings) have no key prefix.
        let body = json!({
            "encoding": "UTF-8",
            "maxBatchSize": 200,
            "sobjects": [{
                "activateable": false, "custom": false, "customSetting": false,
                "createable": false, "deletable": false, "deprecatedAndHidden": false,
                "feedEnabled": false, "label": "Foo", "labelPlural": "Foos",
                "layoutable": false, "mergeable": false, "mruEnabled": false,
                "name": "FooSetting", "queryable": false, "replicateable": false,
                "retrieveable": false, "searchable": false, "triggerable": false,
                "undeletable": false, "updateable": false, "urls": {}
            }]
        })
        .to_string();
        let dg: DescribeGlobal = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(dg.sobjects[0].key_prefix.is_none());
    }

    #[test]
    fn parses_search_result() {
        let body = json!({
            "searchRecords": [
                {
                    "attributes": {
                        "type": "Account",
                        "url": "/services/data/v60.0/sobjects/Account/001xx"
                    },
                    "Id": "001xx"
                },
                {
                    "attributes": {
                        "type": "Contact",
                        "url": "/services/data/v60.0/sobjects/Contact/003yy"
                    },
                    "Id": "003yy"
                }
            ]
        })
        .to_string();
        let sr: SearchResult<Value> = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert_eq!(sr.search_records.len(), 2);
        assert_eq!(sr.search_records[0]["attributes"]["type"], "Account");
        assert_eq!(sr.search_records[1]["Id"], "003yy");
        assert!(sr.metadata.is_none());
    }

    #[test]
    fn parses_search_result_with_metadata() {
        let body = json!({
            "searchRecords": [],
            "metadata": {
                "entityMetadata": [
                    {"entityName": "Account", "fieldMetadata": [
                        {"name": "Name", "label": "Account Name"}
                    ]}
                ]
            }
        })
        .to_string();
        let sr: SearchResult<Value> = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(sr.search_records.is_empty());
        let md = sr.metadata.expect("metadata present");
        assert!(md["entityMetadata"].is_array());
    }

    #[test]
    fn parses_empty_search_result() {
        // No hits at all — searchRecords absent or empty.
        let body = r#"{"searchRecords": []}"#;
        let sr: SearchResult<Value> = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(sr.search_records.is_empty());
    }

    #[test]
    fn parses_batch_response_with_mixed_subresults() {
        // Mirrors the documented example: one PATCH (204 → null result)
        // followed by one GET (200 → record body).
        let body = json!({
            "hasErrors": false,
            "results": [
                {"statusCode": 204, "result": null},
                {"statusCode": 200, "result": {
                    "attributes": {"type": "Account"},
                    "Id": "001D000000K0fXOIAZ",
                    "Name": "NewName"
                }}
            ]
        })
        .to_string();
        let resp: BatchResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(!resp.has_errors);
        assert_eq!(resp.results.len(), 2);
        assert!(resp.results[0].is_success());
        assert_eq!(resp.results[0].status_code, 204);
        assert!(resp.results[0].result.is_null());
        assert!(resp.results[1].is_success());
        assert_eq!(resp.results[1].result["Name"], "NewName");
    }

    #[test]
    fn parses_batch_response_with_subrequest_failure() {
        // hasErrors=true and one of the results carries a Salesforce error
        // array as its body. The transport call still returns 200 — only
        // by inspecting the subresult does the caller learn it failed.
        let body = json!({
            "hasErrors": true,
            "results": [
                {"statusCode": 200, "result": {"Id": "001"}},
                {"statusCode": 404, "result": [
                    {"message": "Provided external ID field does not exist or is not accessible: bogus__c",
                     "errorCode": "NOT_FOUND"}
                ]}
            ]
        })
        .to_string();
        let resp: BatchResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(resp.has_errors);
        assert!(resp.results[0].is_success());
        assert!(!resp.results[1].is_success());
        assert_eq!(resp.results[1].status_code, 404);
        assert_eq!(resp.results[1].result[0]["errorCode"], "NOT_FOUND");
    }

    #[test]
    fn parses_batch_response_with_default_results_when_absent() {
        // Defensive: schema always returns `results`, but our default keeps
        // us from panicking if a future version omits it.
        let body = r#"{"hasErrors": false}"#;
        let resp: BatchResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(resp.results.is_empty());
    }

    #[test]
    fn parses_composite_tree_success_response() {
        // From the documented success example.
        let body = json!({
            "hasErrors": false,
            "results": [
                {"referenceId": "ref1", "id": "001D000000K0fXOIAZ"},
                {"referenceId": "ref4", "id": "001D000000K0fXPIAZ"},
                {"referenceId": "ref2", "id": "003D000000QV9n2IAD"},
                {"referenceId": "ref3", "id": "003D000000QV9n3IAD"}
            ]
        })
        .to_string();
        let resp: CompositeTreeResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(!resp.has_errors);
        assert_eq!(resp.results.len(), 4);
        assert!(resp.results.iter().all(CompositeTreeResult::is_success));
        assert_eq!(resp.results[0].reference_id, "ref1");
        assert_eq!(resp.results[0].id.as_deref(), Some("001D000000K0fXOIAZ"));
        assert!(resp.results[0].errors.is_none());
    }

    #[test]
    fn parses_composite_tree_failure_response() {
        // From the documented failure example: only the failing referenceId
        // appears, with `errors` populated and `id` absent.
        let body = json!({
            "hasErrors": true,
            "results": [{
                "referenceId": "ref2",
                "errors": [{
                    "statusCode": "INVALID_EMAIL_ADDRESS",
                    "message": "Email: invalid email address: 123",
                    "fields": ["Email"]
                }]
            }]
        })
        .to_string();
        let resp: CompositeTreeResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        assert!(resp.has_errors);
        assert_eq!(resp.results.len(), 1);
        let result = &resp.results[0];
        assert!(!result.is_success());
        assert_eq!(result.reference_id, "ref2");
        assert!(result.id.is_none());
        let errors = result.errors.as_ref().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].status_code, "INVALID_EMAIL_ADDRESS");
        assert_eq!(errors[0].fields, vec!["Email".to_string()]);
    }

    #[test]
    fn parses_composite_tree_error_without_fields() {
        // Some statusCodes (e.g. row-lock contention) carry no `fields` key.
        // Default-empty Vec lets us deserialize either shape.
        let body = json!({
            "hasErrors": true,
            "results": [{
                "referenceId": "ref1",
                "errors": [{
                    "statusCode": "UNABLE_TO_LOCK_ROW",
                    "message": "unable to obtain exclusive access"
                }]
            }]
        })
        .to_string();
        let resp: CompositeTreeResponse = parse_response_bytes(200, body.as_bytes()).unwrap();
        let errors = resp.results[0].errors.as_ref().unwrap();
        assert!(errors[0].fields.is_empty());
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
