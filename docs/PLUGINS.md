# 原生输入源插件

DG-LAB Link 以插件包、输入源实例和通道绑定组织输入源。固定波形是核心基础能力；触控和音频是预装原生插件，使用和第三方相同的协议、语义控件、配置事务及业务接口。插件实现采集、网络服务、输入解释与波形算法，core 接收波形并调度设备。

首版只支持 Windows 本地安装包，不包含商店或远程下载。原生插件以当前用户权限运行；独立进程负责故障隔离，不是权限沙箱。插件包可包含其自行携带的运行时，无需 DG-LAB Link 安装 Node.js 或 Python。插件来源只用于署名和预装标记，不改变可调用能力。

## 安装包与实例

`.dglabplugin` 是 ZIP，根目录必须含 `plugin.json`：

```json
{
    "id": "example.pulse-source",
    "version": "1.0.0",
    "protocolVersion": 1,
    "name": "示例输入源",
    "publisher": "Example Developer",
    "license": "AGPL-3.0-only",
    "executable": "bin/source.exe"
}
```

`id` 为小写反向域名格式；`executable` 为包内 Windows `.exe` 相对路径。首版允许未签名包，发布者字段是包作者声明，SHA-256 摘要用于包完整性和本地版本定位，不证明发布者身份。

安装器限制压缩包 64 MiB、展开内容 256 MiB、条目 1024、清单 64 KiB，拒绝路径穿越、链接、特殊文件、DOS 设备名称、重复与大小写冲突路径，并检查入口的 Windows PE 头。仅启用 ZIP Store/Deflate；文件先在暂存目录校验并展开，再发布到摘要目录，原子保存注册表。

一个包可创建多个实例；每个实例有独立进程、配置和数据目录。最多同时运行 32 个实例，注册表最多保留 128 个实例，全局最多 64 个输出通道绑定。core 重启只加载实例定义；首次使用、打开插件界面或显式启动时才运行插件。插件异常退出不自动重启，故障只影响其绑定。

更新先锁定包生命周期和关联实例集合，再停止关联实例，调用待安装插件的 `migrate` 转换已安装实例的配置并校验，通过后一次提交包和配置。期间创建、启动、删除和使用该包实例返回 `plugin_busy`，不能混用旧进程与新包目录。失败保留旧包、旧配置；设备输出不自动恢复。卸载先隔离包目录，失败恢复目录和注册表并允许重试。卸载默认保留实例定义、配置和数据，重新安装相同 ID 可继续使用；显式删除数据同时清除实例。首次初始化才创建预装包和默认实例；用户卸载预装包后不会在下次启动时自动安装。

插件进程不是核心持有者。core 正常退出时并行通知插件关闭，随后终止超时子进程；Windows Job Object 在核心异常退出时收回插件子进程树。普通设备停止仅停止设备输出，插件采集和界面仍继续工作。

## IPC 与波形契约

SDK 位于 `crates/plugin-sdk`，包名 `dg-lab-link-plugin-sdk`。协议与语言无关，可以自行实现，不要求使用 Rust。stdin/stdout 专用于私有 IPC，日志写 stderr。消息为 **4 字节 little-endian 无符号长度 + UTF-8 JSON**，单条上限 1 MiB，禁止输出普通文本到 stdout。

```json
{"type":"request","id":1,"method":"ui","operation_epoch":4,"params":{"surface":"control","bindingId":"<不透明绑定 UUID>"}}
{"type":"response","id":1,"result":{"title":"控制","nodes":[]},"error":null}
{"type":"notification","method":"frame","operation_epoch":null,"params":{"bindingId":"<不透明绑定 UUID>","generation":3,"sequence":7,"frame":{"samples":[{"frequency":100,"pulseIntensity":20},{"frequency":100,"pulseIntensity":20},{"frequency":100,"pulseIntensity":20},{"frequency":100,"pulseIntensity":20}]}}}
```

请求和响应可以由任一侧发出，`id` 只在发起方范围内唯一。错误保持 `{code,message}`。请求和通知带可空 `operation_epoch`；SDK 为每次动作／输入保留宿主给出的上下文，异步克隆不会自动更新。`bindingId` 为不透明 UUID，不可由设备和通道拼接；换源后旧 ID 不再有效。core 请求方法如下：

