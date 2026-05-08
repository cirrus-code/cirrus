//! Composite resources — fan-out primitives that bundle multiple REST
//! sub-requests into a single round trip.
//!
//! Salesforce groups several composite endpoints under
//! `/services/data/{version}/composite/...`. This module hosts the typed
//! handler for them. Currently exposes:
//!
//! - [`CompositeHandler::batch`] — `POST /composite/batch`, up to 25
//!   sub-requests in one call. Sub-requests run serially; the outer call
//!   always returns HTTP 200 for a well-formed batch and per-subrequest
//!   failures surface via [`BatchResponse::has_errors`].
//!
//! Composite tree, composite collections, and the chained `/composite`
//! endpoint are tracked under Phase 2 and will plug into the same
//! [`CompositeHandler`] as additional methods.
//!
//! # Sub-request URL shape
//!
//! Sub-request URLs are *not* instance-rooted and they do *not* go through
//! [`Cloudburst::resolve_url`]. Salesforce dispatches them under the
//! configured `/services/data/` tree, so the expected form is
//! `vXX.X/sobjects/Account/001…` — the API version prefix is mandatory.
//! Each sub-request can target a different version, subject to the rule
//! `v34.0 ≤ subrequest_version ≤ batch_version`.
//!
//! [`Cloudburst::resolve_url`]: crate::Cloudburst

use crate::Cloudburst;
use crate::error::CloudburstResult;
use crate::response::{BatchResponse, CompositeTreeResponse};
use serde::Serialize;

impl Cloudburst {
    /// Returns a handler for the composite REST resources.
    pub fn composite(&self) -> CompositeHandler<'_> {
        CompositeHandler { client: self }
    }
}

/// Handler for `/services/data/{version}/composite/...` resources.
///
/// Returned by [`Cloudburst::composite`].
#[derive(Debug)]
pub struct CompositeHandler<'a> {
    client: &'a Cloudburst,
}

impl CompositeHandler<'_> {
    /// Executes up to 25 sub-requests in a single batch via
    /// `POST /services/data/{api_version}/composite/batch`.
    ///
    /// `body` is any [`Serialize`] value matching Salesforce's documented
    /// shape — typically a [`BatchRequest`] from this crate, or a hand-rolled
    /// `serde_json::json!({...})`. Sub-request URLs must include their own
    /// API version prefix (e.g. `"v60.0/sobjects/Account/001…"`).
    ///
    /// Sub-requests execute serially in submission order. A sub-request that
    /// fails does *not* roll back commits made by earlier sub-requests; the
    /// caller is responsible for compensating logic. Set
    /// [`BatchRequest::halt_on_error`] to stop processing after the first
    /// failure (Salesforce returns HTTP 412 with `BATCH_PROCESSING_HALTED`
    /// for the remaining sub-requests).
    ///
    /// ```ignore
    /// use cloudburst_sdk::{BatchRequest, BatchSubrequest};
    /// use serde_json::json;
    ///
    /// let req = BatchRequest {
    ///     batch_requests: vec![
    ///         BatchSubrequest {
    ///             method: "GET".into(),
    ///             url: "v60.0/limits".into(),
    ///             rich_input: None,
    ///         },
    ///         BatchSubrequest {
    ///             method: "PATCH".into(),
    ///             url: "v60.0/sobjects/Account/001xx".into(),
    ///             rich_input: Some(json!({"Name": "Acme"})),
    ///         },
    ///     ],
    ///     halt_on_error: Some(true),
    /// };
    /// let resp = sf.composite().batch(&req).await?;
    /// if resp.has_errors {
    ///     for sub in &resp.results {
    ///         if !sub.is_success() {
    ///             eprintln!("subrequest failed: {} {}", sub.status_code, sub.result);
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn batch<B>(&self, body: &B) -> CloudburstResult<BatchResponse>
    where
        B: Serialize + ?Sized,
    {
        self.client.post("composite/batch", body).await
    }

    /// Creates a tree of nested records in one round trip via
    /// `POST /services/data/{api_version}/composite/tree/{sobject}`.
    ///
    /// `sobject` is the **root** record type (e.g. `"Account"`) — child
    /// types are inferred from each record's `attributes.type`. The body is
    /// any [`Serialize`] value matching the documented envelope:
    ///
    /// ```ignore
    /// use serde_json::json;
    ///
    /// let body = json!({
    ///     "records": [{
    ///         "attributes": {"type": "Account", "referenceId": "ref1"},
    ///         "Name": "Acme",
    ///         "Contacts": {
    ///             "records": [{
    ///                 "attributes": {"type": "Contact", "referenceId": "ref2"},
    ///                 "LastName": "Doe"
    ///             }]
    ///         }
    ///     }]
    /// });
    /// let resp = sf.composite().tree("Account", &body).await?;
    /// for r in &resp.results {
    ///     if let Some(id) = &r.id {
    ///         println!("{} -> {}", r.reference_id, id);
    ///     }
    /// }
    /// ```
    ///
    /// **Limits:** up to 200 records total across all trees, up to 5
    /// distinct sObject types, and up to 5 levels of nesting.
    ///
    /// **All-or-nothing:** if any record fails validation/save, the entire
    /// request rolls back and [`CompositeTreeResponse::has_errors`] is
    /// `true`. The `results` collection in that case lists only the
    /// failing records' `referenceId`s — *no* records were committed.
    pub async fn tree<B>(
        &self,
        sobject: &str,
        body: &B,
    ) -> CloudburstResult<CompositeTreeResponse>
    where
        B: Serialize + ?Sized,
    {
        let url = self
            .client
            .versioned_segments(&["composite", "tree", sobject])?;
        self.client
            .send_at(reqwest::Method::POST, &url, None::<&()>, Some(body))
            .await
    }
}

