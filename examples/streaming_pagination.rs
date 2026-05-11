#![allow(clippy::print_stdout)]
//! Iterate every Account in the org via `query_stream`, walking
//! `nextRecordsUrl` locators transparently. The stream is lazy — each
//! page is fetched only when the previous page's buffer is drained.
//!
//! `query_stream` returns a `futures::Stream`, which is runtime-agnostic.
//! We use `tokio` here only because reqwest's transport needs it.
//!
//! Run:
//!
//! ```bash
//! export SF_INSTANCE_URL=https://your-org.develop.my.salesforce.com
//! export SF_ACCESS_TOKEN=00D...!AQ...
//! cargo run --example streaming_pagination
//! ```

use cloudburst_sdk::Cloudburst;
use cloudburst_sdk::auth::StaticTokenAuth;
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auth = Arc::new(StaticTokenAuth::new(
        std::env::var("SF_ACCESS_TOKEN")?,
        std::env::var("SF_INSTANCE_URL")?,
    ));
    let sf = Cloudburst::builder().auth(auth).build()?;

    let mut stream = sf.query_stream("SELECT Id, Name FROM Account ORDER BY Name");

    let mut count = 0usize;
    while let Some(item) = stream.next().await {
        let record = item?;
        let name = record.get("Name").and_then(|v| v.as_str()).unwrap_or("?");
        println!("{name}");
        count += 1;
        // Bound the demo so it doesn't print millions on a big org.
        if count >= 50 {
            println!("(stopping after 50 — stream would continue paginating)");
            break;
        }
    }

    Ok(())
}
