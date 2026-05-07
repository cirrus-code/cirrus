//! sObject resources — describe global, per-object describe, and CRUD.
//!
//! Two handler structs gate the surface:
//!
//! - [`SObjectsHandler`] (from [`Cloudburst::sobjects`]): collection-level
//!   operations that don't target a specific object — currently
//!   [`describe_global`].
//! - [`SObjectHandler`] (from [`Cloudburst::sobject`]): per-object
//!   operations — describe metadata, retrieve, create, update, delete,
//!   upsert. Generic over caller-supplied record types: every method that
//!   produces a record returns `serde_json::Value` by default, with an
//!   `_as::<T>()` variant for typed deserialization.
//!
//! [`describe_global`]: SObjectsHandler::describe_global
//! [`Cloudburst::sobjects`]: crate::Cloudburst::sobjects
//! [`Cloudburst::sobject`]: crate::Cloudburst::sobject

use crate::Cloudburst;
use crate::error::CloudburstResult;
use crate::response::{DescribeGlobal, SObjectCreateResult};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

impl Cloudburst {
    /// Returns a handler for collection-level sObject operations
    /// (currently just describe global).
    pub fn sobjects(&self) -> SObjectsHandler<'_> {
        SObjectsHandler { client: self }
    }

    /// Returns a handler scoped to a single sObject by API name (e.g.
    /// `"Account"`, `"My_Object__c"`).
    pub fn sobject<'a>(&'a self, name: &'a str) -> SObjectHandler<'a> {
        SObjectHandler { client: self, name }
    }
}

/// Collection-level sObject handler. Returned by [`Cloudburst::sobjects`].
#[derive(Debug)]
pub struct SObjectsHandler<'a> {
    client: &'a Cloudburst,
}

impl SObjectsHandler<'_> {
    /// Describes every object visible to the authenticated user.
    ///
    /// Calls `GET /services/data/{api_version}/sobjects/`.
    pub async fn describe_global(&self) -> CloudburstResult<DescribeGlobal> {
        self.client.get("sobjects").await
    }
}

/// Per-object handler. Returned by [`Cloudburst::sobject`].
#[derive(Debug)]
pub struct SObjectHandler<'a> {
    client: &'a Cloudburst,
    name: &'a str,
}

impl<'a> SObjectHandler<'a> {
    /// API name of the targeted object.
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// Describes the object's metadata (fields, child relationships,
    /// record-type info, etc.). Returns the raw JSON.
    ///
    /// Calls `GET /services/data/{api_version}/sobjects/{name}/describe`.
    pub async fn describe(&self) -> CloudburstResult<Value> {
        self.describe_as().await
    }

    /// Typed variant of [`describe`](Self::describe). Supply your own
    /// struct to model a subset of the (very large) describe response.
    pub async fn describe_as<R: DeserializeOwned>(&self) -> CloudburstResult<R> {
        let url = self
            .client
            .versioned_segments(&["sobjects", self.name, "describe"])?;
        self.client
            .send_at(reqwest::Method::GET, &url, None::<&()>, None::<&()>)
            .await
    }

    /// Retrieves a record by ID, returning every field. For a subset of
    /// fields use [`retrieve_with_fields`](Self::retrieve_with_fields).
    ///
    /// Calls `GET /services/data/{api_version}/sobjects/{name}/{id}`.
    pub async fn retrieve(&self, id: &str) -> CloudburstResult<Value> {
        self.retrieve_as(id).await
    }

    /// Typed variant of [`retrieve`](Self::retrieve).
    pub async fn retrieve_as<R: DeserializeOwned>(&self, id: &str) -> CloudburstResult<R> {
        let url = self
            .client
            .versioned_segments(&["sobjects", self.name, id])?;
        self.client
            .send_at(reqwest::Method::GET, &url, None::<&()>, None::<&()>)
            .await
    }

    /// Retrieves selected fields of a record by ID.
    ///
    /// Calls `GET /sobjects/{name}/{id}?fields=Field1,Field2,...`.
    pub async fn retrieve_with_fields(&self, id: &str, fields: &[&str]) -> CloudburstResult<Value> {
        self.retrieve_with_fields_as(id, fields).await
    }

    /// Typed variant of
    /// [`retrieve_with_fields`](Self::retrieve_with_fields).
    pub async fn retrieve_with_fields_as<R: DeserializeOwned>(
        &self,
        id: &str,
        fields: &[&str],
    ) -> CloudburstResult<R> {
        let url = self
            .client
            .versioned_segments(&["sobjects", self.name, id])?;
        let joined = fields.join(",");
        let query = [("fields", joined.as_str())];
        self.client
            .send_at(reqwest::Method::GET, &url, Some(&query), None::<&()>)
            .await
    }

