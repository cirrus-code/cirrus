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
pub use response::{ApiVersion, QueryResult, SObjectCreateResult};

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

    /// Builds an absolute URL for a versioned REST path.
    ///
    /// `path` may begin with or omit a leading slash. The resulting URL has
    /// the shape `{instance_url}/services/data/{api_version}/{path}`.
    pub(crate) fn versioned_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!(
            "{}/services/data/{}/{}",
            self.auth.instance_url(),
            self.api_version,
            path
        )
    }

    /// Builds an absolute URL relative to the instance URL, without the
    /// `/services/data/{version}` prefix. Used for endpoints that live
    /// outside the versioned tree (e.g. `/services/data` itself, OAuth
    /// endpoints, Apex REST under `/services/apexrest`).
    pub(crate) fn instance_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{}/{}", self.auth.instance_url(), path)
    }

    /// Internal: GET a versioned path and deserialize the response into `R`.
    #[allow(dead_code)]
    pub(crate) async fn get<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        self.send_versioned(reqwest::Method::GET, path, None::<&()>, None::<&()>)
            .await
    }

    /// Internal: GET a versioned path with query parameters.
    #[allow(dead_code)]
    pub(crate) async fn get_with_query<R, Q>(&self, path: &str, query: &Q) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        self.send_versioned(reqwest::Method::GET, path, Some(query), None::<&()>)
            .await
    }

    /// Internal: POST a JSON body to a versioned path.
    #[allow(dead_code)]
    pub(crate) async fn post<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send_versioned(reqwest::Method::POST, path, None::<&()>, Some(body))
            .await
    }

    /// Internal: PATCH a JSON body to a versioned path.
    #[allow(dead_code)]
    pub(crate) async fn patch<R, B>(&self, path: &str, body: &B) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send_versioned(reqwest::Method::PATCH, path, None::<&()>, Some(body))
            .await
    }

    /// Internal: DELETE a versioned path.
    #[allow(dead_code)]
    pub(crate) async fn delete<R: DeserializeOwned>(&self, path: &str) -> CloudburstResult<R> {
        self.send_versioned(reqwest::Method::DELETE, path, None::<&()>, None::<&()>)
            .await
    }

    /// Internal: GET an unversioned (instance-relative) path. Used for
    /// `/services/data` (the version index) and similar.
    pub(crate) async fn get_unversioned<R: DeserializeOwned>(
        &self,
        path: &str,
    ) -> CloudburstResult<R> {
        let url = self.instance_url(path);
        self.send(reqwest::Method::GET, &url, None::<&()>, None::<&()>)
            .await
    }

    async fn send_versioned<R, Q, B>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<&Q>,
        body: Option<&B>,
    ) -> CloudburstResult<R>
    where
        R: DeserializeOwned,
        Q: Serialize + ?Sized,
        B: Serialize + ?Sized,
    {
        let url = self.versioned_url(path);
        self.send(method, &url, query, body).await
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
    fn versioned_url_uses_default_api_version() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.versioned_url("/sobjects/Account/001");
        assert_eq!(
            url,
            "https://my.salesforce.com/services/data/v60.0/sobjects/Account/001"
        );
    }

    #[test]
    fn versioned_url_handles_path_without_leading_slash() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.versioned_url("limits");
        assert_eq!(url, "https://my.salesforce.com/services/data/v60.0/limits");
    }

    #[test]
    fn instance_url_skips_versioned_prefix() {
        let sf = fixture("https://my.salesforce.com");
        let url = sf.instance_url("/services/data");
        assert_eq!(url, "https://my.salesforce.com/services/data");
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
        assert!(sf.versioned_url("/x").contains("/v61.0/"));
    }
}
