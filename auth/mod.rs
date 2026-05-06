//! Authentication abstractions for the Salesforce REST API.
//!
//! Salesforce supports several OAuth 2.0 flows, each producing the same two
//! pieces of information: a bearer access token and an `instance_url` that
//! becomes the base URL for subsequent REST calls. The rest of the SDK is
//! flow-agnostic — it only needs an [`AuthSession`] that can hand over those
//! two values on demand.
//!
//! Implementations live in submodules (one per flow). They are responsible
//! for their own token acquisition, refresh, and any caching. The trait
//! method is `async` so an implementation may transparently refresh an
//! expired token when called.
//!
//! ## Implementations
//!
//! - [`static_token`] — preset access token + instance URL. Useful for
//!   tests, scripts, or callers that have already obtained credentials by
//!   other means.
//! - [`jwt`] — OAuth 2.0 JWT Bearer flow for server-to-server auth.
//! - [`refresh`] — OAuth 2.0 Refresh Token grant. Long-lived sessions for
//!   any flow that produces a refresh token (Web Server, Device, etc.).
//! - [`client_credentials`] — OAuth 2.0 Client Credentials grant for
//!   server-to-server integrations that run as a pre-configured user.
//! - [`web_server`] — OAuth 2.0 Web Server flow with PKCE for
//!   user-interactive authorization. Yields a [`RefreshTokenAuth`] once the
//!   user comes back through the redirect URL.
//!
//! Additional flows (Token Exchange, etc.) will live in sibling modules.
//! Anything Salesforce lists as legacy or deprecated is intentionally not
//! supported.

use crate::error::CloudburstResult;
use async_trait::async_trait;
use std::borrow::Cow;
use std::sync::Arc;

pub mod client_credentials;
pub mod jwt;
pub mod refresh;
pub mod static_token;
mod token_endpoint;
pub mod token_exchange;
pub mod web_server;

pub use client_credentials::{ClientCredentialsAuth, ClientCredentialsAuthBuilder};
pub use jwt::{JwtAuth, JwtAuthBuilder};
pub use refresh::{RefreshTokenAuth, RefreshTokenAuthBuilder};
pub use static_token::StaticTokenAuth;
pub use token_exchange::{
    SubjectTokenType, TokenExchangeFlow, TokenExchangeFlowBuilder, TokenExchangeGrantType,
    TokenExchangeSession,
};
pub use web_server::{CompletedSession, PendingExchange, WebServerFlow, WebServerFlowBuilder};

/// Abstraction over a Salesforce authentication session.
///
/// An implementation holds whatever credentials the chosen flow needs,
/// produces a bearer access token on demand (refreshing if necessary), and
/// reports the instance URL that REST requests should target.
///
/// The trait is `Send + Sync` and uses [`async_trait`] to remain
/// `dyn`-compatible — the client stores `Arc<dyn AuthSession>` so handlers
/// don't care which flow was configured.
#[async_trait]
pub trait AuthSession: Send + Sync {
    /// Returns a valid bearer access token. Implementations may refresh
    /// expired tokens transparently here; that is why the method is `async`.
    async fn access_token(&self) -> CloudburstResult<Cow<'_, str>>;

    /// Returns the instance URL for REST requests, e.g.
    /// `https://my-org.my.salesforce.com`. No trailing slash.
    fn instance_url(&self) -> &str;
}

/// `Arc<dyn AuthSession>` — the shape stored inside the client.
pub type SharedAuth = Arc<dyn AuthSession>;
