# CLI 与本机 MCP

core、GUI、CLI、MCP 四个程序按职责分开；GUI、CLI、stdio MCP 与本机 HTTP MCP 连接同一个独立核心，共用 Hub、Socket V4／V3／BLE、设备会话、输入源、波形库与持久配置。所有业务操作最终调用类型化 `ControlCommand`；窗口、托盘和开机自启由 GUI 管理。

## 构建与持有核心

无界面入口只需 Rust 工具链：

```powershell
cargo build -p dg-lab-link-core-server -p dg-lab-link-cli -p dg-lab-link-mcp -p dg-lab-link-builtin-plugins -p dg-lab-link-plugin-runtime
$cli = ".\src-tauri\target\debug\dg-lab-link-cli.exe"
& $cli --help
```

上述 Cargo 命令编译程序和插件入口；首次启动前还需用 `dg-lab-link-plugin-pack` 将两个插件清单与对应 exe 打包到可执行文件同级 `plugins/`。已有 Node.js 开发环境可执行 `npm run build:headless` 自动完成构建和打包。插件包准备方式见 [预装插件](PLUGIN_BUILTINS.md)，公共打包器用法见 [插件开发](PLUGINS.md)；运行时无需 Node.js。

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

`holders release <id>` 只释放指定持有者。最后一个持有者退出后停止接受普通请求，停止所有输出、归零强度、关闭插件并断开全部 WS／BLE 连接，再关闭接口和退出；清理最长十秒。异常退出通过连接与心跳最长十秒内释放。核心重启不恢复设备连接、输出、触点或音频活动。

后台持有者不会跟随外部 AI 客户端自动退出。AI 任务须记录自己的 holderId，结束时释放；异常中断后可用 `holders list` 找到带“CLI 后台持有者”标签的进程，确认归属后按 ID 释放。

## 业务命令

`--json` 是全局参数，可放在命令之前或之后。成功结果写入 stdout；错误写入 stderr，结构为 `{ "code": "…", "message": "…" }`，并返回非零退出码。普通写操作超时后不自动重试，先读取状态确认是否已生效。`watch` 始终每行输出一个 JSON 快照，只保留最新状态；Ctrl+C 退出并释放其持有关系。

```powershell
& $cli connections connect --transport v4 --json
& $cli connections list --json
& $cli connections pairing ws-v4 --json
& $cli devices --json
& $cli watch
```