    /// Creates a new record of this object.
    ///
    /// Calls `POST /services/data/{api_version}/sobjects/{name}/`. The body
    /// is serialized as JSON — any `Serialize` value works (typed structs,
    /// `serde_json::json!({...})`, `HashMap<String, Value>`).
    pub async fn create<B>(&self, body: &B) -> CloudburstResult<SObjectCreateResult>
    where
        B: Serialize + ?Sized,
    {
        let url = self.client.versioned_segments(&["sobjects", self.name])?;
        self.client
            .send_at(reqwest::Method::POST, &url, None::<&()>, Some(body))
            .await
    }

    /// Updates a record by ID. Field values in `body` replace the
    /// existing values; fields not present in `body` are left alone.
    ///
    /// Calls `PATCH /services/data/{api_version}/sobjects/{name}/{id}`.
    /// Salesforce returns 204 No Content on success.
    pub async fn update<B>(&self, id: &str, body: &B) -> CloudburstResult<()>
    where
        B: Serialize + ?Sized,
    {
        let url = self
            .client
            .versioned_segments(&["sobjects", self.name, id])?;
        self.client
            .send_at::<(), (), B>(reqwest::Method::PATCH, &url, None, Some(body))
            .await
    }

    /// Deletes a record by ID.
    ///
    /// Calls `DELETE /services/data/{api_version}/sobjects/{name}/{id}`.
    /// Salesforce returns 204 No Content on success.
    pub async fn delete(&self, id: &str) -> CloudburstResult<()> {
        let url = self
            .client
            .versioned_segments(&["sobjects", self.name, id])?;
        self.client
            .send_at::<(), (), ()>(reqwest::Method::DELETE, &url, None, None)
            .await
    }

