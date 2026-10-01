# CLI、MCP 与 GUI 共享核心验证记录

验证日期：2026-10-01。环境：Windows，本地工作区。

## 自动化检查

| 检查 | 结果 |
| --- | --- |
| `cargo test --workspace --offline --quiet` | 入口拆分后重新执行：165 项通过，1 项按原约定忽略，无失败 |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 通过 |
| `cargo fmt --all --check` / `git diff --check` | 通过 |
| `npm run check` | 通过 |
| `npm test` | 6 个文件、63 项测试通过 |
| `npm run test:simulator` | 1 个文件、10 项测试通过 |
| `npm run build:simulator` | TypeScript 检查和生产构建通过 |
| `node scripts/generate-waveforms.mjs --check` | 24 个内置波形与生成源一致 |

Rust 测试包括独立 core 实际子进程的启动、端口冲突和最后持有者释放后退出，CLI 实际子进程的并发冷启动、后台持有者按 ID 释放、端口冲突，以及运行时的鉴权、Host/Origin、请求容量、MCP 协议协商与资源读取。持有者测试覆盖正常释放、仅释放指定持有者、心跳失联十秒内释放和最后持有者触发退出。

本机模拟 V4 Relay 验证同一个控制端 ID 和设备 controlId、GUI 客户端代理/CLI 客户端/MCP 共用状态、跨入口强度修改、释放一个持有者后继续输出，以及最后持有者退出时清空、双通道归零和断开。GUI 入口在该测试中使用其 Rust 客户端代理链路；没有启动原生窗口进行端到端操作。

核心测试覆盖持久配置的并发、失败回滚和调用方取消；波形目录、`.pulse`/JSON 解析与浏览器格式一致性；触控租期和输入源规则；拥塞时的紧急停止、等待 Relay 确认时的停止唤醒，以及停止前排队的连接、输出、强度、同步和音频操作不能恢复活动。音频停止直接使工作线程及播放回调失效，无需等待普通命令队列。

前端测试覆盖波形导入走核心解析、MCP 信息与令牌复制、演示模式，以及核心连接断开后不能由迟到快照恢复可操作状态。慢 TCP 客户端测试验证容量能够回收。

入口拆分后重新执行 workspace Rust 测试与 Clippy；前端、模拟器和波形目录实现没有变化，沿用本次改造前面已经通过的专项检查结果。联合发布构建会重新执行前端 TypeScript 检查与生产构建。

## 发布构建

入口拆分后再次执行 `npm run tauri -- build` 并通过，包含独立 core 与 CLI 的发布构建、前端 TypeScript 检查与生产构建、改名后的 Tauri GUI 发布构建。已确认以下三个产物存在：

- `src-tauri/target/release/dg-lab-link-core.exe`：15,799,296 字节。
- `src-tauri/target/release/dg-lab-link-cli.exe`：5,422,080 字节。
- `src-tauri/target/release/dg-lab-link-gui.exe`：13,310,464 字节。

GUI 和 CLI 从自身目录寻找 core；可按用途分发 core + GUI 或 core + CLI。已移除这次构建目录中先前生成的旧 GUI 文件 `dg-lab-link.exe`，避免混用旧入口。

core 和 CLI 发布产物的 `--version` 已运行通过；CLI 的 `commands --json` 返回 28 项业务命令且不启动核心。MCP 仍由 core 内的协议适配模块提供，继续使用同一地址、鉴权和服务。沿用裸可执行文件交付方式，没有生成安装包或签名 bundle。

## 未验证项目

- `sources::audio::runtime::tests::windows_desktop_loopback_opens_waits_and_stops` 需要本机默认音频播放设备，保留忽略，未执行。
- 原生 GUI 的窗口、托盘、退出与开机自启交互未手工验收。
- 实体 DG-LAB 设备、手机 App、蓝牙、实际电刺激输出及真机音频采集/播放/录音未验收；继续按 [真机验收清单](REAL_DEVICE_CHECKLIST.md) 逐项记录。
- 外部 AI 客户端异常退出后的后台持有者自动回收不属于首版保证；任务方须保存 holderId 并按 [CLI/MCP 使用文档](CLI_MCP.md) 释放。

实现和验证阶段未创建 Git 提交或执行远程写操作；随后按用户要求将改造分批提交到本地仓库。未执行推送或其他远程写操作。
