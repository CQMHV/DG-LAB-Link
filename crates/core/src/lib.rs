pub mod control;
pub mod dglab;
pub mod hub;
pub mod model;
pub mod preferences;
pub mod sources;
pub mod transport;
pub mod waveforms;

pub use control::{ControlCommand, ControlError, ControlService};

pub fn initialize_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
