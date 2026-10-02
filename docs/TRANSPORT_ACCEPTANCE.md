# V3 WS 与郊狼 3.0 BLE 实现验收

> 历史记录：此文档保留当时验证与调查。当前版本已将触控／音频迁为原生插件并移除急停；当前行为与验收见 [插件文档](PLUGINS.md)及[本次验收](PLUGIN_ACCEPTANCE.md)。

日期：2026-10-01。平台：Windows；Rust/Cargo 1.95.0、Node.js 24.15.0、npm 11.12.1，项目版本 0.1.0。实现前基线 `373d6cf`，工作区干净；未进行推送或其他远程写操作。

## 已实现范围

保持 core、CLI、GUI、MCP 四个可执行文件。同一个 core 管理 V4/V3 各一个 WS 连接和多个 Windows 郊狼 3.0 BLE 会话；设备通过带连接身份的地址隔离。GUI、CLI、HTTP MCP、stdio MCP 都调用共享 ControlService，保留旧 V4 命令、快照 `connection` 与 GUI 快照事件。

V3 直接发送 100ms 波形，相对强度采用反馈驱动的单位步进。BLE 先订阅、无输出查询、BF 和标准模式初始化，再允许控制；相对强度只发送一次，等待匹配 B1 时波形不重复附带强度变化。配置与端点持久化；连接、基础强度、输出、触点和音频活动不在 core 重启后恢复。

普通停止保持基础强度；紧急停止优先废弃旧操作、停止音频并发送无输出和归零。持续同步使用显式基准设备，GUI 焦点变化不会更换基准；基准断开后暂停自动同步。紧停后等待相关设备两路实际强度反馈归零，避免旧状态触发补偿。BF 仅标记 `sent`，没有设备回执的写操作不宣称设备已确认。

## 自动验证

| 检查 | 结果 |
| --- | --- |
| 分阶段核心快照 | 核心解耦阶段核心 157 项通过、1 项忽略，runtime 7 项和共享集成 9 项通过；V3 阶段传输 15 项通过；BLE 阶段传输 36 项通过。 |
| `cargo test --workspace --offline --quiet` | 246 项通过、1 项忽略；其中最终核心 197 项通过、1 项忽略，runtime 共享集成 11 项通过。 |
| `cargo fmt --all --check` | 通过。 |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 通过。 |
| 前端类型检查、Vitest 与构建 | 通过，67 项前端测试。 |
| 模拟器测试与构建 | 通过，10 项测试。 |
| `npm run check:waveforms` | 通过，24 个内置波形一致。 |
| 三个无界面程序 debug 构建 | 通过；进程测试使用更新后的 core、CLI、MCP 程序。 |
| `npm run tauri -- build --no-bundle` | 最终构建通过，同时生成四个 release 可执行文件；生产 CLI 描述器包含全部 10 个新增命令。 |

生产输出位于 `src-tauri/target/release/`：`dg-lab-link-core.exe`、`dg-lab-link-cli.exe`、`dg-lab-link-gui.exe`、`dg-lab-link-mcp.exe`。沿用裸可执行文件交付；未生成安装包。Vite 对 526KB 的主 JS 块给出体积提示，构建成功。

测试证据与覆盖：

- `crates/core/src/hub/transport_tests.rs`：三种传输与两台 BLE 的寻址和故障隔离、未知元数据、旧 actor 迟到事件、取消回报、慢会话停止、显式同步基准及紧停归零屏障。该层注入类型化适配器事件，未访问真实蓝牙。
- `crates/core/src/transport/v3.rs` 与 `crates/runtime/tests/shared_runtime.rs`：本机模拟 Relay 的配对、二维码、心跳、双通道波形、单位步进和反馈、断开；V4/V3 同时运行时客户端看到相同设备 ID 和错误码。断开 V4 保留 V3 输出，最后持有者释放发送清理和归零并关闭连接。
- `crates/core/src/transport/ble/tests.rs`、`ble/protocol.rs`、`ble/backend.rs`：替换 GATT 后端的初始化顺序、默认参数、匹配与迟到 B1、序列号回绕、旋钮、BF 回滚、扩展超时、单通道清理、100ms 合并波形、慢原生写入、慢观察者和紧停。原生租期与错误分类测试不调用 Windows 蓝牙。
- `crates/core/src/control.rs` 与 `preferences.rs`：串行配置、端点并发和持久化失败回滚、停止越过配置锁、旧配置默认值及停止取消排队连接。
- `crates/cli/tests/lifecycle.rs` 与 `crates/mcp/tests/stdio.rs`：真实 CLI 和两种 MCP 协议入口使用相同新增命令；MCP 修改 V3 端点后其他客户端读取一致，BLE 参数校验错误码一致，连接和发现资源与共享快照一致。
- 现有进程与运行时测试：并发冷启动单实例、持有者按 ID 释放、异常退出、端口冲突、鉴权、非法 Host/Origin、容量限制、慢客户端及拥塞下停止、MCP 协议协商。
- `src/pages/Transports.test.tsx`：浏览器演示的 V3 专属配对、主动 BLE 发现和连接、未知电量、V4 断开后 BLE 控制、参数校验与状态。Tauri 命令代理编译和测试通过；未进行 GUI 真机手动操作。

忽略项是需要本机默认播放设备的既有桌面音频 loopback 测试，不计入通过数量。网络编解码、Hub 注入事件与模拟 GATT 分层验证不能代替三种真实链路同时运行的硬件验收。

## 真机状态

V3 官方 APP、Windows 原生 BLE 扫描与连接、郊狼 3.0 持续输出、实体旋钮、不同固件的 BC/BD/C4 扩展、触控释放、真实音频、紧急停止与最后持有者退出清理均未进行真机验收。执行时需记录 Windows/适配器、APP 和设备固件版本、测试结果及异常，见 [真机验收清单](REAL_DEVICE_CHECKLIST.md)。

使用方式见 [多传输连接说明](TRANSPORTS.md)，协议与 APK 证据见 [调查记录](TRANSPORT_RESEARCH.md)。