`connections pairing <connectionId>` 输出指定 WS 连接的信息，其中 `controllerId` 是 Relay 控制端 ID，`pairingUrl` 是对应 App 的配对链接；加 `--refresh` 会刷新该连接的配对，断开其已有设备。`connectionId` 从 `connections list` 读取，V4／V3 分别为 `ws-v4`／`ws-v3`。设备列表中每台设备的 `controlId` 是写操作的显式目标；不能使用设备名称、原始硬件 `id` 或 GUI 当前焦点代替。

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
```

开始输出前 A/B 必须都有输入源。输入源 ID 从 `sources list` 读取；固定波形为 `source-fixed-waveform`；预装插件实例可删除或创建其他实例，应按返回的 `id` 操作。普通停止只清理目标设备波形，保留基础强度，插件和音频继续运行。`sync --device $device --enabled on` 开启全设备强度同步，显式使用该设备作为基准；关闭用 `off`。`sources sync --device $device --enabled on` 控制该设备 A/B 的输入源同步。`sources default none` 表示新设备每次询问。

波形导入支持 `.pulse`、`.json`、`.pulses`；JSON 可为帧数组、含 `frames` / `pulseData` 的对象或对象数组。解析、容量、名称和帧校验在核心统一执行，单个文件最多 2 MiB；CLI 每批波形文本最多 4 MiB。`waveforms parse` 只解析不保存，`waveforms import` 导入并分配自定义 ID。`waveforms get <id>` 读取自定义完整配置；`delete <id>` 删除；`reorder <id>…` 提供全部自定义 ID 保存顺序。

```powershell
& $cli waveforms parse .\demo.pulse --json
& $cli waveforms import .\demo.pulse .\demo.json --json
& $cli waveforms set --device $device --channel a --file .\waveform-config.json
```

复杂参数使用 UTF-8 JSON 文件。`waveforms set` 文件是完整 `WaveformConfig`，`safety set` 包含全部安全设置；`sources config` 文件是插件配置，`sources action` 文件是动作的 `value`。对应 `--params` 接受直接 JSON 文本，与 `--file` 互斥。插件配置和动作值可以是对象、数组、字符串、数字、布尔值或 `null`，具体格式由插件校验；公共命令参数及 `sources input` 的外层仍为对象。CLI 在连接核心前读取和解析 JSON，插件验证在核心处理请求时执行。

```powershell
& $cli safety get --json
& $cli safety set --file .\safety.json
```

`commands --json` 无需运行核心，列出全部业务命令、参数 JSON Schema 与只读标记。`call <snake_case 命令> --params <JSON>` 或 `--file <JSON>` 提供完整覆盖；文件是命令参数对象，命令名不写进文件。参数使用 camelCase，`deviceId` 的值必须是设备 `controlId`。例如 `start_output` 的参数文件内容为 `{ "deviceId": "<controlId>" }`；插件动作使用 `source_action`，文件内容为 `{ "sourceId": "<实例 ID>", "params": { "action": "<动作 ID>", "value": 0.5 } }`，数值示例只适用于接受数值的插件动作。

```powershell
& $cli commands --json
& $cli call start_output --file .\start-output.json --json
& $cli call source_action --file .\source-action.json --json
```

`get_app_preferences`、`set_close_to_tray`、`set_start_minimized` 属于 GUI 管理，不暴露给 CLI/MCP。GUI 的设备焦点保存在各自窗口中，不是核心共享状态；CLI/MCP 通过显式目标执行操作，不切换窗口焦点。

## 插件与实例

核心只内置固定波形。触控和音频为普通预装插件；本地安装、更新、卸载与第三方使用相同入口。原生插件以当前用户权限运行，首次使用时启动，普通输出停止不停止插件。插件本身不持有核心。

```powershell
& $cli plugins list --json
& $cli plugins install .\example-source.dglabplugin --json
$source = & $cli sources create example.pulse-source --name "示例实例" --json | ConvertFrom-Json
& $cli sources ui $source.id --json
$instance = & $cli sources list --json | ConvertFrom-Json | Where-Object id -EQ $source.id
& $cli sources config $source.id --expected-revision $instance.revision --file .\source-config.json --json
& $cli sources bind --device $device --channel a $source.id --json
$state = & $cli status --json | ConvertFrom-Json
$binding = $state.sourceBindings | Where-Object { $_.controlId -eq $device -and $_.channel -eq 'a' }
& $cli sources ui $source.id --control --binding $binding.bindingId --json
& $cli sources action $source.id core_snapshot --json
& $cli sources stop $source.id --json
& $cli sources enable $source.id off --json
& $cli plugins update .\example-source-v2.dglabplugin --json
& $cli plugins uninstall example.pulse-source --json
# 卸载默认保留实例、配置和数据；彻底删除需显式 --delete-data
```

`call clear_device_channel --params` 可按设备及通道清空旧波形并保持输出活动；返回绑定代次，不代表设备确认。实例配置通过 `sources config` 持久保存；`--binding <bindingId>` 修改当前设备通道的会话配置，要求插件实现通道配置。上述 SDK 示例只提供实例配置，音频插件的通道配置示例见下文。`--expected-revision` 必填：实例配置从 `sources list` 对应实例的 `revision` 获取，通道配置从 `status.sourceBindings` 对应绑定的 `revision` 获取。它们均不是整个 Hub 的 `revision`。旧版本写入返回 `config_conflict`；重新读取、合并用户修改后再显式提交，不自动重试。`configure`／`configure_binding` 是保留动作，不能用 `sources action` 绕过配置事务。

`bindingId` 为核心生成的 UUID，必须原样使用 `devices[].bindingIdA/B` 或 `sourceBindings[].bindingId`，不能从设备 ID 和通道拼接。替换输入源或重新建立绑定后读取新 ID，避免向已失效的绑定提交配置或输入。`sources input <id> --file` 接收公共 `InputParams` 对象，包含 `action`、`value`、`owner`、递增 `sequence` 及可选 `bindingId`。

`call` 与两种 MCP 开放同一组 `list_plugins/install_plugin/update_plugin/uninstall_plugin/create_source/delete_source/set_source_enabled/start_source/stop_source/set_source_config/get_source_ui/source_action/source_input` 命令。`dglab://plugins` 和 `dglab://sources/<sourceId>` 资源只读，不启动实例。`sources ui` 返回语义控件及动作参数 Schema；例如插件若声明某个动作接受数字，可使用 `sources action <id> <动作 ID> --params '0.5'`，不需要包成对象。完整协议、打包工具和模板见 [插件文档](PLUGINS.md)。

