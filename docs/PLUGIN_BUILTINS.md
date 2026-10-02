# 预装输入源插件

固定波形仍由核心提供。触控与音频为普通原生插件；第三方使用完全相同的 SDK、包格式、语义控件与核心业务接口。其来源只用于名称与发布者展示。

| 插件 ID | 可执行入口 | 默认实例 ID |
| --- | --- | --- |
| `cn.dglab.link.touch` | `dg-lab-link-touch.exe` | `source-touch` |
| `cn.dglab.link.audio` | `dg-lab-link-audio.exe` | `source-audio` |

实现位于 `crates/builtin-plugins`，不依赖 core、Tauri、WebView 或 Node.js。`packages/touch/plugin.json` 与 `packages/audio/plugin.json` 使用公共清单格式。构建后把对应二进制放入清单同目录的暂存副本，再通过公共 `dg-lab-link-plugin-pack` 工具生成 `.dglabplugin` 包。生产程序使用同一安装流程加载预装包。

## 触控

实例配置为原有 `TouchConfig`：自由与律动模式、通道分配、网格、坐标轴、强度渐变、映射曲线、快捷波形和背景波形。默认波形跟随插件编译，无网络运行依赖。

公共持续输入动作 `update_touch_input` 接收 `bindingId`、`owner`、单调 `sequence` 及 `value.pointers`。输入身份及一秒租期由插件管理；不同通道绑定拥有独立租期。释放、取消或租期到期后停止触点波形，按配置继续背景波形。兼容动作接收原有 `TouchInput`，可同时控制设备双通道。

每个设备通道的播放游标和映射独立。普通停止更新绑定活动状态与代次，只撤销该绑定旧触点，不停用实例。重新启动需要新的有效触点输入。触点身份、波形格、释放及租期变化通过公共 `clear_device_channel` 清理该路旧队列；每绑定一个在途清理，其他路继续。失败记录 `inputErrors`，不自动重发。状态发布 `touchConfig`。

## 音频

实例初始为空对象，保持 idle；创建实例不打开麦克风或桌面采集。可设置 `defaultChannelConfig` 作为新通道的默认映射，绑定配置采用原有 `AudioChannelConfig`。

`audio_control` 动作接受原有 `AudioAction`，包括本地音频／视频音轨、麦克风、录音、Windows 桌面回环、播放／暂停／停止／定位、录音保存及循环和扬声器选项。相对音频或录音路径在插件入口转为绝对路径。

采集、解码及音频输出在独立工作线程运行。控制队列与 PCM 队列有界，FFT 特征采用最新值，超过 500ms 的特征不复用；每帧由四个 25ms 窗口的 RMS 与 Hann 窗 FFT 生成。每个通道独立保存自适应和映射状态。

`configure_binding` 与兼容 `set_audio_config` 支持通道配置；`validateOnly` 只校验，不更改运行状态。状态发布 `audio` 和 `audioBindings`。普通设备停止不关闭音频采集或播放；显式音频停止动作与核心退出分别负责停止和清理音频资源。

## 界面与测试

插件提供公共 `form`、`curve`、`waveform_picker`、`xy_pad`、`grid`、`audio_player`、`meter` 等控件。配置表单使用公共 `configure` 动作，由宿主完成校验、应用与持久事务；普通动作不隐式持久化。

原有触控、音频分析、文件解码与工作线程测试迁入插件 crate，另有原生可执行进程测试覆盖协议握手、双通道独立租期、波形提交、映射配置以及真实测试素材的音轨解码。测试素材为本地生成的合成声音／黑色视频，来源说明保留在测试数据目录。真实音频硬件和 DG-LAB 设备验收须单独执行和记录。
