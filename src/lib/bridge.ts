import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
    AppPreferences,
    HubChannel,
    HubSnapshot,
    LogSnapshot,
    SafetyUpdate,
    WaveformConfig,
} from "./contracts";

const SNAPSHOT_EVENT = "hub://snapshot";

type SnapshotListener = (snapshot: HubSnapshot) => void;
type WindowAction = "minimize" | "toggleMaximize" | "close";

const mockListeners = new Set<SnapshotListener>();
let mockAppPreferences: AppPreferences = {
    closeToTray: true,
    autoStart: false,
    startMinimized: true,
};

const now = () => new Date().toISOString();

const makeLog = (
    id: string,
    level: LogSnapshot["level"],
    message: string,
): LogSnapshot => ({
    id,
    level,
    message,
    timestamp: now(),
});

const createDefaultMockSnapshot = (): HubSnapshot => {
    const primaryDevice = {
        controlId: "demo-app:slot-a1",
        id: "coyote-030-demo",
        name: "郊狼 3.0",
        type: "COYOTE_030",
        slotId: "slot-a1",
        power: 86,
        intensityA: 5,
        intensityB: 11,
        intensityLimitA: 100,
        intensityLimitB: 100,
        sourceIdA: "source-fixed-waveform",
        sourceIdB: "source-fixed-waveform",
        waveformIdA: "BREATHING",
        waveformIdB: "BUBBLE",
        waveformNameA: "呼吸",
        waveformNameB: "气泡",
        sourceSync: false,
        outputActive: false,
        channelAStatus: "ready" as const,
        channelBStatus: "ready" as const,
    };
    const secondaryDevice = {
        controlId: "demo-app:slot-b1",
        id: "coyote-020-demo",
        name: "郊狼 2.0",
        type: "COYOTE",
        slotId: "slot-b1",
        power: 64,
        intensityA: 3,
        intensityB: 7,
        intensityLimitA: 80,
        intensityLimitB: 80,
        sourceIdA: "source-fixed-waveform",
        sourceIdB: "source-fixed-waveform",
        waveformIdA: "BREATHING",
        waveformIdB: "BREATHING",
        waveformNameA: "呼吸",
        waveformNameB: "呼吸",
        sourceSync: false,
        outputActive: false,
        channelAStatus: "ready" as const,
        channelBStatus: "disabled" as const,
    };
    return {
    revision: 1,
    connection: {
        state: "connected",
        endpoint: "wss://trex.dungeon-lab.cn/v4",
        controllerId: "7f2ac91e",
        pairingUrl:
            "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Ftrex.dungeon-lab.cn%2Fv4%3Ftid%3D7f2ac91e",
        appCount: 1,
        lastError: null,
    },
    device: { ...primaryDevice },
    devices: [{ ...primaryDevice }, { ...secondaryDevice }],
    selectedDeviceId: primaryDevice.controlId,
    syncAllDevices: false,
    outputDeviceCount: 0,
    sources: [
        {
            id: "source-fixed-waveform",
            kind: "builtin.fixed_waveform",
            name: "固定波形",
            enabled: true,
            assignedChannelCount: 4,
            selectedPresetId: null,
            selectedPresetName: null,
        },
    ],
    customWaveforms: [
        {
            id: "custom-demo",
            name: "演示波形",
            frameCount: 2,
            durationMs: 200,
        },
    ],
    defaultSourceId: null,
    output: {
        state: "idle",
        framesSent: 1284,
        lastError: null,
    },
    channels: {
        a: {
            intensity: 5,
            limit: primaryDevice.intensityLimitA,
            status: "ready",
        },
        b: {
            intensity: 11,
            limit: primaryDevice.intensityLimitB,
            status: "ready",
        },
    },
    safety: {
        connectionTimeoutEnabled: false,
        connectionTimeoutMinutes: 60,
        allowAppIntensityControl: false,
    },
    logs: [
        makeLog("log-3", "info", "DG-LAB 4 APP 已连接"),
        makeLog("log-2", "info", "检测到设备：郊狼 3.0"),
        makeLog("log-1", "info", "固定波形已设为当前输入源"),
    ],
    };
};

