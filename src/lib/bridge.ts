import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
    AppPreferences,
    AudioAction,
    AudioSnapshot,
    BleParameters,
    BluetoothDevice,
    TransportKind,
    TransportConnectionSnapshot,
    HubChannel,
    HubSnapshot,
    LogSnapshot,
    McpConfig,
    RuntimeInfo,
    SafetyUpdate,
    TouchInput,
    WaveformConfig,
    SourceActionParams,
    SourceInputParams,
    SourceSnapshot,
} from "./contracts";
import { asObject } from "./json";
import { createPluginDemoDocument } from "./pluginDemo";
import { OFFICIAL_WAVEFORMS } from "./waveforms";
import type { PluginCommand } from "./plugins";
import { defaultAudioConfig, defaultTouchConfig } from "./inputModes";
import { isTauriRuntime } from "./tauri";
import { defaultBleParameters, defaultV4Capabilities } from "./transports";

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

const defaultDemoAudio = (): AudioSnapshot => ({ mode: "file", state: "idle", fileName: null, positionMs: 0, durationMs: 0, levelLeft: 0, levelRight: 0, peakLeftHz: 0, peakRightHz: 0, lastError: null, hasRecording: false, loop: false, speakerEnabled: true });

const createDefaultMockSnapshot = (): HubSnapshot => {
    const primaryDevice = {
        controlId: "demo-app:slot-a1",
        connectionId: "ws-v4",
        transport: "ws_v4" as const,
        initialization: "ready" as const,
        capabilities: defaultV4Capabilities(),
        bleParameters: null,
        configurationStatus: null,
        bindingIdA: null, bindingIdB: null,
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
        connectionId: "ws-v4",
        transport: "ws_v4" as const,
        initialization: "ready" as const,
        capabilities: defaultV4Capabilities(),
        bleParameters: null,
        configurationStatus: null,
        bindingIdA: null, bindingIdB: null,
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
    connections: [
        {
            connectionId: "ws-v4", transport: "ws_v4", state: "connected",
            endpoint: "wss://trex.dungeon-lab.cn/v4", controllerId: "7f2ac91e",
            pairingUrl: "https://dungeon-lab.cn/s/?v=1&action=socket&url=wss%3A%2F%2Ftrex.dungeon-lab.cn%2Fv4%3Ftid%3D7f2ac91e",
            appCount: 1, lastError: null,
        },
        {
            connectionId: "ws-v3", transport: "ws_v3", state: "disconnected",
            endpoint: "wss://ws.dungeon-lab.cn/", controllerId: null,
            pairingUrl: null, appCount: 0, lastError: null,
        },
    ],
    bluetooth: [],
    sourceBindings: [],
    devices: [{ ...primaryDevice }, { ...secondaryDevice }],
    syncAllDevices: false,
    outputDeviceCount: 0,
    sources: [
        {
            id: "source-fixed-waveform",
            revision: 0,
            config: {}, state: {}, lastError: null, pluginId: null, runtimeStatus: "running",
            kind: "builtin.fixed_waveform",
            name: "固定波形",
            enabled: true,
            assignedChannelCount: 4,
            selectedPresetId: null,
            selectedPresetName: null,
        },
        { id: "source-touch", revision: 0, kind: "plugin", pluginId: "cn.dglab.link.touch", runtimeStatus: "stopped", lastError: null, config: defaultTouchConfig() as unknown as Record<string, unknown>, state: {}, name: "触控模式", enabled: true, assignedChannelCount: 0, selectedPresetId: null, selectedPresetName: null },
        { id: "source-audio", revision: 0, kind: "plugin", pluginId: "cn.dglab.link.audio", runtimeStatus: "stopped", lastError: null, config: { defaultChannelConfig: defaultAudioConfig() }, state: { audio: defaultDemoAudio() }, name: "音频模式", enabled: true, assignedChannelCount: 0, selectedPresetId: null, selectedPresetName: null },
    ],
    plugins: [{ manifest: { id: "cn.dglab.link.touch", version: "0.1.0", protocolVersion: 2, name: "触控输入", publisher: "DG-LAB Link", license: "AGPL-3.0-only", executable: "touch.exe" }, digest: "demo-touch", preinstalled: true }, { manifest: { id: "cn.dglab.link.audio", version: "0.1.0", protocolVersion: 2, name: "音频输入", publisher: "DG-LAB Link", license: "AGPL-3.0-only", executable: "audio.exe" }, digest: "demo-audio", preinstalled: true }],
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
let mockSafetyGeneration = 0;
let mockTouchInput: TouchInput | null = null;
const mockWaveformConfigs = new Map<string, WaveformConfig>();

const cloneSnapshot = (snapshot: HubSnapshot): HubSnapshot =>
    structuredClone(snapshot);

export const isBrowserDemo = (): boolean => !isTauriRuntime();

export const getRuntimeInfo = async (): Promise<RuntimeInfo | null> => {
    return isTauriRuntime() ? invoke<RuntimeInfo>("get_runtime_info") : null;
};

export const getMcpConfig = async (): Promise<McpConfig> => {
    if (!isTauriRuntime()) {
        throw new Error("浏览器演示模式不提供 MCP 连接令牌");
    }
    return invoke<McpConfig>("get_mcp_config");
};

export const listenRuntimeError = async (
    listener: (message: string) => void,
): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) {
        return () => {};
    }
    return listen<{ code: string; message: string }>("hub://runtime-error", (event) => {
        listener(event.payload.message);
    });
};

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
    const bindings = next.sourceBindings;
    next.sourceBindings = [];
    for (const device of next.devices) for (const channel of ["a", "b"] as const) {
        const sourceId = channel === "a" ? device.sourceIdA : device.sourceIdB;
        const source = next.sources.find((item) => item.id === sourceId);
        const previous = bindings.find((item) => item.controlId === device.controlId && item.channel === channel && item.sourceId === sourceId);
        const bindingId = source?.pluginId ? previous?.bindingId ?? globalThis.crypto.randomUUID() : null;
        if (channel === "a") device.bindingIdA = bindingId; else device.bindingIdB = bindingId;
        if (source?.pluginId && bindingId) next.sourceBindings.push(previous ? { ...previous, active: device.outputActive } : { sourceId: source.id, bindingId, controlId: device.controlId, channel, generation: 0, revision: 0, config: source.pluginId === "cn.dglab.link.audio" ? structuredClone((asObject(source.config).defaultChannelConfig ?? defaultAudioConfig()) as Record<string, unknown>) : {}, active: device.outputActive });
    }
    refreshMockSourceCounts(next);
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

