export type ConnectionState =
    | "disconnected"
    | "connecting"
    | "waiting"
    | "connected"
    | "error";

export type OutputState = "idle" | "running" | "stopped" | "error";

export type ChannelStatus =
    | "idle"
    | "ready"
    | "active"
    | "disabled"
    | "disconnected"
    | "fault";

export type LogLevel = "info" | "warning" | "error";

export interface ConnectionSnapshot {
    state: ConnectionState;
    endpoint: string;
    controllerId: string | null;
    pairingUrl: string | null;
    appCount: number;
    lastError: string | null;
}

export interface DeviceSnapshot {
    controlId: string;
    id: string | number;
    name: string;
    type: string;
    slotId: string;
    power: number;
    intensityA: number;
    intensityB: number;
    intensityLimitA: number;
    intensityLimitB: number;
    sourceIdA: string | null;
    sourceIdB: string | null;
    sourceSync: boolean;
    outputActive: boolean;
    channelAStatus: ChannelStatus;
    channelBStatus: ChannelStatus;
}

export interface SourceSnapshot {
    id: string;
    kind: string;
    name: string;
    enabled: boolean;
    assignedChannelCount: number;
}

export interface OutputSnapshot {
    state: OutputState;
    framesSent: number;
    lastError: string | null;
}

export interface ChannelSnapshot {
    intensity: number;
    limit: number;
    status: ChannelStatus;
}

export interface SafetySnapshot {
    channelLimit: number;
    maxDurationMinutes: number;
    allowAppIntensityControl: boolean;
}

export interface LogSnapshot {
    id: string;
    level: LogLevel;
    message: string;
    timestamp: string;
}

export interface HubSnapshot {
    revision: number;
    connection: ConnectionSnapshot;
    device: DeviceSnapshot | null;
    devices: DeviceSnapshot[];
    selectedDeviceId: string | null;
    syncAllDevices: boolean;
    outputDeviceCount: number;
    sources: SourceSnapshot[];
    defaultSourceId: string | null;
    output: OutputSnapshot;
    channels: {
        a: ChannelSnapshot;
        b: ChannelSnapshot;
    };
    safety: SafetySnapshot;
    logs: LogSnapshot[];
}

export type HubChannel = "a" | "b";

export interface SafetyUpdate {
    channelLimit: number;
    maxDurationMinutes: number;
    allowAppIntensityControl: boolean;
}
