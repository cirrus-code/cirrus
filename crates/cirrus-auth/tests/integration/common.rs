//! Integration-test harness — env loading, auth construction, safety
//! guards.
//!
//! Mirrors `crates/cirrus/tests/integration/common.rs` so the two
//! crates share the same environment-variable contract. A single
//! `.env` at the repo root configures both test suites.
//!
//! # Required environment
//!
//! ```text
//! CIRRUS_INTEGRATION=1
//! CIRRUS_INTEGRATION_INSTANCE_URL=https://your-org.{sandbox,develop,scratch,trailblaze}.my.salesforce.com
//! ```
//!
//! Plus *one of*:
//!
//! - `CIRRUS_INTEGRATION_ACCESS_TOKEN=...` — static-token mode
//! - **JWT bearer mode** — all four required:
//!   - `CIRRUS_INTEGRATION_USERNAME=...`
//!   - `CIRRUS_INTEGRATION_CONSUMER_KEY=...`
//!   - `CIRRUS_INTEGRATION_PRIVATE_KEY_PATH=...`
//!   - `CIRRUS_INTEGRATION_LOGIN_URL=...`
//!
//! See `.env.example` at the repo root for the full template.

#![allow(dead_code)] // helper functions used by sibling test modules

use cirrus_auth::{JwtAuth, SharedAuth, StaticTokenAuth};
use std::sync::{Arc, Once};

pub(crate) const ENV_ENABLED: &str = "CIRRUS_INTEGRATION";
pub(crate) const ENV_INSTANCE_URL: &str = "CIRRUS_INTEGRATION_INSTANCE_URL";
pub(crate) const ENV_ACCESS_TOKEN: &str = "CIRRUS_INTEGRATION_ACCESS_TOKEN";
pub(crate) const ENV_USERNAME: &str = "CIRRUS_INTEGRATION_USERNAME";
pub(crate) const ENV_CONSUMER_KEY: &str = "CIRRUS_INTEGRATION_CONSUMER_KEY";
pub(crate) const ENV_PRIVATE_KEY_PATH: &str = "CIRRUS_INTEGRATION_PRIVATE_KEY_PATH";
pub(crate) const ENV_LOGIN_URL: &str = "CIRRUS_INTEGRATION_LOGIN_URL";
pub(crate) const ENV_FORCE: &str = "CIRRUS_INTEGRATION_FORCE";

const SAFE_PARTITIONS: &[&str] = &[
    ".sandbox.my.salesforce.com",
    ".develop.my.salesforce.com",
    ".scratch.my.salesforce.com",
    ".trailblaze.my.salesforce.com",
];

pub fn is_safe_test_url(url: &str) -> bool {
    SAFE_PARTITIONS.iter().any(|p| url.contains(p))
}

fn load_dotenv() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = dotenvy::dotenv();
    });
}

/// Resolves the configured `instance_url` after applying the safety
/// guard. Returns `None` (and prints a skip message) when integration
/// tests aren't enabled or the URL fails the safe-list check.
pub fn try_instance_url() -> Option<String> {
    load_dotenv();

    if std::env::var(ENV_ENABLED).ok().as_deref() != Some("1") {
        eprintln!("skipping: set {ENV_ENABLED}=1 (and other CIRRUS_INTEGRATION_* vars) to enable",);
        return None;
    }

    let instance_url = std::env::var(ENV_INSTANCE_URL).ok()?;

    let force = std::env::var(ENV_FORCE).ok().as_deref() == Some("1");
    if !is_safe_test_url(&instance_url) && !force {
        eprintln!(
            "REFUSING TO RUN: {ENV_INSTANCE_URL} ({instance_url}) doesn't match a known \
             sandbox/dev/scratch pattern. Set {ENV_FORCE}=1 to override.",
        );
        return None;
    }
    Some(instance_url)
}

/// Builds an [`AuthSession`] from environment, preferring static-token
/// mode when both are available. Returns `None` when neither path is
/// configured.
///
/// [`AuthSession`]: cirrus_auth::AuthSession
pub async fn try_init_auth() -> Option<SharedAuth> {
    let instance_url = try_instance_url()?;

    if let Ok(token) = std::env::var(ENV_ACCESS_TOKEN) {
        let shared: SharedAuth = Arc::new(StaticTokenAuth::new(token, instance_url));
        return Some(shared);
    }

    let jwt = try_init_jwt_auth().await?;
    let shared: SharedAuth = Arc::new(jwt);
    Some(shared)
}

/// Builds a [`JwtAuth`] from environment, *ignoring* `ACCESS_TOKEN`.
/// Returns `None` when any of the four JWT vars is missing.
///
/// Use this from JWT-specific tests that must exercise the full
/// bearer flow rather than fall through to the static-token shortcut.
pub async fn try_init_jwt_auth() -> Option<JwtAuth> {
    let instance_url = try_instance_url()?;

    let username = std::env::var(ENV_USERNAME).ok()?;
    let consumer_key = std::env::var(ENV_CONSUMER_KEY).ok()?;
    let private_key_path = std::env::var(ENV_PRIVATE_KEY_PATH).ok()?;
    let login_url = std::env::var(ENV_LOGIN_URL).ok()?;

    let builder = match JwtAuth::builder()
        .consumer_key(consumer_key)
        .username(username)
        .login_url(login_url)
        .instance_url(instance_url)
        .private_key_pem_file(private_key_path.clone())
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "skipping: failed to load private key from \
                 {ENV_PRIVATE_KEY_PATH} ({private_key_path}): {e}",
            );
            return None;
        }
    };

    match builder.build() {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("skipping: JwtAuth construction failed: {e}");
            None
        }
    }
}

/// Lightweight verifier: hits a trivial Salesforce REST endpoint
/// (`/services/data`) with the given bearer token and asserts the org
/// accepted it. Used as an end-to-end check that a minted token is
/// actually valid against the live org, without pulling in the `cirrus`
/// crate as a dev-dep.
///
/// Returns the HTTP status the org returned for the call so the caller
/// can also assert on it.
pub async fn ping_with_token(instance_url: &str, token: &str) -> reqwest::Result<u16> {
    let url = format!("{instance_url}/services/data");
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await?;
    Ok(resp.status().as_u16())
}
