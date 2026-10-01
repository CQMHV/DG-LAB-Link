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
    | "fault"
    | "unknown";

export type TransportKind = "ws_v4" | "ws_v3" | "ble";
export type InitializationState = "initializing" | "ready" | "fault";

export interface TransportConnectionSnapshot extends ConnectionSnapshot {
    connectionId: string;
    transport: TransportKind;
}

export interface DeviceCapabilities {
    battery: boolean;
    loadStatus: boolean;
    softLimits: boolean;
    balance: boolean;
    wheelProtection: boolean;
    standardMode: boolean;
    operationConfirmation: boolean;
}

export interface BleParameters {
    maxStrengthA: number;
    maxStrengthB: number;
    frequencyBalanceA: number;
    frequencyBalanceB: number;
    strengthBalanceA: number;
    strengthBalanceB: number;
    wheelProtectionEnabled: boolean;
    wheelProtectionValue: number;
}

export interface BluetoothDevice {
    deviceId: string;
    name: string;
    rssi: number | null;
}

export type LogLevel = "info" | "warning" | "error";

export interface RuntimeInfo {
    instanceId: string;
    pid: number;
    holderCount: number;
    mcpUrl: string;
}

export interface McpConfig {
    url: string;
    token: string;
}

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
    connectionId: string;
    transport: TransportKind;
    initialization: InitializationState;
    capabilities: DeviceCapabilities;
    bleParameters: BleParameters | null;
    configurationStatus: string | null;
    id: string | number;
    name: string;
    type: string;
    slotId: string;
    power: number | null;
    intensityA: number;
    intensityB: number;
    intensityLimitA: number;
    intensityLimitB: number;
    sourceIdA: string | null;
    sourceIdB: string | null;
    waveformIdA: string | null;
    waveformIdB: string | null;
    waveformNameA: string | null;
    waveformNameB: string | null;
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
    selectedPresetId: string | null;
    selectedPresetName: string | null;
}

export interface WaveformConfig {
    presetId: string;
    presetName: string;
    frames: string[];
}

export interface CustomWaveformSnapshot {
    id: string;
    name: string;
    frameCount: number;
    durationMs: number;
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
    connectionTimeoutEnabled: boolean;
    connectionTimeoutMinutes: number;
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
    connections: TransportConnectionSnapshot[];
    bluetooth: BluetoothDevice[];
    device: DeviceSnapshot | null;
    devices: DeviceSnapshot[];
    selectedDeviceId: string | null;
    syncAllDevices: boolean;
    outputDeviceCount: number;
    sources: SourceSnapshot[];
    customWaveforms: CustomWaveformSnapshot[];
    defaultSourceId: string | null;
    inputModes: InputModesSnapshot;
    output: OutputSnapshot;
    channels: {
        a: ChannelSnapshot;
        b: ChannelSnapshot;
    };
    safety: SafetySnapshot;
    logs: LogSnapshot[];
}

export type HubChannel = "a" | "b";

export interface MappingPoint {
    x: number;
    y: number;
}

export interface TouchConfig {
    mode: "free" | "rhythm";
    routing: "a" | "b" | "sync" | "separate" | "alternate";
    gridSize: number;
    swapAxes: boolean;
    intensityMode: "classic" | "gradient";
    gradientDirection: "left" | "right" | "both";
    intensityCurve: MappingPoint[];
    periodCurve: MappingPoint[];
    freeWaveforms: WaveformConfig[];
    rhythmWaveforms: WaveformConfig[];
    background: WaveformConfig | null;
}

export interface TouchInput {
    deviceId: string;
    ownerId: string;
    sequence: number;
    pointers: { id: number; x: number; y: number; cell: number | null; channel?: HubChannel }[];
}

export interface AudioChannelConfig {
    enabled: boolean;
    inputChannel: "left" | "right" | "mix";
    gain: number;
    volumeLower: number;
    volumeUpper: number;
    adaptive: boolean;
    adaptiveLower: number;
    adaptiveUpper: number;
    hysteresisMs: number;
    frequencyMin: number;
    frequencyMax: number;
    periodCurve: MappingPoint[];
}

export interface AudioSnapshot {
    mode: "file" | "microphone" | "recording" | "desktop";
    state: "idle" | "loading" | "playing" | "paused" | "capturing" | "recording" | "error";
    fileName: string | null;
    positionMs: number;
    durationMs: number;
    levelLeft: number;
    levelRight: number;
    peakLeftHz: number;
    peakRightHz: number;
    lastError: string | null;
    hasRecording: boolean;
    loop: boolean;
    speakerEnabled: boolean;
}

export type AudioAction =
    | { type: "loadFile"; path: string }
    | { type: "play" | "pause" | "stop" | "startMicrophone" | "startDesktop" | "startRecording" | "stopRecording" }
    | { type: "seek"; positionMs: number }
    | { type: "setPlaybackOptions"; loop: boolean; speakerEnabled: boolean }
    | { type: "saveRecording"; path: string };

export interface InputModesSnapshot {
    touchConfig: TouchConfig;
    audio: AudioSnapshot;
    audioBindings: { deviceId: string; channel: HubChannel; config: AudioChannelConfig }[];
}

export interface SafetyUpdate {
    connectionTimeoutEnabled: boolean;
    connectionTimeoutMinutes: number;
    allowAppIntensityControl: boolean;
}

export interface AppPreferences {
    closeToTray: boolean;
    autoStart: boolean;
    startMinimized: boolean;
}
