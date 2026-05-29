//! Read-only smoke tests for the [`AuthSession`] surface against a
//! real org.
//!
//! These run whichever mode is configured (static or JWT) — they're
//! checking that the trait contract holds end-to-end, not that any
//! specific flow works. Per-flow tests live in sibling modules
//! (currently only `jwt`).
//!
//! [`AuthSession`]: cirrus_auth::AuthSession

use crate::common::{ping_with_token, try_init_auth, try_instance_url};

#[tokio::test]
#[ignore]
async fn auth_session_produces_a_usable_bearer_token() {
    let Some(auth) = try_init_auth().await else {
        return;
    };

    let token = auth
        .access_token()
        .await
        .expect("access_token should succeed against a real org");
    assert!(!token.is_empty(), "minted token should not be empty");

    let instance_url = try_instance_url().expect("instance URL was just validated");
    let status = ping_with_token(&instance_url, &token)
        .await
        .expect("REST call to /services/data should not fail at the transport layer");
    assert_eq!(
        status, 200,
        "Salesforce should accept the freshly minted bearer token (got HTTP {status})",
    );
}

#[tokio::test]
#[ignore]
async fn auth_session_instance_url_round_trips() {
    let Some(auth) = try_init_auth().await else {
        return;
    };
    let configured = try_instance_url().expect("instance URL was just validated");

    // The trait's `instance_url()` returns the *normalized* (trailing-
    // slash-stripped) value. We pre-validate `configured` doesn't have
    // a trailing slash by trimming on the comparison side.
    assert_eq!(
        auth.instance_url(),
        configured.trim_end_matches('/'),
        "AuthSession::instance_url should round-trip the configured value",
    );
}

#[tokio::test]
#[ignore]
async fn invalidate_then_access_token_still_returns_a_valid_token() {
    let Some(auth) = try_init_auth().await else {
        return;
    };

    // First mint.
    let first = auth.access_token().await.unwrap().to_string();

    // Invalidate that exact token. Stateful flows clear their cache;
    // stateless static-token auth is a no-op.
    auth.invalidate(&first).await;

    // Second mint. For static-token: same value. For stateful flows: a
    // *fresh* token (or the same one if the cache was repopulated by
    // a concurrent call — unlikely in this single-threaded test).
    let second = auth.access_token().await.unwrap();
    assert!(
        !second.is_empty(),
        "post-invalidate access_token should still produce a valid bearer",
    );

    // Either way, the post-invalidate token should be accepted by the
    // org — that's the property `invalidate` is supposed to preserve.
    let instance_url = try_instance_url().unwrap();
    let status = ping_with_token(&instance_url, &second).await.unwrap();
    assert_eq!(status, 200, "post-invalidate token should be accepted");
}
