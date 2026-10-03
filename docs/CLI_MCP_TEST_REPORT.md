# CLI、MCP 与 GUI 共享核心验证记录

> 历史记录：此文档保留当时验证与调查。后续开发已将触控／音频迁为原生插件并移除急停；当前行为与验收见 [插件文档](PLUGINS.md)及[本次验收](PLUGIN_ACCEPTANCE.md)。

验证日期：2026-10-01。环境：Windows，本地工作区。

## 自动化检查

| 检查 | 结果 |
| --- | --- |
| `cargo test --workspace --offline --quiet` | MCP 两种传输迁到独立程序后重新执行：188 项通过，1 项按原约定忽略，无失败 |
| MCP 测试（随 workspace 执行） | 7 项单元测试、9 项集成测试通过，覆盖独立 HTTP/stdio 进程及模拟 Relay |
| runtime 测试（随 workspace 执行） | 7 项单元测试、9 项集成测试通过，覆盖观察者、端口配置及跨入口停止代次 |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 通过 |
| `cargo fmt --all --check` / `git diff --check` | 通过 |
| `npm run check` | 通过 |
| `npm test` | 6 个文件、64 项测试通过 |
| `npm run test:simulator` | 1 个文件、10 项测试通过 |
| `npm run build:simulator` | TypeScript 检查和生产构建通过 |
| `node scripts/generate-waveforms.mjs --check` | 24 个内置波形与生成源一致 |

Rust 测试包括独立 core 实际子进程的启动、端口冲突和最后持有者释放后退出，CLI 实际子进程的并发冷启动、后台持有者按 ID 释放、端口冲突，以及运行时和独立 MCP 程序的鉴权、Host/Origin、请求容量、MCP 协议协商与资源读取。持有者测试覆盖正常释放、仅释放指定持有者、心跳失联十秒内释放和最后持有者触发退出。

独立 HTTP 进程检查确认其 PID 与 core 不同，不自动启动 core、不增加持有者，core 最后持有者释放后两个接口关闭且 HTTP 进程退出。检查 HTTP 单实例锁、端口占用、配置更新互斥、8 MiB 请求限制和不完整请求占用 TCP 容量后的回收。core 的控制端口不提供 `/mcp`；依赖树确认 core 不含 `rmcp`、Tauri 或 WebView。

配置检查覆盖 core/HTTP 端口并发更新不丢失、校验失败不覆盖配置。GUI 在线时可更新离线 HTTP 的端口，并读取最新配置地址。

新增 stdio 实际子进程检查包括：冷启动 core、协议协商、全部工具与四项资源、与 HTTP/本机客户端共享配置和错误、EOF 释放最后持有者、保留 GUI 持有者、异常退出、启动端口冲突且 stdout 无诊断污染。显式按 ID 释放 stdio 持有者时保留父进程 stdin 管道开放，MCP 仍在五秒内退出并返回 `core_closed`，GUI 所持 core 的 PID 与实例不变。CLI 配置检查覆盖 HTTP 默认值、stdio 命令和参数的绝对路径、不启动 core、不导出令牌，以及仅 HTTP 参数的互斥校验。

stdio 传输检查覆盖超过 8 MiB 的请求释放持有者、普通请求满载后仍能读取并执行停止、旧输出请求被停止取消、取消请求容量回收，以及阻塞 stdout 不延迟紧急停止并在一秒写入超时后释放响应容量。HTTP 检查普通容量占满时停止仍能执行、响应容量保持到流被读取或断开、16 MiB 响应限制及请求头绝对超时。写入超时通过有界内存流验证生产流包装器的完整 `poll_write` 路径，避免 Windows TCP 自动缓冲使测试无法稳定制造拥塞。

跨入口停止检查确认：HTTP 观察者或 GUI 接受但尚未转发的输出请求，被另一个 CLI 的紧急停止取消，返回 `queue_busy`；读取最新代次后的新请求进入正常业务校验。代次随 WebSocket 传到 core，不在 MCP SDK 调度或客户端转发时重新取得；普通 MCP 命令缺少入口代次时拒绝执行。