const refreshMockOutputSummary = (snapshot: HubSnapshot): void => {
    snapshot.outputDeviceCount = snapshot.devices.filter((item) => item.outputActive).length;
    snapshot.output.state = snapshot.outputDeviceCount > 0 ? "running" : "idle";
    refreshMockSourceCounts(snapshot);
};

export const getConnections = async (): Promise<TransportConnectionSnapshot[]> =>
    isTauriRuntime() ? invoke("get_connections") : structuredClone(mockSnapshot.connections);

export const connectTransport = async (
    transport: Exclude<TransportKind, "ble">,
    endpoint: string | null = null,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("connect_transport", { transport, endpoint });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const connection = snapshot.connections.find((item) => item.transport === transport)!;
        if (endpoint) connection.endpoint = endpoint;
        connection.state = "waiting";
        connection.controllerId = transport === "ws_v4" ? "7f2ac91e" : "v3-demo-controller";
        connection.pairingUrl = transport === "ws_v4"
            ? createDefaultMockSnapshot().connections[0].pairingUrl
            : `https://www.dungeon-lab.com/app-download.php#DGLAB-SOCKET#${encodeURIComponent(`${connection.endpoint}/${connection.controllerId}`)}`;
        connection.lastError = null;
    });
};

export const disconnectConnection = async (connectionId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("disconnect_connection", { connectionId });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const connection = snapshot.connections.find((item) => item.connectionId === connectionId);
        if (!connection) throw new Error("连接不存在");
        Object.assign(connection, { state: "disconnected", controllerId: null, pairingUrl: null, appCount: 0 });
        snapshot.devices = snapshot.devices.filter((device) => device.connectionId !== connectionId);
        refreshMockOutputSummary(snapshot);
    });
};

