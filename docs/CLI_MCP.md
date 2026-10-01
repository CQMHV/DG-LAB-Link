# CLI 与本机 MCP

GUI、CLI 和 MCP 连接同一个独立核心，共用 Hub、Socket V4 Relay、设备会话、输入源、波形库与持久配置。所有业务操作最终调用类型化 `ControlCommand`；窗口、托盘和开机自启由 GUI 管理。

## 构建与持有核心

只运行 CLI 时只需 Rust 工具链：

```powershell
cargo build -p dg-lab-link-core-server -p dg-lab-link-cli
$cli = ".\src-tauri\target\debug\dg-lab-link-cli.exe"
& $cli --help
```

可执行文件按职责分为 `dg-lab-link-core`（核心、实时链路及 MCP 服务）、`dg-lab-link-cli`（命令与持有者管理）、`dg-lab-link-gui`（窗口、托盘及自启动）。GUI 和 CLI 自动连接或启动同目录的 core；无界面使用须同时准备 core 和 CLI。MCP 是 core 内的独立协议适配模块，目前没有第四个 MCP 可执行文件。

`serve` 前台持有核心，Ctrl+C 释放当前持有者。`serve --background --json` 创建隐藏的后台 CLI 进程并返回 `holderId`、持有者 `pid` 与核心 `runtime` 信息。启动只恢复配置，不自动连接 Relay 或开始输出。

```powershell
$holder = & $cli serve --background --json | ConvertFrom-Json
& $cli holders list --json
& $cli status --json
# 任务完成：只释放本次创建的持有者
& $cli holders release $holder.holderId --json
```

GUI、常驻 CLI、`watch` 和一次性业务 CLI 在连接期间持有核心；独立设备窗口共用 GUI 进程的持有关系，缩到托盘继续持有。GUI 或业务 CLI 会自动连接或启动核心，管理命令 `holders` 只连接已有核心。一次性 CLI 在操作结束后立即释放；如果没有其他持有者，核心随后清理并退出。因此多步操作应先 `serve`，或保持 GUI 运行。

`holders release <id>` 只释放指定持有者。最后一个持有者退出后停止接受普通请求，停止所有输出、归零强度、停止音频并断开 Relay，再关闭接口和退出；清理最长十秒。异常退出通过连接与心跳最长十秒内释放。核心重启不恢复 Relay 连接、输出、触点或音频活动。

后台持有者不会跟随外部 AI 客户端自动退出。AI 任务须记录自己的 holderId，结束时释放；异常中断后可用 `holders list` 找到带“CLI 后台持有者”标签的进程，确认归属后按 ID 释放。

## 业务命令

`--json` 是全局参数，可放在命令之前或之后。成功结果写入 stdout；错误写入 stderr，结构为 `{ "code": "…", "message": "…" }`，并返回非零退出码。普通写操作超时后不自动重试，先读取状态确认是否已生效。`watch` 始终每行输出一个 JSON 快照，只保留最新状态；Ctrl+C 退出并释放其持有关系。

```powershell
& $cli relay connect --json
& $cli pairing --json
& $cli devices --json
& $cli watch
```

`pairing` 输出连接信息，其中 `controllerId` 是 Relay 控制端 ID，`pairingUrl` 是 DG-LAB 4 App 配对链接；`pairing --refresh` 会刷新配对，可能断开已有设备。设备列表中每台设备的 `controlId` 是写操作的显式目标；不能使用设备名称、原始硬件 `id` 或 GUI 当前焦点代替。

```powershell
$device = "<devices 返回的 controlId>"
& $cli sources list --json
& $cli sources bind --device $device --channel a source-fixed-waveform
& $cli sources bind --device $device --channel b source-fixed-waveform
& $cli waveforms list --json
& $cli waveforms select --device $device --channel a BREATHING
& $cli intensity --device $device --channel a --delta 1
& $cli output start --device $device
& $cli output stop --device $device
& $cli output emergency-stop
```

开始输出前 A/B 必须都有输入源。输入源 ID 从 `sources list` 读取；内置来源为 `source-fixed-waveform`、`source-touch`、`source-audio`。普通停止只清理目标设备；紧急停止清理并归零所有在线设备，同时停止音频。`sync --device $device --enabled on` 开启全设备强度同步，显式使用该设备作为基准；关闭用 `off`。`sources sync --device $device --enabled on` 控制该设备 A/B 的输入源同步。`sources default none` 表示新设备每次询问。

波形导入支持 `.pulse`、`.json`、`.pulses`；JSON 可为帧数组、含 `frames` / `pulseData` 的对象或对象数组。解析、容量、名称和帧校验在核心统一执行，单个文件最多 2 MiB；CLI 每批波形文本最多 4 MiB。`waveforms parse` 只解析不保存，`waveforms import` 导入并分配自定义 ID。`waveforms get <id>` 读取自定义完整配置；`delete <id>` 删除；`reorder <id>…` 提供全部自定义 ID 保存顺序。

```powershell
& $cli waveforms parse .\demo.pulse --json
& $cli waveforms import .\demo.pulse .\demo.json --json
& $cli waveforms set --device $device --channel a --file .\waveform-config.json
```

复杂配置使用 UTF-8 JSON 文件：`waveforms set` 文件是完整 `WaveformConfig`，`touch config` 是 `TouchConfig`，`touch input` 是 `TouchInput`，`audio config` 是 `AudioChannelConfig`，`safety set` 包含全部安全设置。对应 `--params` 接受直接 JSON 对象文本，与 `--file` 互斥。读取文件和类型校验发生在 CLI 启动核心之前。

```powershell
& $cli touch config --file .\touch-config.json
& $cli touch input --file .\touch-input.json
& $cli audio config --device $device --channel a --file .\audio-config.json
& $cli safety get --json
& $cli safety set --file .\safety.json
```

