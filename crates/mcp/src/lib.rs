mod handler;
mod http;
mod stdio;

pub use http::serve_http_mcp;
pub use stdio::serve_stdio_mcp;

pub use dg_lab_link_runtime::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};

const SOCKET_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
