//! Apex REST passthrough — `/services/apexrest/{path}`.
//!
//! Apex REST lets Salesforce admins/developers expose custom Apex classes as
//! REST endpoints by annotating them with `@RestResource(urlMapping='...')`.
//! The wire shape (request body, response body) is entirely defined by the
//! developer who wrote the Apex class — Salesforce only provides the
//! transport, auth, and (on non-2xx) the standard `[{message, errorCode}]`
//! error array.
//!
//! # Why a handler instead of just the escape hatch?
//!
//! The escape hatch already covers Apex REST today:
//!
//! ```ignore
//! sf.get::<MyResp>("/services/apexrest/MyEndpoint").await?;
//! ```
//!
//! This handler exists for **discoverability and ergonomics**:
//!
//! - Callers who don't know the URL convention can find Apex support via
//!   [`Cloudburst::apex`].
//! - The `/services/apexrest/` prefix is added automatically — pass just
//!   the Apex `urlMapping` (e.g. `"MyEndpoint"` or `"/MyEndpoint/123"`).
//!
//! Behavior is otherwise identical to the matching method on
//! [`Cloudburst`]; calls flow through the same auth, retry, and error
//! parsing as any other request.
//!
//! # Path encoding
//!
//! The handler does **not** percent-encode path segments — it just
//! prepends `/services/apexrest/` (stripping a single leading slash if
//! present) and lets the path through. For paths containing reserved
//! characters (spaces, `?`, `#`, `&`), pre-encode the segments yourself
//! before calling. This matches the existing
//! [`Cloudburst::resolve_url`]'s leading-`/` mode behavior.
//!
//! [`Cloudburst::apex`]: crate::Cloudburst::apex
//! [`Cloudburst::resolve_url`]: crate::Cloudburst

use crate::Cloudburst;
use crate::error::CloudburstResult;
use serde::Serialize;
use serde::de::DeserializeOwned;

impl Cloudburst {
    /// Returns a handler for Apex REST endpoints exposed under
    /// `/services/apexrest/`.
    pub fn apex(&self) -> ApexHandler<'_> {
        ApexHandler { client: self }
    }
}

/// Handler for `/services/apexrest/{path}` endpoints.
///
/// Each method takes the Apex `urlMapping` (with or without a leading
/// slash) and forwards through the corresponding [`Cloudburst`] verb.
/// Body and response types are caller-defined since Apex REST endpoints
/// have no platform-defined wire shape.
#[derive(Debug)]
pub struct ApexHandler<'a> {
    client: &'a Cloudburst,
}

impl ApexHandler<'_> {
    /// `GET /services/apexrest/{path}`.
    pub async fn get<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        self.client.get(&apex_path(path)).await
    }

    /// `GET /services/apexrest/{path}` with a query string. `query` is
    /// any [`Serialize`] value — typically `&[("key", "value")]` or a
    /// struct.
    pub async fn get_with_query<R, Q>(&self, path: &str, query: &Q) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        self.client.get_with_query(&apex_path(path), query).await
    }

    /// `POST /services/apexrest/{path}` with a JSON body.
    pub async fn post<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.client.post(&apex_path(path), body).await
    }

    /// `PUT /services/apexrest/{path}` with a JSON body.
    pub async fn put<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.client.put(&apex_path(path), body).await
    }

    /// `PATCH /services/apexrest/{path}` with a JSON body.
    pub async fn patch<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.client.patch(&apex_path(path), body).await
    }

    /// `DELETE /services/apexrest/{path}`.
    pub async fn delete<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        self.client.delete(&apex_path(path)).await
    }
}

