//! # Cloudburst SDK
//!
//! An ergonomic Rust HTTP client for the Salesforce REST API.
//!
//! Inspired by [Octocrab](https://github.com/XAMPPRocky/octocrab), Cloudburst
//! provides a type-safe, async interface for interacting with Salesforce
//! while leaving response shapes entirely up to the caller — no hard-coded
//! sObject types like `Account` or `Contact`.
//!
//! ## Design principles
//!
//! - **No user-facing types.** The SDK never models org-specific data. Every
//!   handler that returns records is generic over a caller-supplied type, and
//!   defaults to [`serde_json::Value`] when none is specified.
//! - **Hard-coded platform types only.** Schema-independent envelopes
//!   ([`response::QueryResult`], [`response::SObjectCreateResult`],
//!   [`response::ApiVersion`], the Salesforce error array) are concrete,
//!   because their shape is part of the platform contract.
//! - **Auth is pluggable.** Any [`auth::AuthSession`] implementation works;
//!   handlers don't know which OAuth flow produced the token.
//! - **No legacy surface.** Anything Salesforce labels deprecated or legacy
//!   is intentionally not supported.
//!
//! ## Quick start
//!
//! ```no_run
//! use cloudburst_sdk::{Cloudburst, auth::StaticTokenAuth};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), cloudburst_sdk::CloudburstError> {
//! let auth = Arc::new(StaticTokenAuth::new(
//!     "00D...!AQ...",
//!     "https://my-org.my.salesforce.com",
//! ));
//!
//! let sf = Cloudburst::builder()
//!     .auth(auth)
//!     .build()?;
//!
//! let versions = sf.versions().await?;
//! # let _ = versions;
//! # Ok(())
//! # }
//! ```

pub mod auth;
mod error;
pub mod handlers;
mod response;

pub use auth::{AuthSession, SharedAuth};
pub use error::{CloudburstError, CloudburstResult, SalesforceError};
pub use response::{
    ApiVersion, DescribeGlobal, Limit, OrgLimits, QueryResult, SObjectCreateResult,
    SObjectMetadata, SearchResult,
};

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Default Salesforce REST API version when the caller doesn't override it.
pub const DEFAULT_API_VERSION: &str = "v60.0";

/// Default User-Agent header value sent on every request.
pub(crate) const DEFAULT_USER_AGENT: &str = concat!(
    "cloudburst-sdk/",
    env!("CARGO_PKG_VERSION"),
    " (Rust SDK for Salesforce)"
);

/// The main Salesforce client.
///
/// Holds the underlying HTTP client, an [`AuthSession`] for credentials, and
/// the API version to use. Cheap to clone — internal state is reference
/// counted.
#[derive(Clone)]
pub struct Cloudburst {
    client: reqwest::Client,
    auth: SharedAuth,
    api_version: String,
}

impl std::fmt::Debug for Cloudburst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit `auth` — it may carry secrets — and the reqwest
        // client (no useful Debug). Show only the safe configuration knobs.
        f.debug_struct("Cloudburst")
            .field("api_version", &self.api_version)
            .field("instance_url", &self.auth.instance_url())
            .finish_non_exhaustive()
    }
}

impl Cloudburst {
    /// Creates a new builder for constructing a [`Cloudburst`] client.
    pub fn builder() -> CloudburstBuilder {
        CloudburstBuilder::default()
    }

    /// Returns the configured API version (e.g. `"v60.0"`).
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    /// Returns a reference to the underlying `reqwest` client. Useful for
    /// callers who want to compose additional requests against the same
    /// connection pool.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Returns the auth session backing this client.
    pub fn auth(&self) -> &SharedAuth {
        &self.auth
    }

