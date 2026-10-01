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

#[tokio::main]
async fn main() -> ExitCode {
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
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error, args.json);
            ExitCode::FAILURE
        }
    }
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