/// Request body for [`CompositeHandler::batch`].
///
/// Use this when you want a typed builder for the batch payload. Equivalent
/// to a `serde_json::json!({ "batchRequests": [...], "haltOnError": ... })`
/// literal — both flow through [`CompositeHandler::batch`]'s generic
/// `Serialize` bound.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchRequest {
    /// Sub-requests to execute (max 25).
    #[serde(rename = "batchRequests")]
    pub batch_requests: Vec<BatchSubrequest>,
    /// When `Some(true)`, Salesforce halts after the first 4xx/5xx
    /// sub-response and returns 412 `BATCH_PROCESSING_HALTED` for the
    /// remainder. Defaults to `false` server-side; we omit the field
    /// entirely when `None` so the wire shape matches the documented
    /// example exactly.
    #[serde(rename = "haltOnError", skip_serializing_if = "Option::is_none")]
    pub halt_on_error: Option<bool>,
}

/// One entry in [`BatchRequest::batch_requests`].
///
/// `url` is the sub-request target as Salesforce dispatches it under
/// `/services/data/`. It must include the API version prefix
/// (e.g. `"v60.0/sobjects/Account/001…"`); sub-request URLs do *not* go
/// through the SDK's normal path resolution.
///
/// `binaryPartName` / `binaryPartNameAlias` (used for multipart blob
/// uploads) are intentionally not exposed here. If you need them, drop
/// down to a `serde_json::json!({...})` body — the multipart transport
/// itself isn't supported yet.
#[derive(Debug, Clone, Serialize)]
pub struct BatchSubrequest {
    /// HTTP method, e.g. `"GET"`, `"POST"`, `"PATCH"`, `"DELETE"`.
    pub method: String,
    /// Sub-request URL, including the API version prefix.
    pub url: String,
    /// Request body for the sub-request. `None` for verbs that don't carry
    /// a body (GET, DELETE).
    #[serde(rename = "richInput", skip_serializing_if = "Option::is_none")]
    pub rich_input: Option<serde_json::Value>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Cloudburst;
    use crate::auth::StaticTokenAuth;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(uri: String) -> Cloudburst {
        let auth = Arc::new(StaticTokenAuth::new("tok", uri));
        Cloudburst::builder().auth(auth).build().unwrap()
    }

