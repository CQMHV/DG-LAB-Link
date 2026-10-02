//! First-party input sources, built as ordinary native plugins.
//!
//! This crate deliberately has no dependency on the DG-LAB Link core. The copied
//! algorithms own capture, input leases and mapping; only the public plugin SDK
//! connects them to a host and its device bindings.

pub mod model;
pub mod plugins;
pub mod sources;
