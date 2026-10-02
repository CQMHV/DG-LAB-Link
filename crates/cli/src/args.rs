use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "dg-lab-link-cli",
    version,
    about = "DG-LAB Link 共享核心控制台"
)]
pub struct Cli {
    /// 输出机器可读 JSON；watch 始终逐行输出 JSON
    #[arg(long, global = true)]
    pub json: bool,
    /// 本地配置目录；默认与桌面 GUI 共用
    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 持有共享核心；Ctrl+C 释放当前持有者
    Serve {
        /// 在后台持续持有，返回可显式释放的 holderId
        #[arg(long)]
        background: bool,
    },
    /// 查看或释放核心持有者
    Holders {
        #[command(subcommand)]
        command: HolderCommand,
    },
    /// 读取完整 Hub 快照
    Status,
    /// 持续订阅最新快照，输出 NDJSON
    Watch,
    /// 连接和断开 Socket V4 Relay
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    /// 管理并行运行的 V4、V3 与蓝牙连接
    Connections {
        #[command(subcommand)]
        command: ConnectionCommand,
    },
    /// 扫描、连接及配置郊狼 3.0 蓝牙设备
    Bluetooth {
        #[command(subcommand)]
        command: BluetoothCommand,
    },
    /// 读取配对信息；--refresh 重新生成配对
    Pairing {
        #[arg(long)]
        refresh: bool,
    },
    /// 查看设备或更改 GUI 当前焦点
    Devices {
        #[command(subcommand)]
        command: Option<DeviceCommand>,
    },
    /// 按设备 controlId 和通道相对调整强度
    Intensity {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        channel: ChannelArg,
        #[arg(long, allow_hyphen_values = true)]
        delta: i32,
    },
    /// 按设备 controlId 开始或停止输出
    Output {
        #[command(subcommand)]
        command: OutputCommand,
    },
    /// 本地输入源插件包管理
    Plugins {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// 输入源列表、绑定及默认值
    Sources {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// 内置和自定义波形库
    Waveforms {
        #[command(subcommand)]
        command: WaveformCommand,
    },
    /// 触控配置和带租期的触点状态
    Touch {
        #[command(subcommand)]
        command: TouchCommand,
    },
    /// 音频播放、采集、录音及通道映射
    Audio {
        #[command(subcommand)]
        command: AudioCommand,
    },
    /// 查看或更新安全设置
    Safety {
        #[command(subcommand)]
        command: SafetyCommand,
    },
    /// 设置全设备强度同步；device 明确指定基准设备
    Sync {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        enabled: Toggle,
    },
    /// 调用任意类型化业务命令；可用 commands 查看命令及参数 Schema
    Call {
        command: String,
        #[command(flatten)]
        input: JsonInput,
    },
    /// 列出全部业务命令及 JSON 参数 Schema，不连接核心
    Commands,
    /// 本机 MCP 连接配置
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    #[command(name = "__hold", hide = true)]
    Hold {
        #[arg(long)]
        holder_id: String,
        #[arg(long)]
        ready_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum HolderCommand {
    List,
    Release { id: String },
}

#[derive(Debug, Subcommand)]
pub enum RelayCommand {
    Connect,
    Disconnect,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WsTransport {
    V4,
    V3,
}

#[derive(Debug, Subcommand)]
pub enum ConnectionCommand {
    List,
    Connect {
        #[arg(long, value_enum, default_value = "v4")]
        transport: WsTransport,
        #[arg(long)]
        endpoint: Option<String>,
    },
    Disconnect {
        connection_id: String,
    },
    Pairing {
        connection_id: String,
        #[arg(long)]
        refresh: bool,
    },
    /// 保存端点；连接运行时须先断开
    Endpoint {
        #[arg(long, value_enum, default_value = "v4")]
        transport: WsTransport,
        endpoint: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum BluetoothCommand {
    Scan {
        #[arg(long, default_value_t = 3000)]
        duration_ms: u64,
    },
    /// 使用 scan 返回的 deviceId
    Connect { device_id: String },
    /// 使用快照中的 controlId
    Disconnect {
        #[arg(long)]
        device: String,
    },
    /// 无输入时读取配置；--params 或 --file 更新持久参数
    Config {
        #[arg(long)]
        device: String,
        #[command(flatten)]
        input: JsonInput,
    },
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    Select { device: String },
}

#[derive(Debug, Subcommand)]
pub enum OutputCommand {
    Start {
        #[arg(long)]
        device: String,
    },
    Stop {
        #[arg(long)]
        device: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    List,
    Install {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
    },
    Uninstall {
        plugin: String,
        #[arg(long)]
        delete_data: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SourceCommand {
    List,
    Create {
        plugin: String,
        #[arg(long)]
        name: String,
    },
    Delete {
        source: String,
        #[arg(long)]
        delete_data: bool,
    },
    Enable {
        source: String,
        #[arg(value_enum)]
        enabled: Toggle,
    },
    Start {
        source: String,
    },
    Stop {
        source: String,
    },
    Config {
        source: String,
        #[arg(long)]
        binding: Option<String>,
        #[command(flatten)]
        input: JsonInput,
    },
    Ui {
        source: String,
        #[arg(long)]
        binding: Option<String>,
        #[arg(long)]
        control: bool,
    },
    Action {
        source: String,
        action: String,
        #[arg(long)]
        binding: Option<String>,
        #[command(flatten)]
        input: JsonInput,
    },
    Input {
        source: String,
        #[command(flatten)]
        input: JsonInput,
    },
    Bind {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        channel: ChannelArg,
        source: String,
    },
    Default {
        /// 输入源 ID，或 none 表示每次询问
        source: String,
    },
    Sync {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        enabled: Toggle,
    },
}

#[derive(Debug, Subcommand)]
pub enum WaveformCommand {
    List,
    Get {
        id: String,
    },
    Select {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        channel: ChannelArg,
        id: String,
    },
    Set {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        channel: ChannelArg,
        #[command(flatten)]
        input: JsonInput,
    },
    /// 导入 .pulse、.json、.pulses 文本（最多每个 2 MiB）
    Import {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// 只解析波形文件，返回标准配置，不保存
    Parse {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    Delete {
        id: String,
    },
    /// 按指定 ID 顺序排列全部自定义波形
    Reorder {
        #[arg(required = true)]
        ids: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TouchCommand {
    Config {
        #[command(flatten)]
        input: JsonInput,
    },
    /// JSON 包含 deviceId、ownerId、sequence、pointers；持续触控须一秒内续租
    Input {
        #[command(flatten)]
        input: JsonInput,
    },
}

#[derive(Debug, Subcommand)]
pub enum AudioCommand {
    Status,
    Load {
        path: PathBuf,
    },
    Play,
    Pause,
    Stop,
    Seek {
        position_ms: u64,
    },
    Microphone,
    Desktop,
    Record,
    StopRecording,
    Save {
        path: PathBuf,
    },
    Options {
        #[arg(long, value_enum)]
        repeat: Toggle,
        #[arg(long, value_enum)]
        speaker: Toggle,
    },
    Config {
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        channel: ChannelArg,
        #[command(flatten)]
        input: JsonInput,
    },
}

#[derive(Debug, Subcommand)]
pub enum SafetyCommand {
    Get,
    /// JSON 必须含 connectionTimeoutEnabled、connectionTimeoutMinutes、allowAppIntensityControl
    Set {
        #[command(flatten)]
        input: JsonInput,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// 查看 HTTP 或 stdio MCP 连接配置；HTTP 令牌仅在 --show-token 时输出
    Config {
        #[arg(long, value_enum, default_value = "http")]
        transport: McpTransport,
        /// 仅 HTTP：显示本机 Bearer 令牌
        #[arg(long)]
        show_token: bool,
        /// 仅 HTTP：MCP HTTP 服务停止后修改其监听端口，核心可继续运行
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpTransport {
    Http,
    Stdio,
}

#[derive(Debug, Args)]
pub struct JsonInput {
    /// JSON 对象文本，与 --file 互斥
    #[arg(long, conflicts_with = "file")]
    pub params: Option<String>,
    /// 从 UTF-8 JSON 文件读取对象
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ChannelArg {
    A,
    B,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub fn enabled(self) -> bool {
        matches!(self, Self::On)
    }
}