### 预装插件的公共调用

触控与音频操作通过相同的 `sources config/action/input` 入口。实例 ID 从列表读取，以下示例要求相应预装插件已安装且已有实例：

```powershell
$sources = & $cli sources list --json | ConvertFrom-Json
$touch = $sources | Where-Object pluginId -EQ 'cn.dglab.link.touch' | Select-Object -First 1
& $cli sources config $touch.id --expected-revision $touch.revision --file .\touch-config.json --json
& $cli sources bind --device $device --channel a $touch.id --json
$state = & $cli status --json | ConvertFrom-Json
$binding = $state.sourceBindings | Where-Object { $_.controlId -eq $device -and $_.channel -eq 'a' }
$owner = [guid]::NewGuid().ToString()
$touchInput = @{
    action = 'update_touch_input'; bindingId = $binding.bindingId
    owner = $owner; sequence = 1
    value = @{ pointers = @(@{ id = 1; x = 0.5; y = 0.5; cell = $null }) }
} | ConvertTo-Json -Depth 8 -Compress
& $cli sources input $touch.id --params $touchInput --json
# 释放当前绑定的触点；持续按住须在一秒租期内以递增 sequence 续租
$release = @{ action = 'update_touch_input'; bindingId = $binding.bindingId; owner = $owner; sequence = 2; value = @{ pointers = @() } } | ConvertTo-Json -Depth 8 -Compress
& $cli sources input $touch.id --params $release --json
```

触控插件在绑定上下文中确定目标设备和通道，`value.pointers` 的每个触点包含 `id`、归一化 `x`／`y` 及可选 `cell`。输出仍由 `output start --device` 管理；释放一个绑定不释放另一个绑定。插件限制活动所有者，过期或乱序输入不会复活触点。

音频播放、采集、录音和映射均在插件 Rust 工作线程执行，`audio_control` 是音频插件的动作 ID。`sources list` 对应实例的 `state` 包含音频活动状态，`runtimeStatus` 表示插件进程状态。实例默认映射配置及逐通道配置的完整格式见 [预装插件](PLUGIN_BUILTINS.md)。