| 方法 | 参数与行为 |
| --- | --- |
| `migrate` | 更新时、初始化前调用；`{fromVersion,config}` → 新配置。SDK 默认原样返回。实现应只迁移配置，不启动输出。 |
| `initialize` | `{protocolVersion,source,dataDirectory}`，其中 source 为 `{id,pluginId,name,enabled,config}`。版本不兼容或配置无效返回错误。 |
| `configure` | `{config,validateOnly}`。验证阶段不得修改运行状态；正式应用失败须报错，core 将重新应用旧配置；恢复失败停止实例。 |
| `bindings` | 最新绑定数组，元素为 `{bindingId,controlId,channel,generation,config,active}`；替换旧列表，不是追加。 |
| `action` | `{action,value,bindingId?}`，离散操作。 |
| `ui` | `{surface:"settings"\|"control",bindingId?}` → 语义界面文档。 |
| `input` | `{action,value,bindingId?,owner,sequence}`，持续交互，最新值覆盖旧值。释放、取消和租期由对应输入契约处理。 |
| `shutdown` | 停止插件自身工作线程和采集、完成录音文件，返回后退出。 |

每帧恰好四组 25ms 采样，合计 100ms；`frequency` 为设备频率编码 10..240，并非 Hz；`pulseIntensity` 为波形相对强度 0..100，并非设备基础强度。SDK 每 100ms 调用 `tick`，提供 `emit_frame` 和 `emit_for_bindings`。只向 `active` 绑定生成帧；各绑定可以独立生成，也可使用 SDK 广播同一帧。

core 校验绑定归属、通道代次、进程身份与递增序列号，保存每绑定最新一帧，消费一次后清空。停止、解绑及重连令旧帧失效；缺帧输出静默，连续 500ms 无有效帧时停输受影响通道。插件 I/O、配置、界面及业务调用不进入 Hub 实时锁。缓慢插件不能积累长期波形播放队列。

插件通知包括 `frame` 和 `status`，后者更新实例状态，每实例状态及日志合计最大 64 KiB。stderr 每条日志截断为 4096 字节，展示每条最多 512 字符，实例只保留最近 64 条。普通 IPC 请求队列为 64、等待响应最多 32、插件反向业务请求最多 16。请求超时不自动重试，避免重复设备写操作。

插件通过 `context.business_call` 发出 `core.call`，参数为公开类型化业务命令，和 GUI、CLI、两种 MCP 调用同一个 `ControlService`：

```json
{"type":"request","id":8,"method":"core.call","operation_epoch":4,"params":{"command":"get_hub_snapshot"}}
```

设备写操作必须给出明确设备 ID。`clear_device_channel` 清空指定通道的旧波形，保留基础强度与输出活动，返回 `{bindingId,generation}`；该结果表示宿主已隔离旧帧并排队清理，不代表设备 RPC 已确认。插件不能绕过领域校验或直接获取 Relay/GATT 句柄；它可调用所有公开核心业务能力，第一方没有额外权限。同实例正在处理界面或配置时，重入该实例返回 `queue_busy`，不等待形成 RPC 死锁。

停止会撤销旧操作上下文。旧动作／输入派生的异步 `core.call` 若会恢复输出、连接或调整强度，返回 `queue_busy`，不得自动重试。长期插件 HTTP 服务或采集服务应在**新的外部业务事件到达时**调用 `context.begin_operation().await?`，取得新的上下文副本，再将其传入该事件的异步任务。原上下文不改变；禁止为了重试停止前的旧任务而调用此方法。私有 `core.begin_operation` 参数为 `null`，返回 `{"operationEpoch":4}`，与反向业务请求共用容量和超时。单纯读取状态和普通通道清理不要求可恢复活动的上下文。

实例和绑定配置分别拥有 `revision`。公共 `set_source_config` 必须带当前 `expectedRevision`；冲突返回 `config_conflict`，不会启动插件或应用陈旧值。成功保存递增修订号；新绑定从零开始，插件包更新中的配置迁移也递增实例修订号。`revision` 用于并发冲突检测。`source_action` 的 `configure`／`configure_binding` 名称为宿主保留入口；配置须使用公共事务命令，不能通过离散动作绕过修订号检查。插件收到的私有 `configure`／`configure_binding` 校验和回滚流程保持一致。

