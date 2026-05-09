//! Handlers for Salesforce REST API surfaces.
//!
//! The module layout mirrors how Salesforce organizes its own APIs: each
//! submodule corresponds to one platform-level API surface (sObjects, query,
//! composite, bulk, tooling, etc.). Top-level methods on [`Cloudburst`]
//! return either a *handler struct* (`client.sobjects(...)`, `client.bulk()`)
//! or a directly-invokable *verb* (`client.query(...)`, `client.versions()`).
//!
//! # The canonical handler module template
//!
//! Two patterns are blessed; pick by surface shape:
//!
//! ### Pattern A — verb method on [`Cloudburst`]
//!
//! For surfaces with **no intrinsic scoping** — a single entry point, no
//! per-call configuration beyond the request body. Examples:
//! [`Cloudburst::query`], [`Cloudburst::versions`], [`Cloudburst::search`].
//! No handler struct; methods live in `impl Cloudburst { ... }` blocks
//! inside the surface's module.
//!
//! ### Pattern B — handler struct returned by a method on [`Cloudburst`]
//!
//! For surfaces with **intrinsic scoping** (per-object / per-job / per-flow)
//! or **multiple sub-surfaces**. The handler holds `&'a Cloudburst` and
//! exposes endpoint methods or returns sub-handlers. Examples:
//!
//! - [`Cloudburst::sobject`]`("Account")` → [`SObjectHandler`] — scoped to
//!   one sObject by name.
//! - [`Cloudburst::bulk`]`()` → [`BulkHandler`] → `.ingest()` /
//!   `.query()` → sub-handlers — multi-flow API.
//! - [`Cloudburst::composite`]`()` → [`CompositeHandler`] →
//!   `.sobjects()` → sub-handler — nested surface with its own API.
//!
//! Sub-handlers follow the same shape: returned by a method on the parent
//! handler, hold `&'a Cloudburst` (not `&'a ParentHandler`).
//!
//! ### Choosing between A and B
//!
//! | Signal | Pattern A (verb) | Pattern B (handler) |
//! |---|---|---|
//! | "What does this surface need to know upfront?" | nothing | a name / scope |
//! | Method count | 1–4 closely related | 5+ or naturally grouped |
//! | Sub-surfaces? | no | yes |
//! | Reads naturally as | `sf.query(...)` | `sf.bulk().query().create(...)` |
//!
//! When in doubt, start with Pattern A — promote to Pattern B if you find
//! yourself wanting to group methods or scope by an identifier.
//!
//! # Universal rules (both patterns)
//!
//! 1. **Layer over the public verb methods on [`Cloudburst`]**
//!    (`get`, `get_with_query`, `post`, `put`, `patch`, `delete`). Use
//!    [`crate::Cloudburst::versioned_segments`] + `send_at` only when a
//!    path segment needs percent-encoding (e.g., upsert by external ID
//!    with `/` in the value). **Don't introduce parallel transport
//!    code** — the four send paths in `lib.rs` are the sanctioned set.
//!
//! 2. **Never define user-facing record types.** Records produced by
//!    Salesforce that carry org-specific fields stay generic over a
//!    caller-supplied `R: DeserializeOwned`. Hard-coded structs are
//!    only for platform-level envelopes — query result, create result,
//!    describe metadata, limits, version listings — whose shape is part
//!    of the platform contract.
//!
//! 3. **Untyped + typed variants.** Every method that returns records
//!    (or generic JSON) exposes two variants:
//!    - The default returns [`serde_json::Value`] (or a typed envelope
//!      generic over `Value`).
//!    - The `_as::<R>()` variant returns the same shape but
//!      deserialized into a caller-supplied `R`.
//!
//!    See `query` / `query_as` / `query_all` / `query_all_as` for the
//!    canonical example.
//!
//! 4. **Pagination → `Records<R>`.** Handlers with paginated GET
//!    endpoints add `_stream` / `_stream_as` variants returning
//!    [`crate::pagination::Records<R>`] (a `futures::Stream`). Builds
//!    the initial-page future via the same handler method as the
//!    one-shot variant; `Records` walks `nextRecordsUrl` from there.
//!    See `query_stream` / `tooling().query_stream` for the pattern.
//!
//! 5. **Complex inputs → typed request struct.** When an endpoint takes
//!    a non-trivial body (Bulk job spec, composite envelope, multipart
//!    blob params), define a typed `Spec` / `Request` struct in the
//!    handler's module and re-export it at the crate root.
//!    Examples: [`crate::BulkIngestSpec`], [`crate::BatchRequest`],
//!    [`crate::BlobUploadSpec`]. Callers construct with struct
//!    literals; we don't add a `Builder` unless the struct gets
//!    awkward (>5 fields with Optional).
//!
//! 6. **Errors flow through the standard parser.** All non-2xx
//!    responses are parsed by [`crate::error`]'s shared logic — handler
//!    code never invents a new error parser. The diverged
//!    `CompositeError` shape (with `statusCode` instead of `errorCode`)
//!    is parsed by the `composite::sobjects` and `composite::tree`
//!    handlers because Salesforce specifically uses that shape there;
//!    it's the only such exception.
//!
//! # Module layout per handler
//!
//! ```text
//! handlers/{name}.rs
//!   //! Doc comment with module overview, wire shape, and any wire
//!   //! warts worth flagging.
//!
//!   use crate::Cloudburst;
//!   use crate::error::CloudburstResult;
//!   use crate::response::{...};      // platform envelope types only
//!   use serde::de::DeserializeOwned;
//!   use serde_json::Value;
//!
//!   impl Cloudburst {
//!       pub fn {name}(&self) -> {Name}Handler<'_> { ... }   // for Pattern B
//!       // or:
//!       pub async fn {verb}(&self, ...) -> CloudburstResult<...> { ... }  // Pattern A
//!   }
//!
//!   #[derive(Debug)]
//!   pub struct {Name}Handler<'a> { client: &'a Cloudburst, ... }
//!
//!   impl<'a> {Name}Handler<'a> {
//!       pub async fn endpoint(&self, ...) -> CloudburstResult<Value> {
//!           self.endpoint_as().await
//!       }
//!       pub async fn endpoint_as<R: DeserializeOwned>(&self, ...) -> CloudburstResult<R> {
//!           self.client.get_with_query("path", &query).await
//!       }
//!   }
//!
//!   // Optional: typed request structs for complex inputs
//!   pub struct {Name}Spec<'a> { ... }
//!
//!   #[cfg(test)]
//!   #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//!   mod tests {
//!       // wiremock-backed tests citing the doc page they verify
//!   }
//! ```
//!
//! # Test conventions
//!
//! Each handler ships with wiremock-backed tests covering: happy path,
//! error array, and any wire-shape edge cases documented for the
//! endpoint (partial-success semantics, header cursors, etc.). Mock
//! JSON should match the doc's example response body byte-for-byte
//! where possible — see the doc-driven audit lessons in `CLAUDE.md`.
//!
//! [`Cloudburst`]: crate::Cloudburst
//! [`Cloudburst::query`]: crate::Cloudburst::query
//! [`Cloudburst::versions`]: crate::Cloudburst::versions
//! [`Cloudburst::search`]: crate::Cloudburst::search
//! [`Cloudburst::sobject`]: crate::Cloudburst::sobject
//! [`Cloudburst::bulk`]: crate::Cloudburst::bulk
//! [`Cloudburst::composite`]: crate::Cloudburst::composite
//! [`SObjectHandler`]: crate::handlers::sobjects::SObjectHandler
//! [`BulkHandler`]: crate::handlers::bulk::BulkHandler
//! [`CompositeHandler`]: crate::handlers::composite::CompositeHandler

pub mod apex;
pub mod bulk;
pub mod composite;
pub mod event_monitoring;
pub mod limits;
pub mod query;
pub mod search;
pub mod sobjects;
pub mod tooling;
pub mod versions;