let mockSnapshot = createDefaultMockSnapshot();
let mockStartOutputCompletion: Promise<void> | null = null;

const cloneSnapshot = (snapshot: HubSnapshot): HubSnapshot =>
    structuredClone(snapshot);

const isTauriRuntime = (): boolean => {
    if (typeof window === "undefined") {
        return false;
    }

    return "__TAURI_INTERNALS__" in window || "__TAURI__" in window;
};

export const isBrowserDemo = (): boolean => !isTauriRuntime();

const emitMockSnapshot = (): void => {
    const snapshot = cloneSnapshot(mockSnapshot);
    mockListeners.forEach((listener) => listener(snapshot));
};

const updateMockSnapshot = (
    updater: (snapshot: HubSnapshot) => void,
): HubSnapshot => {
    const next = cloneSnapshot(mockSnapshot);
    next.revision += 1;
    updater(next);
    mockSnapshot = next;
    emitMockSnapshot();
    return cloneSnapshot(next);
};

const prependMockLog = (
    snapshot: HubSnapshot,
    level: LogSnapshot["level"],
    message: string,
): void => {
    snapshot.logs.unshift(
        makeLog(`log-${snapshot.revision}-${snapshot.logs.length}`, level, message),
    );
    snapshot.logs = snapshot.logs.slice(0, 100);
};

const refreshMockSourceCounts = (snapshot: HubSnapshot): void => {
    snapshot.sources.forEach((source) => {
        source.assignedChannelCount = snapshot.devices.reduce(
            (count, device) =>
                count +
                Number(device.sourceIdA === source.id) +
                Number(device.sourceIdB === source.id),
            0,
        );
    });
};

export const getHubSnapshot = async (): Promise<HubSnapshot> => {
    if (isTauriRuntime()) {
        return invoke<HubSnapshot>("get_hub_snapshot");
    }

    return cloneSnapshot(mockSnapshot);
};

export const getAppPreferences = async (): Promise<AppPreferences> => {
    if (isTauriRuntime()) {
        return invoke<AppPreferences>("get_app_preferences");
    }

    return { ...mockAppPreferences };
};

export const setCloseToTray = async (
    enabled: boolean,
): Promise<AppPreferences> => {
    if (isTauriRuntime()) {
        return invoke<AppPreferences>("set_close_to_tray", { enabled });
    }

    mockAppPreferences = { ...mockAppPreferences, closeToTray: enabled };
    return { ...mockAppPreferences };
};

export const setAutoStart = async (
    enabled: boolean,
): Promise<AppPreferences> => {
    if (isTauriRuntime()) {
        return invoke<AppPreferences>("set_auto_start", { enabled });
    }

    mockAppPreferences = { ...mockAppPreferences, autoStart: enabled };
    return { ...mockAppPreferences };
};

export const setStartMinimized = async (
    enabled: boolean,
): Promise<AppPreferences> => {
    if (isTauriRuntime()) {
        return invoke<AppPreferences>("set_start_minimized", { enabled });
    }

    mockAppPreferences = { ...mockAppPreferences, startMinimized: enabled };
    return { ...mockAppPreferences };
};

export const listenHubSnapshot = async (
    listener: SnapshotListener,
): Promise<UnlistenFn> => {
    if (isTauriRuntime()) {
        return listen<HubSnapshot>(SNAPSHOT_EVENT, (event) => {
            listener(event.payload);
        });
    }

    mockListeners.add(listener);
    return () => {
        mockListeners.delete(listener);
    };
};

export const connectRelay = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("connect_relay");
        return;
    }

    updateMockSnapshot((snapshot) => {
        const defaults = createDefaultMockSnapshot();
        snapshot.connection = defaults.connection;
        snapshot.device = defaults.device;
        snapshot.devices = defaults.devices;
        snapshot.sources = defaults.sources;
        snapshot.selectedDeviceId = defaults.selectedDeviceId;
        snapshot.outputDeviceCount = 0;
        snapshot.channels.a.status = "ready";
        snapshot.channels.b.status = "ready";
        snapshot.connection.lastError = null;
        prependMockLog(snapshot, "info", "Relay 已重新连接");
    });
};

