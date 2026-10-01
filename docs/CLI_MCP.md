# CLI 与本机 MCP

core、GUI、CLI、MCP 四个程序按职责分开；GUI、CLI、stdio MCP 与本机 HTTP MCP 连接同一个独立核心，共用 Hub、Socket V4 Relay、设备会话、输入源、波形库与持久配置。所有业务操作最终调用类型化 `ControlCommand`；窗口、托盘和开机自启由 GUI 管理。

## 构建与持有核心

无界面入口只需 Rust 工具链：

```powershell
cargo build -p dg-lab-link-core-server -p dg-lab-link-cli -p dg-lab-link-mcp
$cli = ".\src-tauri\target\debug\dg-lab-link-cli.exe"
& $cli --help
```

可执行文件按职责分为 `dg-lab-link-core`（核心、实时链路及本机 WebSocket `/control`）、`dg-lab-link-cli`（命令与持有者管理）、`dg-lab-link-mcp`（默认 stdio，`--transport http` 提供 HTTP MCP）、`dg-lab-link-gui`（窗口、托盘及自启动）。GUI、CLI 和 stdio MCP 自动连接或启动同目录的 core，HTTP MCP 只连接已有 core；实时链路始终留在 core。CLI 需要 core + CLI，stdio MCP 需要 core + MCP；HTTP MCP 需要 core + MCP，并通过 GUI 或 CLI `serve` 保持核心。桌面联合构建会准备全部四个程序。

`serve` 前台持有核心，Ctrl+C 释放当前持有者。`serve --background --json` 创建隐藏的后台 CLI 进程并返回 `holderId`、持有者 `pid` 与核心 `runtime` 信息。启动只恢复配置，不自动连接 Relay 或开始输出。

```powershell
$holder = & $cli serve --background --json | ConvertFrom-Json
& $cli holders list --json
& $cli status --json
# 任务完成：只释放本次创建的持有者
& $cli holders release $holder.holderId --json
```

GUI、常驻 CLI、`watch`、一次性业务 CLI 和 stdio MCP 在连接期间持有核心；独立设备窗口共用 GUI 进程的持有关系，缩到托盘继续持有。GUI、业务 CLI 和 stdio MCP 会自动连接或启动核心，管理命令 `holders` 只连接已有核心。一次性 CLI 在操作结束后立即释放；如果没有其他持有者，核心随后清理并退出。因此 CLI 多步操作应先 `serve`，或保持 GUI 运行。stdio MCP 会在子进程会话期间持续持有，无须额外的后台 CLI。

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

触控输入包含 `deviceId`（设备 controlId）、`ownerId`、递增 `sequence` 和 `pointers`。每个触点含 `id`、归一化 `x` / `y`、可选 `cell` 和可选 `channel`（`a` / `b`）。指定 `channel` 时每路最多一个触点，两路可同时独立控制，不受共享 `routing` 影响；不指定时按 `TouchConfig.routing` 分配。一次输入不可混用这两种方式。释放单路时只移除该路触点并保留另一触点，全部释放时提交空 `pointers`；持续触控必须在一秒租期内续租。核心限制活动所有者，过期或乱序输入不会复活触点。

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

MCP 使用官方 Rust SDK `rmcp`，支持独立子进程 stdio 和本机 Streamable HTTP 两种传输，工具、资源及业务 Schema 相同。`mcp config --transport http|stdio --json` 输出连接配置，默认仍为 HTTP。

### stdio：客户端自动启动并持有核心

适用于按命令启动 MCP 子进程的 AI 客户端，无须先运行 GUI 或 `serve`：

```powershell
& $cli mcp config --transport stdio --json
```

返回同目录 `dg-lab-link-mcp` 的绝对路径及配置目录参数，形如：

```json
{
    "transport": "stdio",
    "command": "C:\\DG-LAB-Link\\dg-lab-link-mcp.exe",
    "args": ["--config-dir", "C:\\Users\\user\\AppData\\Roaming\\cn.dglab.link"]
}
```

