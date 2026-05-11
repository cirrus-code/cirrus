#![allow(clippy::print_stdout)]
//! Connect to Salesforce with a static access token and run a SOQL query.
//!
//! Run:
//!
//! ```bash
//! export SF_INSTANCE_URL=https://your-org.develop.my.salesforce.com
//! export SF_ACCESS_TOKEN=00D...!AQ...  # from `sf org display`
//! cargo run --example simple_query
//! ```

use cloudburst_sdk::Cloudburst;
use cloudburst_sdk::auth::StaticTokenAuth;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let instance_url = std::env::var("SF_INSTANCE_URL")?;
    let access_token = std::env::var("SF_ACCESS_TOKEN")?;

    let auth = Arc::new(StaticTokenAuth::new(access_token, instance_url));
    let sf = Cloudburst::builder().auth(auth).build()?;

    let result = sf.query("SELECT Id, Name FROM Account LIMIT 5").await?;

    println!("total_size: {}", result.total_size);
    println!("done:       {}", result.done);
    println!();
    for record in &result.records {
        let id = record.get("Id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = record.get("Name").and_then(|v| v.as_str()).unwrap_or("?");
        println!("{id}  {name}");
    }

    Ok(())
}