实现逐通道配置的插件还需在私有 `action` 方法中处理以下 `ActionParams`；宿主先以 `validateOnly:true` 校验，再以 `false` 应用，失败时用同一形状重新应用旧配置。验证阶段不得改变运行状态，应用阶段按 `bindingId` 更新该通道。下例只展示消息形状，`config` 必须符合目标插件的配置格式。未实现此能力的插件可返回 `unsupported_action`；SDK 的 `pulse-source` 示例和 Rust 模板只实现实例配置，音频插件提供通道配置实现。

宿主在整个绑定事务（包括 Hub 提交）中持有同一个实例锁和会话。并发停止返回 `queue_busy`；应用或提交失败在原会话内回滚，恢复失败停止该实例并报告 `rollback_failed`，不会为回滚重新创建进程。

```json
{
    "action": "configure_binding",
    "bindingId": "<不透明绑定 UUID>",
    "value": {
        "bindingId": "<同一绑定 UUID>",
        "config": { "gain": 1.0 },
        "validateOnly": true
    }
}
```

## 语义界面

所有控件向全部插件公开，GUI 统一渲染，不执行插件 HTML、JavaScript 或 CSS。文档为 `{title,nodes,actions,revision}`；节点为 `{id,type,label?,value?,configKey?,action?,input?,props,children}`，节点 ID 必须唯一。最多 512 个节点、16 层、128 个动作。配置草稿由 GUI 保存，提交时携带最初读取的配置修订号；刷新不会覆盖正在编辑的值，冲突时显示重新加载入口。静态 UI 读取按实例有界排队，GUI 合并相同读取并按 source／surface／binding／revision 缓存；离散写操作不自动重试。

公共节点类型包括 `page/section/stack/group/form/list`、`text/status/key_value/progress/divider`、`button/switch/text_field/integer_field/number_field/select/slider`，以及 `xy_pad/grid/audio_player/meter/curve/waveform_picker/file_field`。

`props` 声明控件属性，例如 slider 的 `min/max/step`、select 的选项、曲线的坐标点和触控板的布局。`configKey` 指定配置字段，`action` 指定提交或离散动作，`input` 指定持续输入动作。动作描述包含 `{id,label,description,paramsSchema}`，CLI/MCP 可先读取描述；`configure` 表单提交经带 `expectedRevision` 的 `set_source_config`，其他离散动作经 `source_action`。`bindingId` 将通道面板与设备通道关联，配置页面使用实例上下文。文件选择返回绝对路径。

首版控件契约如下；所有节点都可用 `props.disabled` 禁用交互，容器和表单的禁用状态向子控件及提交按钮传播。字段置于 `form` 中时通过 `configKey` 更新表单草稿；字段置于表单外时把新值交给其 `action`。配置字段名是当前对象的直接键，不解析点分隔路径。离散动作参数和配置允许任意 JSON 值，具体语义由插件 Schema 和校验决定。

| 节点 | `value` 与公开 `props` |
| --- | --- |
| `form` | `value` 为初始配置对象，未展示的配置字段保留；`submitLabel` 指定提交文案，`configPrefix` 选择实例配置中的一个子对象。默认动作 `configure`，通道上下文由 `bindingId` 传递。 |
| `text_field`、数值字段、`slider`、`switch` | 值分别为字符串、数字或布尔；字符串支持 `maxLength`，数字支持 `min/max/step`。 |
| `select` | `options` 为 `[{value,label}]`，值可为字符串或数字。 |
| `curve` | 值为至少两个 `{x,y}` 点，`x` 归一化到 0..1；支持 `min/max/unit`。 |
| `waveform_picker` | 值为完整 `WaveformConfig`；`multiple:true` 使用数组，`count` 指定区域数，`nullable:true` 允许单选无波形。目录来自公共 `list_waveforms`。 |
| `xy_pad`、`grid` | `input` 指定持续输入动作；每次发送 `{pointers:[{id,x,y,cell}]}`，释放发送空数组。坐标为 0..1，格编号从零开始；`grid` 支持 `rows/columns/cells`，格可为文字或 `{label}`。面板每 200ms 续租，失焦或关闭释放。 |
| 触控专用布局 | 任意插件均可在 `xy_pad/grid` 提供 `touchConfig` 使用公开的完整触控布局，并用 `channel` 选择 `a/b`；值结构与预装触控 `TouchConfig` 相同，参见 [预装插件](PLUGIN_BUILTINS.md)。宿主仍使用该节点的公共输入动作。 |
| `audio_player` | 值为公开 `AudioSnapshot`；`modes` 可筛选 `file/microphone/recording/desktop`，支持 `description/channel`。动作收到 `AudioAction` 对象，如 `{type:"play"}`；默认动作名为 `audio_control`。快照与动作格式见 `crates/builtin-plugins/src/sources/audio.rs`，预装插件提供完整 UI 示例。 |
| `file_field` | 选择文件后，有 `configKey` 则更新绝对路径字段，否则调用动作并发送 `{...props.payload,path}`。不启动文件内容或插件提供的脚本。 |
| `meter`、`progress`、`key_value` | 分别使用数字／数字数组、数字、对象；支持 `min/max`，仪表数组可提供 `labels`。 |

