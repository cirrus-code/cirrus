//! Retry policy and backoff machinery for transient HTTP failures.
//!
//! Policy: retry only what's clearly safe — 429 / 503 (server-asserted
//! "didn't happen, try again"), and idempotent-method 5xx — honor any
//! `Retry-After` hint, and add jitter to avoid thundering herds. The
//! Metadata API SOAP endpoint is always POST (non-idempotent), so the
//! [`retry_idempotent_5xx`] flag only governs the open-ended escape
//! hatch; the SOAP path retries solely on 429 / 503 / connect-phase
//! failures.
//!
//! [`retry_idempotent_5xx`]: RetryPolicy::retry_idempotent_5xx

use crate::error::MetadataError;
use std::time::Duration;

/// Configuration for transient-failure retry behavior.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of *additional* attempts after the initial
    /// request. `0` disables retries; `3` (default) means up to four
    /// total attempts.
    pub max_retries: u32,
    /// Base delay for the exponential backoff schedule. Default 100 ms.
    pub base_delay: Duration,
    /// Cap on the computed backoff delay. Default 30 s.
    pub max_delay: Duration,
    /// Apply full jitter — pick a random delay in `[0, computed]`
    /// rather than using the deterministic exponential value. Default
    /// `true`.
    pub jitter: bool,
    /// When `true`, retry idempotent methods (GET, HEAD, DELETE, PUT)
    /// on transient 5xx errors (500, 502, 504). When `false`, only
    /// retry on 429 / 503. Default `true`. The Metadata API SOAP
    /// surface is POST-only so this flag is dormant for SOAP calls,
    /// but it applies to the open-ended escape hatch.
    pub retry_idempotent_5xx: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            jitter: true,
            retry_idempotent_5xx: true,
        }
    }
}

impl RetryPolicy {
    /// A policy that disables retries. Useful for non-idempotent flows
    /// or for tests that want deterministic single-shot semantics.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }
}

/// Decision point: should we retry this HTTP response?
///
/// `attempt` is the zero-indexed *previous* attempt count.
pub(crate) fn should_retry_status(
    policy: &RetryPolicy,
    method: &reqwest::Method,
    status: u16,
    attempt: u32,
) -> bool {
    if attempt >= policy.max_retries {
        return false;
    }
    match status {
        429 | 503 => true,
        500 | 502 | 504 if policy.retry_idempotent_5xx => is_idempotent(method),
        _ => false,
    }
}

/// Decision point: should we retry this network-level failure?
pub(crate) fn should_retry_network(
    policy: &RetryPolicy,
    method: &reqwest::Method,
    error: &MetadataError,
    attempt: u32,
) -> bool {
    if attempt >= policy.max_retries {
        return false;
    }
    let MetadataError::Http(http) = error else {
        return false;
    };
    // Connect-phase errors mean the request never reached the server,
    // so retrying is safe even for non-idempotent methods (which is
    // every SOAP call — the API is POST-only). This covers DNS
    // failures, TCP RSTs, TLS handshake failures, and connect timeouts.
    if http.is_connect() {
        return true;
    }
    is_idempotent(method)
}

fn is_idempotent(method: &reqwest::Method) -> bool {
    matches!(
        *method,
        reqwest::Method::GET
            | reqwest::Method::HEAD
            | reqwest::Method::DELETE
            | reqwest::Method::PUT
            | reqwest::Method::OPTIONS
            | reqwest::Method::TRACE
    )
}

/// Parse a `Retry-After` header value as RFC 7231 §7.1.3 delta-seconds.
pub(crate) fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?;
    let s = raw.to_str().ok()?;
    s.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Compute the next backoff delay.
