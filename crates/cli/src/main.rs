mod args;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::Parser;
use dg_lab_link_core::model::Channel;
use dg_lab_link_core::sources::audio::AudioAction;
use dg_lab_link_core::transport::TransportKind;
use dg_lab_link_core::waveforms::WaveformFile;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, config_dir, connect_or_spawn, core_executable};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::process::Command as ProcessCommand;

use args::*;

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_WAVEFORM_BYTES: u64 = 2 * 1024 * 1024;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
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
    let machine = cli.json || matches!(cli.command, Command::Hold { .. });
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error, machine);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), ControlError> {
    prevent_standard_handle_inheritance()?;
    if matches!(cli.command, Command::Commands) {
        let commands: Vec<Value> = ControlCommand::descriptors().into_iter().map(|command| {
            json!({"command":command.name,"description":command.description,"params":command.input_schema,"readOnly":command.read_only})
        }).collect();
        return print_value(&commands, cli.json);
    }
    let directory = match cli.config_dir {
        Some(path) => absolute_path(&path)?,
        None => config_dir()?,
    };
    match cli.command {
        Command::Serve { background: true } => start_background(&directory, cli.json).await,
        Command::Serve { background: false } => hold(&directory, None, cli.json, None).await,
        Command::Hold {
            holder_id,
            ready_file,
        } => {
            let result = hold(&directory, Some(&holder_id), true, Some(&ready_file)).await;
            if let Err(error) = &result {
                let _ = std::fs::write(
                    &ready_file,
                    serde_json::to_vec(error).expect("ControlError 可序列化"),
                );
            }
            result
        }
        Command::Holders { command } => {
            let client = Client::connect(&directory, "CLI 管理", None).await?;
            let result = match command {
                HolderCommand::List => client
                    .holders()
                    .await
                    .and_then(|holders| print_value(&holders, cli.json)),
                HolderCommand::Release { id } => client
                    .release_holder(&id)
                    .await
                    .and_then(|()| print_value(&json!({"released":id}), cli.json)),
            };
            let release = client.release().await;
            result.and(release)
        }
        Command::Mcp {
            command:
                McpCommand::Config {
                    transport,
                    show_token,
                    port,
                },
        } => {
            let executable = std::env::current_exe()?;
            let directory_of_executable = executable
                .parent()
                .ok_or_else(|| ControlError::new("invalid_path", "无法确定 MCP 可执行文件目录"))?;
            let mcp_executable = directory_of_executable.join(if cfg!(windows) {
                "dg-lab-link-mcp.exe"
            } else {
                "dg-lab-link-mcp"
            });
            if transport == McpTransport::Stdio {
                if show_token || port.is_some() {
                    return Err(ControlError::new(
                        "invalid_arguments",
                        "--show-token 和 --port 仅适用于 --transport http",
                    ));
                }
                return print_value(
                    &json!({
                        "transport": "stdio",
                        "command": mcp_executable,
                        "args": ["--config-dir", directory],
                    }),
                    cli.json,
                );
            }
            if let Some(port) = port {
                LocalConfig::save_mcp_port(&directory, port)?;
            }
            let config = LocalConfig::load(&directory)?;
            let mut value = json!({
                "url": config.mcp_url(),
                "transport": "streamable-http",
                "configDir": directory,
                "server": {
                    "command": mcp_executable,
                    "args": ["--transport", "http", "--config-dir", directory],
                },
                "authorization": "Bearer <使用 --show-token 查看本机令牌>",
            });
            if show_token {
                value["headers"] = json!({"Authorization":format!("Bearer {}",config.token)});
                value.as_object_mut().unwrap().remove("authorization");
            }
            print_value(&value, cli.json)
        }
        Command::Watch => {
            let client = client(&directory).await?;
            let mut snapshots = client.subscribe();
            let result = async {
                print_value(&*snapshots.borrow_and_update(), true)?;
                loop {
                    tokio::select! {
                        signal = tokio::signal::ctrl_c() => { signal?; break; }
                        () = client.closed() => return Err(ControlError::new("core_closed", "共享核心连接已关闭")),
                        changed = snapshots.changed() => {
                            changed.map_err(|_| ControlError::new("core_closed", "快照订阅已关闭"))?;
                            print_value(&*snapshots.borrow_and_update(), true)?;
                        }
                    }
                }
                Ok(())
            }.await;
            let release = client.release().await;
            result.and(release)
        }
        command => {
            // Parse all files and typed parameters before starting or joining a core.
            let request = business_request(command)?;
            let client = client(&directory).await?;
            let result = execute(&client, request)
                .await
                .and_then(|value| print_value(&value, cli.json));
            let release = client.release().await;
            result.and(release)
        }
    }
}