export const disconnectRelay = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("disconnect_relay");
        return;
    }

    updateMockSnapshot((snapshot) => {
        snapshot.connection.state = "disconnected";
        snapshot.connection.appCount = 0;
        snapshot.connection.controllerId = null;
        snapshot.connection.pairingUrl = null;
        snapshot.device = null;
        snapshot.devices = [];
        snapshot.sources.forEach((source) => {
            source.assignedChannelCount = 0;
        });
        snapshot.selectedDeviceId = null;
        snapshot.outputDeviceCount = 0;
        snapshot.output.state = "idle";
        snapshot.channels.a.status = "disconnected";
        snapshot.channels.b.status = "disconnected";
        prependMockLog(snapshot, "warning", "已断开 Relay 连接");
    });
};

export const refreshConnection = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("refresh_pairing");
        return;
    }

    updateMockSnapshot((snapshot) => {
        prependMockLog(snapshot, "info", "连接状态已刷新");
    });
};

export const adjustIntensity = async (
    channel: HubChannel,
    delta: number,
    deviceId?: string,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("adjust_intensity", {
            channel,
            delta,
            deviceId: deviceId ?? null,
        });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const roundedDelta = Math.round(delta);
        const selected = snapshot.devices.find(
            (device) =>
                device.controlId === (deviceId ?? snapshot.selectedDeviceId),
        );
        if (!selected) {
            throw new Error("设备尚未连接");
        }
        const selectedCurrent = channel === "a"
            ? selected.intensityA
            : selected.intensityB;
        const targetIntensity = selectedCurrent + roundedDelta;
        const targets = snapshot.syncAllDevices ? snapshot.devices : [selected];
        const changes = targets.map((device) => {
            const deviceLimit = channel === "a"
                ? device.intensityLimitA
                : device.intensityLimitB;
            if (targetIntensity < 0 || targetIntensity > deviceLimit) {
                throw new Error("调整后的强度会超过设备上报的通道上限或低于 0");
            }
            return { device, target: targetIntensity };
        });
        changes.forEach(({ device, target }) => {
            if (channel === "a") {
                device.intensityA = target;
            } else {
                device.intensityB = target;
            }
        });
        const selectedIntensity = selected
            ? channel === "a"
                ? selected.intensityA
                : selected.intensityB
            : snapshot.channels[channel].intensity;
        const isCurrentControlDevice =
            snapshot.selectedDeviceId === selected.controlId;
        if (isCurrentControlDevice) {
            snapshot.channels[channel].intensity = selectedIntensity;
        }
        if (snapshot.device?.controlId === selected.controlId) {
            if (channel === "a") {
                snapshot.device.intensityA = selectedIntensity;
            } else {
                snapshot.device.intensityB = selectedIntensity;
            }
        }
        prependMockLog(
            snapshot,
            "info",
            snapshot.syncAllDevices
                ? `${targets.length} 台设备的 ${channel.toUpperCase()} 通道已同步到 ${targetIntensity}`
                : `${channel.toUpperCase()} 通道强度已调整为 ${selectedIntensity}`,
        );
    });
};

export const setSyncAllDevices = async (enabled: boolean): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_sync_all_devices", { enabled });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const selected = snapshot.devices.find(
            (device) => device.controlId === snapshot.selectedDeviceId,
        );
        if (enabled && !selected) {
            throw new Error("设备尚未连接");
        }
        if (enabled && selected) {
            for (const device of snapshot.devices) {
                const limitA = device.intensityLimitA;
                const limitB = device.intensityLimitB;
                if (selected.intensityA > limitA || selected.intensityB > limitB) {
                    throw new Error("当前控制设备的强度超过其他设备的安全上限");
                }
            }
            snapshot.devices.forEach((device) => {
                device.intensityA = selected.intensityA;
                device.intensityB = selected.intensityB;
            });
        }
        snapshot.syncAllDevices = enabled;
        prependMockLog(
            snapshot,
            "info",
            enabled
                ? "已开启所有设备强度同步控制，并完成 A/B 强度对齐"
                : "已关闭所有设备强度同步控制",
        );
    });
};