```powershell
$audio = $sources | Where-Object pluginId -EQ 'cn.dglab.link.audio' | Select-Object -First 1
& $cli sources bind --device $device --channel b $audio.id --json
$state = & $cli status --json | ConvertFrom-Json
$audioBinding = $state.sourceBindings | Where-Object { $_.controlId -eq $device -and $_.channel -eq 'b' }
# 通道配置使用该绑定的 revision，不使用实例 revision
& $cli sources config $audio.id --binding $audioBinding.bindingId --expected-revision $audioBinding.revision --file .\audio-config.json --json
$load = @{ type = 'loadFile'; path = (Resolve-Path .\demo.mp3).Path } | ConvertTo-Json -Compress
& $cli sources action $audio.id audio_control --params $load --json
& $cli sources action $audio.id audio_control --params '{"type":"play"}' --json
& $cli sources action $audio.id audio_control --params '{"type":"seek","positionMs":1000}' --json
& $cli sources action $audio.id audio_control --params '{"type":"pause"}' --json
& $cli sources action $audio.id audio_control --params '{"type":"startMicrophone"}' --json
& $cli sources action $audio.id audio_control --params '{"type":"startDesktop"}' --json
& $cli sources action $audio.id audio_control --params '{"type":"startRecording"}' --json
& $cli sources action $audio.id audio_control --params '{"type":"stopRecording"}' --json
$save = @{ type = 'saveRecording'; path = (Join-Path (Get-Location).Path 'recording.wav') } | ConvertTo-Json -Compress
& $cli sources action $audio.id audio_control --params $save --json
& $cli sources action $audio.id audio_control --params '{"type":"stop"}' --json
& $cli sources list --json
```

CLI 的 `--file` 路径和插件安装包路径相对于 CLI 工作目录读取；配置和动作 JSON 内的路径原样传给插件，不相对于 CLI 或 JSON 文件自动转换。相对路径的解释由插件定义；当前音频插件以插件可执行文件所在目录为工作目录解析相对路径。GUI 文件选择器返回绝对路径，CLI/MCP 也应像以上示例提供绝对媒体和录音路径。

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

将返回的 `command` 与 `args` 放入客户端的 stdio MCP 配置。客户端启动 MCP 子进程后，子进程自动连接已有 core，或启动同目录 core，并持续持有这一个会话。冷启动只恢复配置，不连接 Relay、不开始输出；初始化后仍须明确调用 `connect_transport`，例如参数 `{ "transport": "ws_v4" }`，或使用 `connect_bluetooth` 连接已发现的蓝牙设备。

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

两种传输的 MCP 工具名称都与 `ControlCommand` 的 snake_case 名称一致，例如 `get_hub_snapshot`、`connect_transport`、`adjust_intensity`、`start_output`、`stop_output`、`import_waveform_files`。设备写参数仍为 `{ "deviceId": "<controlId>" }`。插件操作使用 `source_action`／`source_input`；配置使用 `set_source_config`，参数包含 `sourceId`、`config`、`expectedRevision` 和可选 `bindingId`，版本来源与 CLI 相同。工具使用相同核心校验、持久化、回滚、队列和停止优先级；Hub 快照、设备、输入源与运行记录还通过只读资源 `dglab://status`、`dglab://devices`、`dglab://sources`、`dglab://plugins`、`dglab://sources/<sourceId>`、`dglab://logs` 提供。

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

含版本 1 插件清单的旧 profile 会在 core 启动校验时失败，不能原地执行 `plugins update` 升级。先备份旧目录，再用 `--config-dir <新的空目录>` 重新安装版本 2 包、创建实例及绑定。GUI 固定使用默认 profile，需要备份并移走旧默认目录后由新版 GUI 重建。完整命令与数据恢复要求见 [破坏性升级](PLUGINS.md#破坏性升级)。

排障时可直接执行 `dg-lab-link-core --json --config-dir <目录>`，端口占用等启动错误以 `{code, message}` 写入 stderr；core 的 `--port` 在绑定成功后持久保存控制端口，`--relay-endpoint` 用于覆盖模拟 Relay 地址。直接启动 core 不增加持有者，十秒内没有 GUI/CLI/stdio MCP 持有时仍按原生命周期退出；HTTP observer 不延长这个期限。需要长期运行时使用 CLI `serve` 或保持 stdio MCP 会话。

CLI/MCP/core 无界面运行也需要本机音频设备才可使用音频采集或播放；设备输出和声音功能仍须按 [真机验收清单](REAL_DEVICE_CHECKLIST.md) 验证。

V3／V4 连接管理、郊狼 3.0 BLE 扫描和设备参数用法见 [传输接入说明](TRANSPORTS.md)。两种 MCP 传输共用新增工具及 `dglab://connections`、`dglab://bluetooth` 资源。