将返回的 `command` 与 `args` 放入客户端的 stdio MCP 配置。客户端启动 MCP 子进程后，子进程自动连接已有 core，或启动同目录 core，并持续持有这一个会话。冷启动只恢复配置，不连接 Relay、不开始输出；初始化后调用 `connect_relay` 等业务工具仍须明确执行。

stdio stdout 只输出 MCP 协议消息，诊断和启动错误写入 stderr。stdin EOF、正常退出时释放当前持有者；MCP 进程异常退出通过本机连接及心跳最长十秒内释放。关闭一个 MCP 客户端不影响其他 GUI、CLI 或 stdio MCP 持有者；最后一个持有者退出才触发全局安全清理。任务完成后由客户端关闭 stdio 会话即可，无须管理后台 CLI holderId。

stdio 每条请求最多 8 MiB，响应最多 16 MiB；普通请求容量为 32，停止操作预留 8 个位置。初始化须在三十秒内完成；stdout 写入超过一秒或响应队列耗尽时关闭会话并释放本进程的持有者，诊断写入 stderr。慢 stdout 不阻塞读取停止请求，停止前排队的输出操作不会在停止后恢复活动。按 ID 释放 stdio 持有者时，即使 stdin 仍然开放，MCP 进程也会退出并报告 `core_closed`。

stdio 配置不包含 Bearer 令牌，也不会为了显示配置而启动 core。`--show-token` 和 `--port` 只适用于 HTTP，和 `--transport stdio` 一起使用会返回参数错误。stdio 程序仍通过已鉴权的本机 WebSocket 访问核心，令牌在本机读取，不通过 stdio 配置导出。

### HTTP：启动独立 MCP 服务，连接已被持有的核心

`dg-lab-link-mcp --transport http` 监听本机 `http://127.0.0.1:17846/mcp`，使用随机 Bearer 令牌并验证 Host 与 Origin。该进程以 observer 连接已运行的 core，HTTP 进程与请求都不增加核心持有者，也不自动唤起 core；先运行 GUI 或 CLI `serve` 保持核心。core 关闭时 HTTP 服务也退出。core 自身只提供默认 `17845` 端口的 `/control`，不提供 `/mcp`。

后台 CLI 工作流：

1. `serve --background --json`，保存返回的 holderId；也可使用已运行 GUI。
2. `mcp config --transport http --json`，按返回的 `server.command` 和 `server.args` 在另一个终端启动独立 MCP HTTP 进程，或直接执行同目录 `dg-lab-link-mcp --transport http`。启动参数使用相同配置目录，不包含令牌。
3. `mcp config --transport http --show-token --json`，取得 MCP URL 与 `headers.Authorization`，传给支持 Streamable HTTP 的客户端。令牌只在显式 `--show-token` 时输出，不记录到公开日志；随后初始化并调用业务工具，设备写操作始终显式使用 controlId。
4. 完成工作后停止需要结束的输出，结束自己启动的 HTTP 进程（前台运行用 Ctrl+C），再执行 `holders release <自己创建的 holderId>`。其他持有者仍存在时核心继续运行。

例如，先创建后台持有者，再在另一个终端用导出的启动提示运行 HTTP 服务：

```powershell
$holder = & $cli serve --background --json | ConvertFrom-Json
# 另一个终端（相同配置目录），Ctrl+C 结束这个 HTTP 进程
$httpConfig = & $cli mcp config --transport http --json | ConvertFrom-Json
$serverArgs = $httpConfig.server.args
& $httpConfig.server.command @serverArgs
```

HTTP 服务关闭后，在创建持有者的终端运行 `& $cli holders release $holder.holderId --json`。关闭 HTTP 服务只移除观察者连接，不释放 GUI 或 CLI 的持有者。

连接配置示意（将占位符替换为本机命令返回值）：

```json
{
    "url": "http://127.0.0.1:17846/mcp",
    "transport": "streamable-http",
    "headers": { "Authorization": "Bearer <本机令牌>" }
}
```

