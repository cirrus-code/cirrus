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
pub mod pagination;
mod response;
pub mod retry;

pub use auth::{AuthSession, SharedAuth};
pub use error::{CloudburstError, CloudburstResult, SalesforceError};
pub use handlers::bulk::{BulkIngestSpec, BulkQuerySpec};
pub use handlers::composite::{
    BatchRequest, BatchSubrequest, CompositeRequest, CompositeSubrequest,
};
pub use pagination::Records;
pub use response::LimitInfo;
pub use response::{
    ApiVersion, BatchResponse, BatchSubresult, BulkColumnDelimiter, BulkIngestJob, BulkJobState,
    BulkLineEnding, BulkOperation, BulkQueryJob, BulkQueryResults, CompositeError,
    CompositeResponse, CompositeSubresponse, CompositeTreeResponse, CompositeTreeResult,
    DescribeGlobal, EventLogFileRecord, ExecuteAnonymousResult, Limit, OrgLimits, QueryResult,
    SObjectCollectionResult, SObjectCreateResult, SObjectMetadata, SearchResult,
};
pub use retry::RetryPolicy;

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::{Arc, RwLock};

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
    retry_policy: RetryPolicy,
    /// Most recent `Sforce-Limit-Info` header value, parsed. Wrapped
    /// in `Arc<RwLock<...>>` so updates are visible across cloned
    /// clients (clones share state).
    last_limit_info: Arc<RwLock<Option<LimitInfo>>>,
}

