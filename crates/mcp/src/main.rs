use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use dg_lab_link_core::ControlError;
use dg_lab_link_mcp::{serve_http_mcp, serve_stdio_mcp};
use dg_lab_link_runtime::{Client, LocalConfig, config_dir, connect_or_spawn, core_executable};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

#[derive(Debug, Parser)]
#[command(
    name = "dg-lab-link-mcp",
    version,
    about = "DG-LAB Link MCP 接入；stdio 自动持有核心，HTTP 观察已运行核心"
)]
struct Args {
    /// MCP 传输：stdio 默认自动持有核心；http 不启动或持有核心
    #[arg(long, value_enum, default_value = "stdio")]
    transport: Transport,
    /// 本地配置目录；默认与 GUI、CLI 及 HTTP MCP 共用
    #[arg(long)]
    config_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) if error.use_stderr() => {
            report_error(&ControlError::new("invalid_arguments", error.to_string()));
            return ExitCode::from(2);
        }
        Err(error) => {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report_error(&error.into());
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(args));
    // Tokio's stdin reader uses a blocking OS read. Core shutdown or an output
    // failure must still let this process exit while the parent's stdin is open.
    runtime.shutdown_timeout(Duration::from_millis(100));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), ControlError> {
    prevent_standard_handle_inheritance()?;
    let directory = match args.config_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => std::env::current_dir()?.join(path),
        None => config_dir()?,
    };
    match args.transport {
        Transport::Stdio => {
            let client =
                connect_or_spawn(&directory, &core_executable()?, "MCP stdio", None).await?;
            serve_stdio_mcp(client, tokio::io::stdin(), tokio::io::stdout()).await
        }
        Transport::Http => {
            std::fs::create_dir_all(&directory)?;
            // Lock before loading the endpoint so port updates and startup use
            // the same lock order and cannot bind different configurations.
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(directory.join("mcp-http.lock"))?;
            match lock.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(ControlError::new(
                        "mcp_already_running",
                        "当前配置目录已有 HTTP MCP 接入",
                    ));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
            let config = LocalConfig::load(&directory)?;
            let client = Client::connect_observer(&directory).await?;
            let result = serve_http_mcp(client, config).await;
            drop(lock);
            result
        }
    }
}

fn report_error(error: &ControlError) {
    eprintln!(
        "{}",
        serde_json::to_string(error).expect("ControlError 可序列化")
    );
}

#[cfg(windows)]
fn prevent_standard_handle_inheritance() -> Result<(), ControlError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };

    // Keep the detached core from inheriting the MCP parent's stdio pipes.
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: standard handles are borrowed; only inheritance flags change.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn prevent_standard_handle_inheritance() -> Result<(), ControlError> {
    Ok(())
}
