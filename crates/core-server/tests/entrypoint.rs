use std::process::{Child, Command, Stdio};
use std::time::Duration;

use dg_lab_link_runtime::{Client, LocalConfig};
use serde_json::Value;
use tokio::time::{sleep, timeout};

struct CoreProcess(Child);

impl Drop for CoreProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn standalone_core_reports_port_conflict_without_changing_config() {
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = listener.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dg-lab-link-core"))
        .args(["--json", "--config-dir"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "runtime_bind_failed");
    let unchanged = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(unchanged.port, config.port);
    assert_eq!(unchanged.token, config.token);
}

#[tokio::test]
async fn standalone_core_accepts_clients_and_exits_after_last_release() {
    let directory = tempfile::tempdir().unwrap();
    let port_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = port_reservation.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    drop(port_reservation);
    let mut core = CoreProcess(
        Command::new(env!("CARGO_BIN_EXE_dg-lab-link-core"))
            .args(["--json", "--config-dir"])
            .arg(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let first = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(client) = Client::connect(directory.path(), "GUI", None).await {
                break client;
            }
            assert!(
                core.0.try_wait().unwrap().is_none(),
                "core exited before handshake"
            );
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("standalone core started");
    let second = Client::connect(directory.path(), "CLI", None)
        .await
        .unwrap();
    let first_info = first.runtime_info().await.unwrap();
    let second_info = second.runtime_info().await.unwrap();
    assert_eq!(first_info.pid, core.0.id());
    assert_eq!(first_info.instance_id, second_info.instance_id);
    assert_eq!(first_info.holder_count, 2);
    assert_eq!(
        serde_json::to_value(first.snapshot()).unwrap()["connection"]["state"],
        "disconnected"
    );
    first.release().await.unwrap();
    assert_eq!(second.runtime_info().await.unwrap().holder_count, 1);
    assert!(core.0.try_wait().unwrap().is_none());
    second.release().await.unwrap();
    let status = timeout(Duration::from_secs(12), async {
        loop {
            if let Some(status) = core.0.try_wait().unwrap() {
                break status;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("standalone core exits after the last release");
    assert!(status.success());
}