export const startOutput = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("start_output", { deviceId });
        return;
    }

    await mockStartOutputCompletion;
    updateMockSnapshot((snapshot) => {
        if (snapshot.connection.state !== "connected") {
            throw new Error("设备尚未连接");
        }
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        if (!device.sourceIdA || !device.sourceIdB) {
            throw new Error("请先为此设备的 A/B 通道选择输入源");
        }
        snapshot.output.state = "running";
        device.outputActive = true;
        device.channelAStatus =
            device.channelAStatus === "disabled" ? "disabled" : "active";
        device.channelBStatus =
            device.channelBStatus === "disabled" ? "disabled" : "active";
        snapshot.outputDeviceCount = snapshot.devices.filter(
            (candidate) => candidate.outputActive,
        ).length;
        snapshot.output.lastError = null;
        snapshot.output.framesSent += 1;
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
            snapshot.channels.a.status = device.channelAStatus;
            snapshot.channels.b.status = device.channelBStatus;
        }
        prependMockLog(snapshot, "info", `${device.name} 的波形输出已开始`);
    });
};

export const stopOutput = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("stop_output", { deviceId });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        device.outputActive = false;
        device.channelAStatus =
            device.channelAStatus === "active" ? "ready" : device.channelAStatus;
        device.channelBStatus =
            device.channelBStatus === "active" ? "ready" : device.channelBStatus;
        snapshot.outputDeviceCount = snapshot.devices.filter(
            (candidate) => candidate.outputActive,
        ).length;
        snapshot.output.state = snapshot.outputDeviceCount > 0 ? "running" : "idle";
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
            snapshot.channels.a.status = device.channelAStatus;
            snapshot.channels.b.status = device.channelBStatus;
        }
        prependMockLog(snapshot, "info", `${device.name} 的波形输出已停止`);
    });
};

export const emergencyStop = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("emergency_stop");
        return;
    }

    updateMockSnapshot((snapshot) => {
        snapshot.output.state = "stopped";
        snapshot.outputDeviceCount = 0;
        snapshot.devices.forEach((device) => {
            device.outputActive = false;
            device.channelAStatus =
                device.channelAStatus === "active" ? "ready" : device.channelAStatus;
            device.channelBStatus =
                device.channelBStatus === "active" ? "ready" : device.channelBStatus;
        });
        snapshot.channels.a.status = snapshot.device ? "ready" : "disconnected";
        snapshot.channels.b.status = snapshot.device ? "ready" : "disconnected";
        prependMockLog(snapshot, "warning", "已执行紧急停止并清空输出队列");
    });
};

export const setDeviceChannelSource = async (
    deviceId: string,
    channel: HubChannel,
    sourceId: string,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_device_channel_source", {
            deviceId,
            channel,
            sourceId,
        });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        const target = snapshot.sources.find((source) => source.id === sourceId);
        if (!target || !target.enabled) {
            throw new Error("输入源不可用");
        }
        if (device.sourceSync) {
            device.sourceIdA = sourceId;
            device.sourceIdB = sourceId;
        } else if (channel === "a") {
            device.sourceIdA = sourceId;
        } else {
            device.sourceIdB = sourceId;
        }
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
        }
        refreshMockSourceCounts(snapshot);
        prependMockLog(
            snapshot,
            "info",
            `${device.name} 的 ${device.sourceSync ? "A/B" : channel.toUpperCase()} 通道已切换输入源：${target.name}`,
        );
    });
};

export const setDeviceChannelSourceSync = async (
    deviceId: string,
    enabled: boolean,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_device_channel_source_sync", { deviceId, enabled });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        if (device.sourceSync === enabled) {
            return;
        }
        device.sourceSync = enabled;
        if (enabled) {
            device.sourceIdA = snapshot.defaultSourceId;
            device.sourceIdB = snapshot.defaultSourceId;
            if (!snapshot.defaultSourceId) {
                device.outputActive = false;
                snapshot.outputDeviceCount = snapshot.devices.filter(
                    (candidate) => candidate.outputActive,
                ).length;
                if (snapshot.output.state === "running" && snapshot.outputDeviceCount === 0) {
                    snapshot.output.state = "idle";
                }
            }
        }
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
        }
        refreshMockSourceCounts(snapshot);
        prependMockLog(
            snapshot,
            "info",
            enabled
                ? `${device.name} 已开启 A/B 输入源同步并重置为默认源`
                : `${device.name} 已关闭 A/B 输入源同步`,
        );
    });
};

