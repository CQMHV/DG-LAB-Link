# DG-LAB Link

可扩展的 DG-LAB 波形连接中枢。core、GUI、CLI 和 MCP 四个程序按职责分开；GUI、CLI、stdio MCP 与本机 HTTP MCP 共用一个独立 Rust 核心进程、Socket V4、Socket V3 与多台郊狼 3.0 BLE 连接和相同设备会话。桌面端使用 Tauri 2、React 与 TypeScript；CLI、MCP 与核心可独立运行，无需 WebView 或 Node.js。

> 当前为首个可运行版本。自动化测试覆盖协议编码、Hub、安全停止与浏览器交互，但仍需按照真机清单完成手机、蓝牙和设备验证后再用于实际输出。

## 当前能力

- GUI、`dg-lab-link-cli`、stdio MCP 和 Streamable HTTP MCP 使用同一 `ControlService`、Hub、波形库、配置校验和安全停止链路。两种 MCP 传输由 `dg-lab-link-mcp` 提供；HTTP 仅监听本机，使用随机 Bearer 令牌。
- GUI 进程、常驻 CLI、一次性 CLI 和 stdio MCP 进程持有核心；最后一个持有者退出后安全清理并关闭核心。缩到托盘继续持有，独立设备窗口不增加持有者；HTTP MCP 请求本身不持有核心。
- 同时连接 Socket V4、Socket V3 Relay 和多台郊狼 3.0 BLE；WS 各一条连接，Windows 直接蓝牙可扫描、初始化和按设备配置参数，V3 与 V4 独立配对。见 [传输接入说明](docs/TRANSPORTS.md)。
- 同步多个 APP 下的全部设备槽位、强度和通道状态；按设备独立开始和停止输出，同时输出最多 32 台。
- 可选开启“同步所有设备”，立即按显式基准设备（GUI 为当前仪表盘设备）对齐所有在线设备的 A/B 强度，并在后续调整中保持相同目标值；默认关闭。
- 内置固定波形、触控模式和音频模式三个输入源；通过 `SourceFactory` 编译期注册。
- 触控模式支持自由坐标面板、八个波形快捷格、2×2 / 3×3 / 4×4 律动网格，A / B / 同步 / 分离 / 交替路由，坐标轴交换、经典曲线、渐变强度和背景波形。鼠标、触屏和笔共用 Pointer Events；松手或触控租期到期会清理目标通道旧波形，基础强度保留。配置保存在本地。
- 音频模式支持本地音频与视频音轨播放、循环、扬声器开关、实时麦克风、录音回放与 WAV 保存。视频支持 MP4、M4V、MOV、MKV、WebM，自动选择默认的可解码音轨；默认编码不受支持时选择第一条可解码音轨。支持 AAC、MP3、FLAC、ALAC、PCM、Vorbis 等音轨，暂不支持 Opus、AC-3/E-AC-3；无音轨或不支持的编码会显示错误。音量映射相对波形强度，指定频段的频谱峰值映射输出周期；每台设备的 A/B 可独立选择音频声道、增益、固定或自适应阈值、迟滞、频段和映射曲线。录音期间只保存声音，回放时产生波形。导入音轨最多一小时，视频文件上限 2 GB，压缩音频文件上限 200 MB；WAV 允许一小时录音对应的有界 PCM 容量。音频采集、播放、分析在 Rust 工作线程运行。浏览器开发预览只演示界面状态，实际声音功能在桌面端运行。
- 音频源还提供第四种“桌面音频”模式，在 Windows 上监听默认播放设备的系统声音并共用现有通道映射。静音期间清除旧特征并保持监听，声音恢复后自动继续；多声道的其他声道混入左右两路。点击“停止桌面监听”、切换声音模式、紧急停止或退出都会清理采集；更换默认播放设备或设备失效会停止并提示重新开启。桌面采集不会把声音再次播放或自动保存录音。
- 输入源页面可设置新接入设备 A/B 共用的默认输入源，也可选择“每次询问”而不自动分配；每台设备可在自己的仪表盘标签中分别选择 A、B 通道输入源，也可开启 A/B 源同步。开启后两路立即重置为当前默认源，之后修改任一路都会同步到另一路。固定波形游标和音频映射按设备通道独立维护，触控按设备维护触点与路由。控制台可直接切换控制焦点，或将任意标签拉出为一台设备一个独立窗口，切换和关闭设备窗口都不会停止其他设备。
- 支持输出启停与可选的连接超时自动断开（默认关闭，默认时长 60 分钟）；每条连接独立计时，到期只停止所属设备并断开该连接。A/B 通道强度上限以设备通过协议上报的数值为准，连接超时与手机反向控制设置会持久保存。
- 普通停止清空当前设备；紧急停止优先清空所有在线设备，再将每台设备的两个通道归零，并停止音频采集和播放。
- 黑金 DG-LAB 风格控制台，包含输入源、设备、运行记录和设置页面。

## 开发

环境要求：Node.js、Rust stable，以及 Windows 上构建 Tauri 所需的 WebView2/Visual Studio C++ 工具链。

```powershell
npm install
npm run dev
```

`npm run dev` 默认只监听 `127.0.0.1`，不会向局域网开放开发服务器。

启动桌面应用：

```powershell
npm run tauri -- dev
```

桌面开发和生产构建会同时准备 `dg-lab-link-core`、`dg-lab-link-cli`、`dg-lab-link-mcp`、`dg-lab-link-gui` 四个可执行文件；GUI、CLI 和 MCP 从自身目录寻找 core。只开发无界面入口时只需 Rust 工具链：

