# DG-LAB Link

可扩展的 DG-LAB 波形连接中枢。桌面端使用 Tauri 2、React 与 TypeScript；Rust 后端连接 DG-LAB Socket V4 Relay，并把编译进应用的波形输入分别路由到 DG-LAB 4 App 的 A、B 通道。

> 当前为首个可运行版本。自动化测试覆盖协议编码、Hub、安全停止与浏览器交互，但仍需按照真机清单完成手机、蓝牙和设备验证后再用于实际输出。

## 当前能力

- 对接官方 Socket V4 Relay，生成 DG-LAB 4 App 配对二维码。
- 同步多个 APP 下的全部设备槽位、强度和通道状态；开始后向所有在线设备并行输出（单次最多 32 台，超出时明确拒绝启动）。
- 可选开启“同步所有设备”，立即按当前控制设备对齐所有在线设备的 A/B 强度，并在后续调整中保持相同目标值；默认关闭。
- 内置手动波形与可重复测试波形；新输入源通过 `SourceFactory` 编译期注册，不加载运行时动态插件。
- 输入源页面可设置新接入设备 A/B 共用的默认输入源，也可选择“每次询问”而不自动分配；每台设备可在自己的仪表盘标签中分别选择 A、B 通道输入源，也可开启 A/B 源同步。开启后两路立即重置为当前默认源，之后修改任一路都会同步到另一路。多路绑定同一输入源实例时，同一帧只生成一次再按设备与通道扇出。控制台可直接切换控制焦点，或将任意标签拉出为一台设备一个独立窗口，切换和关闭设备窗口都不会停止其他设备。
- 支持输出启停与可选的连接超时自动断开（默认关闭，默认时长 60 分钟）；计时从 Relay 建立连接开始，到期停止所有输出并断开 Relay。A/B 通道强度上限以设备通过协议上报的数值为准，连接超时与手机反向控制设置会持久保存。
- 普通停止逐台清空所有输出设备；紧急停止优先清空所有在线设备，再将每台设备的两个通道归零。
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
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

`npm run build` 是 `build:client` 的别名。

当前 `bundle.active` 为 `false`，因此以下命令只生成裸可执行文件，不会产出安装包、更新包或签名 bundle：

```powershell
npm run tauri -- build
```

架构边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，连接真实设备前请逐项执行 [docs/REAL_DEVICE_CHECKLIST.md](docs/REAL_DEVICE_CHECKLIST.md)。

## 协议说明

项目以 DG-LAB Socket V4 为主线。旧 Socket V3 目前不作为运行路径；只有出现明确的旧版 APP 兼容需求后才会增加。

本项目依据公开协议文档独立实现，采用 AGPL-3.0-only 许可证。DG-LAB 相关商标与产品归其权利人所有。
