//! Retry policy and backoff machinery for transient HTTP failures.
//!
//! Salesforce's REST surface (and any HTTP API) periodically emits
//! transient failures — rate limits (429), service unavailable (503),
//! short-lived 5xx, network blips. A small amount of automatic retry
//! with exponential backoff hides almost all of these from the caller
//! without sacrificing correctness.
//!
//! This module is designed around three principles:
//!
//! 1. **Default to "do less."** Only retry status codes that the spec
//!    explicitly says are retryable (429, 503), or 5xx codes on
//!    request methods that are spec-idempotent (GET, HEAD, DELETE,
//!    PUT). Retry-on-POST is opt-out-only territory because a
//!    duplicate `INSERT` is a worse failure mode than a one-shot
//!    error surfaced to the caller.
//! 2. **Honor server hints.** When the server provides a
//!    [`Retry-After`] header (RFC 7231 §7.1.3 delta-seconds form), use
//!    that delay instead of our backoff schedule.
//! 3. **Jitter to avoid thundering herd.** Default policy applies
//!    *full jitter* — random uniform `[0, computed_delay]` — per
//!    AWS's recommendations for distributed clients hitting a shared
//!    backend.
//!
//! [`Retry-After`]: https://datatracker.ietf.org/doc/html/rfc7231#section-7.1.3

use crate::error::CirrusError;
use std::time::Duration;

/// Configuration for retry-on-transient-failure behavior.
///
/// Construct via [`RetryPolicy::default`] for sensible defaults, or
/// [`RetryPolicy::none`] to disable retries entirely. All fields are
/// public for ad-hoc tweaking.
///
/// # Example
///
/// ```no_run
/// use cirrus::{Cirrus, RetryPolicy, auth::StaticTokenAuth};
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// # fn example() -> Result<(), cirrus::CirrusError> {
/// let policy = RetryPolicy {
///     max_retries: 5,
///     base_delay: Duration::from_millis(250),
///     ..RetryPolicy::default()
/// };
/// let auth = Arc::new(StaticTokenAuth::new("tok", "https://x.my.salesforce.com"));
/// let sf = Cirrus::builder()
///     .auth(auth)
///     .retry_policy(policy)
///     .build()?;
/// # let _ = sf;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of *additional* attempts after the initial
    /// request. `0` disables retries; `3` (default) means up to four
    /// total attempts.
    pub max_retries: u32,
    /// Base delay for the exponential backoff schedule. Default
    /// 100 ms.
    pub base_delay: Duration,
    /// Cap on the computed backoff delay — prevents pathological
    /// growth on high attempt counts. Default 30 s.
    pub max_delay: Duration,
    /// Apply *full jitter* — pick a random delay in `[0, computed]`
    /// rather than using the deterministic exponential value. Default
    /// `true`, recommended for distributed clients.
    pub jitter: bool,
    /// When `true`, retry idempotent methods (GET, HEAD, DELETE, PUT)
    /// on transient 5xx errors (500, 502, 504). When `false`, only
    /// retry on 429 / 503 (which Salesforce explicitly documents as
    /// "didn't happen, retry"). Default `true`.
    ///
    /// Non-idempotent methods (POST, PATCH) are *never* retried on
    /// 5xx, regardless of this flag — duplicate-record risk outweighs
    /// the convenience.
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
    /// A policy that disables retries. Useful for non-idempotent
    /// flows you've audited yourself, or for tests that want
    /// deterministic single-shot semantics.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }
}

/// Decision point: should we retry this HTTP response?
///
/// `attempt` is the zero-indexed *previous* attempt count — i.e. on
/// the first call after the initial failure, `attempt == 0`. This
/// makes the comparison `attempt < max_retries` directly express
/// "have we already retried fewer times than the cap?".
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
        // Salesforce explicitly documents these as retryable. The
        // server is asserting the request did *not* take effect.
        429 | 503 => true,
        // Other 5xx — retry only if the method is spec-idempotent.
        500 | 502 | 504 if policy.retry_idempotent_5xx => is_idempotent(method),
        _ => false,
    }
}