持续输入的 core 返回结果表示已接收入队，不是插件或设备执行确认。插件自行约定输入错误与租期反馈，可通过 `status` 展示；需要明确执行结果的操作使用 `action` 请求。GUI 自动附带输入所有者与递增序列号，CLI/MCP 调用 `source_input` 时需自行提供。

## 构建示例与模板

真实示例在 `crates/plugin-sdk/examples/pulse-source.rs`，包括波形输出、配置验证、语义滑块及读取核心快照的反向调用。构建并打包：

```powershell
cargo build -p dg-lab-link-plugin-sdk --example pulse-source --release
New-Item -ItemType Directory -Force tmp/example-plugin | Out-Null
Copy-Item crates/plugin-sdk/examples/package/plugin.json tmp/example-plugin/plugin.json
Copy-Item src-tauri/target/release/examples/pulse-source.exe tmp/example-plugin/pulse-source.exe
cargo run -p dg-lab-link-plugin-runtime --bin dg-lab-link-plugin-pack -- tmp/example-plugin tmp/example-source.dglabplugin
```

在“输入源 → 插件管理”选择该包安装，再创建输入源实例并将设备 A/B 通道绑定到实例。也可经 CLI 类型化 `call` 调用 `install_plugin`、`create_source`、`set_device_channel_source` 等命令；GUI、CLI、HTTP MCP、stdio MCP 共用相同命令与状态。

打包输出必须位于输入目录之外，包含 Windows 路径别名、大小写与符号链接解析后的实际路径检查。打包先写同级临时文件，完成后原子发布；失败不会截断输入清单、入口 exe 或已有输出包。宿主、SDK 和包清单使用相同的初始协议 `protocolVersion: 1`。应用尚未发布，当前开发接口没有历史版本升级流程。

独立 Rust 项目模板在 `crates/plugin-sdk/templates/rust`。可在仓库内直接执行 `cargo build --manifest-path crates/plugin-sdk/templates/rust/Cargo.toml --release`。复制到其他目录后修改 `dg-lab-link-plugin-sdk` 的路径依赖，修改清单 ID、发布者和入口，并将编译后的 `custom-source.exe` 与清单放在同一打包目录。SDK 与示例遵循本项目 AGPL-3.0-only；插件自己的许可证由清单声明，第三方开发者自行选择是否复用 SDK 或独立实现公开协议。

## 依赖与验证

新增 ZIP 解析依赖为 [zip 8.6.0 官方文档](https://docs.rs/zip/8.6.0)和[官方代码仓库](https://github.com/zip-rs/zip2)，MIT 许可证，最低 Rust 1.88，低于项目 Rust 1.95。只启用 flate2 Deflate 并选 Rust 后端，不引入加密、Zstandard、LZMA 或 Zopfli。SHA-256、Tokio、serde、Windows API 等复用已有依赖系列。

自动验证覆盖路径与大小写冲突、包内容一致、一次消费帧、迟到代次、进程身份隔离、实例持久化、卸载标记、真实第三方 exe 的多实例输出、语义界面、业务回调、配置并发与回滚、暂存更新失败和启动/退出竞争。模拟和本机进程测试不替代设备及真实音频验收；真机清单见项目验收文档。
