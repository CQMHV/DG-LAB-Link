import type { WaveformConfig } from "./generated/contracts";
export type { ConnectionState, OutputState, ChannelStatus, TransportKind, InitializationState, TransportConnectionSnapshot, DeviceCapabilities, BleParameters, BluetoothDevice, LogLevel, DeviceSnapshot, SourceSnapshot, WaveformConfig, CustomWaveformSnapshot, OutputSnapshot, SafetySnapshot, LogSnapshot, HubSnapshot, SourceBindingSnapshot, PluginManifest, InstalledPlugin } from "./generated/contracts";
export type { AppPreferencesSnapshot as AppPreferences } from "./generated/contracts";

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

export type HubChannel = import("./generated/contracts").Channel;

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

export interface SafetyUpdate {
    connectionTimeoutEnabled: boolean;
    connectionTimeoutMinutes: number;
    allowAppIntensityControl: boolean;
}

export type UiNodeKind = "page" | "section" | "stack" | "group" | "form" | "list" | "text" | "status" | "key_value" | "progress" | "divider" | "button" | "switch" | "text_field" | "integer_field" | "number_field" | "select" | "slider" | "xy_pad" | "grid" | "audio_player" | "meter" | "curve" | "waveform_picker" | "file_field";
export interface UiNode {
    id: string; type: UiNodeKind; label?: string; value?: unknown; configKey?: string; action?: string; input?: string; props?: Record<string, unknown>; children?: UiNode[];
}
export interface UiDocument {
    title: string; nodes: UiNode[]; actions?: { id: string; label: string; description: string; paramsSchema: unknown }[]; revision?: number;
}
export interface SourceActionParams { action: string; value?: unknown; bindingId?: string; }
export interface SourceInputParams extends SourceActionParams { owner: string; sequence: number; }