export const setDefaultSource = async (sourceId: string | null): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_default_source", { sourceId });
        return;
    }

    updateMockSnapshot((snapshot) => {
        if (sourceId === null) {
            snapshot.defaultSourceId = null;
            prependMockLog(
                snapshot,
                "info",
                "默认输入源已改为每次询问；已有设备绑定保持不变",
            );
            return;
        }
        const target = snapshot.sources.find((source) => source.id === sourceId);
        if (!target || !target.enabled) {
            throw new Error("输入源不可用");
        }
        snapshot.defaultSourceId = sourceId;
        prependMockLog(
            snapshot,
            "info",
            `默认输入源已切换：${target.name}；已有设备绑定保持不变`,
        );
    });
};

export const setFixedWaveform = async (
    deviceId: string,
    channel: HubChannel,
    config: WaveformConfig,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_fixed_waveform", { deviceId, channel, config });
        return;
    }

    updateMockSnapshot((snapshot) => {
        requireMockFixedWaveformSource(snapshot);
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        const sourceId = channel === "a" ? device.sourceIdA : device.sourceIdB;
        if (sourceId !== "source-fixed-waveform") {
            throw new Error("此通道没有使用固定波形输入源");
        }
        if (channel === "a") {
            device.waveformIdA = config.presetId;
            device.waveformNameA = config.presetName;
        } else {
            device.waveformIdB = config.presetId;
            device.waveformNameB = config.presetName;
        }
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
        }
        prependMockLog(
            snapshot,
            "info",
            `${device.name} 的 ${channel.toUpperCase()} 通道固定波形已切换：${config.presetName}`,
        );
    });
};

export const importCustomWaveforms = async (
    configs: WaveformConfig[],
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("import_custom_waveforms", { configs });
        return;
    }
    updateMockSnapshot((snapshot) => {
        if (snapshot.customWaveforms.length + configs.length > 128) {
            throw new Error("自定义波形库最多保存 128 项");
        }
        for (const config of configs) {
            if (snapshot.customWaveforms.some((waveform) => waveform.id === config.presetId)) {
                throw new Error("自定义波形标识不能重复");
            }
            snapshot.customWaveforms.push({
                id: config.presetId,
                name: config.presetName,
                frameCount: config.frames.length,
                durationMs: config.frames.length * 100,
            });
        }
        prependMockLog(snapshot, "info", `已导入 ${configs.length} 个自定义波形`);
    });
};

export const selectCustomWaveform = async (
    deviceId: string,
    channel: HubChannel,
    presetId: string,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("select_custom_waveform", { deviceId, channel, presetId });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const waveform = snapshot.customWaveforms.find((item) => item.id === presetId);
        if (!waveform) {
            throw new Error("选择的自定义波形不存在");
        }
        requireMockFixedWaveformSource(snapshot);
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        const sourceId = channel === "a" ? device.sourceIdA : device.sourceIdB;
        if (sourceId !== "source-fixed-waveform") {
            throw new Error("此通道没有使用固定波形输入源");
        }
        if (channel === "a") {
            device.waveformIdA = waveform.id;
            device.waveformNameA = waveform.name;
        } else {
            device.waveformIdB = waveform.id;
            device.waveformNameB = waveform.name;
        }
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device = { ...device };
        }
        prependMockLog(
            snapshot,
            "info",
            `${device.name} 的 ${channel.toUpperCase()} 通道自定义波形已切换：${waveform.name}`,
        );
    });
};

