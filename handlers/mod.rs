//! Handlers for Salesforce REST API surfaces.
//!
//! Each submodule corresponds to one platform-level API surface (sObjects,
//! query, composite, bulk, tooling, etc.). Top-level methods on
//! [`Cirrus`](crate::Cirrus) return either a handler struct
//! (`sf.sobject("Account")`, `sf.bulk()`) or invoke a verb directly
//! (`sf.query(...)`, `sf.versions()`).
//!
//! Methods that return records typically come in two variants: a default
//! returning [`serde_json::Value`] and an `_as::<R>()` variant that
//! deserializes into a caller-supplied type. Paginated GET endpoints add
//! `_stream` / `_stream_as` variants returning
//! [`crate::pagination::Records`].

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