pub(crate) fn compute_delay(
    policy: &RetryPolicy,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    if let Some(hint) = retry_after {
        let capped = hint.min(policy.max_delay);
        tracing::warn!(
            target: "cirrus_metadata::retry",
            attempt = attempt + 1,
            delay_ms = capped.as_millis() as u64,
            source = "retry-after-header",
            "scheduling request retry",
        );
        return capped;
    }
    let factor: u128 = 1u128.checked_shl(attempt).unwrap_or(u128::MAX);
    let computed_ms = policy.base_delay.as_millis().saturating_mul(factor);
    let max_ms = policy.max_delay.as_millis();
    let capped_ms = computed_ms.min(max_ms);
    let computed = Duration::from_millis(capped_ms.min(u64::MAX as u128) as u64);

    let final_delay = if !policy.jitter {
        computed
    } else {
        let max_ms = computed.as_millis() as u64;
        if max_ms == 0 {
            Duration::ZERO
        } else {
            let mut buf = [0u8; 8];
            if getrandom::fill(&mut buf).is_err() {
                computed
            } else {
                let r = u64::from_le_bytes(buf) % (max_ms + 1);
                Duration::from_millis(r)
            }
        }
    };
    tracing::warn!(
        target: "cirrus_metadata::retry",
        attempt = attempt + 1,
        delay_ms = final_delay.as_millis() as u64,
        source = "exponential-backoff",
        "scheduling request retry",
    );
    final_delay
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_retries_three_times() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_retries, 3);
        assert!(p.jitter);
    }

    #[test]
    fn none_policy_disables_retry() {
        let p = RetryPolicy::none();
        assert!(!should_retry_status(&p, &reqwest::Method::POST, 503, 0));
    }

    #[test]
    fn retries_429_and_503_for_post_metadata_calls() {
        let p = RetryPolicy::default();
        // Metadata API SOAP endpoint is POST. We retry 429/503 anyway
        // because the server is asserting "didn't happen, retry."
        assert!(should_retry_status(&p, &reqwest::Method::POST, 429, 0));
        assert!(should_retry_status(&p, &reqwest::Method::POST, 503, 0));
    }

    #[test]
    fn does_not_retry_post_on_other_5xx() {
        let p = RetryPolicy::default();
        // 500/502/504 on POST is ambiguous — the request may have
        // partially landed. Don't retry.
        assert!(!should_retry_status(&p, &reqwest::Method::POST, 500, 0));
        assert!(!should_retry_status(&p, &reqwest::Method::POST, 502, 0));
        assert!(!should_retry_status(&p, &reqwest::Method::POST, 504, 0));
    }

    #[test]
    fn stops_at_max_retries() {
        let p = RetryPolicy::default();
        assert!(should_retry_status(&p, &reqwest::Method::POST, 429, 2));
        assert!(!should_retry_status(&p, &reqwest::Method::POST, 429, 3));
    }

    #[test]
    fn parse_retry_after_handles_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("7"),
        );
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(7)));
    }

    #[test]
    fn compute_delay_honors_retry_after_capped_at_max() {
        let p = RetryPolicy {
            max_delay: Duration::from_secs(10),
            ..RetryPolicy::default()
        };
        assert_eq!(
            compute_delay(&p, 0, Some(Duration::from_secs(99))),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn compute_delay_caps_exponential_at_max_delay() {
        let p = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            jitter: false,
            ..RetryPolicy::default()
        };
        assert_eq!(compute_delay(&p, 0, None), Duration::from_millis(100));
        assert_eq!(compute_delay(&p, 4, None), Duration::from_secs(1));
        assert_eq!(compute_delay(&p, 100, None), Duration::from_secs(1));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy producing a deterministic (non-jittered) policy.
    /// Jitter randomizes within `[0, computed]`, which makes the
    /// monotonicity property unprovable for any single sample. The
    /// jitter path itself gets its own property below.
    fn deterministic_policy() -> impl Strategy<Value = RetryPolicy> {
        (1u64..=500u64, 1u64..=60_000u64).prop_map(|(base_ms, max_ms)| {
            // Force max_delay >= base_delay so the cap math has
            // somewhere to land.
            let max_ms = max_ms.max(base_ms);
            RetryPolicy {
                base_delay: Duration::from_millis(base_ms),
                max_delay: Duration::from_millis(max_ms),
                jitter: false,
                ..RetryPolicy::default()
            }
        })
    }

    /// Same shape as `deterministic_policy` but with jitter enabled.
    /// Sampled separately because the jitter path has different
    /// invariants (upper bound, not monotonicity).
    fn jittered_policy() -> impl Strategy<Value = RetryPolicy> {
        (1u64..=500u64, 1u64..=60_000u64).prop_map(|(base_ms, max_ms)| {
            let max_ms = max_ms.max(base_ms);
            RetryPolicy {
                base_delay: Duration::from_millis(base_ms),
                max_delay: Duration::from_millis(max_ms),
                jitter: true,
                ..RetryPolicy::default()
            }
        })
    }

    proptest! {
        /// Cap invariant: regardless of attempt count or `retry_after`
        /// hint, the returned delay never exceeds `max_delay`. The
        /// `1u128.checked_shl(attempt)` saturating path means
        /// `attempt = u32::MAX` should still bound the result.
        ///
        /// Covers both jittered and non-jittered policies — jitter
        /// picks within `[0, computed]`, so the cap holds.
        #[test]
        fn compute_delay_respects_max_delay_cap(
            policy in jittered_policy(),
            attempt in 0u32..=u32::MAX,
            hint_ms in proptest::option::of(0u64..=300_000u64),
        ) {
            let hint = hint_ms.map(Duration::from_millis);
            let delay = compute_delay(&policy, attempt, hint);
            prop_assert!(
                delay <= policy.max_delay,
                "delay {:?} exceeded max_delay {:?} (attempt={attempt}, hint={hint:?})",
                delay,
                policy.max_delay,
            );
        }

        /// Without jitter and without a `retry_after` hint, the
        /// exponential schedule is monotonically non-decreasing.
        /// Concretely: `delay(a) <= delay(b)` whenever `a <= b`.
        /// Catches base/exp/shift off-by-ones — including the
        /// `1u128.checked_shl` saturating path at `attempt >= 128`.
        #[test]
        fn compute_delay_is_monotonic_without_jitter_or_hint(
            policy in deterministic_policy(),
            a in 0u32..=200,
            b in 0u32..=200,
        ) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let dl = compute_delay(&policy, lo, None);
            let dh = compute_delay(&policy, hi, None);
            prop_assert!(
                dl <= dh,
                "non-monotonic: delay({lo})={dl:?} > delay({hi})={dh:?} for policy {policy:?}",
            );
        }

        /// A `retry_after` hint always wins over the computed
        /// exponential value AND is itself capped at `max_delay`.
        /// This is the explicit-server-control path documented in
        /// `should_retry_status` / `parse_retry_after`.
        #[test]
        fn compute_delay_with_hint_returns_capped_hint(
            policy in deterministic_policy(),
            attempt in 0u32..=200,
            hint_ms in 0u64..=120_000u64,
        ) {
            let hint = Duration::from_millis(hint_ms);
            let delay = compute_delay(&policy, attempt, Some(hint));
            prop_assert_eq!(delay, hint.min(policy.max_delay));
        }

        /// `u32::MAX` attempts shouldn't panic. The `1u128.checked_shl`
        /// path triggers above `attempt >= 128`; `saturating_mul` and
        /// the final `min(u64::MAX as u128)` cast must handle the
        /// saturated factor without overflow.
        ///
        /// Run only with the non-jittered policy because the jittered
        /// path is also exercised by the cap-invariant property.
        #[test]
        fn compute_delay_does_not_panic_at_overflow_attempts(
            policy in deterministic_policy(),
            attempt in (u32::MAX - 100)..=u32::MAX,
        ) {
            let delay = compute_delay(&policy, attempt, None);
            prop_assert!(delay <= policy.max_delay);
        }
    }
}
