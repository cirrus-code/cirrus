//! `GET /services/data` — list of supported Salesforce REST API versions.
//!
//! This is the simplest endpoint Salesforce exposes: it returns a JSON array
//! of [`ApiVersion`] entries describing every API version available on the
//! org. It does require auth (a bearer token), but lives outside the
//! versioned `/services/data/{version}` tree.

use crate::Cloudburst;
use crate::error::CloudburstResult;
use crate::response::ApiVersion;

impl Cloudburst {
    /// Fetches the list of REST API versions this org supports.
    ///
    /// Calls `GET /services/data` (unversioned). Useful for discovering the
    /// latest available `vNN.N` to use with [`CloudburstBuilder::api_version`].
    ///
    /// [`CloudburstBuilder::api_version`]: crate::CloudburstBuilder::api_version
    pub async fn versions(&self) -> CloudburstResult<Vec<ApiVersion>> {
        self.get_unversioned("/services/data").await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::Cloudburst;
    use crate::auth::StaticTokenAuth;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn versions_returns_parsed_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"label": "Winter '24", "url": "/services/data/v60.0", "version": "60.0"},
                {"label": "Spring '24", "url": "/services/data/v61.0", "version": "61.0"}
            ])))
            .mount(&server)
            .await;

        let auth = Arc::new(StaticTokenAuth::new("tok", server.uri()));
        let sf = Cloudburst::builder().auth(auth).build().unwrap();

        let versions = sf.versions().await.unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, "60.0");
        assert_eq!(versions[1].label, "Spring '24");
    }

    #[tokio::test]
    async fn versions_surfaces_api_error_array() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/services/data"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!([
                {"message": "Session expired or invalid", "errorCode": "INVALID_SESSION_ID"}
            ])))
            .mount(&server)
            .await;

        let auth = Arc::new(StaticTokenAuth::new("tok", server.uri()));
        let sf = Cloudburst::builder().auth(auth).build().unwrap();

        let err = sf.versions().await.unwrap_err();
        match err {
            crate::CloudburstError::Api {
                status,
                errors,
                raw,
            } => {
                assert_eq!(status, 401);
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].error_code, "INVALID_SESSION_ID");
                assert!(raw.is_none());
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