export const refreshConnectionPairing = async (connectionId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("refresh_connection_pairing", { connectionId });
        return;
    }
    const connection = mockSnapshot.connections.find((item) => item.connectionId === connectionId);
    if (!connection || connection.transport === "ble") throw new Error("该连接不支持 APP 配对");
    await disconnectConnection(connectionId);
    await connectTransport(connection.transport);
};

export const setRelayEndpoint = async (transport: Exclude<TransportKind, "ble">, endpoint: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_relay_endpoint", { transport, endpoint });
        return;
    }
    const parsed = new URL(endpoint);
    if (!["ws:", "wss:"].includes(parsed.protocol)) throw new Error("端点必须使用 ws:// 或 wss://");
    updateMockSnapshot((snapshot) => {
        const connection = snapshot.connections.find((item) => item.transport === transport)!;
        if (!["disconnected", "error"].includes(connection.state)) throw new Error("请先断开连接再保存端点");
        connection.endpoint = endpoint;
    });
};

export const scanBluetooth = async (durationMs = 3000): Promise<BluetoothDevice[]> => {
    if (isTauriRuntime()) return invoke("scan_bluetooth", { durationMs });
    return updateMockSnapshot((snapshot) => {
        snapshot.bluetooth = [
            { deviceId: "ble-demo-030", name: "47L121000 演示设备", rssi: -52 },
            { deviceId: "ble-demo-031", name: "47L121001 演示设备", rssi: -64 },
        ];
    }).bluetooth;
};

export const connectBluetooth = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("connect_bluetooth", { deviceId });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const discovered = snapshot.bluetooth.find((item) => item.deviceId === deviceId);
        if (!discovered) throw new Error("请先扫描并选择郊狼 3.0");
        const connectionId = `ble:${deviceId}`;
        if (snapshot.connections.some((item) => item.connectionId === connectionId && item.state === "connected")) throw new Error("设备已连接");
        const connection: TransportConnectionSnapshot = {
            connectionId, transport: "ble", endpoint: deviceId, state: "connected",
            controllerId: null, pairingUrl: null, appCount: 0, lastError: null,
        };
        snapshot.connections = snapshot.connections.filter((item) => item.connectionId !== connectionId);
        snapshot.connections.push(connection);
        const device = {
            ...createDefaultMockSnapshot().devices[0], connectionId, transport: "ble" as const,
            controlId: `${connectionId}:device`, id: deviceId, name: "郊狼 3.0 蓝牙", slotId: "ble",
            power: null, intensityA: 0, intensityB: 0, outputActive: false,
            capabilities: { ...defaultV4Capabilities(), loadStatus: false, softLimits: true, balance: true, wheelProtection: true, standardMode: true },
            bleParameters: defaultBleParameters(), configurationStatus: "sent",
            channelAStatus: "unknown" as const, channelBStatus: "unknown" as const,
        };
        snapshot.devices.push(device);
        refreshMockOutputSummary(snapshot);
    });
};

export const disconnectBluetooth = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("disconnect_bluetooth", { deviceId });
        return;
    }
    const device = mockSnapshot.devices.find((item) => item.controlId === deviceId && item.transport === "ble");
    if (!device) throw new Error("蓝牙设备不存在");
    await disconnectConnection(device.connectionId);
};

export const getBluetoothConfig = async (deviceId: string): Promise<BleParameters> => {
    if (isTauriRuntime()) return invoke("get_bluetooth_config", { deviceId });
    const parameters = mockSnapshot.devices.find((item) => item.controlId === deviceId)?.bleParameters;
    if (!parameters) throw new Error("蓝牙设备不存在");
    return { ...parameters };
};