async fn client(directory: &Path) -> Result<Client, ControlError> {
    connect_or_spawn(directory, &core_executable()?, "CLI", None).await
}

async fn hold(
    directory: &Path,
    id: Option<&str>,
    machine: bool,
    ready_file: Option<&Path>,
) -> Result<(), ControlError> {
    let client = connect_or_spawn(
        directory,
        &core_executable()?,
        if ready_file.is_some() {
            "CLI 后台持有者"
        } else {
            "CLI 常驻"
        },
        id,
    )
    .await?;
    let result = async {
        let info = client.runtime_info().await?;
        let ready = json!({"holderId":client.holder_id(),"pid":std::process::id(),"runtime":info});
        if let Some(path) = ready_file {
            // All detached stdio is null: inherited pipes otherwise keep callers'
            // output() readers waiting until the background holder exits on Windows.
            let temporary = path.with_extension("tmp");
            std::fs::write(
                &temporary,
                serde_json::to_vec(&ready).expect("启动信息可序列化"),
            )?;
            std::fs::rename(temporary, path)?;
        } else {
            print_value(&ready, machine)?;
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; let _ = client.release().await; }
            () = client.closed() => {}
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = client.release().await;
    }
    result
}

async fn start_background(directory: &Path, machine: bool) -> Result<(), ControlError> {
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(directory)?;
    let ready_file = directory.join(format!("holder-ready-{id}.json"));
    let mut command = ProcessCommand::new(std::env::current_exe()?);
    command
        .args(["--json", "--config-dir"])
        .arg(directory)
        .args(["__hold", "--holder-id", &id, "--ready-file"])
        .arg(&ready_file);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000 | 0x0000_0200);
    let mut child = command.spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let result = loop {
        match std::fs::read(&ready_file) {
            Ok(bytes) => {
                break serde_json::from_slice::<Value>(&bytes)
                    .map_err(|error| ControlError::new("startup_error", error.to_string()));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => break Err(error.into()),
        }
        if child.try_wait()?.is_some() {
            break Err(ControlError::new(
                "startup_error",
                "后台持有者未能连接共享核心",
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            break Err(ControlError::new("startup_timeout", "后台持有者启动超时"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let _ = std::fs::remove_file(&ready_file);
    let _ = std::fs::remove_file(ready_file.with_extension("tmp"));
    match result {
        Ok(value) if value["holderId"] == id => {
            let printed = print_value(&value, machine);
            if printed.is_err() {
                let _ = child.kill().await;
            }
            printed
        }
        Ok(value) => {
            let _ = child.kill().await;
            Err(
                serde_json::from_value::<ControlError>(value).unwrap_or_else(|_| {
                    ControlError::new("startup_error", "后台持有者返回了不匹配的身份")
                }),
            )
        }
        Err(error) => {
            let _ = child.kill().await;
            Err(error)
        }
    }
}

enum Request {
    Command(ControlCommand),
    SnapshotField(&'static str),
    Connection(String),
}

async fn execute(client: &Client, request: Request) -> Result<Value, ControlError> {
    match request {
        Request::Command(command) => client.call(command).await,
        Request::SnapshotField(field) => {
            let snapshot = client.call(ControlCommand::GetHubSnapshot).await?;
            Ok(snapshot[field].clone())
        }
        Request::Connection(id) => {
            let value = client.call(ControlCommand::GetConnections).await?;
            value
                .as_array()
                .and_then(|connections| {
                    connections
                        .iter()
                        .find(|connection| connection["connectionId"] == id)
                })
                .cloned()
                .ok_or_else(|| ControlError::new("connection_not_found", "连接不存在"))
        }
    }
}

fn business_request(command: Command) -> Result<Request, ControlError> {
    use ControlCommand as C;
    let command = match command {
        Command::Status => C::GetHubSnapshot,
        Command::Relay {
            command: RelayCommand::Connect,
        } => C::ConnectRelay,
        Command::Relay {
            command: RelayCommand::Disconnect,
        } => C::DisconnectRelay,
        Command::Connections { command } => match command {
            ConnectionCommand::List => C::GetConnections,
            ConnectionCommand::Connect {
                transport,
                endpoint,
            } => C::ConnectTransport {
                transport: transport.into(),
                endpoint,
            },
            ConnectionCommand::Disconnect { connection_id } => {
                C::DisconnectConnection { connection_id }
            }
            ConnectionCommand::Pairing {
                connection_id,
                refresh: true,
            } => C::RefreshConnectionPairing { connection_id },
            ConnectionCommand::Pairing {
                connection_id,
                refresh: false,
            } => return Ok(Request::Connection(connection_id)),
            ConnectionCommand::Endpoint {
                transport,
                endpoint,
            } => C::SetRelayEndpoint {
                transport: transport.into(),
                endpoint,
            },
        },
        Command::Bluetooth { command } => match command {
            BluetoothCommand::Scan { duration_ms } => C::ScanBluetooth { duration_ms },
            BluetoothCommand::Connect { device_id } => C::ConnectBluetooth { device_id },
            BluetoothCommand::Disconnect { device } => C::DisconnectBluetooth { device_id: device },
            BluetoothCommand::Config { device, input } => {
                if input.params.is_none() && input.file.is_none() {
                    C::GetBluetoothConfig { device_id: device }
                } else {
                    C::from_call(
                        "set_bluetooth_config",
                        json!({"deviceId":device,"config":read_json(input)?}),
                    )?
                }
            }
        },
        Command::Pairing { refresh: true } => C::RefreshPairing,
        Command::Pairing { refresh: false } => return Ok(Request::SnapshotField("connection")),
        Command::Devices { command: None } => return Ok(Request::SnapshotField("devices")),
        Command::Devices {
            command: Some(DeviceCommand::Select { device }),
        } => C::SelectDevice { device_id: device },
        Command::Intensity {
            device,
            channel,
            delta,
        } => C::AdjustIntensity {
            device_id: device,
            channel: channel.into(),
            delta,
        },
        Command::Output { command } => match command {
            OutputCommand::Start { device } => C::StartOutput { device_id: device },
            OutputCommand::Stop { device } => C::StopOutput { device_id: device },
        },
        Command::Plugins { command } => match command {
            PluginCommand::List => C::ListPlugins,
            PluginCommand::Install { path } => C::InstallPlugin {
                path: path_text(&path)?,
            },
            PluginCommand::Update { path } => C::UpdatePlugin {
                path: path_text(&path)?,
            },
            PluginCommand::Uninstall {
                plugin,
                delete_data,
            } => C::UninstallPlugin {
                plugin_id: plugin,
                delete_data,
            },
        },
        Command::Sources { command } => match command {
            SourceCommand::List => return Ok(Request::SnapshotField("sources")),
            SourceCommand::Create { plugin, name } => C::CreateSource {
                plugin_id: plugin,
                name,
            },
            SourceCommand::Delete {
                source,
                delete_data,
            } => C::DeleteSource {
                source_id: source,
                delete_data,
            },
            SourceCommand::Enable { source, enabled } => C::SetSourceEnabled {
                source_id: source,
                enabled: enabled.enabled(),
            },
            SourceCommand::Start { source } => C::StartSource { source_id: source },
            SourceCommand::Stop { source } => C::StopSource { source_id: source },
            SourceCommand::Config {
                source,
                binding,
                input,
            } => C::SetSourceConfig {
                source_id: source,
                config: read_json(input)?,
                binding_id: binding,
            },
            SourceCommand::Ui {
                source,
                binding,
                control,
            } => C::from_call(
                "get_source_ui",
                json!({"sourceId":source,"params":{"bindingId":binding,"surface":if control {"control"} else {"settings"}}}),
            )?,
            SourceCommand::Action {
                source,
                action,
                binding,
                input,
            } => C::from_call(
                "source_action",
                json!({"sourceId":source,"params":{"action":action,"value":read_json(input)?,"bindingId":binding}}),
            )?,
            SourceCommand::Input { source, input } => C::from_call(
                "source_input",
                json!({"sourceId":source,"params":read_json(input)?}),
            )?,
            SourceCommand::Bind {
                device,
                channel,
                source,
            } => C::SetDeviceChannelSource {
                device_id: device,
                channel: channel.into(),
                source_id: source,
            },
            SourceCommand::Default { source } => C::SetDefaultSource {
                source_id: (source != "none").then_some(source),
            },
            SourceCommand::Sync { device, enabled } => C::SetDeviceChannelSourceSync {
                device_id: device,
                enabled: enabled.enabled(),
            },
        },
        Command::Waveforms { command } => match command {
            WaveformCommand::List => C::ListWaveforms,
            WaveformCommand::Get { id } => C::GetCustomWaveform { preset_id: id },
            WaveformCommand::Select {
                device,
                channel,
                id,
            } => C::SelectWaveform {
                device_id: device,
                channel: channel.into(),
                preset_id: id,
            },
            WaveformCommand::Set {
                device,
                channel,
                input,
            } => C::from_call(
                "set_fixed_waveform",
                json!({"deviceId":device,"channel":channel_name(channel),"config":read_json(input)?}),
            )?,
            WaveformCommand::Import { files } => C::ImportWaveformFiles {
                files: read_waveforms(files)?,
            },
            WaveformCommand::Parse { files } => C::ParseWaveformFiles {
                files: read_waveforms(files)?,
            },
            WaveformCommand::Delete { id } => C::DeleteCustomWaveform { preset_id: id },
            WaveformCommand::Reorder { ids } => C::ReorderCustomWaveforms { preset_ids: ids },
        },
        Command::Touch { command } => match command {
            TouchCommand::Config { input } => {
                C::from_call("set_touch_config", json!({"config":read_json(input)?}))?
            }
            TouchCommand::Input { input } => {
                C::from_call("update_touch_input", json!({"input":read_json(input)?}))?
            }
        },
        Command::Audio { command } => match command {
            AudioCommand::Status => {
                // This field includes touchConfig as well as audio and per-device audioBindings.
                return Ok(Request::SnapshotField("inputModes"));
            }
            AudioCommand::Config {
                device,
                channel,
                input,
            } => C::from_call(
                "set_audio_config",
                json!({"deviceId":device,"channel":channel_name(channel),"config":read_json(input)?}),
            )?,
            other => C::AudioControl {
                action: audio_action(other)?,
            },
        },
        Command::Safety {
            command: SafetyCommand::Get,
        } => return Ok(Request::SnapshotField("safety")),
        Command::Safety {
            command: SafetyCommand::Set { input },
        } => C::from_call("update_safety", read_json(input)?)?,
        Command::Sync { device, enabled } => C::SetSyncAllDevices {
            device_id: device,
            enabled: enabled.enabled(),
        },
        Command::Call { command, input } => {
            let mut typed = C::from_call(&command, read_json(input)?)?;
            if !typed.is_business() {
                return Err(ControlError::new(
                    "gui_only",
                    "窗口、托盘与启动偏好由 GUI 管理",
                ));
            }
            normalize_audio_paths(&mut typed)?;
            typed
        }
        _ => return Err(ControlError::new("invalid_command", "该命令不是业务调用")),
    };
    let mut command = command;
    normalize_audio_paths(&mut command)?;
    Ok(Request::Command(command))
}

fn audio_action(command: AudioCommand) -> Result<AudioAction, ControlError> {
    Ok(match command {
        AudioCommand::Load { path } => AudioAction::LoadFile {
            path: path_text(&path)?,
        },
        AudioCommand::Play => AudioAction::Play,
        AudioCommand::Pause => AudioAction::Pause,
        AudioCommand::Stop => AudioAction::Stop,
        AudioCommand::Seek { position_ms } => AudioAction::Seek { position_ms },
        AudioCommand::Microphone => AudioAction::StartMicrophone,
        AudioCommand::Desktop => AudioAction::StartDesktop,
        AudioCommand::Record => AudioAction::StartRecording,
        AudioCommand::StopRecording => AudioAction::StopRecording,
        AudioCommand::Save { path } => AudioAction::SaveRecording {
            path: path_text(&path)?,
        },
        AudioCommand::Options { repeat, speaker } => AudioAction::SetPlaybackOptions {
            loop_enabled: repeat.enabled(),
            speaker_enabled: speaker.enabled(),
        },
        _ => return Err(ControlError::new("invalid_command", "该命令不是音频控制")),
    })
}

fn normalize_audio_paths(command: &mut ControlCommand) -> Result<(), ControlError> {
    if let ControlCommand::InstallPlugin { path } | ControlCommand::UpdatePlugin { path } = command
    {
        *path = path_text(Path::new(path))?;
    }
    if let ControlCommand::AudioControl {
        action: AudioAction::LoadFile { path } | AudioAction::SaveRecording { path },
    } = command
    {
        *path = path_text(Path::new(path))?;
    }
    Ok(())
}

fn read_json(input: JsonInput) -> Result<Value, ControlError> {
    let text = match (input.params, input.file) {
        (Some(text), None) => text,
        (None, Some(path)) => read_bounded_text(&path, MAX_JSON_BYTES as u64)?,
        (None, None) => "{}".to_owned(),
        _ => {
            return Err(ControlError::new(
                "invalid_params",
                "--params 与 --file 互斥",
            ));
        }
    };
    if text.len() > MAX_JSON_BYTES {
        return Err(ControlError::new(
            "request_too_large",
            "JSON 参数超过 8 MiB",
        ));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| ControlError::new("invalid_params", error.to_string()))?;
    if !value.is_object() {
        return Err(ControlError::new("invalid_params", "JSON 参数必须是对象"));
    }
    Ok(value)
}

fn read_waveforms(paths: Vec<PathBuf>) -> Result<Vec<WaveformFile>, ControlError> {
    if paths.len() > 128 {
        return Err(ControlError::new(
            "request_too_large",
            "一次最多读取 128 个波形文件",
        ));
    }
    let mut bytes = 0;
    paths
        .into_iter()
        .map(|path| {
            let content = read_bounded_text(&path, MAX_WAVEFORM_BYTES)?;
            bytes += content.len();
            if bytes > MAX_JSON_BYTES / 2 {
                return Err(ControlError::new(
                    "request_too_large",
                    "波形文本总计不能超过 4 MiB",
                ));
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ControlError::new("invalid_path", "波形文件名必须是 UTF-8"))?
                .to_owned();
            Ok(WaveformFile { name, content })
        })
        .collect()
}

fn read_bounded_text(path: &Path, limit: u64) -> Result<String, ControlError> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buffer = Vec::new();
    file.take(limit + 1).read_to_end(&mut buffer)?;
    if buffer.len() as u64 > limit {
        return Err(ControlError::new(
            "request_too_large",
            format!("{} 超过文件容量限制", path.display()),
        ));
    }
    String::from_utf8(buffer)
        .map_err(|error| ControlError::new("invalid_params", error.to_string()))
}

fn absolute_path(path: &Path) -> Result<PathBuf, ControlError> {
    std::path::absolute(path).map_err(Into::into)
}
fn path_text(path: &Path) -> Result<String, ControlError> {
    absolute_path(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| ControlError::new("invalid_path", "路径必须是有效 UTF-8"))
}
fn channel_name(channel: ChannelArg) -> &'static str {
    match channel {
        ChannelArg::A => "a",
        ChannelArg::B => "b",
    }
}

impl From<WsTransport> for TransportKind {
    fn from(transport: WsTransport) -> Self {
        match transport {
            WsTransport::V4 => Self::WsV4,
            WsTransport::V3 => Self::WsV3,
        }
    }
}
impl From<ChannelArg> for Channel {
    fn from(channel: ChannelArg) -> Self {
        match channel {
            ChannelArg::A => Self::A,
            ChannelArg::B => Self::B,
        }
    }
}

fn print_value(value: &impl Serialize, machine: bool) -> Result<(), ControlError> {
    let text = if machine {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    }
    .map_err(|error| ControlError::new("serialization_error", error.to_string()))?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}")?;
    stdout.flush()?;
    Ok(())
}

fn report_error(error: &ControlError, machine: bool) {
    if machine {
        eprintln!(
            "{}",
            serde_json::to_string(error).expect("ControlError 可序列化")
        );
    } else {
        eprintln!("{}: {}", error.code, error.message);
    }
}

#[cfg(windows)]
fn prevent_standard_handle_inheritance() -> Result<(), ControlError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };

    // Detached children use explicit null stdio. Windows otherwise also inherits
    // ancestors' pipe handles and prevents shell/agent output collectors seeing EOF.
    for handle in [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ] {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: these are borrowed standard handles; the call changes only their
        // inheritance flag, does not close them, and runs before spawning children.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn prevent_standard_handle_inheritance() -> Result<(), ControlError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_and_explicit_targets() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        assert!(
            Cli::try_parse_from(["cli", "intensity", "--channel", "a", "--delta", "1"]).is_err()
        );
        let cli = Cli::try_parse_from([
            "cli",
            "intensity",
            "--device",
            "target",
            "--channel",
            "b",
            "--delta",
            "-2",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        let Request::Command(ControlCommand::AdjustIntensity {
            device_id,
            channel,
            delta,
        }) = business_request(cli.command).unwrap()
        else {
            panic!("expected intensity")
        };
        assert_eq!(device_id, "target");
        assert_eq!(channel, Channel::B);
        assert_eq!(delta, -2);
    }

    #[test]
    fn call_rejects_gui_preferences_and_invalid_payload_without_starting_core() {
        let input = || JsonInput {
            params: None,
            file: None,
        };
        assert!(
            business_request(Command::Call {
                command: "get_app_preferences".into(),
                input: input()
            })
            .is_err()
        );
        assert!(
            business_request(Command::Call {
                command: "start_output".into(),
                input: input()
            })
            .is_err()
        );
        assert!(
            read_json(JsonInput {
                params: Some("[]".into()),
                file: None
            })
            .is_err()
        );
    }

    #[test]
    fn audio_paths_are_absolute_for_short_and_typed_commands() {
        let AudioAction::SaveRecording { path } = audio_action(AudioCommand::Save {
            path: "recording.wav".into(),
        })
        .unwrap() else {
            panic!()
        };
        assert!(Path::new(&path).is_absolute());
        let Request::Command(ControlCommand::AudioControl {
            action: AudioAction::LoadFile { path },
        }) = business_request(Command::Call {
            command: "audio_control".into(),
            input: JsonInput {
                params: Some(r#"{"action":{"type":"loadFile","path":"demo.mp3"}}"#.into()),
                file: None,
            },
        })
        .unwrap()
        else {
            panic!()
        };
        assert!(Path::new(&path).is_absolute());
    }

    #[test]
    fn plugin_commands_normalize_packages_and_keep_explicit_instance_and_binding() {
        let cli =
            Cli::try_parse_from(["cli", "plugins", "install", "example.dglabplugin", "--json"])
                .unwrap();
        let Request::Command(ControlCommand::InstallPlugin { path }) =
            business_request(cli.command).unwrap()
        else {
            panic!("expected install")
        };
        assert!(Path::new(&path).is_absolute());
        let cli = Cli::try_parse_from([
            "cli",
            "sources",
            "config",
            "source-123",
            "--binding",
            "device/a",
            "--params",
            r#"{"gain":2}"#,
        ])
        .unwrap();
        let Request::Command(ControlCommand::SetSourceConfig {
            source_id,
            binding_id,
            config,
        }) = business_request(cli.command).unwrap()
        else {
            panic!("expected config")
        };
        assert_eq!(source_id, "source-123");
        assert_eq!(binding_id.as_deref(), Some("device/a"));
        assert_eq!(config, json!({"gain":2}));
        let cli = Cli::try_parse_from([
            "cli",
            "sources",
            "ui",
            "source-123",
            "--binding",
            "device/b",
            "--control",
        ])
        .unwrap();
        let Request::Command(ControlCommand::GetSourceUi { params, .. }) =
            business_request(cli.command).unwrap()
        else {
            panic!("expected UI")
        };
        assert_eq!(params.binding_id.as_deref(), Some("device/b"));
        assert_eq!(serde_json::to_value(params.surface).unwrap(), "control");
        assert!(Cli::try_parse_from(["cli", "output", "emergency-stop"]).is_err());
    }

    #[test]
    fn waveform_import_preserves_text_and_enforces_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("demo.json");
        std::fs::write(&path, r#"["0A0A0A0A64646464"]"#).unwrap();
        let files = read_waveforms(vec![path.clone()]).unwrap();
        assert_eq!(files[0].name, "demo.json");
        assert_eq!(files[0].content, r#"["0A0A0A0A64646464"]"#);
        std::fs::write(&path, vec![b' '; MAX_WAVEFORM_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_waveforms(vec![path]).unwrap_err().code,
            "request_too_large"
        );
    }

    #[test]
    fn transport_commands_keep_v4_default_and_require_explicit_ble_targets() {
        let cli = Cli::try_parse_from(["cli", "connections", "connect"]).unwrap();
        let Request::Command(ControlCommand::ConnectTransport {
            transport,
            endpoint,
        }) = business_request(cli.command).unwrap()
        else {
            panic!("expected connection")
        };
        assert_eq!(transport, TransportKind::WsV4);
        assert_eq!(endpoint, None);
        let cli = Cli::try_parse_from([
            "cli",
            "connections",
            "connect",
            "--transport",
            "v3",
            "--endpoint",
            "ws://127.0.0.1:9000",
        ])
        .unwrap();
        let Request::Command(ControlCommand::ConnectTransport {
            transport,
            endpoint,
        }) = business_request(cli.command).unwrap()
        else {
            panic!("expected V3")
        };
        assert_eq!(transport, TransportKind::WsV3);
        assert_eq!(endpoint.as_deref(), Some("ws://127.0.0.1:9000"));
        assert!(Cli::try_parse_from(["cli", "bluetooth", "disconnect"]).is_err());
        assert!(Cli::try_parse_from(["cli", "bluetooth", "config"]).is_err());
        let cli = Cli::try_parse_from(["cli", "bluetooth", "scan", "--json"]).unwrap();
        assert!(cli.json);
        assert!(matches!(
            business_request(cli.command).unwrap(),
            Request::Command(ControlCommand::ScanBluetooth { duration_ms: 3000 })
        ));
    }

    #[test]
    fn bluetooth_configuration_loads_json_file_before_connecting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ble.json");
        std::fs::write(&path, r#"{"maxStrengthA":90,"wheelProtectionValue":12}"#).unwrap();
        let request = business_request(Command::Bluetooth {
            command: BluetoothCommand::Config {
                device: "ble-control-id".to_owned(),
                input: JsonInput {
                    params: None,
                    file: Some(path.clone()),
                },
            },
        })
        .unwrap();
        let Request::Command(ControlCommand::SetBluetoothConfig { device_id, config }) = request
        else {
            panic!("expected BLE config")
        };
        assert_eq!(device_id, "ble-control-id");
        assert_eq!(config.max_strength_a, 90);
        assert_eq!(config.frequency_balance_a, 160);
        assert_eq!(config.wheel_protection_value, 12);
        std::fs::write(&path, "invalid JSON").unwrap();
        assert!(
            business_request(Command::Bluetooth {
                command: BluetoothCommand::Config {
                    device: "ble-control-id".to_owned(),
                    input: JsonInput {
                        params: None,
                        file: Some(path)
                    },
                }
            })
            .is_err()
        );
    }
}
