//! `GET /services/data/vXX.X/limits` — per-org limit allocations.
//!
//! Returns a flat map keyed by limit name. Most entries are
//! `{Max, Remaining}` pairs; a few (e.g. `PermissionSets`) embed sub-limits
//! captured in [`Limit::nested`].
//!
//! Salesforce notes: values are accurate within five minutes of resource
//! consumption — avoid concurrent or rapid polling. The endpoint requires
//! API version 29.0+ and the *View Setup and Configuration* permission.
//!
//! [`Limit::nested`]: crate::Limit::nested

use crate::Cirrus;
use crate::error::CirrusResult;
use crate::response::OrgLimits;

impl Cirrus {
    /// Fetches the org's limits.
    ///
    /// Calls `GET /services/data/{api_version}/limits`. Returns a map keyed
    /// by limit name (e.g. `"DailyApiRequests"`).
    pub async fn limits(&self) -> CirrusResult<OrgLimits> {
        self.get("limits").await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::Cirrus;
    use crate::auth::StaticTokenAuth;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn limits_returns_parsed_map() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data/v60.0/limits"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "DailyApiRequests": {"Max": 5000, "Remaining": 4937},
                "DataStorageMB": {"Max": 1024, "Remaining": 1024},
                "PermissionSets": {
                    "Max": 1500,
                    "Remaining": 1499,
                    "CreateCustom": {"Max": 1000, "Remaining": 999}
                }
            })))
            .mount(&server)
            .await;

        let auth = Arc::new(StaticTokenAuth::new("tok", server.uri()));
        let sf = Cirrus::builder().auth(auth).build().unwrap();

        let limits = sf.limits().await.unwrap();
        assert_eq!(limits.len(), 3);
        let daily = limits.get("DailyApiRequests").unwrap();
        assert_eq!(daily.max, 5000);
        assert_eq!(daily.remaining, 4937);
        assert!(daily.nested.is_empty());

        let perm = limits.get("PermissionSets").unwrap();
        assert_eq!(perm.max, 1500);
        let custom = perm.nested.get("CreateCustom").unwrap();
        assert_eq!(custom.max, 1000);
        assert_eq!(custom.remaining, 999);
    }

    #[tokio::test]
    async fn limits_surfaces_api_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data/v60.0/limits"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(serde_json::json!([{
                    "message": "The user does not have View Setup and Configuration permission",
                    "errorCode": "INSUFFICIENT_ACCESS"
                }])),
            )
            .mount(&server)
            .await;

        let auth = Arc::new(StaticTokenAuth::new("tok", server.uri()));
        let sf = Cirrus::builder().auth(auth).build().unwrap();

        let err = sf.limits().await.unwrap_err();
        match err {
            crate::CirrusError::Api { status, errors, .. } => {
                assert_eq!(status, 403);
                assert_eq!(errors[0].error_code, "INSUFFICIENT_ACCESS");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