```powershell
cargo build -p dg-lab-link-core-server -p dg-lab-link-cli -p dg-lab-link-mcp
& .\src-tauri\target\debug\dg-lab-link-cli.exe --help
```

## CLI 与 MCP

先创建一个后台持有者，再执行跨命令操作；保留返回的 `holderId`，任务结束时只释放自己的持有者：

```powershell
$cli = ".\src-tauri\target\debug\dg-lab-link-cli.exe"
$holder = & $cli serve --background --json | ConvertFrom-Json
& $cli relay connect --json
& $cli watch
# Ctrl+C 结束 watch，后台持有者仍保持核心
& $cli holders release $holder.holderId --json
```

MCP 支持两种传输。`mcp config --transport stdio --json` 输出同目录 `dg-lab-link-mcp` 的绝对命令路径和参数，交给支持 stdio 的 AI 客户端启动；MCP 子进程自动连接或唤起 core，并在会话期间持续持有，无须预先运行 GUI 或后台 CLI。stdin EOF 或退出时释放，异常退出由本机连接／心跳检测释放；stdout 只输出 MCP 协议消息。

HTTP MCP 使用独立入口 `dg-lab-link-mcp --transport http`，地址默认为 `http://127.0.0.1:17846/mcp`。先启动 GUI 或 CLI `serve` 保持 core，再启动 HTTP MCP 进程；它只连接已有 core，作为观察者不增加持有者，core 关闭时也会退出。`mcp config` 默认选择 HTTP，输出连接地址和 `server` 启动命令；`mcp config --transport http --show-token --json` 显式导出 Authorization 头。任务结束时关闭自己启动的 HTTP 进程，再释放自己的后台 CLI holderId。后台 CLI 持有者不会跟随外部 AI 客户端自动退出。

core 的本机控制端口默认为 `17845`，只提供 `/control`；MCP HTTP 端口默认 `17846`，只由 MCP 程序提供 `/mcp`。`mcp config --port` 修改 HTTP 端口，要求 HTTP 服务已经停止，core 可继续在线。

完整业务命令、参数文件、AI 客户端连接与生命周期见 [CLI 与 MCP 使用文档](docs/CLI_MCP.md)。

## 被控端模拟器

仓库内包含一个独立的 Socket V4 被控端模拟器，可在没有第二台实体设备时验证多设备控制。它不是主应用中的前端假数据，而是作为真实被控 APP 连接 Relay，并在一个 APP 会话下暴露多台带独立 `slotId` 的虚拟设备。

启动模拟器：

```powershell
npm run dev:simulator
```

浏览器打开 `http://127.0.0.1:1421`，然后：

1. 在 DG-LAB Link 中连接 Relay 并唤出配对二维码。
2. 复制二维码对应的完整配对链接；也可以直接复制当前控制端 ID。
3. 粘贴到模拟器顶部并点击“连接控制端”。
4. DG-LAB Link 的设备页应出现 `虚拟郊狼 1` 和 `虚拟郊狼 2`。
5. 开始输出后，模拟器会按设备和 A/B 通道分别累计波形包与波形帧。
6. 在控制台切换设备标签，分别调整两台设备；点击标签右侧的拉出图标，确认独立窗口只控制其绑定设备。

模拟器支持动态添加/移除设备、单设备蓝牙上下线、手机端反向强度上报、设备强度上限、通道状态、通道屏蔽和 `custom.action`。默认使用官方 Relay；自托管 Relay 可展开“Relay 地址”修改。生产静态产物通过以下命令生成到 `dist/simulator`：

```powershell
npm run build:simulator
```

模拟器不会产生真实电刺激，只验证 Relay、RPC、设备寻址和状态同步。真实蓝牙执行与多设备同时输出仍需实体硬件完成最终验收。

## 验证

```powershell
npm run check
npm test
npm run test:simulator
npm run build:client
npm run build:simulator
npm run check:waveforms
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`npm run build` 是 `build:client` 的别名。`npm run build:headless` 联合构建调试 core、CLI 和 MCP，`npm run build:headless:release` 构建相同的生产程序；`build:cli`、`build:cli:release` 分别是上述命令的别名。内置波形提交在 `shared/official-waveforms.json` 中；只有更新波形依赖时才需要运行 `npm run generate:waveforms`，Rust 构建不需要 Node.js。

当前 `bundle.active` 为 `false`，因此以下命令只生成裸可执行文件，不会产出安装包、更新包或签名 bundle：

```powershell
npm run tauri -- build
```

产物为 `src-tauri/target/release/dg-lab-link-core.exe`、`dg-lab-link-cli.exe`、`dg-lab-link-mcp.exe`、`dg-lab-link-gui.exe`；分发时将所需客户端与 core 放在同一目录，并保留许可证和第三方声明。仅使用 GUI 时可分发 core + GUI；仅使用 stdio MCP 时可分发 core + MCP；HTTP MCP 需要 core + MCP，并通过 GUI 或 CLI `serve` 持有核心。

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，连接真实设备前请逐项执行 [docs/REAL_DEVICE_CHECKLIST.md](docs/REAL_DEVICE_CHECKLIST.md)。

本次共享核心改造的自动化结果、构建产物与未验证项目见 [CLI/MCP 验证记录](docs/CLI_MCP_TEST_REPORT.md)。

## 协议说明

项目以 DG-LAB Socket V4 为主线。旧 Socket V3 目前不作为运行路径；只有出现明确的旧版 APP 兼容需求后才会增加。

本项目依据公开协议文档独立实现，采用 AGPL-3.0-only 许可证。DG-LAB 相关商标与产品归其权利人所有。