两种传输的 MCP 工具名称都与 `ControlCommand` 的 snake_case 名称一致，例如 `get_hub_snapshot`、`connect_relay`、`adjust_intensity`、`start_output`、`stop_output`、`emergency_stop`、`import_waveform_files`。设备写参数仍为 `{ "deviceId": "<controlId>" }`。工具使用相同核心校验、持久化、回滚、队列和停止优先级；Hub 快照、设备、输入源与运行记录还通过只读资源 `dglab://status`、`dglab://devices`、`dglab://sources`、`dglab://logs` 提供。

HTTP 使用 `initialize` 协商协议版本，后续请求带 `MCP-Protocol-Version` 和 `Accept: application/json, text/event-stream`。服务使用无状态 HTTP，不要求 `Mcp-Session-Id`。例如用 PowerShell 验证 HTTP 初始化（core 已被持有，独立 HTTP MCP 已启动）：

```powershell
$mcp = & $cli mcp config --transport http --show-token --json | ConvertFrom-Json
$headers = @{
    Authorization = $mcp.headers.Authorization
    Accept = "application/json, text/event-stream"
}
$body = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"local-check","version":"1"}}}'
Invoke-WebRequest -Uri $mcp.url -Method Post -Headers $headers -ContentType "application/json" -Body $body
```

## 配置与故障处理

`mcp config --json` 默认显示 HTTP 地址、配置目录及 `server` 启动提示，隐藏令牌；`mcp config --transport stdio --json` 显示 stdio 命令和参数。全局 `--config-dir <绝对或相对路径>` 允许隔离开发/测试状态；日常使用默认目录以便和 GUI 共用核心。

本机配置的 `port` 是 core `/control` 端口（默认 `17845`），`mcpPort` 是独立 HTTP MCP 端口（默认 `17846`）。执行 `mcp config --transport http --port 17847` 只更新 `mcpPort`；先关闭该配置目录的 HTTP MCP 服务，`mcp-http.lock` 被占用时会拒绝修改，GUI、CLI 和 stdio MCP 可继续持有 core。重新启动 HTTP 服务使用新值，token 和 core 端口保持不变；端口不能与 core 相同，绑定失败明确报错，不自动选择其他端口。

旧本机配置只含 `port` 时会将原值迁为 HTTP MCP 端口，core 使用 `17845`；若原 HTTP 端口恰为 `17845`，core 改用 `17846` 避免冲突。迁移保留原令牌。若旧 core 还在运行，会返回 `runtime_config_migration_required`，先退出旧 core 后重试，防止改变运行中的控制地址。

如果 GUI、CLI 或 MCP 找不到 core，确认 `dg-lab-link-core` 与客户端位于同一目录，并运行 `npm run build:headless` 或重新执行桌面构建。配置中的 MCP 命令找不到时，确认 `dg-lab-link-mcp` 已构建，并重新导出配置。HTTP MCP 连接失败时先运行 GUI 或 CLI `serve`；能读取 CLI 状态但无法访问 `/mcp` 时，确认独立 HTTP MCP 进程已启动及 URL 端口正确。GUI 不依赖 CLI 可执行文件，CLI/MCP 不承载核心服务。

排障时可直接执行 `dg-lab-link-core --json --config-dir <目录>`，端口占用等启动错误以 `{code, message}` 写入 stderr；core 的 `--port` 在绑定成功后持久保存控制端口，`--relay-endpoint` 用于覆盖模拟 Relay 地址。直接启动 core 不增加持有者，十秒内没有 GUI/CLI/stdio MCP 持有时仍按原生命周期退出；HTTP observer 不延长这个期限。需要长期运行时使用 CLI `serve` 或保持 stdio MCP 会话。

CLI/MCP/core 无界面运行也需要本机音频设备才可使用音频采集或播放；设备输出和声音功能仍须按 [真机验收清单](REAL_DEVICE_CHECKLIST.md) 验证。

V3／V4 连接管理、郊狼 3.0 BLE 扫描和设备参数用法见 [传输接入说明](TRANSPORTS.md)。两种 MCP 传输共用新增工具及 `dglab://connections`、`dglab://bluetooth` 资源。
