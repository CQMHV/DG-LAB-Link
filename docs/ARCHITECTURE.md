# 架构说明

DG-LAB Link 分为 core、GUI、CLI、MCP 四个程序。所有入口连接同一个独立 Rust 核心，共用 `ControlService`、Hub、设备会话与持久配置。React 管理配置与展示，实时链路及 100ms 波形调度不依赖窗口刷新。

```text
React → Tauri 代理 ──────────────────┐
CLI ────────────────────────────────┤
MCP → dg-lab-link-mcp (stdio / HTTP) ┤
                                    ↓ 本机 WebSocket / Bearer
                         dg-lab-link-core /control
                           ControlService
                             ├── PluginManager → 原生插件进程
                             │     ↕ 私有有界 IPC / core.call
                             └── Hub actor
                                   ├── 固定波形
                                   ├── 最新插件帧 / 逐通道绑定
                                   └── V4 / V3 / BLE 设备会话
```

## 运行边界

Cargo workspace 包含核心库、客户端与通信运行时、核心启动程序、CLI、MCP、GUI、插件 SDK、插件运行时及预装插件。核心、CLI、MCP 和原生插件不依赖 Tauri、WebView 或 Node.js。GUI 管理窗口、托盘、自启动和本机文件选择。`rmcp` 两种传输都在 MCP crate，core 只提供本机 `/control` 与业务链路。

`ControlCommand` 是所有入口共用的类型化接口，错误为 `{code,message}`。设备写操作明确指定 `controlId`，全设备同步明确指定基准设备。wire types 与领域模型分开；V4、V3、BLE 分别编码和转换反馈，一条连接的故障只影响所属设备。WS 每协议一条连接，BLE 可多台，同时输出最多 32 台。

普通停止清理指定设备波形并保留基础强度，不停止插件或音频活动。断开清理所属设备，核心退出清理全部链路并归零。普通停止与断开走优先队列，取消旧输出及连接请求代次；旧波形和在途操作不能在停止后恢复。所有网络、输入、进程及响应队列有界。

## 核心生命周期

GUI、业务 CLI、stdio MCP 先连接已有 core，不存在时启动同目录 `dg-lab-link-core`。配置目录内文件锁保证单实例。HTTP MCP 只连接已有核心，不启动或持有 core。GUI 每进程一个持有关系；独立设备窗口共享它，缩到托盘继续持有。常驻 CLI、一次性 CLI、watch 和 stdio MCP 在连接期间持有，插件不是持有者。

正常退出立即释放持有关系；异常退出由本机连接及心跳最长十秒内释放。最后一个持有者退出后停止接收普通请求，并行清理设备及插件，断开三类链路，关闭接口与进程；总期限十秒。核心重启恢复配置和实例定义，不自动扫描、连接、启动插件、恢复触点或输出。

core 默认 `127.0.0.1:17845/control`，HTTP MCP 默认 `127.0.0.1:17846/mcp`；使用保存的随机 Bearer 令牌，检查 Host／Origin，限制请求、连接及响应容量。`mcp config --port` 仅修改停止状态的 HTTP 服务端口。stdio stdout 只输出协议，诊断写 stderr。详细迁移及持有流程见 [CLI/MCP](CLI_MCP.md)。

## 输入源插件

固定波形是唯一核心输入源。触控和音频在 `crates/builtin-plugins`，以普通 `.dglabplugin` 包预装；第三方使用同一 SDK、安装流程、语义 UI、配置事务和完整业务 API。包、实例、通道绑定分开：一个包可有多个实例，每个运行实例一个独立进程，各通道同一时刻只绑定一个实例。

本机包先校验 ZIP 和 Windows exe，再原子安装。更新对配置调用 `migrate`，失败保留旧包及配置；默认卸载保留实例和数据，显式删除才清除。预装包只初始化一次，卸载后不自动重新安装。原生插件以当前用户权限运行，不是权限沙箱；Windows Job Object 收回 core 异常退出时的子进程树。

打开实例界面、绑定通道或显式启动时才创建进程；显式停止、故障与禁用不会自动重启。普通设备停止只更新绑定活动状态和代次，音频采集／播放可继续。停用、卸载、删除或插件故障清理关联通道，其他通道继续。

私有 IPC 为长度前缀 JSON。SDK 处理握手、配置、动作、持续输入、语义 UI 和 `core.call` 反向业务调用。Hub 不等待插件 I/O；每绑定只保留最新一帧，校验来源进程、绑定、代次和序列号。每帧四组 25ms 采样，frequency 为 10..240 设备编码，pulseIntensity 为 0..100 相对波形强度。帧消费一次，缺帧静默，连续 500ms 缺帧只停止该绑定通道。

GUI 通用渲染语义控件，不执行插件 HTML／JavaScript／CSS。表单、滑块、触控板、网格、播放器、曲线、波形选择与文件控件全部向第三方公开；控制台按返回的 UI 文档渲染，没有预装插件种类分支。CLI/MCP 可读取同一 UI 和动作 Schema 后调用公共命令。完整协议与开发模板见 [PLUGINS.md](PLUGINS.md)。

## 配置与通道

实例配置按实例串行完成验证、应用与持久化；插件 I/O 不占全局配置锁。失败重新应用旧值，回滚失败停止实例。通道配置同样验证与回滚，但只保存在设备会话中。安装注册表另有串行原子保存。连接端点、BLE 参数、连接超时和反向控制配置保持原事务路径；旧触控偏好迁入默认预装实例。

固定波形按绑定维护游标，触控按绑定维护租期、触点和波形游标，音频按实例采集并按绑定映射。GUI 每通道交互有独立所有者与单调序列；释放、失焦、关闭面板撤销该路触点，异常输入一秒租期到期。音频工作线程处理 PCM、解码、录音和 FFT，只有 100ms 波形与低频状态跨进程，不传原始 PCM。

设备窗口只改变控制焦点，不改变输出范围。A/B 输入源同步及默认输入源沿用现有规则。波形目录与 `.pulse`／JSON 解析在 core，来自提交的 `shared/official-waveforms.json`；浏览器演示共享目录，仅演示插件状态。来源声明见 `THIRD_PARTY_NOTICES.md`。

## 构建与验证

`build:client` 生成 React；`build:headless` 构建 core、CLI、MCP、预装插件及公共打包器，并将插件包放入 `src-tauri/target/debug/plugins/`。release 版本使用 `release/`。Tauri 开发与生产前置命令执行同样准备步骤。继续交付四个裸可执行文件及同级 `plugins/`，不新增常驻 plugin-host 程序。

SDK 提供示例、独立模板和包工具。测试覆盖包校验、真实进程、一次消费帧、配置回滚、迟到反馈、跨入口资源及设备输出。浏览器模拟器经真实 V4 Relay 接入并模拟设备操作；它不模拟蓝牙时序、负载或体感。自动结果见 [插件验收](PLUGIN_ACCEPTANCE.md)，真机项目见 [REAL_DEVICE_CHECKLIST.md](REAL_DEVICE_CHECKLIST.md)。
