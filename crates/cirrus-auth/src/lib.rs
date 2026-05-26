//! Salesforce OAuth 2.0 authentication flows for the Cirrus SDK.
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
//! ## Crate boundary
//!
//! This crate is the canonical home of every auth flow used by the
//! Cirrus SDK. It is re-exported as `cirrus::auth` so end users never
//! depend on `cirrus-auth` directly — `cirrus = "..."` is enough. Other
//! Cirrus subcrates (e.g. `cirrus-metadata`) that need an authenticated
//! session depend on `cirrus-auth` so they don't pull in the full REST
//! client.
//!
//! ## Implementations
//!
//! - [`static_token`] — preset access token + instance URL. Useful for
//!   tests, scripts, or callers that have already obtained credentials by
//!   other means.
//! - [`jwt`] — OAuth 2.0 JWT Bearer flow for server-to-server auth.
//! - [`refresh`] — OAuth 2.0 Refresh Token grant. Long-lived sessions for
//!   any flow that produces a refresh token.
//! - [`client_credentials`] — OAuth 2.0 Client Credentials grant for
//!   server-to-server integrations that run as a pre-configured user.
//! - [`web_server`] — OAuth 2.0 Web Server flow with PKCE for
//!   user-interactive authorization. Yields a [`RefreshTokenAuth`] once the
//!   user comes back through the redirect URL.
//! - [`token_exchange`] — OAuth 2.0 Token Exchange (RFC 8693), including
//!   Salesforce's hybrid grant.
//!
//! Flows Salesforce lists as legacy or deprecated are intentionally not
//! supported.

use async_trait::async_trait;
use std::borrow::Cow;
use std::sync::Arc;

pub mod client_credentials;
mod error;
pub mod jwt;
pub mod refresh;
pub mod static_token;
mod token_endpoint;
pub mod token_exchange;
pub mod web_server;

pub use client_credentials::{ClientCredentialsAuth, ClientCredentialsAuthBuilder};
pub use error::{AuthError, AuthResult};
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
/// The trait is `Send + Sync` and uses [`async_trait`](mod@async_trait) to remain
/// `dyn`-compatible — the client stores `Arc<dyn AuthSession>` so handlers
/// don't care which flow was configured.
#[async_trait]
pub trait AuthSession: Send + Sync {
    /// Returns a valid bearer access token. Implementations may refresh
    /// expired tokens transparently here; that is why the method is `async`.
    async fn access_token(&self) -> AuthResult<Cow<'_, str>>;

    /// Returns the instance URL for REST requests, e.g.
    /// `https://my-org.my.salesforce.com`. No trailing slash.
    fn instance_url(&self) -> &str;

    /// Invalidates a cached token that the SDK has determined is no
    /// longer valid (typically because Salesforce returned 401
    /// `INVALID_SESSION_ID` when it was used).
    ///
    /// The next [`access_token`](Self::access_token) call should mint
    /// a fresh token. The `stale_token` parameter lets implementations
    /// compare-and-swap — only clear the cache if the cached value
    /// still matches what the caller actually used. This avoids
    /// blowing away a *newer* token that another concurrent task may
    /// have refreshed in the meantime.
    ///
    /// Default impl is a no-op for static or stateless sessions
    /// ([`StaticTokenAuth`]) where there is nothing to invalidate.
    /// Stateful flows ([`JwtAuth`], [`RefreshTokenAuth`],
    /// [`ClientCredentialsAuth`]) override to clear their cached
    /// token.
    ///
    /// `stale_token` is borrowed for the duration of the call only —
    /// implementations should compare it against their cached value
    /// and not retain it.
    async fn invalidate(&self, stale_token: &str) {
        // Default: nothing to invalidate. Suppress the unused-arg
        // warning on the default impl without requiring overriders to
        // bind a different name.
        let _ = stale_token;
    }
}

/// `Arc<dyn AuthSession>` — the shape stored inside the client.
pub type SharedAuth = Arc<dyn AuthSession>;
