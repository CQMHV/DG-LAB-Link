# Socket V4、Socket V3 与郊狼 3.0 蓝牙直连

三种传输同时运行在同一个 `dg-lab-link-core` 中。GUI、CLI、stdio MCP 和 HTTP MCP 共享 Hub、设备会话、输入源、强度与安全配置，不为某个入口另外建立实时连接。每种 WS 协议各维护一个 Relay 连接，蓝牙可连接多台郊狼 3.0；同时输出最多 32 台设备。

首版蓝牙面向 Windows，仅支持郊狼 3.0。Socket V4 保留现有设备与控制端标识；Socket V3 的一个 APP 配对作为一个双通道设备，未提供的型号、电量和回路状态显示为未知。

## GUI 操作

在“设备”页面分别管理 Socket V4、Socket V3 与蓝牙连接。WS 端点需先断开对应连接后修改并保存，再点击连接；配对按钮打开该连接自己的控制端 ID 和二维码。刷新配对会断开该连接已有的 APP 和设备，其他连接继续运行。

开启郊狼 3.0 并断开手机 APP 的蓝牙连接后，点击“扫描蓝牙设备”，选择结果中的设备连接。扫描只更新发现结果，不自动连接或开启输出。蓝牙不可用、权限不足或初始化失败时显示错误，WS 会话可继续运行。

新连接设备完成初始化后出现在设备列表，可在仪表盘标签页或独立窗口使用同一套强度、输入源、触控、音频与输出控件。蓝牙设备卡片的“蓝牙参数”提供软上限、频率平衡、强度平衡及受固件支持限制的旋钮保护配置。参数成功下发后持久保存；失败时由核心恢复旧配置，恢复失败则停止该设备并报告错误。

手机反向控制设置只作用于 Socket V4／V3。蓝牙直连始终接纳 B1 中的实际强度，包括实体旋钮调整；关闭全设备同步时只影响自身，开启时沿用显式基准设备的同步规则。

## CLI 连接与寻址

多步 CLI 操作前保持 GUI 运行，或先创建后台持有者。一条业务 CLI 命令执行完会释放自己的临时持有关系；最后一个持有者退出会停止输出、归零、断开全部连接并退出 core。

```powershell
$cli = ".\dg-lab-link-cli.exe"
$holder = & $cli serve --background --json | ConvertFrom-Json
& $cli connections list --json
& $cli connections connect --transport v4 --json
& $cli connections connect --transport v3 --json
& $cli connections pairing ws-v4 --json
& $cli connections pairing ws-v3 --json
& $cli devices --json
```

`connections connect` 默认 V4。旧的 `relay connect`、`relay disconnect` 和 `pairing` 仍只操作 V4；断开 V4 不影响 V3 或蓝牙。默认 V3 端点为 `wss://ws.dungeon-lab.cn/`。

```powershell
# 只断开 V3，然后持久保存自定义 Relay 端点
& $cli connections disconnect ws-v3 --json
& $cli connections endpoint --transport v3 ws://127.0.0.1:9000 --json
& $cli connections connect --transport v3 --json
# 重新生成 V3 配对，会断开此连接已有的 APP
& $cli connections pairing ws-v3 --refresh --json
```

也可使用 `connections connect --transport v3 --endpoint <url>`。`connections list` 返回 `connectionId`、`transport`、连接状态、端点和配对信息；`status`／`watch` 快照新增 `connections`、`bluetooth` 和每台设备的连接来源、能力、初始化及参数状态。旧 `connection` 字段继续表示 V4。

三种标识用途不同，不能互换：

| 标识 | 来源 | 用途 |
| --- | --- | --- |
| `connectionId` | `connections list`、快照 `connections` | 断开连接、刷新 WS 配对 |
| 蓝牙发现 `deviceId` | `bluetooth scan`、快照 `bluetooth` | 首次或重新连接蓝牙设备 |
| 设备 `controlId` | `devices`、快照 `devices` | 强度、输出、输入源、同步、蓝牙断开及参数配置 |

```powershell
& $cli bluetooth scan --duration-ms 3000 --json
# 将扫描结果的 deviceId 原样传入，保留引号
& $cli bluetooth connect "<扫描返回的 deviceId>" --json
& $cli devices --json
$device = "<蓝牙设备的 controlId>"
& $cli bluetooth config --device $device --json
& $cli bluetooth config --device $device --file .\ble-config.json --json
& $cli intensity --device $device --channel a --delta 1 --json
& $cli output stop --device $device --json
& $cli bluetooth disconnect --device $device --json
# 任务结束，只释放本次创建的持有者
& $cli holders release $holder.holderId --json
```

