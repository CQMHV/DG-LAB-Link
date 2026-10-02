import type { HubSnapshot, SourceSnapshot, UiDocument, UiNode } from "./contracts";

export const createPluginDemoDocument = (snapshot: HubSnapshot, source: SourceSnapshot, params: { bindingId?: string; surface?: string }): UiDocument => {
    const binding = params.bindingId ? snapshot.devices.flatMap((device) => (["a", "b"] as const).map((channel) => ({ device, channel, id: channel === "a" ? device.bindingIdA : device.bindingIdB }))).find((item) => item.id === params.bindingId) : undefined;
    const field = (key: string, type: UiNode["type"], label: string, value: unknown, props: Record<string, unknown> = {}): UiNode => ({ id: key, type, label, configKey: key, value, props });
    if (source.pluginId === "cn.dglab.link.touch") {
        const config = snapshot.inputModes.touchConfig;
        if (params.surface === "control") return { title: "触控", nodes: [{ id: "touch-input", type: config.mode === "free" ? "xy_pad" : "grid", input: "update_touch_input", props: { touchConfig: config, channel: binding?.channel } }] };
        return { title: "触控设置", nodes: [{ id: "touch-config", type: "form", label: "触控设置", action: "configure", value: config, props: { submitLabel: "应用触控配置" }, children: [
            field("mode", "select", "触控面板", config.mode, { options: [{ value: "free", label: "自由触控" }, { value: "rhythm", label: "律动网格" }] }),
            field("routing", "select", "触控通道分配", config.routing, { options: ["a", "b", "sync", "separate", "alternate"].map((value) => ({ value, label: value })) }),
            field("gridSize", "select", "律动网格大小", config.gridSize, { options: [1, 2, 3, 4].map((value) => ({ value, label: `${value} × ${value}` })) }),
            field("swapAxes", "switch", "交换触控两轴", config.swapAxes),
            field("intensityMode", "select", "触控强度映射", config.intensityMode, { options: ["classic", "gradient"].map((value) => ({ value, label: value })) }),
            field("gradientDirection", "select", "触控渐变方向", config.gradientDirection, { options: ["left", "right", "both"].map((value) => ({ value, label: value })) }),
            field("intensityCurve", "curve", "触控相对强度", config.intensityCurve, { min: 0, max: 100 }),
            field("periodCurve", "curve", "触控周期", config.periodCurve, { min: 10, max: 100 }),
            field("freeWaveforms", "waveform_picker", "自由网格波形", config.freeWaveforms, { multiple: true, count: 8 }),
            field("rhythmWaveforms", "waveform_picker", "律动网格波形", config.rhythmWaveforms, { multiple: true, count: 16 }),
            field("background", "waveform_picker", "触控背景波形", config.background, { nullable: true }),
        ] }] };
    }
    if (source.pluginId === "cn.dglab.link.audio") {
        const config = binding ? snapshot.inputModes.audioBindings.find((item) => item.deviceId === binding.device.controlId && item.channel === binding.channel)?.config : undefined;
        return { title: "音频输入", nodes: [{ id: "audio-player", type: "audio_player", action: "audio_control", value: snapshot.inputModes.audio, props: { channel: binding?.channel } }, ...(config ? [{ id: "audio-config", type: "form" as const, label: `${binding!.channel.toUpperCase()} 通道音频映射`, action: "configure", value: config, children: [
            field("enabled", "switch", "启用映射", config.enabled),
            field("inputChannel", "select", "音频声道", config.inputChannel, { options: ["left", "right", "mix"].map((value) => ({ value, label: value })) }),
            field("gain", "slider", "增益", config.gain, { min: 1, max: 10, step: 0.1 }),
            field("volumeLower", "number_field", "强度下限", config.volumeLower, { min: 0, max: 1, step: 0.01 }),
            field("volumeUpper", "number_field", "强度上限", config.volumeUpper, { min: 0, max: 1, step: 0.01 }),
            field("adaptive", "switch", "自适应", config.adaptive),
            field("adaptiveLower", "number_field", "低适应系数", config.adaptiveLower),
            field("adaptiveUpper", "number_field", "高适应系数", config.adaptiveUpper),
            field("hysteresisMs", "integer_field", "迟滞 ms", config.hysteresisMs),
            field("frequencyMin", "integer_field", "观察频段下限 Hz", config.frequencyMin),
            field("frequencyMax", "integer_field", "观察频段上限 Hz", config.frequencyMax),
            field("periodCurve", "curve", "音频周期", config.periodCurve, { min: 10, max: 100 }),
        ] }] : [])] };
    }
    const config = source.config ?? {};
    return { title: source.name, nodes: [{ id: "sample-form", type: "form", label: "示例插件配置", action: "configure", value: config, children: [field("frequency", "integer_field", "频率编码", config.frequency ?? 100, { min: 10, max: 240 }), field("intensity", "slider", "相对强度", config.intensity ?? 30, { min: 0, max: 100 })] }, { id: "sample-state", type: "status", label: "插件状态", value: source.runtimeStatus ?? "stopped" }] };
};