/// Normalizes an Apex REST path into an instance-rooted form.
///
/// - `"MyEndpoint"` → `"/services/apexrest/MyEndpoint"`
/// - `"/MyEndpoint"` → `"/services/apexrest/MyEndpoint"`
/// - `"MyEndpoint/sub/123"` → `"/services/apexrest/MyEndpoint/sub/123"`
///
/// The leading `/` triggers [`Cloudburst::resolve_url`]'s instance-rooted
/// branch, bypassing the versioned `/services/data/{version}/` prefix.
fn apex_path(path: &str) -> String {
    format!("/services/apexrest/{}", path.trim_start_matches('/'))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::auth::StaticTokenAuth;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(uri: String) -> Cloudburst {
        let auth = Arc::new(StaticTokenAuth::new("tok", uri));
        Cloudburst::builder().auth(auth).build().unwrap()
    }

    #[test]
    fn apex_path_normalizes_relative_input() {
        assert_eq!(apex_path("MyEndpoint"), "/services/apexrest/MyEndpoint");
    }

    #[test]
    fn apex_path_normalizes_leading_slash_input() {
        assert_eq!(apex_path("/MyEndpoint"), "/services/apexrest/MyEndpoint");
    }

    #[test]
    fn apex_path_preserves_subpaths() {
        assert_eq!(
            apex_path("Cases/12345/comments"),
            "/services/apexrest/Cases/12345/comments"
        );
        assert_eq!(
            apex_path("/Cases/12345/comments"),
            "/services/apexrest/Cases/12345/comments"
        );
    }

    #[tokio::test]
    async fn apex_get_targets_apexrest_path_without_version() {
        let server = MockServer::start().await;

        // Note the URL: NO /services/data/v60.0/ prefix. Apex REST lives
        // outside the versioned tree.
        Mock::given(method("GET"))
            .and(path("/services/apexrest/MyEndpoint"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v: serde_json::Value = sf.apex().get("MyEndpoint").await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn apex_get_accepts_leading_slash_input() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/apexrest/MyEndpoint"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"hit": true})))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v: serde_json::Value = sf.apex().get("/MyEndpoint").await.unwrap();
        assert_eq!(v["hit"], true);
    }

    #[tokio::test]
    async fn apex_get_with_query_passes_params() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/apexrest/Cases"))
            .and(query_param("status", "Open"))
            .and(query_param("limit", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": "500xx"}])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v: serde_json::Value = sf
            .apex()
            .get_with_query("Cases", &[("status", "Open"), ("limit", "10")])
            .await
            .unwrap();
        assert_eq!(v[0]["id"], "500xx");
    }

    #[tokio::test]
    async fn apex_get_typed_response_deserializes_caller_struct() {
        // Demonstrates the entire reason this handler is generic over R:
        // the Apex developer defined CaseSummary, not Salesforce.
        #[derive(serde::Deserialize)]
        struct CaseSummary {
            #[serde(rename = "Id")]
            id: String,
            #[serde(rename = "Subject")]
            subject: String,
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/services/apexrest/Cases/500xx"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Id": "500xx", "Subject": "Login issue"})),
            )
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let case: CaseSummary = sf.apex().get("Cases/500xx").await.unwrap();
        assert_eq!(case.id, "500xx");
        assert_eq!(case.subject, "Login issue");
    }

    #[tokio::test]
    async fn apex_post_sends_json_body() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/apexrest/Cases"))
            .and(body_json(json!({
                "subject": "Login issue",
                "priority": "High"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "500xx"})))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v: serde_json::Value = sf
            .apex()
            .post(
                "Cases",
                &json!({"subject": "Login issue", "priority": "High"}),
            )
            .await
            .unwrap();
        assert_eq!(v["id"], "500xx");
    }

    #[tokio::test]
    async fn apex_put_sends_json_body() {
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/services/apexrest/Settings/SomeKey"))
            .and(body_json(json!({"value": "new"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"updated": true})))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v: serde_json::Value = sf
            .apex()
            .put("Settings/SomeKey", &json!({"value": "new"}))
            .await
            .unwrap();
        assert_eq!(v["updated"], true);
    }

    #[tokio::test]
    async fn apex_patch_handles_204_no_content() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path("/services/apexrest/Cases/500xx"))
            .and(body_json(json!({"status": "Closed"})))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        sf.apex()
            .patch::<(), _>("Cases/500xx", &json!({"status": "Closed"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apex_delete_handles_204_no_content() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path("/services/apexrest/Cases/500xx"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        sf.apex().delete::<()>("Cases/500xx").await.unwrap();
    }

    #[tokio::test]
    async fn apex_surfaces_standard_salesforce_error_array() {
        // Even though the body shape is dev-defined, the platform error
        // shape is still the standard [{message, errorCode}] array.
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/apexrest/Missing"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!([{
                "message": "Could not find a match for URL /Missing",
                "errorCode": "NOT_FOUND"
            }])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let err = sf
            .apex()
            .get::<serde_json::Value>("Missing")
            .await
            .unwrap_err();
        match err {
            crate::CloudburstError::Api { status, errors, .. } => {
                assert_eq!(status, 404);
                assert_eq!(errors[0].error_code, "NOT_FOUND");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apex_handles_nested_subpath_with_record_id() {
        // A typical Apex REST pattern: urlMapping='/Cases/*', the trailing
        // segment is a record ID parsed by the Apex code.
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/apexrest/Cases/500xx0000000001/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"author": "ryf", "text": "first"},
                {"author": "other", "text": "second"}
            ])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let comments: serde_json::Value = sf
            .apex()
            .get("/Cases/500xx0000000001/comments")
            .await
            .unwrap();
        assert_eq!(comments.as_array().unwrap().len(), 2);
        assert_eq!(comments[0]["author"], "ryf");
    }
}