本机模拟 V4 Relay 验证同一个控制端 ID 和设备 controlId、GUI 客户端代理/CLI 客户端/MCP 共用状态、跨入口强度修改、释放一个持有者后继续输出，以及最后持有者退出时清空、双通道归零和断开。GUI 入口在该测试中使用其 Rust 客户端代理链路；没有启动原生窗口进行端到端操作。

核心测试覆盖持久配置的并发、失败回滚和调用方取消；波形目录、`.pulse`/JSON 解析与浏览器格式一致性；触控租期和输入源规则；拥塞时的紧急停止、等待 Relay 确认时的停止唤醒，以及停止前排队的连接、输出、强度、同步和音频操作不能恢复活动。音频停止直接使工作线程及播放回调失效，无需等待普通命令队列。

前端测试覆盖波形导入走核心解析、MCP 信息与令牌复制、演示模式，以及核心连接断开后不能由迟到快照恢复可操作状态。慢 TCP 客户端测试验证容量能够回收。

迁出 HTTP 后重新执行 workspace Rust 测试与 Clippy，并重新执行前端类型检查和 64 项测试。模拟器和波形目录实现没有变化，沿用前面已经通过的专项检查结果。联合本地 release 构建重新执行前端 TypeScript 检查与生产构建。

## 本地 release 构建

将 HTTP 和 stdio 都移到独立 MCP 程序后执行 `npm run tauri -- build` 并通过，包含 core、CLI 与 MCP 的 release 构建、前端 TypeScript 检查与生产构建、Tauri GUI release 构建。已确认以下四个产物存在：

- `src-tauri/target/release/dg-lab-link-core.exe`：11,062,272 字节。
- `src-tauri/target/release/dg-lab-link-cli.exe`：5,503,488 字节。
- `src-tauri/target/release/dg-lab-link-mcp.exe`：12,368,896 字节。
- `src-tauri/target/release/dg-lab-link-gui.exe`：13,327,872 字节。

GUI、CLI 和 stdio MCP 从自身目录寻找 core；可按用途分发 core + GUI、core + CLI 或 core + MCP。HTTP MCP 连接已运行的 core，需要 GUI 或 CLI `serve` 保持核心。

core、CLI 和 MCP 本地构建产物的 `--version` 已运行通过；CLI 的 `commands --json` 返回 28 项业务命令。在独立临时配置目录运行 release CLI 的 HTTP/stdio `mcp config --json`，确认两种配置指向同一个 MCP 可执行文件绝对路径，HTTP 使用 `http://127.0.0.1:17846/mcp`，默认配置不导出令牌。没有修改用户默认配置。

HTTP `/mcp` 和 stdio 均由 `dg-lab-link-mcp` 承载，通过本机客户端调用同一核心服务；core 只保留 `/control`。本地生成裸可执行文件，没有生成完整分发包、安装包或签名 bundle，也未发布应用。前端构建仍有大于 500 kB 的分块体积提示，未影响构建成功。

## 未验证项目

- `sources::audio::runtime::tests::windows_desktop_loopback_opens_waits_and_stops` 需要本机默认音频播放设备，保留忽略，未执行。
- 原生 GUI 的窗口、托盘、退出与开机自启交互未手工验收。
- 外部 AI 客户端的 HTTP/stdio 配置、启动及结束会话未手工验收；自动化使用真实 MCP/core 子进程与协议测试客户端。
- 实体 DG-LAB 设备、手机 App、蓝牙、实际电刺激输出及真机音频采集/播放/录音未验收；继续按 [真机验收清单](REAL_DEVICE_CHECKLIST.md) 逐项记录。
- HTTP 工作流中外部 AI 客户端异常退出后的后台 CLI 持有者自动回收不属于首版保证；任务方须保存 holderId 并按 [CLI/MCP 使用文档](CLI_MCP.md) 释放。stdio 子进程的 EOF 与异常退出回收已经自动化验证。

本记录涵盖本地验证；未执行推送或其他远程写操作。