export const setBluetoothConfig = async (deviceId: string, config: BleParameters): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_bluetooth_config", { deviceId, config });
        return;
    }
    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find((item) => item.controlId === deviceId && item.transport === "ble");
        if (!device) throw new Error("蓝牙设备不存在");
        device.bleParameters = { ...config };
        device.intensityLimitA = config.maxStrengthA;
        device.intensityLimitB = config.maxStrengthB;
        device.configurationStatus = "sent";
        refreshMockOutputSummary(snapshot);
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


export const adjustIntensity = async (
    channel: HubChannel,
    delta: number,
    deviceId: string,
): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("adjust_intensity", {
            channel,
            delta,
            deviceId,
        });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const roundedDelta = Math.round(delta);
        const selected = snapshot.devices.find(
            (device) =>
                device.controlId === deviceId,
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
        const selectedIntensity = channel === "a" ? selected.intensityA : selected.intensityB;
        prependMockLog(
            snapshot,
            "info",
            snapshot.syncAllDevices
                ? `${targets.length} 台设备的 ${channel.toUpperCase()} 通道已同步到 ${targetIntensity}`
                : `${channel.toUpperCase()} 通道强度已调整为 ${selectedIntensity}`,
        );
    });
};

export const setSyncAllDevices = async (enabled: boolean, deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("set_sync_all_devices", { enabled, deviceId });
        return;
    }

    updateMockSnapshot((snapshot) => {
        const selected = snapshot.devices.find(
            (device) => device.controlId === deviceId,
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
        refreshMockOutputSummary(snapshot);
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

    const acceptedGeneration = mockSafetyGeneration;
    await mockStartOutputCompletion;
    if (acceptedGeneration !== mockSafetyGeneration) {
        throw new Error("输出请求已被停止取消");
    }
    updateMockSnapshot((snapshot) => {
        const device = snapshot.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            throw new Error("设备不存在或已断开");
        }
        const connection = snapshot.connections.find((item) => item.connectionId === device.connectionId);
        if (connection?.state !== "connected" || device.initialization !== "ready") {
            throw new Error("设备未连接或尚未初始化");
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

        prependMockLog(snapshot, "info", `${device.name} 的波形输出已开始`);
    });
};

export const stopOutput = async (deviceId: string): Promise<void> => {
    if (isTauriRuntime()) {
        await invoke("stop_output", { deviceId });
        return;
    }

    mockSafetyGeneration += 1;
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

        prependMockLog(snapshot, "info", `${device.name} 的波形输出已停止`);
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
            mockWaveformConfigs.set(config.presetId, structuredClone(config));
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

export const getCustomWaveform = async (presetId: string): Promise<WaveformConfig> => {
    if (isTauriRuntime()) {
        return invoke<WaveformConfig>("get_custom_waveform", { presetId });
    }
    const config = mockWaveformConfigs.get(presetId);
    if (config) return structuredClone(config);
    const waveform = mockSnapshot.customWaveforms.find((item) => item.id === presetId);
    if (!waveform) throw new Error("选择的自定义波形不存在");
    return { presetId, presetName: waveform.name, frames: ["0A0A0A0A64646464"] };
};


const recordDemoInput = async (input: TouchInput): Promise<void> => {
    const device = mockSnapshot.devices.find((candidate) => candidate.controlId === input.deviceId);
    if (input.pointers.length > 0 && (!device?.outputActive ||
        (device.sourceIdA !== "source-touch" && device.sourceIdB !== "source-touch"))) {
        throw new Error("请先开始此设备的触控输出");
    }
    mockTouchInput = structuredClone(input);
};


const updateDemoAudio = async (sourceId: string, action: AudioAction): Promise<void> => {
    updateMockSnapshot((snapshot) => {
        const audio = asObject(snapshot.sources.find((item) => item.id === sourceId)!.state).audio as AudioSnapshot;
        audio.lastError = null;
        switch (action.type) {
            case "loadFile":
                audio.mode = "file"; audio.state = "paused";
                audio.fileName = action.path.split(/[\\/]/).at(-1)!;
                audio.positionMs = 0; audio.durationMs = 60000;
                break;
            case "play": audio.state = "playing"; break;
            case "pause": audio.state = "paused"; break;
            case "stop": audio.state = "idle"; audio.positionMs = 0; break;
            case "seek": audio.positionMs = action.positionMs; break;
            case "startMicrophone":
            case "startDesktop":
                audio.mode = action.type === "startDesktop" ? "desktop" : "microphone";
                audio.state = "capturing"; audio.fileName = null;
                audio.positionMs = 0; audio.durationMs = 0;
                break;
            case "startRecording": audio.mode = "recording"; audio.state = "recording"; audio.positionMs = 0; break;
            case "stopRecording":
                audio.mode = "recording"; audio.state = "idle"; audio.hasRecording = true;
                audio.fileName = "录音回放"; audio.durationMs = 5000;
                break;
            case "setPlaybackOptions": audio.loop = action.loop; audio.speakerEnabled = action.speakerEnabled; break;
            case "saveRecording": prependMockLog(snapshot, "info", "演示录音保存操作已完成"); break;
        }
        if (!["capturing", "playing"].includes(audio.state)) {
            audio.levelLeft = 0; audio.levelRight = 0;
            audio.peakLeftHz = 0; audio.peakRightHz = 0;
        }
    });
};

export const __getMockTouchInput = (): TouchInput | null => mockTouchInput ? structuredClone(mockTouchInput) : null;

export const __resetMockBridge = (): void => {
    mockSnapshot = createDefaultMockSnapshot();
    mockAppPreferences = {
        closeToTray: true,
        autoStart: false,
        startMinimized: true,
    };
    mockStartOutputCompletion = null;
    mockSafetyGeneration += 1;
    mockTouchInput = null;
    mockWaveformConfigs.clear();
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

export const pluginDemoCall = async (command: PluginCommand): Promise<unknown> => {
    const params: Record<string, unknown> = "params" in command ? command.params : {};
    const sourceId = String(params.sourceId ?? "");
    const source = mockSnapshot.sources.find((item) => item.id === sourceId);
    const requireSource = (): SourceSnapshot => {
        if (!source?.pluginId) throw new Error("插件输入源不存在");
        return source;
    };
    switch (command.command) {
        case "list_plugins": return structuredClone(mockSnapshot.plugins ?? []);
        case "list_waveforms": return { official: OFFICIAL_WAVEFORMS, custom: [...mockWaveformConfigs.values()] };
        case "get_source_ui": {
            const item = requireSource();
            if (!item.enabled) throw new Error("输入源实例已停用");
            if (item.runtimeStatus === "faulted") throw new Error("插件进程运行异常，请显式重试");
            if (item.runtimeStatus === "stopped") updateMockSnapshot((snapshot) => { snapshot.sources.find((candidate) => candidate.id === sourceId)!.runtimeStatus = "running"; });
            return createPluginDemoDocument(mockSnapshot, mockSnapshot.sources.find((candidate) => candidate.id === sourceId)!, (params.params ?? {}) as { bindingId?: string; surface?: string });
        }
        case "install_plugin": case "update_plugin": return updateMockSnapshot((snapshot) => {
            const id = "example.sample";
            const existing = snapshot.plugins?.find((item) => item.manifest.id === id);
            if (existing) { if (command.command === "install_plugin") throw new Error("插件已安装，请使用更新"); existing.manifest.version = "0.2.0"; }
            else { if (command.command === "update_plugin") throw new Error("插件尚未安装"); snapshot.plugins ??= []; snapshot.plugins.push({ manifest: { id, name: "示例插件", version: "0.1.0", protocolVersion: 2, publisher: "示例开发者", license: "MIT", executable: "sample.exe" }, digest: "demo-sample", preinstalled: false }); }
        }).plugins;
        case "uninstall_plugin": return updateMockSnapshot((snapshot) => {
            const ids = snapshot.sources.filter((item) => item.pluginId === params.pluginId).map((item) => item.id);
            snapshot.plugins = snapshot.plugins?.filter((item) => item.manifest.id !== params.pluginId);
            snapshot.sources = snapshot.sources.filter((item) => !ids.includes(item.id));
            for (const device of snapshot.devices) { if (ids.includes(device.sourceIdA ?? "")) device.sourceIdA = null; if (ids.includes(device.sourceIdB ?? "")) device.sourceIdB = null; if (!device.sourceIdA || !device.sourceIdB) device.outputActive = false; }
            refreshMockOutputSummary(snapshot);
        });
        case "create_source": {
            const plugin = mockSnapshot.plugins?.find((item) => item.manifest.id === params.pluginId);
            if (!plugin) throw new Error("插件尚未安装");
            const id = `source-${globalThis.crypto?.randomUUID?.() ?? Date.now()}`;
            updateMockSnapshot((snapshot) => { snapshot.sources.push({ id, revision: 0, kind: "plugin", pluginId: plugin.manifest.id, name: String(params.name), enabled: true, assignedChannelCount: 0, selectedPresetId: null, selectedPresetName: null, runtimeStatus: "stopped", lastError: null, config: plugin.manifest.id === "cn.dglab.link.touch" ? defaultTouchConfig() : plugin.manifest.id === "cn.dglab.link.audio" ? { defaultChannelConfig: defaultAudioConfig() } : {}, state: plugin.manifest.id === "cn.dglab.link.audio" ? { audio: defaultDemoAudio() } : {} }); });
            return id;
        }
        case "delete_source": requireSource(); return updateMockSnapshot((snapshot) => { snapshot.sources = snapshot.sources.filter((item) => item.id !== sourceId); for (const device of snapshot.devices) { if (device.sourceIdA === sourceId) device.sourceIdA = null; if (device.sourceIdB === sourceId) device.sourceIdB = null; if (!device.sourceIdA || !device.sourceIdB) device.outputActive = false; } refreshMockOutputSummary(snapshot); });
        case "set_source_enabled": case "start_source": case "stop_source": requireSource(); return updateMockSnapshot((snapshot) => {
            const item = snapshot.sources.find((item) => item.id === sourceId)!;
            if (command.command === "set_source_enabled") { item.enabled = Boolean(params.enabled); item.runtimeStatus = item.enabled ? "stopped" : "disabled"; }
            else item.runtimeStatus = command.command === "start_source" ? "running" : "stopped";
        });
        case "set_source_config": {
            const item = requireSource();
            return updateMockSnapshot((snapshot) => {
                const current = snapshot.sources.find((candidate) => candidate.id === sourceId)!;
                const target = params.bindingId ? snapshot.sourceBindings.find((binding) => binding.bindingId === params.bindingId && binding.sourceId === sourceId) : current;
                if (!target) throw new Error("输入源通道绑定已失效");
                if (params.expectedRevision !== target.revision) throw { code: "config_conflict", message: "配置已由其他入口修改，请重新读取后提交" };
                target.config = structuredClone(params.config as Record<string, unknown>);
                target.revision += 1;
                if (item.pluginId === "cn.dglab.link.touch") mockTouchInput = null;
            });
        }
        case "source_action": {
            const item = requireSource(); const action = params.params as SourceActionParams;
            if (action.action === "audio_control") return updateDemoAudio(sourceId, action.value as AudioAction);
            if (["configure", "configure_binding"].includes(action.action)) throw { code: "invalid_command", message: "配置请使用 set_source_config 并提供 expectedRevision" };
            return {};
        }
        case "source_input": {
            requireSource(); const input = params.params as SourceInputParams;
            const binding = mockSnapshot.devices.flatMap((device) => (["a", "b"] as const).map((channel) => ({ device, channel, id: channel === "a" ? device.bindingIdA : device.bindingIdB }))).find((item) => item.id === input.bindingId);
            if (!binding) throw new Error("输入源通道绑定已失效");
            const pointers = (input.value as { pointers: TouchInput["pointers"] }).pointers.map((pointer) => ({ ...pointer, channel: binding.channel }));
            const existing = mockTouchInput?.deviceId === binding.device.controlId ? mockTouchInput.pointers.filter((pointer) => pointer.channel !== binding.channel) : [];
            return recordDemoInput({ deviceId: binding.device.controlId, ownerId: input.owner, sequence: input.sequence, pointers: [...existing, ...pointers] });
        }
        default: throw new Error(`演示模式不支持命令：${command.command}`);
    }
};