impl std::fmt::Debug for Cloudburst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit `auth` — it may carry secrets — and the reqwest
        // client (no useful Debug). Show only the safe configuration knobs.
        f.debug_struct("Cloudburst")
            .field("api_version", &self.api_version)
            .field("instance_url", &self.auth.instance_url())
            .field("retry_policy", &self.retry_policy)
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

    /// Returns the configured retry policy.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry_policy
    }

    /// Returns the most recent [`LimitInfo`] parsed from a
    /// `Sforce-Limit-Info` response header, if one has been seen.
    ///
    /// Salesforce includes this header on most REST API responses to
    /// surface near-real-time API call usage. The SDK captures it
    /// transparently on every request — successful, retried, or
    /// errored at the HTTP layer. Returns `None` until the first
    /// response with the header lands.
    ///
    /// Cloned clients share the same underlying state, so updates
    /// from one are visible from others. Same surfacing pattern
    /// jsforce uses for `Connection.limitInfo`.
    pub fn last_limit_info(&self) -> Option<LimitInfo> {
        self.last_limit_info.read().ok().and_then(|guard| *guard)
    }

    /// Internal: parse `Sforce-Limit-Info` from a response and stash
    /// the latest value. Called from every send path on every attempt.
    fn update_limit_info(&self, headers: &reqwest::header::HeaderMap) {
        let Some(value) = headers.get("Sforce-Limit-Info") else {
            return;
        };
        let Ok(s) = value.to_str() else { return };
        let Some(info) = LimitInfo::parse(s) else {
            return;
        };
        // Silently ignore poison — this is a best-effort stat surface,
        // not load-bearing for any operation.
        if let Ok(mut guard) = self.last_limit_info.write() {
            *guard = Some(info);
        }
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
        // Cache the access token across the retry burst — token
        // refresh on 401 is a separate concern (handled by AuthSession
        // implementations that auto-refresh, or by future
        // refresh-aware retry logic).
        let token = self.auth.access_token().await?;
        let mut attempt: u32 = 0;

        loop {
            let mut request = self
                .client
                .request(method.clone(), url)
                .bearer_auth(&*token);
            if let Some(q) = query {
                request = request.query(q);
            }
            if let Some(b) = body {
                request = request.json(b);
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let headers = response.headers().clone();
                    self.update_limit_info(&headers);

                    if retry::should_retry_status(&self.retry_policy, &method, status, attempt) {
                        // Drain the body so the connection returns to
                        // the pool clean. Errors here aren't actionable
                        // — we're about to retry anyway.
                        let _ = response.bytes().await;
                        let retry_after = retry::parse_retry_after(&headers);
                        let delay = retry::compute_delay(&self.retry_policy, attempt, retry_after);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }

                    let bytes = response.bytes().await?;
                    return response::parse_response_bytes(status, &bytes);
                }
                Err(e) => {
                    let err: CloudburstError = e.into();
                    if retry::should_retry_network(&self.retry_policy, &method, &err, attempt) {
                        let delay = retry::compute_delay(&self.retry_policy, attempt, None);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Sends a request with a raw body (e.g. CSV) and a custom Content-Type,
    /// parsing the response as JSON via [`response::parse_response_bytes`].
    ///
    /// Used by Bulk 2.0 ingest uploads — the request body is `text/csv`, the
    /// response is the standard JSON job envelope. Path resolution still
    /// follows [`Cloudburst::resolve_url`]'s three-mode semantics.
    pub(crate) async fn send_with_body<R>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: bytes::Bytes,
        content_type: &str,
    ) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
    {
        let url = self.resolve_url(path);
        let token = self.auth.access_token().await?;
        let mut attempt: u32 = 0;

        loop {
            // bytes::Bytes is Arc-backed — clone is cheap, doesn't copy
            // the underlying buffer.
            let request = self
                .client
                .request(method.clone(), &url)
                .bearer_auth(&*token)
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body.clone());

            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let headers = response.headers().clone();
                    self.update_limit_info(&headers);

                    if retry::should_retry_status(&self.retry_policy, &method, status, attempt) {
                        let _ = response.bytes().await;
                        let retry_after = retry::parse_retry_after(&headers);
                        let delay = retry::compute_delay(&self.retry_policy, attempt, retry_after);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }

                    let resp_bytes = response.bytes().await?;
                    return response::parse_response_bytes(status, &resp_bytes);
                }
                Err(e) => {
                    let err: CloudburstError = e.into();
                    if retry::should_retry_network(&self.retry_policy, &method, &err, attempt) {
                        let delay = retry::compute_delay(&self.retry_policy, attempt, None);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Fetches a response as raw bytes (e.g. CSV) plus its headers, with
    /// the standard Salesforce error-array parsing on non-2xx.
    ///
    /// Used by Bulk 2.0 result downloads — the response body is `text/csv`
    /// and the caller may need response headers for cursor pagination
    /// (`Sforce-Locator`, `Sforce-NumberOfRecords`). Path resolution still
    /// follows [`Cloudburst::resolve_url`]'s three-mode semantics.
    pub(crate) async fn fetch_raw(
        &self,
        method: reqwest::Method,
        path: &str,
        accept: &str,
        query: Option<&[(&str, &str)]>,
    ) -> CloudburstResult<(reqwest::header::HeaderMap, bytes::Bytes)> {
        let url = self.resolve_url(path);
        let token = self.auth.access_token().await?;
        let mut attempt: u32 = 0;

        loop {
            let mut request = self
                .client
                .request(method.clone(), &url)
                .bearer_auth(&*token)
                .header(reqwest::header::ACCEPT, accept);
            if let Some(q) = query {
                request = request.query(q);
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let headers = response.headers().clone();
                    self.update_limit_info(&headers);

                    if retry::should_retry_status(&self.retry_policy, &method, status, attempt) {
                        let _ = response.bytes().await;
                        let retry_after = retry::parse_retry_after(&headers);
                        let delay = retry::compute_delay(&self.retry_policy, attempt, retry_after);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }

                    let bytes = response.bytes().await?;
                    if (200..300).contains(&status) {
                        return Ok((headers, bytes));
                    }
                    return Err(response::parse_error_response(status, &bytes));
                }
                Err(e) => {
                    let err: CloudburstError = e.into();
                    if retry::should_retry_network(&self.retry_policy, &method, &err, attempt) {
                        let delay = retry::compute_delay(&self.retry_policy, attempt, None);
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
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
    retry_policy: Option<RetryPolicy>,
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

    /// Sets the [`RetryPolicy`] for transient-failure handling.
    /// Defaults to [`RetryPolicy::default`] (3 retries, exponential
    /// backoff with full jitter, 100 ms base, 30 s cap, retry on
    /// idempotent 5xx). Pass [`RetryPolicy::none`] to disable retries
    /// entirely.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
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
            retry_policy: self.retry_policy.unwrap_or_default(),
            last_limit_info: Arc::new(RwLock::new(None)),
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

    /// Retry policy + Sforce-Limit-Info header surfacing. Tests use a
    /// retry policy with zero base/max delays so they don't sleep —
    /// the retry behavior is what we're verifying, not the timing.
    mod retry_and_limits {
        use super::*;
        use crate::RetryPolicy;
        use serde_json::{Value, json};
        use std::sync::Arc;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn fast_retry_policy() -> RetryPolicy {
            RetryPolicy {
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
                jitter: false,
                ..RetryPolicy::default()
            }
        }

        fn fixture_with_policy(uri: String, policy: RetryPolicy) -> Cloudburst {
            let auth = Arc::new(StaticTokenAuth::new("tok", uri));
            Cloudburst::builder()
                .auth(auth)
                .retry_policy(policy)
                .build()
                .unwrap()
        }

        #[tokio::test]
        async fn retries_429_until_success() {
            let server = MockServer::start().await;

            // First two attempts return 429; third returns 200.
            // wiremock matches mocks in registration order with priority,
            // so we use `up_to_n_times` to scope the 429 mock to the
            // first two requests.
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(429))
                .up_to_n_times(2)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let v: Value = sf.get("limits").await.unwrap();
            assert_eq!(v["ok"], true);
        }

        #[tokio::test]
        async fn retries_503_until_success() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(503))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let v: Value = sf.get("limits").await.unwrap();
            assert_eq!(v["ok"], true);
        }

        #[tokio::test]
        async fn surfaces_error_after_max_retries_exhausted() {
            let server = MockServer::start().await;

            // Default policy retries 3 times → 4 total attempts.
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(503).set_body_json(json!([{
                    "errorCode": "SERVER_UNAVAILABLE",
                    "message": "Service Unavailable"
                }])))
                .expect(4)
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let err = sf.get::<Value>("limits").await.unwrap_err();
            match err {
                CloudburstError::Api { status, .. } => assert_eq!(status, 503),
                other => panic!("expected Api error, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn does_not_retry_4xx_caller_errors() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(404).set_body_json(json!([{
                    "errorCode": "NOT_FOUND",
                    "message": "not found"
                }])))
                // expect(1) — 4xx caller errors must not retry.
                .expect(1)
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let err = sf.get::<Value>("limits").await.unwrap_err();
            assert!(matches!(
                err,
                CloudburstError::Api { status: 404, .. }
            ));
        }

        #[tokio::test]
        async fn does_not_retry_500_on_post() {
            // POST is non-idempotent — even on 5xx (other than 429/503)
            // we must not retry, to avoid duplicate-record creation.
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/services/data/v60.0/sobjects/Account"))
                .respond_with(ResponseTemplate::new(500).set_body_json(json!([{
                    "errorCode": "INTERNAL_ERROR",
                    "message": "boom"
                }])))
                .expect(1)
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let err = sf
                .post::<Value, _>("sobjects/Account", &json!({"Name": "Acme"}))
                .await
                .unwrap_err();
            assert!(matches!(
                err,
                CloudburstError::Api { status: 500, .. }
            ));
        }

        #[tokio::test]
        async fn retries_500_on_get_when_idempotent_5xx_enabled() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(500))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let v: Value = sf.get("limits").await.unwrap();
            assert_eq!(v["ok"], true);
        }

        #[tokio::test]
        async fn none_policy_disables_retries_entirely() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(429))
                .expect(1)
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), RetryPolicy::none());
            let err = sf.get::<Value>("limits").await.unwrap_err();
            assert!(matches!(
                err,
                CloudburstError::Api { status: 429, .. }
            ));
        }

        #[tokio::test]
        async fn captures_sforce_limit_info_on_response() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"ok": true}))
                        .insert_header("Sforce-Limit-Info", "api-usage=42/15000"),
                )
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), RetryPolicy::none());
            // Before any request, no info captured.
            assert!(sf.last_limit_info().is_none());

            let _: Value = sf.get("limits").await.unwrap();

            let info = sf.last_limit_info().expect("limit info should be set");
            assert_eq!(info.used, 42);
            assert_eq!(info.allowed, 15000);
            assert_eq!(info.remaining(), 14958);
        }

        #[tokio::test]
        async fn limit_info_updates_on_subsequent_requests() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"ok": true}))
                        .insert_header("Sforce-Limit-Info", "api-usage=10/100"),
                )
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"ok": true}))
                        .insert_header("Sforce-Limit-Info", "api-usage=11/100"),
                )
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), RetryPolicy::none());
            let _: Value = sf.get("limits").await.unwrap();
            assert_eq!(sf.last_limit_info().unwrap().used, 10);
            let _: Value = sf.get("limits").await.unwrap();
            assert_eq!(sf.last_limit_info().unwrap().used, 11);
        }

        #[tokio::test]
        async fn malformed_limit_info_header_is_ignored() {
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"ok": true}))
                        // Wrong key, missing slash, etc. — just garbage.
                        .insert_header("Sforce-Limit-Info", "junk-data=oops"),
                )
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), RetryPolicy::none());
            let _: Value = sf.get("limits").await.unwrap();
            // Header didn't parse → no info stored.
            assert!(sf.last_limit_info().is_none());
        }

        #[tokio::test]
        async fn retry_after_header_overrides_backoff() {
            // The Retry-After hint, if present, takes precedence over
            // the policy's exponential schedule. We don't test the
            // *duration* directly (jitter would muddy that anyway) —
            // we just verify the retry happens and the request count
            // advances.
            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(
                    ResponseTemplate::new(429).insert_header("Retry-After", "0"),
                )
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/services/data/v60.0/limits"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
                .mount(&server)
                .await;

            let sf = fixture_with_policy(server.uri(), fast_retry_policy());
            let v: Value = sf.get("limits").await.unwrap();
            assert_eq!(v["ok"], true);
        }
    }
}