/// Decision point: should we retry this network-level failure?
///
/// Network errors (DNS resolution failure, connection refused,
/// connection reset, timeout) are ambiguous — the server may or may
/// not have processed the request before the connection dropped. We
/// only retry idempotent methods, where a duplicated effect is
/// harmless.
pub(crate) fn should_retry_network(
    policy: &RetryPolicy,
    method: &reqwest::Method,
    error: &CirrusError,
    attempt: u32,
) -> bool {
    if attempt >= policy.max_retries {
        return false;
    }
    if !is_idempotent(method) {
        return false;
    }
    matches!(error, CirrusError::Http(_))
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
///
/// The HTTP-date variant of `Retry-After` is also valid per spec but
/// we don't accept it — Salesforce documentation only shows the
/// integer form, and the date-parsing surface isn't worth pulling in
/// `httpdate` for this niche.
pub(crate) fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?;
    let s = raw.to_str().ok()?;
    s.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Compute the next backoff delay.
///
/// Precedence:
/// 1. If a `Retry-After` hint is present, honor it (capped at
///    [`max_delay`](RetryPolicy::max_delay)).
/// 2. Otherwise compute `base_delay * 2^attempt`, capped at
///    `max_delay`.
/// 3. If [`jitter`](RetryPolicy::jitter) is enabled, sample uniformly
///    from `[0, computed]`. If the random source fails, fall back to
///    the deterministic value.
pub(crate) fn compute_delay(
    policy: &RetryPolicy,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    if let Some(hint) = retry_after {
        let capped = hint.min(policy.max_delay);
        tracing::warn!(
            target: "cirrus::retry",
            attempt = attempt + 1,
            delay_ms = capped.as_millis() as u64,
            source = "retry-after-header",
            "scheduling request retry",
        );
        return capped;
    }
    // base_delay * 2^attempt, in milliseconds, saturating on overflow.
    let factor: u128 = 1u128.checked_shl(attempt).unwrap_or(u128::MAX);
    let computed_ms = policy.base_delay.as_millis().saturating_mul(factor);
    // Saturate to max_delay so we never sleep more than the cap.
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
                // Random source down — degrade gracefully to deterministic.
                computed
            } else {
                let r = u64::from_le_bytes(buf) % (max_ms + 1);
                Duration::from_millis(r)
            }
        }
    };
    tracing::warn!(
        target: "cirrus::retry",
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
        assert!(p.retry_idempotent_5xx);
    }

    #[test]
    fn none_policy_disables_retry() {
        let p = RetryPolicy::none();
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 429, 0));
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 503, 0));
    }

    #[test]
    fn retries_429_and_503_for_any_method() {
        let p = RetryPolicy::default();
        for m in [
            reqwest::Method::GET,
            reqwest::Method::POST,
            reqwest::Method::PATCH,
            reqwest::Method::DELETE,
        ] {
            assert!(should_retry_status(&p, &m, 429, 0), "429 retry for {m}");
            assert!(should_retry_status(&p, &m, 503, 0), "503 retry for {m}");
        }
    }

    #[test]
    fn retries_5xx_only_for_idempotent_methods() {
        let p = RetryPolicy::default();
        for status in [500, 502, 504] {
            assert!(should_retry_status(&p, &reqwest::Method::GET, status, 0));
            assert!(should_retry_status(&p, &reqwest::Method::DELETE, status, 0));
            assert!(should_retry_status(&p, &reqwest::Method::PUT, status, 0));
            // Non-idempotent — never retry.
            assert!(!should_retry_status(&p, &reqwest::Method::POST, status, 0));
            assert!(!should_retry_status(&p, &reqwest::Method::PATCH, status, 0));
        }
    }

    #[test]
    fn does_not_retry_4xx_caller_errors() {
        let p = RetryPolicy::default();
        for status in [400, 401, 403, 404, 405, 422] {
            assert!(
                !should_retry_status(&p, &reqwest::Method::GET, status, 0),
                "should not retry {status}"
            );
        }
    }

    #[test]
    fn stops_retrying_at_max_retries() {
        let p = RetryPolicy::default();
        assert!(should_retry_status(&p, &reqwest::Method::GET, 429, 0));
        assert!(should_retry_status(&p, &reqwest::Method::GET, 429, 2));
        // attempt == 3 means we've already retried 3 times — stop.
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 429, 3));
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 429, 99));
    }

    #[test]
    fn retry_5xx_disabled_skips_other_5xx_but_keeps_429_503() {
        let p = RetryPolicy {
            retry_idempotent_5xx: false,
            ..RetryPolicy::default()
        };
        assert!(should_retry_status(&p, &reqwest::Method::GET, 429, 0));
        assert!(should_retry_status(&p, &reqwest::Method::GET, 503, 0));
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 500, 0));
        assert!(!should_retry_status(&p, &reqwest::Method::GET, 502, 0));
    }

    #[test]
    fn parse_retry_after_handles_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("5"),
        );
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_retry_after_returns_none_for_http_date_form() {
        // We don't support the HTTP-date variant — returning None
        // makes the caller fall back to the backoff schedule.
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn parse_retry_after_returns_none_when_absent() {
        let h = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn compute_delay_honors_retry_after_capped_at_max() {
        let p = RetryPolicy {
            max_delay: Duration::from_secs(10),
            ..RetryPolicy::default()
        };
        // Hint within cap → use it.
        assert_eq!(
            compute_delay(&p, 0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        // Hint over cap → clamp.
        assert_eq!(
            compute_delay(&p, 0, Some(Duration::from_secs(99))),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn compute_delay_caps_exponential_at_max_delay() {
        // No jitter so we can assert exact values.
        let p = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            jitter: false,
            ..RetryPolicy::default()
        };
        assert_eq!(compute_delay(&p, 0, None), Duration::from_millis(100));
        assert_eq!(compute_delay(&p, 1, None), Duration::from_millis(200));
        assert_eq!(compute_delay(&p, 2, None), Duration::from_millis(400));
        assert_eq!(compute_delay(&p, 3, None), Duration::from_millis(800));
        // 100ms * 2^4 = 1600ms → clamped to 1000ms (max_delay).
        assert_eq!(compute_delay(&p, 4, None), Duration::from_secs(1));
        // Way beyond cap — still clamped, no overflow.
        assert_eq!(compute_delay(&p, 100, None), Duration::from_secs(1));
    }

    #[test]
    fn compute_delay_jitter_stays_within_bounds() {
        let p = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            jitter: true,
            ..RetryPolicy::default()
        };
        // 100ms * 2^2 = 400ms ceiling.
        for _ in 0..50 {
            let d = compute_delay(&p, 2, None);
            assert!(d <= Duration::from_millis(400));
        }
    }

    #[test]
    fn compute_delay_with_zero_base_returns_zero() {
        let p = RetryPolicy {
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            jitter: true,
            ..RetryPolicy::default()
        };
        assert_eq!(compute_delay(&p, 0, None), Duration::ZERO);
        assert_eq!(compute_delay(&p, 5, None), Duration::ZERO);
    }
}
