//! Typed handlers for Metadata API operations.
//!
//! Each submodule corresponds to one operation family. Handlers
//! construct [`SoapOperation`](crate::SoapOperation) implementations
//! and dispatch them through [`MetadataClient::call`](crate::MetadataClient::call).
//!
//! Public methods on [`MetadataClient`](crate::MetadataClient) are
//! defined in these submodules via `impl` blocks, so callers see
//! everything at the top level (`md.deploy(...)`, `md.retrieve(...)`,
//! etc.) without having to import handler types.

pub mod crud;
pub mod file_based;
pub mod utility;