触控输入包含 `deviceId`（设备 controlId）、`ownerId`、递增 `sequence` 和 `pointers`。每个触点含 `id`、归一化 `x` / `y`、可选 `cell`；释放时提交空 `pointers`，持续触控必须在一秒租期内续租。核心限制活动所有者，过期或乱序输入不会复活触点。

音频播放、麦克风、桌面监听、录音和映射均在核心 Rust 工作线程执行。文件或录音保存路径相对于调用 CLI 的工作目录解析为绝对路径；MCP 的路径须直接提供绝对路径。

```powershell
& $cli audio load .\demo.mp3
& $cli audio options --repeat on --speaker on
& $cli audio play
& $cli audio seek 1000
& $cli audio pause
& $cli audio microphone
& $cli audio desktop
& $cli audio record
& $cli audio stop-recording
& $cli audio save .\recording.wav
& $cli audio stop
& $cli audio status --json
```

`commands --json` 无需运行核心，列出全部业务命令、参数 JSON Schema 与只读标记。`call <snake_case 命令> --params <JSON>` 或 `--file <JSON>` 提供完整覆盖；文件是命令参数对象，命令名不写进文件。参数沿用 GUI 的 camelCase，`deviceId` 的值必须是设备 `controlId`。例如 `start_output` 的参数文件内容为 `{ "deviceId": "<controlId>" }`，`audio_control` 为 `{ "action": { "type": "loadFile", "path": "demo.mp3" } }`。

```powershell
& $cli commands --json
& $cli call start_output --file .\start-output.json --json
& $cli call audio_control --file .\audio-action.json --json
```

`get_app_preferences`、`set_close_to_tray`、`set_start_minimized` 属于 GUI 管理，不暴露给 CLI/MCP。`devices select <controlId>` 显式切换共享 GUI 焦点，其他设备写操作不依赖该焦点。

## AI 使用 MCP

MCP 使用官方 Rust SDK `rmcp` 的 Streamable HTTP，地址默认为 `http://127.0.0.1:17846/mcp`。不提供 stdio、远程监听或持续 MCP 持有关系。本机 WebSocket 和 HTTP 共用随机 Bearer 令牌；核心验证 Host 与 Origin，不允许网页跨站访问。

AI 工作流：

1. `serve --background --json`，保存返回的 holderId；也可使用已运行 GUI。
2. `mcp config --show-token --json`，取得 MCP URL 与 `headers.Authorization`，传给支持 Streamable HTTP 的客户端。令牌只在显式 `--show-token` 时输出，不记录到公开日志。
3. 客户端初始化并列举工具，读取状态、配对与设备，调用业务工具。设备写操作始终显式使用 controlId；命令参数是 CLI `commands` 返回的同一 Schema。
4. 完成工作后停止需要结束的输出，再执行 `holders release <自己创建的 holderId>`。其他持有者仍存在时核心继续运行。

连接配置示意（将占位符替换为本机命令返回值）：

```json
{
    "url": "http://127.0.0.1:17846/mcp",
    "transport": "streamable-http",
    "headers": { "Authorization": "Bearer <本机令牌>" }
}
```

MCP 工具名称与 `ControlCommand` 的 snake_case 名称一致，例如 `get_hub_snapshot`、`connect_relay`、`adjust_intensity`、`start_output`、`stop_output`、`emergency_stop`、`import_waveform_files`。设备写参数仍为 `{ "deviceId": "<controlId>" }`。工具使用相同核心校验、持久化、回滚、队列和停止优先级；Hub 快照、设备、输入源与运行记录还通过只读资源 `dglab://status`、`dglab://devices`、`dglab://sources`、`dglab://logs` 提供。MCP 请求不会启动核心；不存在持有者时先启动 GUI 或 `serve`。

使用 `initialize` 协商协议版本，后续请求带 `MCP-Protocol-Version` 和 `Accept: application/json, text/event-stream`。服务使用无状态 HTTP，不要求 `Mcp-Session-Id`。例如用 PowerShell 验证初始化（核心须已被持有）：

```powershell
$mcp = & $cli mcp config --show-token --json | ConvertFrom-Json
$headers = @{
    Authorization = $mcp.headers.Authorization
    Accept = "application/json, text/event-stream"
}
$body = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"local-check","version":"1"}}}'
Invoke-WebRequest -Uri $mcp.url -Method Post -Headers $headers -ContentType "application/json" -Body $body
```

## 配置与故障处理

`mcp config --json` 显示当前地址和配置目录，默认隐藏令牌。全局 `--config-dir <绝对或相对路径>` 允许隔离开发/测试状态；日常使用默认目录以便和 GUI 共用核心。修改端口前须退出所有 GUI/常驻 CLI 并等待核心清理完成，然后执行 `mcp config --port 17847`。端口持久保存，下次启动使用新值；绑定失败明确报错，不自动选择另一个端口。

如果 GUI 或 CLI 找不到 core，确认 `dg-lab-link-core` 与客户端位于同一目录，并运行 `npm run build:headless` 或重新执行桌面构建。GUI 不依赖 CLI 可执行文件，CLI 不承载核心服务。

排障时可直接执行 `dg-lab-link-core --json --config-dir <目录>`，端口占用等启动错误以 `{code, message}` 写入 stderr；`--port` 在绑定成功后持久保存，`--relay-endpoint` 用于覆盖模拟 Relay 地址。直接启动 core 不增加持有者，十秒内没有 GUI/CLI 连接时仍按原生命周期退出；需要长期运行时使用 CLI `serve`。

CLI/core 无界面运行也需要本机音频设备才可使用音频采集或播放；设备输出和声音功能仍须按 [真机验收清单](REAL_DEVICE_CHECKLIST.md) 验证。
