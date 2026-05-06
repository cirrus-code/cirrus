//! Handlers for Salesforce REST API surfaces.
//!
//! The module layout mirrors how Salesforce organizes its own APIs: each
//! submodule corresponds to one platform-level API surface (sObjects, query,
//! composite, bulk, tooling, etc.). Top-level methods on [`Cloudburst`]
//! return handler structs (`client.sobjects("Account")`, `client.query(...)`,
//! `client.bulk()`) which then expose endpoint methods or further builders.
//!
//! Handler-level guidance:
//!
//! - **Never define user-facing record types here.** Anything that would
//!   carry org-specific fields stays generic over a caller-supplied `R`.
//! - **Default to `serde_json::Value`** for the untyped path; provide
//!   `_as::<T>()` variants for callers who want a typed deserialization.
//! - **Hard-coded types are fine for platform-level envelopes** (query
//!   result, create result, describe metadata, limits, version listings).
//!
//! [`Cloudburst`]: crate::Cloudburst

pub mod versions;
