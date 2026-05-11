#![allow(clippy::print_stdout)]
//! Full create → retrieve → update → delete cycle on an `Account`,
//! using typed deserialization for the retrieve step.
//!
//! Run:
//!
//! ```bash
//! export SF_INSTANCE_URL=https://your-org.develop.my.salesforce.com
//! export SF_ACCESS_TOKEN=00D...!AQ...
//! cargo run --example crud_cycle
//! ```

use cloudburst_sdk::Cloudburst;
use cloudburst_sdk::auth::StaticTokenAuth;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
struct Account {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Description")]
    description: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auth = Arc::new(StaticTokenAuth::new(
        std::env::var("SF_ACCESS_TOKEN")?,
        std::env::var("SF_INSTANCE_URL")?,
    ));
    let sf = Cloudburst::builder().auth(auth).build()?;
    let accounts = sf.sobject("Account");

    // Create
    let created = accounts
        .create(&json!({
            "Name": "cloudburst-sdk example",
            "Description": "initial",
        }))
        .await?;
    println!("created: id={}, success={}", created.id, created.success);

    // Retrieve (typed)
    let account: Account = accounts
        .retrieve_with_fields_as(&created.id, &["Id", "Name", "Description"])
        .await?;
    println!(
        "retrieved: id={}, name={}, description={:?}",
        account.id, account.name, account.description,
    );

    // Update
    accounts
        .update(&created.id, &json!({ "Description": "updated" }))
        .await?;
    println!("updated: Description -> 'updated'");

    // Delete
    accounts.delete(&created.id).await?;
    println!("deleted: {}", created.id);

    Ok(())
}
