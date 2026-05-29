//! Read-only smoke tests against a real org.
//!
//! These hit the read-only Metadata API surface:
//! - `describeMetadata` (catalog of supported types)
//! - `listMetadata` (enumeration of components)
//!
//! No writes; no test-data dependencies. Safe to run on any
//! sandbox/dev/scratch org without prior setup.

use crate::common::try_init_client;
use cirrus_metadata::ListMetadataQuery;

#[tokio::test]
#[ignore]
async fn endpoint_url_targets_soap_metadata_path() {
    let Some(md) = try_init_client().await else {
        return;
    };
    let url = md.endpoint_url();
    // The Metadata API endpoint is /services/Soap/m/{api_version} —
    // distinct from REST's /services/data/. Catching a regression
    // where the client points at the wrong subtree is cheap and would
    // otherwise only surface as cryptic 404s in production.
    assert!(
        url.contains("/services/Soap/m/"),
        "endpoint_url should target the SOAP Metadata API subtree; got {url}",
    );
    assert!(
        url.ends_with(md.api_version()),
        "endpoint_url should end with the configured API version ({}); got {url}",
        md.api_version(),
    );
}

#[tokio::test]
#[ignore]
async fn describe_metadata_returns_a_populated_catalog() {
    let Some(md) = try_init_client().await else {
        return;
    };
    let api_version = md.api_version().to_string();
    let catalog = md
        .describe_metadata(&api_version)
        .await
        .expect("describe_metadata should succeed against a real org");

    assert!(
        !catalog.metadata_objects.is_empty(),
        "real org should expose at least one metadata type",
    );

    // ApexClass is universally available across every edition and
    // every supported API version. If it's missing, either the org
    // configuration is unusual or our parser dropped types.
    let has_apex_class = catalog
        .metadata_objects
        .iter()
        .any(|o| o.xml_name == "ApexClass");
    assert!(
        has_apex_class,
        "describe_metadata should include ApexClass; got {} types",
        catalog.metadata_objects.len(),
    );

    // Spot-check that `directory_name` and `suffix` populate for at
    // least one type. These are the fields most exposed to xsi:nil
    // mis-parsing — if every entry came back with None, the fix to
    // `deserialize_nil_string` is over-broad.
    let with_dir = catalog
        .metadata_objects
        .iter()
        .filter(|o| o.directory_name.is_some())
        .count();
    assert!(
        with_dir > 0,
        "expected at least one metadata object to have a directory_name; got 0",
    );
}

#[tokio::test]
#[ignore]
async fn list_metadata_for_apex_class_returns_file_properties() {
    let Some(md) = try_init_client().await else {
        return;
    };
    let api_version = md.api_version().to_string();

    // ApexClass is universal. Result may be empty on a freshly-created
    // org, but the call must succeed.
    let results = md
        .list_metadata(
            vec![ListMetadataQuery {
                type_name: "ApexClass".to_string(),
                folder: None,
            }],
            &api_version,
        )
        .await
        .expect("list_metadata for ApexClass should succeed");

    // If there ARE results, verify the wire-shape contract — full_name
    // is required (per Salesforce), file_name should be populated.
    for fp in &results {
        assert!(
            !fp.full_name.is_empty(),
            "FileProperties.full_name should never be empty",
        );
        assert!(
            !fp.file_name.is_empty(),
            "FileProperties.file_name should never be empty for ApexClass",
        );
    }
}

#[tokio::test]
#[ignore]
async fn list_metadata_rejects_more_than_three_queries_pre_wire() {
    // Server enforces a 3-query max; the client short-circuits before
    // hitting the wire. Verify the short-circuit fires even when
    // talking to a real org configuration.
    let Some(md) = try_init_client().await else {
        return;
    };
    let api_version = md.api_version().to_string();

    let too_many = vec![
        ListMetadataQuery {
            type_name: "ApexClass".into(),
            folder: None,
        },
        ListMetadataQuery {
            type_name: "ApexTrigger".into(),
            folder: None,
        },
        ListMetadataQuery {
            type_name: "CustomLabels".into(),
            folder: None,
        },
        ListMetadataQuery {
            type_name: "CustomObject".into(),
            folder: None,
        },
    ];
    let err = md
        .list_metadata(too_many, &api_version)
        .await
        .expect_err("4 queries should be rejected before going to the wire");
    let msg = err.to_string();
    assert!(
        msg.contains("3") || msg.to_lowercase().contains("at most"),
        "error should mention the 3-query limit; got {msg}",
    );
}