扫描时长范围为 100–10000ms，结果最多 64 项。扫描不代表设备已通过 GATT 校验；连接时还会验证郊狼 3.0 特征。不存在或不支持的设备、重复连接和未初始化设备均返回结构化 `{code, message}` 错误。普通写操作超时后先读取状态，不自动重试。

蓝牙参数文件是配置对象，`--params` 与 `--file` 互斥；无参数时读取当前配置。以下为新设备默认值。软上限范围 0–200，平衡范围 0–255，旋钮保护值范围 1–50。省略字段采用默认值，因此更新已有配置时应提供完整对象。

```json
{
    "maxStrengthA": 100,
    "maxStrengthB": 100,
    "frequencyBalanceA": 160,
    "frequencyBalanceB": 160,
    "strengthBalanceA": 0,
    "strengthBalanceB": 0,
    "wheelProtectionEnabled": true,
    "wheelProtectionValue": 10
}
```

全部新增能力也支持 `call`；命令名使用 snake_case，参数使用 camelCase，`transport` 的 wire 值为 `ws_v4`／`ws_v3`／`ble`。`connect_transport` 仅接受 WS 传输，蓝牙使用专门命令。

```powershell
& $cli commands --json
& $cli call connect_transport --params '{"transport":"ws_v3"}' --json
& $cli call scan_bluetooth --params '{"durationMs":3000}' --json
& $cli call get_bluetooth_config --params '{"deviceId":"<controlId>"}' --json
```

## MCP 与状态解释

两种 MCP 传输开放相同的业务工具和参数 Schema。新增工具为 `get_connections`、`connect_transport`、`disconnect_connection`、`refresh_connection_pairing`、`set_relay_endpoint`、`scan_bluetooth`、`connect_bluetooth`、`disconnect_bluetooth`、`get_bluetooth_config` 和 `set_bluetooth_config`；现有强度、波形、触控、音频和停止工具适用于所有设备。

新增只读资源 `dglab://connections` 返回所有连接，`dglab://bluetooth` 返回最近一次主动扫描结果。读取资源不会触发扫描或连接。`dglab://status`、`dglab://devices`、`dglab://sources` 和 `dglab://logs` 沿用共享数据源。

stdio MCP 自动启动或连接 core，并在会话期间持有。HTTP MCP 只连接已有 core，需先保持 GUI 或后台 CLI 持有者，结束后释放本任务自己的 holderId。详细接入与鉴权配置见 [CLI 与本机 MCP](CLI_MCP.md)。

`initialization` 为 `initializing`、`ready` 或 `fault`，完成初始化前不能输出。能力字段用于判断电量、回路状态、软上限、平衡、旋钮保护、标准模式和操作反馈支持情况。`power: null` 代表未知，不能解读为电量耗尽；缺少回路状态同样不能解读为未接负载。

BLE `configurationStatus: "sent"` 表示 BF 参数的 GATT 写入成功。BF 没有设备回执，已下发不等于设备已确认；B1 用于确认强度变化，BC／BD、C4 是独立固件扩展。首版固定标准模式，旋钮保护按固件能力开放，扩展不可用时展示能力缺失。

核心持续发送 100ms 波形，使用同一输入源和停止代次。V3 大幅相对调整按单位步进并等待反馈；BLE 相对强度只发送一次并等待匹配 B1，等待时波形不重复附带强度调整。普通停止保留基础强度；紧急停止优先清理所有设备并归零，旧排队操作不能恢复输出。重启 core 只恢复端点与参数，不恢复连接、基础强度、触点、音频活动或输出。

## 真机待验

模拟协议与替换 GATT 后端用于自动验证；真实 APP 与硬件行为必须另行记录版本和结果。待验项目包括 V3 APP 配对与单位步进、Windows BLE 扫描与连接、实体旋钮反馈、不同固件的标准模式和旋钮保护、连续波形、触控释放、音频、紧急停止和最后持有者退出清理。未执行的项目保留在 [真机验收清单](REAL_DEVICE_CHECKLIST.md)，不以自动测试通过替代真机结论。

协议与默认参数证据见 [传输调查记录](TRANSPORT_RESEARCH.md)。首版不包含郊狼 2.0 BLE、温柔模式、完整 APP 舒适限制算法、OTA、设备绑定管理及 macOS／Linux 真机支持。