export const deleteCustomWaveform = async (presetId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("delete_custom_waveform", { presetId });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const index = snapshot.customWaveforms.findIndex((item) => item.id === presetId);
        if (index < 0) {
            throw new Error("要删除的自定义波形不存在");
        }
        snapshot.customWaveforms.splice(index, 1);
        snapshot.devices.forEach((device) => {
            if (device.waveformIdA === presetId) {
                device.waveformIdA = null;
                device.waveformNameA = null;
            }
            if (device.waveformIdB === presetId) {
                device.waveformIdB = null;
                device.waveformNameB = null;
            }
        });
        const selected = snapshot.devices.find(
            (device) => device.controlId === snapshot.selectedDeviceId,
        );
        if (selected) {
            snapshot.device = { ...selected };
        }
        prependMockLog(snapshot, "info", "已删除自定义波形");
    });
};

export const reorderCustomWaveforms = async (presetIds: string[]): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("reorder_custom_waveforms", { presetIds });
        return;
    }
    updateMockSnapshot((snapshot) => {
        if (presetIds.length !== snapshot.customWaveforms.length) {
            throw new Error("排序结果必须包含全部自定义波形");
        }
        const byId = new Map(snapshot.customWaveforms.map((waveform) => [waveform.id, waveform]));
        const reordered = presetIds.map((id) => byId.get(id));
        if (reordered.some((waveform) => !waveform) || new Set(presetIds).size !== presetIds.length) {
            throw new Error("排序结果包含未知或重复的自定义波形");
        }
        snapshot.customWaveforms = reordered as typeof snapshot.customWaveforms;
        prependMockLog(snapshot, "info", "自定义波形顺序已更新");
    });
};

const requireMockFixedWaveformSource = (snapshot: HubSnapshot) => {
    const source = snapshot.sources.find(
        (candidate) => candidate.kind === "builtin.fixed_waveform",
    );
    if (!source) {
        throw new Error("固定波形输入源不可用");
    }
    return source;
};

export const selectDevice = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("select_device", { deviceId });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        snapshot.selectedDeviceId = deviceId;
        snapshot.device = { ...device };
        snapshot.channels.a = {
            intensity: device.intensityA,
            limit: device.intensityLimitA,
            status: device.channelAStatus,
        };
        snapshot.channels.b = {
            intensity: device.intensityB,
            limit: device.intensityLimitB,
            status: device.channelBStatus,
        };
        prependMockLog(snapshot, "info", `已切换当前控制设备：${device.name}`);
    });
};

export const updateSafety = async (update: SafetyUpdate): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("update_safety", {
            connectionTimeoutEnabled: update.connectionTimeoutEnabled,
            connectionTimeoutMinutes: update.connectionTimeoutMinutes,
            allowAppIntensityControl: update.allowAppIntensityControl,
        });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const connectionTimeoutMinutes = Math.max(
            1,
            Math.min(1440, update.connectionTimeoutMinutes),
        );
        snapshot.safety = {
            ...snapshot.safety,
            connectionTimeoutEnabled: update.connectionTimeoutEnabled,
            connectionTimeoutMinutes,
            allowAppIntensityControl: update.allowAppIntensityControl,
        };
        prependMockLog(snapshot, "info", "安全限制已更新");
    });
};

export const performWindowAction = async (
    action: WindowAction,
): Promise<void> => {
    if (!isTauriRuntime()) {
        return;
    }

    const currentWindow = getCurrentWindow();
    if (action === "minimize") {
        await currentWindow.minimize();
    } else if (action === "toggleMaximize") {
        await currentWindow.toggleMaximize();
    } else {
        await currentWindow.close();
    }
};

export const __resetMockBridge = (): void => {
    mockSnapshot = createDefaultMockSnapshot();
    mockAppPreferences = {
        closeToTray: true,
        autoStart: false,
        startMinimized: true,
    };
    mockStartOutputCompletion = null;
    emitMockSnapshot();
};

export const __emitMockSnapshot = (snapshot: HubSnapshot): void => {
    mockSnapshot = cloneSnapshot(snapshot);
    emitMockSnapshot();
};

export const __setMockStartOutputCompletion = (
    completion: Promise<void> | null,
): void => {
    mockStartOutputCompletion = completion;
};