    #[tokio::test]
    async fn batch_posts_and_parses_mixed_subresults() {
        let server = MockServer::start().await;

        let request_body = json!({
            "batchRequests": [
                {
                    "method": "PATCH",
                    "url": "v60.0/sobjects/Account/001xx",
                    "richInput": {"Name": "NewName"}
                },
                {
                    "method": "GET",
                    "url": "v60.0/sobjects/Account/001xx?fields=Name"
                }
            ]
        });

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(request_body.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasErrors": false,
                "results": [
                    {"statusCode": 204, "result": null},
                    {"statusCode": 200, "result": {
                        "attributes": {"type": "Account"},
                        "Name": "NewName",
                        "Id": "001xx"
                    }}
                ]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let resp = sf.composite().batch(&request_body).await.unwrap();
        assert!(!resp.has_errors);
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].status_code, 204);
        assert!(resp.results[0].result.is_null());
        assert_eq!(resp.results[1].result["Name"], "NewName");
    }

    #[tokio::test]
    async fn batch_typed_request_matches_documented_wire_shape() {
        // Construct via the typed BatchRequest/BatchSubrequest structs and
        // assert the serialized body matches the JSON example from the
        // Salesforce Batch Request Body docs.
        let server = MockServer::start().await;

        let expected_body = json!({
            "batchRequests": [
                {
                    "method": "PATCH",
                    "url": "v60.0/sobjects/account/001D000000K0fXOIAZ",
                    "richInput": {"Name": "NewName"}
                },
                {
                    "method": "GET",
                    "url": "v60.0/sobjects/account/001D000000K0fXOIAZ?fields=Name,BillingPostalCode"
                }
            ]
        });

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .and(body_json(expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasErrors": false,
                "results": []
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let req = BatchRequest {
            batch_requests: vec![
                BatchSubrequest {
                    method: "PATCH".into(),
                    url: "v60.0/sobjects/account/001D000000K0fXOIAZ".into(),
                    rich_input: Some(json!({"Name": "NewName"})),
                },
                BatchSubrequest {
                    method: "GET".into(),
                    url: "v60.0/sobjects/account/001D000000K0fXOIAZ?fields=Name,BillingPostalCode"
                        .into(),
                    rich_input: None,
                },
            ],
            halt_on_error: None,
        };
        let resp = sf.composite().batch(&req).await.unwrap();
        assert!(!resp.has_errors);
    }

    #[tokio::test]
    async fn batch_serializes_halt_on_error_when_set() {
        let server = MockServer::start().await;

        // haltOnError is present and `true` in the body.
        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .and(body_json(json!({
                "batchRequests": [
                    {"method": "GET", "url": "v60.0/limits"}
                ],
                "haltOnError": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasErrors": false,
                "results": [{"statusCode": 200, "result": {}}]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let req = BatchRequest {
            batch_requests: vec![BatchSubrequest {
                method: "GET".into(),
                url: "v60.0/limits".into(),
                rich_input: None,
            }],
            halt_on_error: Some(true),
        };
        sf.composite().batch(&req).await.unwrap();
    }

    #[tokio::test]
    async fn batch_omits_halt_on_error_when_none() {
        // None must be skipped entirely — not serialized as null. Any
        // request body with a `haltOnError` key would not match.
        let server = MockServer::start().await;

        let body_without_halt = json!({
            "batchRequests": [
                {"method": "GET", "url": "v60.0/limits"}
            ]
        });

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .and(body_json(body_without_halt))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasErrors": false,
                "results": []
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let req = BatchRequest {
            batch_requests: vec![BatchSubrequest {
                method: "GET".into(),
                url: "v60.0/limits".into(),
                rich_input: None,
            }],
            halt_on_error: None,
        };
        sf.composite().batch(&req).await.unwrap();
    }

    #[tokio::test]
    async fn batch_surfaces_subrequest_errors_via_has_errors() {
        // Outer call returns 200 even though a sub-request failed —
        // hasErrors=true and the failed result carries a SF error array.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasErrors": true,
                "results": [
                    {"statusCode": 200, "result": {"Id": "001xx"}},
                    {"statusCode": 400, "result": [
                        {"message": "Required fields are missing: [Name]",
                         "errorCode": "REQUIRED_FIELD_MISSING",
                         "fields": ["Name"]}
                    ]}
                ]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let resp = sf
            .composite()
            .batch(&json!({"batchRequests": []}))
            .await
            .unwrap();
        assert!(resp.has_errors);
        assert!(resp.results[0].is_success());
        assert!(!resp.results[1].is_success());
        assert_eq!(resp.results[1].status_code, 400);
        assert_eq!(resp.results[1].result[0]["errorCode"], "REQUIRED_FIELD_MISSING");
    }

    #[tokio::test]
    async fn tree_creates_nested_records_and_returns_id_map() {
        let server = MockServer::start().await;

        let request_body = json!({
            "records": [{
                "attributes": {"type": "Account", "referenceId": "ref1"},
                "Name": "Acme",
                "Contacts": {
                    "records": [{
                        "attributes": {"type": "Contact", "referenceId": "ref2"},
                        "LastName": "Smith"
                    }, {
                        "attributes": {"type": "Contact", "referenceId": "ref3"},
                        "LastName": "Evans"
                    }]
                }
            }]
        });

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/tree/Account"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(request_body.clone()))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "hasErrors": false,
                "results": [
                    {"referenceId": "ref1", "id": "001D000000K0fXOIAZ"},
                    {"referenceId": "ref2", "id": "003D000000QV9n2IAD"},
                    {"referenceId": "ref3", "id": "003D000000QV9n3IAD"}
                ]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let resp = sf
            .composite()
            .tree("Account", &request_body)
            .await
            .unwrap();
        assert!(!resp.has_errors);
        assert_eq!(resp.results.len(), 3);
        assert_eq!(resp.results[0].reference_id, "ref1");
        assert_eq!(resp.results[0].id.as_deref(), Some("001D000000K0fXOIAZ"));
        assert!(resp.results.iter().all(|r| r.is_success()));
    }

    #[tokio::test]
    async fn tree_surfaces_per_record_failure() {
        // All-or-nothing — has_errors=true, only the failing referenceId
        // appears in results. The transport call returns 201 (yes, even
        // for a rolled-back request — the docs document this).
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/tree/Account"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "hasErrors": true,
                "results": [{
                    "referenceId": "ref2",
                    "errors": [{
                        "statusCode": "INVALID_EMAIL_ADDRESS",
                        "message": "Email: invalid email address: 123",
                        "fields": ["Email"]
                    }]
                }]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let resp = sf
            .composite()
            .tree("Account", &json!({"records": []}))
            .await
            .unwrap();
        assert!(resp.has_errors);
        let result = &resp.results[0];
        assert!(!result.is_success());
        assert!(result.id.is_none());
        let errors = result.errors.as_ref().unwrap();
        assert_eq!(errors[0].status_code, "INVALID_EMAIL_ADDRESS");
        assert_eq!(errors[0].fields, vec!["Email".to_string()]);
    }

    #[tokio::test]
    async fn tree_percent_encodes_sobject_name() {
        // Custom-object names are alphanumeric + underscores in practice,
        // but versioned_segments encodes any reserved character we hand it
        // — covers the unlikely case of a dev sandbox name with a space.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/tree/My_Custom__c"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "hasErrors": false,
                "results": []
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let resp = sf
            .composite()
            .tree("My_Custom__c", &json!({"records": []}))
            .await
            .unwrap();
        assert!(!resp.has_errors);
    }

    #[tokio::test]
    async fn tree_top_level_400_surfaces_as_api_error() {
        // Malformed top-level body (e.g. invalid attributes) — the batch
        // endpoint itself returns 400, parsed as our standard Api error.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/tree/Account"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
                "message": "Invalid request: missing 'records'",
                "errorCode": "INVALID_REQUEST"
            }])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let err = sf
            .composite()
            .tree("Account", &json!({}))
            .await
            .unwrap_err();
        match err {
            crate::CloudburstError::Api { status, errors, .. } => {
                assert_eq!(status, 400);
                assert_eq!(errors[0].error_code, "INVALID_REQUEST");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn batch_top_level_400_surfaces_as_api_error() {
        // A top-level deserialization error (per docs: invalid method/url
        // in a sub-request) returns HTTP 400 from the batch endpoint
        // itself, *not* the per-subrequest path. Our normal Api error
        // surfacing applies.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/composite/batch"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
                "message": "Invalid HTTP method: BOGUS",
                "errorCode": "JSON_PARSER_ERROR"
            }])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let err = sf
            .composite()
            .batch(&json!({"batchRequests": [{"method": "BOGUS", "url": "v60.0/limits"}]}))
            .await
            .unwrap_err();
        match err {
            crate::CloudburstError::Api { status, errors, .. } => {
                assert_eq!(status, 400);
                assert_eq!(errors[0].error_code, "JSON_PARSER_ERROR");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
