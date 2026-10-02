use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dg_lab_link_core::ControlError;
use dg_lab_link_runtime::{config_dir, run_core};

#[derive(Debug, Parser)]
#[command(
    name = "dg-lab-link-core",
    version,
    about = "DG-LAB Link 共享业务核心与本机控制接口"
)]
struct CoreArgs {
    /// 输出机器可读错误，供 GUI 和 CLI 启动器使用
    #[arg(long)]
    json: bool,
    /// 本地配置目录；默认与 GUI 和 CLI 共用
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// 本机监听端口；绑定成功后保存到配置
    #[arg(long)]
    port: Option<u16>,
    /// 覆盖 Socket V4 Relay 地址，用于本机模拟与排障
    #[arg(long)]
    relay_endpoint: Option<String>,
}

fn main() -> ExitCode {
    let args = match CoreArgs::try_parse() {
        Ok(args) => args,
        Err(error) if error.use_stderr() => {
            if std::env::args().any(|arg| arg == "--json") {
                report_error(
                    &ControlError::new("invalid_arguments", error.to_string()),
                    true,
                );
            } else {
                let _ = error.print();
            }
            return ExitCode::from(2);
        }
        Err(error) => {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
    };
    match run_with_runtime(run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error, args.json);
            ExitCode::FAILURE
        }
    }
}

fn run_with_runtime<F: std::future::Future<Output = Result<(), ControlError>>>(
    future: F,
) -> Result<(), ControlError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ControlError::new("runtime_start_failed", error.to_string()))?;
    let result = runtime.block_on(future);
    // run_core has already enforced the shared ten-second cleanup deadline.
    // Uninterruptible filesystem workers must not add an unbounded runtime Drop
    // wait; returning from main ends this dedicated core process and its threads.
    runtime.shutdown_timeout(std::time::Duration::ZERO);
    result
}

async fn run(args: &CoreArgs) -> Result<(), ControlError> {
    let directory = match &args.config_dir {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => std::env::current_dir()?.join(path),
        None => config_dir()?,
    };
    run_core(directory, args.port, args.relay_endpoint.clone()).await
}

fn report_error(error: &ControlError, json: bool) {
    if json {
        eprintln!(
            "{}",
            serde_json::to_string(error).expect("ControlError 可序列化")
        );
    } else {
        eprintln!("{}: {}", error.code, error.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_exit_does_not_wait_for_blocked_filesystem_workers() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        let began = std::time::Instant::now();
        run_with_runtime(async move {
            tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
            });
            ready.await.unwrap();
            Ok(())
        })
        .unwrap();
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
        release.send(()).unwrap();
    }
}