    /// Resolves a path to a fully-qualified URL using three-mode semantics:
    ///
    /// - Fully-qualified (`http://…` or `https://…`): used as-is.
    /// - Instance-rooted (leading `/`): resolved against the instance URL,
    ///   e.g. `/services/data` → `{instance}/services/data`.
    /// - Versioned (anything else): prefixed with `/services/data/{version}/`,
    ///   e.g. `limits` → `{instance}/services/data/{version}/limits`.
    ///
    /// This is the path-resolution contract used by every public verb method
    /// on [`Cloudburst`] and is the load-bearing piece of the open-ended
    /// client escape hatch.
    pub(crate) fn resolve_url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if let Some(rest) = path.strip_prefix('/') {
            format!("{}/{}", self.auth.instance_url(), rest)
        } else {
            format!(
                "{}/services/data/{}/{}",
                self.auth.instance_url(),
                self.api_version,
                path
            )
        }
    }

    /// Builds a versioned URL by appending percent-encoded path segments.
    ///
    /// Use this when any segment may contain reserved characters (slash,
    /// equals, percent, etc.) — e.g. an upsert external-ID value. Each
    /// element of `segments` is encoded as a single path segment.
    pub(crate) fn versioned_segments(&self, segments: &[&str]) -> CloudburstResult<String> {
        let base = format!(
            "{}/services/data/{}/",
            self.auth.instance_url(),
            self.api_version
        );
        let mut url = url::Url::parse(&base)?;
        // The trailing '/' on `base` leaves an empty path segment; without
        // popping it, `extend` produces `.../v60.0//sobjects/...`.
        url.path_segments_mut()
            .map_err(|()| {
                CloudburstError::InvalidResponse("instance URL is not hierarchical".into())
            })?
            .pop_if_empty()
            .extend(segments);
        Ok(url.to_string())
    }

    /// GET an arbitrary Salesforce path, deserializing the response into `R`.
    ///
    /// Path resolution follows [`Cloudburst::resolve_url`]'s three-mode
    /// semantics. Use this as the open-ended client escape hatch when no
    /// typed builder exists for the resource you need.
    pub async fn get<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        let url = self.resolve_url(path);
        self.send::<R, (), ()>(reqwest::Method::GET, &url, None, None)
            .await
    }

    /// GET with query parameters. `query` is anything `Serialize` —
    /// typically `&[("k", "v")]` or a struct.
    pub async fn get_with_query<R, Q>(&self, path: &str, query: &Q) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        let url = self.resolve_url(path);
        self.send::<R, Q, ()>(reqwest::Method::GET, &url, Some(query), None)
            .await
    }

    /// POST a JSON body.
    pub async fn post<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = self.resolve_url(path);
        self.send::<R, (), B>(reqwest::Method::POST, &url, None, Some(body))
            .await
    }

    /// PUT a JSON body.
    ///
    /// Salesforce REST proper rarely uses PUT — it's included for symmetry
    /// with the rest of the verb set so the escape hatch can address any
    /// Salesforce surface (Tooling API, Apex REST, etc.) that does.
    pub async fn put<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = self.resolve_url(path);
        self.send::<R, (), B>(reqwest::Method::PUT, &url, None, Some(body))
            .await
    }

    /// PATCH a JSON body.
    pub async fn patch<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = self.resolve_url(path);
        self.send::<R, (), B>(reqwest::Method::PATCH, &url, None, Some(body))
            .await
    }

    /// DELETE a resource. Salesforce typically returns 204 No Content on
    /// success — call with `R = ()`.
    pub async fn delete<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        let url = self.resolve_url(path);
        self.send::<R, (), ()>(reqwest::Method::DELETE, &url, None, None)
            .await
    }

    /// Internal: send a request to a fully-built absolute URL. Used by
    /// handlers that need percent-encoded path segments (sObject upsert by
    /// external ID, for example) — they construct the URL via
    /// [`Self::versioned_segments`] and dispatch through this method.
    pub(crate) async fn send_at<R, Q, B>(
        &self,
        method: reqwest::Method,
        url: &str,
        query: Option<&Q>,
        body: Option<&B>,
    ) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
        B: Serialize + ?Sized,
    {
        self.send(method, url, query, body).await
    }

    /// Returns a pre-authenticated [`reqwest::RequestBuilder`] targeting
    /// the resolved URL. Path resolution follows
    /// [`Cloudburst::resolve_url`]'s three-mode semantics; the bearer
    /// token is injected via [`AuthSession::access_token`]. The caller is
    /// then free to add headers, configure timeouts, set a custom body
    /// (multipart, form, raw bytes), and `.send()` the request.
    ///
    /// Response parsing is *not* applied — call sites that want
    /// Salesforce-error-aware deserialization should use the typed verb
    /// methods ([`Self::get`], [`Self::post`], etc.) instead.
    pub async fn request_builder(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> CloudburstResult<reqwest::RequestBuilder> {
        let url = self.resolve_url(path);
        let token = self.auth.access_token().await?;
        Ok(self.client.request(method, url).bearer_auth(&*token))
    }

    /// Executes a fully-prepared [`reqwest::Request`] using the SDK's
    /// HTTP client.
    ///
    /// Hands-off passthrough — no auth injection, no URL resolution, no
    /// response parsing. Useful for unusual cases where the caller has
    /// constructed the entire request themselves and just wants to share
    /// the SDK's connection pool.
    ///
    /// To get an auth token for a custom request, use
    /// `client.auth().access_token().await?`.
    pub async fn execute(&self, request: reqwest::Request) -> CloudburstResult<reqwest::Response> {
        self.client.execute(request).await.map_err(Into::into)
    }

    async fn send<R, Q, B>(
        &self,
        method: reqwest::Method,
        url: &str,
        query: Option<&Q>,
        body: Option<&B>,
    ) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
        B: Serialize + ?Sized,
    {
        let token = self.auth.access_token().await?;
        let mut request = self.client.request(method, url).bearer_auth(&*token);

        if let Some(q) = query {
            request = request.query(q);
        }
        if let Some(b) = body {
            request = request.json(b);
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await?;
        response::parse_response_bytes(status, &bytes)
    }
}

/// Builder for [`Cloudburst`].
///
/// Required: an [`AuthSession`] via [`auth`](Self::auth). Everything else has
/// a sensible default.
#[derive(Default)]
pub struct CloudburstBuilder {
    auth: Option<SharedAuth>,
    api_version: Option<String>,
    user_agent: Option<String>,
    http_client: Option<reqwest::Client>,
}

impl CloudburstBuilder {
    /// Sets the auth session (any [`AuthSession`] implementation wrapped in
    /// `Arc`). Required.
    pub fn auth(mut self, auth: SharedAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Sets the Salesforce REST API version, e.g. `"v60.0"`. Defaults to
    /// [`DEFAULT_API_VERSION`].
    pub fn api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = Some(version.into());
        self
    }

    /// Overrides the default User-Agent header.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Supplies a pre-configured `reqwest::Client`. Useful for sharing a
    /// connection pool across multiple SDK clients or for installing custom
    /// middleware. When provided, the builder's `user_agent` setting is
    /// ignored — configure that on the supplied client instead.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Finalizes the builder.
    pub fn build(self) -> CloudburstResult<Cloudburst> {
        let auth = self.auth.ok_or(CloudburstError::MissingField("auth"))?;

        let client = if let Some(c) = self.http_client {
            c
        } else {
            let ua = self.user_agent.as_deref().unwrap_or(DEFAULT_USER_AGENT);
            let mut headers = HeaderMap::new();
            headers.insert(
                USER_AGENT,
                HeaderValue::from_str(ua)
                    .map_err(|e| CloudburstError::InvalidHeader(e.to_string()))?,
            );
            reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .map_err(CloudburstError::HttpClient)?
        };

        Ok(Cloudburst {
            client,
            auth,
            api_version: self
                .api_version
                .unwrap_or_else(|| DEFAULT_API_VERSION.to_string()),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::auth::StaticTokenAuth;
    use std::sync::Arc;

    fn fixture(instance: &str) -> Cloudburst {
        let auth = Arc::new(StaticTokenAuth::new("tok", instance));
        Cloudburst::builder().auth(auth).build().unwrap()
    }

    #[test]
    fn build_requires_auth() {
        let err = Cloudburst::builder().build().unwrap_err();
        assert!(matches!(err, CloudburstError::MissingField("auth")));
    }

    #[test]
    fn resolve_url_versioned_for_relative_path() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.resolve_url("limits");
        assert_eq!(url, "https://my.salesforce.com/services/data/v60.0/limits");
    }

    #[test]
    fn resolve_url_versioned_for_nested_relative_path() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.resolve_url("sobjects/Account/001");
        assert_eq!(
            url,
            "https://my.salesforce.com/services/data/v60.0/sobjects/Account/001"
        );
    }

    #[test]
    fn resolve_url_instance_rooted_for_leading_slash() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.resolve_url("/services/data");
        assert_eq!(url, "https://my.salesforce.com/services/data");
    }

    #[test]
    fn resolve_url_passthrough_for_https_url() {
        let sf = fixture("https://my.salesforce.com");
        let absolute = "https://other.example.com/some/path";
        assert_eq!(sf.resolve_url(absolute), absolute);
    }

    #[test]
    fn resolve_url_passthrough_for_http_url() {
        let sf = fixture("https://my.salesforce.com");
        let absolute = "http://localhost:1234/path";
        assert_eq!(sf.resolve_url(absolute), absolute);
    }

    #[test]
    fn api_version_can_be_overridden() {
        let auth = Arc::new(StaticTokenAuth::new("tok", "https://my.salesforce.com"));
        let sf = Cloudburst::builder()
            .auth(auth)
            .api_version("v61.0")
            .build()
            .unwrap();
        assert_eq!(sf.api_version(), "v61.0");
        assert!(sf.resolve_url("x").contains("/v61.0/"));
    }

    mod escape_hatch {
        use super::*;
        use serde_json::{Value, json};
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn server_fixture(uri: String) -> Cloudburst {
            let auth = Arc::new(StaticTokenAuth::new("tok", uri));
            Cloudburst::builder().auth(auth).build().unwrap()
        }

        #[tokio::test]
        async fn get_resolves_relative_path_as_versioned() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .and(header("authorization", "Bearer tok"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let v: Value = sf.get("limits").await.unwrap();
            assert_eq!(v["ok"], true);
        }

        #[tokio::test]
        async fn get_resolves_leading_slash_as_instance_rooted() {
            let server = MockServer::start().await;
            // Note: /services/apexrest/foo lives outside the versioned tree,
            // so passing a leading-slash path is the only correct way.
            Mock::given(method("GET"))
                .and(path("/services/apexrest/foo"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"called": "apex"})))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let v: Value = sf.get("/services/apexrest/foo").await.unwrap();
            assert_eq!(v["called"], "apex");
        }

        #[tokio::test]
        async fn get_passes_through_absolute_url() {
            // The "passthrough" mode targets a different host entirely from
            // the configured instance URL, proving resolve_url didn't try to
            // prefix it.
            let other = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/some/other/path"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"hit": "other"})))
                .mount(&other)
                .await;

            let sf = server_fixture("https://unused.invalid".to_string());
            let target = format!("{}/some/other/path", other.uri());
            let v: Value = sf.get(&target).await.unwrap();
            assert_eq!(v["hit"], "other");
        }

        #[tokio::test]
        async fn post_sends_json_body() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/services/data/v60.0/composite/batch"))
                .and(body_json(json!({"batchRequests": []})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let v: Value = sf
                .post("composite/batch", &json!({"batchRequests": []}))
                .await
                .unwrap();
            assert!(v["results"].is_array());
        }

        #[tokio::test]
        async fn put_sends_json_body() {
            let server = MockServer::start().await;
            Mock::given(method("PUT"))
                .and(path("/services/data/v60.0/custom/resource"))
                .and(body_json(json!({"k": "v"})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"updated": true})))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let v: Value = sf.put("custom/resource", &json!({"k": "v"})).await.unwrap();
            assert_eq!(v["updated"], true);
        }

        #[tokio::test]
        async fn patch_sends_json_body() {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .and(path("/services/data/v60.0/sobjects/Account/001"))
                .and(body_json(json!({"Name": "X"})))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            sf.patch::<(), _>("sobjects/Account/001", &json!({"Name": "X"}))
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn delete_handles_204() {
            let server = MockServer::start().await;
            Mock::given(method("DELETE"))
                .and(path("/services/data/v60.0/sobjects/Account/001"))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            sf.delete::<()>("sobjects/Account/001").await.unwrap();
        }

        #[tokio::test]
        async fn request_builder_pre_injects_bearer_auth() {
            // Verifies the returned RequestBuilder already has the auth
            // header set — a caller adding their own headers shouldn't
            // need to re-attach the bearer token.
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .and(header("authorization", "Bearer tok"))
                .and(header("x-custom", "added-by-caller"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let resp = sf
                .request_builder(reqwest::Method::GET, "limits")
                .await
                .unwrap()
                .header("X-Custom", "added-by-caller")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 200);
        }

        #[tokio::test]
        async fn execute_runs_caller_built_request() {
            // execute() is fully hands-off — no auth injection. Caller is
            // responsible for everything. Used here without a bearer token
            // intentionally to prove no auth is injected.
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/raw/path"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"raw": true})))
                .mount(&server)
                .await;

            let sf = server_fixture(server.uri());
            let url = format!("{}/raw/path", server.uri());
            let req = sf.http_client().get(&url).build().unwrap();
            let resp = sf.execute(req).await.unwrap();
            assert_eq!(resp.status().as_u16(), 200);
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["raw"], true);
        }
    }
}
