//! Shared business contracts; no device, audio, plugin process or GUI implementation.
pub mod control;
pub mod hub;
pub mod model;
pub mod preferences;
pub mod sources;
pub mod transport;
pub mod waveforms;
pub use control::{CommandDescriptor, ControlCommand, ControlError};
pub use dg_lab_link_plugin_sdk::{ActionParams, InputParams, UiParams};
