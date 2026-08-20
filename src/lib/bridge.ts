import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
    HubChannel,
    HubSnapshot,
    LogSnapshot,
    SafetyUpdate,
} from "./contracts";

const SNAPSHOT_EVENT = "hub://snapshot";

type SnapshotListener = (snapshot: HubSnapshot) => void;
type WindowAction = "minimize" | "toggleMaximize" | "close";

const mockListeners = new Set<SnapshotListener>();

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
        sourceId: "source-test-pattern",
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
        sourceId: "source-manual",
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
            id: "source-test-pattern",
            kind: "builtin.test_pattern",
            name: "测试波形",
            enabled: true,
            assignedDeviceCount: 1,
        },
        {
            id: "source-manual",
            kind: "builtin.manual",
            name: "手动波形",
            enabled: true,
            assignedDeviceCount: 1,
        },
    ],
    defaultSourceId: "source-test-pattern",
    output: {
        state: "idle",
        framesSent: 1284,
        lastError: null,
    },
    channels: {
        a: {
            intensity: 5,
            limit: 80,
            status: "ready",
        },
        b: {
            intensity: 11,
            limit: 80,
            status: "ready",
        },
    },
    safety: {
        channelLimit: 80,
        maxDurationMinutes: 30,
        allowAppIntensityControl: false,
    },
    logs: [
        makeLog("log-3", "info", "DG-LAB 4 APP 已连接"),
        makeLog("log-2", "info", "检测到设备：郊狼 3.0"),
        makeLog("log-1", "info", "测试波形已设为当前输入源"),
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

export const getHubSnapshot = async (): Promise<HubSnapshot> => {
    if (isTauriRuntime()) {
        return invoke<HubSnapshot>("get_hub_snapshot");
    }

    return cloneSnapshot(mockSnapshot);
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
            source.assignedDeviceCount = 0;
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
            const limit = Math.min(snapshot.safety.channelLimit, deviceLimit);
            if (targetIntensity < 0 || targetIntensity > limit) {
                throw new Error("调整后的强度会超过安全上限或低于 0");
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
                const limitA = Math.min(
                    snapshot.safety.channelLimit,
                    device.intensityLimitA,
                );
                const limitB = Math.min(
                    snapshot.safety.channelLimit,
                    device.intensityLimitB,
                );
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

export const startOutput = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("start_output");
        return;
    }

    await mockStartOutputCompletion;
    updateMockSnapshot((snapshot) => {
        if (snapshot.connection.state !== "connected" || !snapshot.device) {
            throw new Error("设备尚未连接");
        }
        if (snapshot.devices.some((device) => !device.sourceId)) {
            throw new Error("请先为所有设备选择输入源");
        }
        snapshot.output.state = "running";
        snapshot.outputDeviceCount = snapshot.devices.length;
        snapshot.devices.forEach((device) => {
            device.outputActive = true;
            device.channelAStatus =
                device.channelAStatus === "disabled" ? "disabled" : "active";
            device.channelBStatus =
                device.channelBStatus === "disabled" ? "disabled" : "active";
        });
        snapshot.output.lastError = null;
        snapshot.output.framesSent += 1;
        snapshot.channels.a.status =
            snapshot.device?.channelAStatus === "disabled"
                ? "disabled"
                : "active";
        snapshot.channels.b.status =
            snapshot.device?.channelBStatus === "disabled"
                ? "disabled"
                : "active";
        prependMockLog(snapshot, "info", "波形输出已开始");
    });
};

export const stopOutput = async (): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("stop_output");
        return;
    }

    updateMockSnapshot((snapshot) => {
        snapshot.output.state = "idle";
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
        prependMockLog(snapshot, "info", "波形输出已停止");
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

export const setDeviceSource = async (
    deviceId: string,
    sourceId: string,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_device_source", { deviceId, sourceId });
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
        device.sourceId = sourceId;
        if (snapshot.device?.controlId === deviceId) {
            snapshot.device.sourceId = sourceId;
        }
        snapshot.sources.forEach((source) => {
            source.assignedDeviceCount = snapshot.devices.filter(
                (candidate) => candidate.sourceId === source.id,
            ).length;
        });
        prependMockLog(
            snapshot,
            "info",
            `${device.name} 已切换输入源：${target.name}`,
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
            limit: Math.min(
                snapshot.safety.channelLimit,
                device.intensityLimitA,
            ),
            status: device.channelAStatus,
        };
        snapshot.channels.b = {
            intensity: device.intensityB,
            limit: Math.min(
                snapshot.safety.channelLimit,
                device.intensityLimitB,
            ),
            status: device.channelBStatus,
        };
        prependMockLog(snapshot, "info", `已切换当前控制设备：${device.name}`);
    });
};

export const updateSafety = async (update: SafetyUpdate): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("update_safety", {
            channelLimit: update.channelLimit,
            maxDurationMinutes: update.maxDurationMinutes,
            allowAppIntensityControl: update.allowAppIntensityControl,
        });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const channelLimit = Math.max(1, Math.min(200, update.channelLimit));
        const maxDurationMinutes = Math.max(
            1,
            Math.min(120, update.maxDurationMinutes),
        );
        snapshot.safety = {
            ...snapshot.safety,
            channelLimit,
            maxDurationMinutes,
            allowAppIntensityControl: update.allowAppIntensityControl,
        };
        snapshot.channels.a.limit = channelLimit;
        snapshot.channels.b.limit = channelLimit;
        snapshot.channels.a.intensity = Math.min(
            snapshot.channels.a.intensity,
            channelLimit,
        );
        snapshot.channels.b.intensity = Math.min(
            snapshot.channels.b.intensity,
            channelLimit,
        );
        if (snapshot.device) {
            snapshot.device.intensityA = snapshot.channels.a.intensity;
            snapshot.device.intensityB = snapshot.channels.b.intensity;
        }
        snapshot.devices.forEach((device) => {
            device.intensityA = Math.min(device.intensityA, channelLimit);
            device.intensityB = Math.min(device.intensityB, channelLimit);
        });
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
