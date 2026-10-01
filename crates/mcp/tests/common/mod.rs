use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use dg_lab_link_core::ControlError;
use dg_lab_link_runtime::LocalConfig;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::time::timeout;

pub struct HttpProcess {
    child: Child,
}

impl HttpProcess {
    pub async fn start(directory: &Path, config: &LocalConfig) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp"))
            .args(["--transport", "http", "--config-dir"])
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    let mut stderr = String::new();
                    child
                        .stderr
                        .take()
                        .unwrap()
                        .read_to_string(&mut stderr)
                        .await
                        .unwrap();
                    panic!("HTTP MCP exited before readiness ({status}): {stderr}");
                }
                if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("independent HTTP MCP process started");
        Self { child }
    }

    pub fn pid(&self) -> u32 {
        self.child.id().unwrap()
    }

    pub async fn wait_core_closed(mut self) {
        let status = timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("HTTP observer exits after the core closes")
            .unwrap();
        assert!(!status.success());
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .await
            .unwrap();
        let error: ControlError = serde_json::from_str(&stderr).unwrap();
        assert_eq!(error.code, "core_closed");
        let mut stdout = Vec::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .await
            .unwrap();
        assert!(
            stdout.is_empty(),
            "HTTP diagnostics do not contain tokens or pollute stdout"
        );
    }
}

pub async fn request(config: &LocalConfig, method: &str, params: Value) -> Value {
    let response = reqwest::Client::new()
        .post(config.mcp_url())
        .bearer_auth(&config.token)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}