    /// Upserts a record by external ID. If a record matching
    /// `external_value` exists, it's updated; otherwise a new record is
    /// created. The `created` flag on the returned
    /// [`SObjectCreateResult`] distinguishes the two outcomes.
    ///
    /// Calls
    /// `PATCH /services/data/{api_version}/sobjects/{name}/{external_field}/{external_value}`.
    /// `external_value` is percent-encoded, so values containing `/`,
    /// `=`, or other reserved characters are passed safely.
    ///
    /// If multiple records match the external ID, Salesforce returns 300
    /// — surfaced as [`crate::CloudburstError::Api`].
    pub async fn upsert<B>(
        &self,
        external_field: &str,
        external_value: &str,
        body: &B,
    ) -> CloudburstResult<SObjectCreateResult>
    where
        B: Serialize + ?Sized,
    {
        let url = self.client.versioned_segments(&[
            "sobjects",
            self.name,
            external_field,
            external_value,
        ])?;
        self.client
            .send_at(reqwest::Method::PATCH, &url, None::<&()>, Some(body))
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::Cloudburst;
    use crate::auth::StaticTokenAuth;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{body_json, header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(uri: String) -> Cloudburst {
        let auth = Arc::new(StaticTokenAuth::new("tok", uri));
        Cloudburst::builder().auth(auth).build().unwrap()
    }

    #[tokio::test]
    async fn describe_global_returns_envelope() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data/v60.0/sobjects"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "encoding": "UTF-8",
                "maxBatchSize": 200,
                "sobjects": [{
                    "activateable": false, "custom": false, "customSetting": false,
                    "createable": true, "deletable": true, "deprecatedAndHidden": false,
                    "feedEnabled": true, "keyPrefix": "001",
                    "label": "Account", "labelPlural": "Accounts",
                    "layoutable": true, "mergeable": true, "mruEnabled": true,
                    "name": "Account", "queryable": true, "replicateable": true,
                    "retrieveable": true, "searchable": true, "triggerable": true,
                    "undeletable": true, "updateable": true,
                    "urls": {
                        "sobject": "/services/data/v60.0/sobjects/Account",
                        "describe": "/services/data/v60.0/sobjects/Account/describe",
                        "rowTemplate": "/services/data/v60.0/sobjects/Account/{ID}"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let dg = sf.sobjects().describe_global().await.unwrap();
        assert_eq!(dg.encoding, "UTF-8");
        assert_eq!(dg.max_batch_size, 200);
        assert_eq!(dg.sobjects.len(), 1);
        assert_eq!(dg.sobjects[0].name, "Account");
    }

    #[tokio::test]
    async fn describe_returns_value() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data/v60.0/sobjects/Account/describe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "Account",
                "label": "Account",
                "fields": []
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v = sf.sobject("Account").describe().await.unwrap();
        assert_eq!(v["name"], "Account");
    }

    #[tokio::test]
    async fn retrieve_full_record() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/001xx0000000001",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Id": "001xx0000000001",
                "Name": "Acme",
                "Industry": "Tech"
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v = sf
            .sobject("Account")
            .retrieve("001xx0000000001")
            .await
            .unwrap();
        assert_eq!(v["Name"], "Acme");
    }

    #[tokio::test]
    async fn retrieve_with_fields_sets_query() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/001xx0000000001",
            ))
            .and(query_param("fields", "Name,Industry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Name": "Acme",
                "Industry": "Tech"
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let v = sf
            .sobject("Account")
            .retrieve_with_fields("001xx0000000001", &["Name", "Industry"])
            .await
            .unwrap();
        assert_eq!(v["Name"], "Acme");
        assert_eq!(v["Industry"], "Tech");
    }

    #[tokio::test]
    async fn create_posts_body_and_returns_id() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/sobjects/Account"))
            .and(body_json(json!({"Name": "Acme"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "001xx0000000001",
                "success": true,
                "errors": []
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let result = sf
            .sobject("Account")
            .create(&json!({"Name": "Acme"}))
            .await
            .unwrap();
        assert_eq!(result.id, "001xx0000000001");
        assert!(result.success);
    }

    #[tokio::test]
    async fn update_sends_patch_and_handles_204() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/001xx0000000001",
            ))
            .and(body_json(json!({"Industry": "Biotech"})))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        sf.sobject("Account")
            .update("001xx0000000001", &json!({"Industry": "Biotech"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_sends_delete_and_handles_204() {
        let server = MockServer::start().await;

        Mock::given(method("DELETE"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/001xx0000000001",
            ))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        sf.sobject("Account")
            .delete("001xx0000000001")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn upsert_patches_to_external_id_path() {
        let server = MockServer::start().await;

        Mock::given(method("PATCH"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/External_Id__c/EXT-1",
            ))
            .and(body_json(json!({"Name": "Acme"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "001xx0000000001",
                "success": true,
                "errors": [],
                "created": true
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let result = sf
            .sobject("Account")
            .upsert("External_Id__c", "EXT-1", &json!({"Name": "Acme"}))
            .await
            .unwrap();
        assert_eq!(result.id, "001xx0000000001");
        assert_eq!(result.created, Some(true));
    }

    #[tokio::test]
    async fn upsert_percent_encodes_external_value() {
        // External-ID value contains characters that MUST be percent-encoded
        // in a URL path segment: '/', '=', '#', and a space.
        let server = MockServer::start().await;

        // wiremock's `path` matcher works on the decoded path, so we assert
        // the literal value is what arrives at the server.
        Mock::given(method("PATCH"))
            .and(path_regex(
                r"^/services/data/v60\.0/sobjects/Account/External_Id__c/.+$",
            ))
            .and(body_json(json!({"Name": "Edge"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "001xx0000000002",
                "success": true,
                "errors": [],
                "created": false
            })))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let result = sf
            .sobject("Account")
            .upsert("External_Id__c", "a/b=c d", &json!({"Name": "Edge"}))
            .await
            .unwrap();
        assert_eq!(result.created, Some(false));
    }

    #[tokio::test]
    async fn retrieve_typed() {
        #[derive(serde::Deserialize)]
        struct Account {
            #[serde(rename = "Name")]
            name: String,
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/services/data/v60.0/sobjects/Account/001xx0000000001",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Name": "Acme"})))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let acct: Account = sf
            .sobject("Account")
            .retrieve_as("001xx0000000001")
            .await
            .unwrap();
        assert_eq!(acct.name, "Acme");
    }

    #[tokio::test]
    async fn create_surfaces_validation_errors() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/data/v60.0/sobjects/Account"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
                "message": "Required fields are missing: [Name]",
                "errorCode": "REQUIRED_FIELD_MISSING",
                "fields": ["Name"]
            }])))
            .mount(&server)
            .await;

        let sf = fixture(server.uri());
        let err = sf.sobject("Account").create(&json!({})).await.unwrap_err();
        match err {
            crate::CloudburstError::Api { status, errors, .. } => {
                assert_eq!(status, 400);
                assert_eq!(errors[0].error_code, "REQUIRED_FIELD_MISSING");
                assert_eq!(errors[0].fields, vec!["Name".to_string()]);
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
