import type { BleParameters, ChannelStatus, DeviceCapabilities, TransportKind } from "./contracts";

export const displayChannelStatus = (status: ChannelStatus, loadStatus: boolean): ChannelStatus =>
    !loadStatus && (status === "ready" || status === "idle") ? "unknown" : status;

export const transportName = (transport: TransportKind): string => ({
    ws_v4: "Socket V4",
    ws_v3: "Socket V3",
    ble: "蓝牙直连",
})[transport];

export const defaultBleParameters = (): BleParameters => ({
    maxStrengthA: 100,
    maxStrengthB: 100,
    frequencyBalanceA: 160,
    frequencyBalanceB: 160,
    strengthBalanceA: 0,
    strengthBalanceB: 0,
    wheelProtectionEnabled: true,
    wheelProtectionValue: 10,
});

export const defaultV4Capabilities = (): DeviceCapabilities => ({
    battery: true,
    loadStatus: true,
    softLimits: false,
    balance: false,
    wheelProtection: false,
    standardMode: false,
    operationConfirmation: true,
});
