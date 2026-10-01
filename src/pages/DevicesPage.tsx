import {
    ArrowSquareOut,
    BatteryCharging,
    Bluetooth,
    Broadcast,
    Circle,
    DeviceMobile,
    DotsThree,
    HandTap,
    LinkSimple,
    Plugs,
    Plus,
    Pulse,
    SlidersHorizontal,
    SpeakerHigh,
    Square,
    Tabs,
} from "@phosphor-icons/react";
import { useEffect, useId, useState } from "react";

import { BleDeviceSettings } from "../components/BleDeviceSettings";
import { ConnectionManager } from "../components/ConnectionManager";
import { Dialog } from "../components/Dialog";
import type {
    BleParameters,
    ChannelStatus,
    DeviceSnapshot,
    HubSnapshot,
    TransportKind,
} from "../lib/contracts";
import { displayChannelStatus, transportName } from "../lib/transports";
import "./DevicesPage.css";

interface DevicesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    selectedDeviceId: string | null;
    onSelectDevice: (deviceId: string) => void;
    syncBaseDeviceId: string | null;
    onSelectSyncBaseDevice: (deviceId: string) => void;
    onStopOutput: (deviceId: string) => void;
    onOpenInNewTab: (deviceId: string) => void;
    onOpenInNewWindow: (deviceId: string) => void;
    onOpenPairing: (connectionId?: string) => void;
    onConnect: (transport: Exclude<TransportKind, "ble">) => void;
    onDisconnect: (connectionId: string) => void;
    onRefreshPairing: (connectionId: string) => void;
    onSetEndpoint: (transport: Exclude<TransportKind, "ble">, endpoint: string) => void;
    onScanBluetooth: () => void;
    onConnectBluetooth: (deviceId: string) => void;
    onDisconnectBluetooth: (deviceId: string) => void;
    onSaveBluetoothConfig: (deviceId: string, config: BleParameters) => void;
    onSetSyncAllDevices: (enabled: boolean, baseDeviceId: string) => void;
}

const statusCopy = (status: ChannelStatus): string => ({
    active: "输出中",
    ready: "回路正常",
    fault: "需要检查",
    disabled: "被控端已关闭",
    unknown: "回路状态未知",
    idle: "待机",
    disconnected: "未连接",
})[status];

const transportLabel = (transport: TransportKind): string => ({
    ws_v4: "V4",
    ws_v3: "V3",
    ble: "BLE",
})[transport];

const connectionLabels = {
    disconnected: "未连接",
    connecting: "正在连接",
    waiting: "等待配对",
    connected: "已连接",
    error: "连接异常",
};

const initializationCopy = (device: DeviceSnapshot): string => {
    if (device.initialization === "initializing") {
        return "正在初始化";
    }
    if (device.initialization === "fault") {
        return "初始化失败";
    }
    return device.transport === "ble" ? "初始化完成" : "已连接";
};

const shortIdentifier = (device: DeviceSnapshot): string => {
    if (device.transport !== "ble") {
        return device.slotId;
    }
    const id = String(device.id || device.controlId);
    return id.length > 12 ? `…${id.slice(-10)}` : id;
};

const SourceLabel = ({
    snapshot,
    sourceId,
    waveformName,
}: {
    snapshot: HubSnapshot;
    sourceId: string | null;
    waveformName: string | null;
}) => {
    const source = snapshot.sources.find((item) => item.id === sourceId);
    const SourceIcon = source?.kind === "builtin.audio"
        ? SpeakerHigh
        : source?.kind === "builtin.touch" ? HandTap
            : source?.kind === "builtin.fixed_waveform" ? Pulse : LinkSimple;
    const label = !sourceId ? "未绑定输入源"
        : !source ? "输入源不可用"
            : source.kind === "builtin.fixed_waveform"
                ? `波形 · ${waveformName ?? "未选择"}`
                : source.name;
    return (
        <span className="overview-source" title={label}>
            <SourceIcon aria-hidden="true" size={16} weight="light" />
            <span>{label}</span>
        </span>
    );
};

export const DevicesPage = ({
    snapshot,
    pendingAction,
    selectedDeviceId,
    onSelectDevice,
    syncBaseDeviceId,
    onSelectSyncBaseDevice,
    onStopOutput,
    onOpenInNewTab,
    onOpenInNewWindow,
    onOpenPairing,
    onConnect,
    onDisconnect,
    onRefreshPairing,
    onSetEndpoint,
    onScanBluetooth,
    onConnectBluetooth,
    onDisconnectBluetooth,
    onSaveBluetoothConfig,
    onSetSyncAllDevices,
}: DevicesPageProps) => {
    const pageId = useId();
    const [connectionMode, setConnectionMode] = useState<"add" | "manage" | null>(null);
    const [detailsDeviceId, setDetailsDeviceId] = useState<string | null>(null);
    const baseDeviceId = snapshot.devices.some((device) => device.controlId === syncBaseDeviceId)
        ? syncBaseDeviceId ?? "" : "";
    const detailsDevice = snapshot.devices.find((device) => device.controlId === detailsDeviceId);
    const disabled = pendingAction !== null;
    useEffect(() => {
        if (detailsDeviceId && !detailsDevice) {
            setDetailsDeviceId(null);
        }
    }, [detailsDeviceId, detailsDevice]);

    return (
        <div className="standard-page devices-overview-page">
            <header className="devices-overview-heading">
                <div><h1>设备</h1><p>每台设备独立控制</p></div>
                <button className="primary-compact-button" onClick={() => setConnectionMode("add")} type="button">
                    <Plus aria-hidden="true" size={20} />添加设备
                </button>
            </header>

            <section className="connection-overview" aria-label="连接概览">
                {(["ws_v4", "ws_v3"] as const).map((transport) => {
                    const connection = snapshot.connections.find((item) => item.transport === transport);
                    const count = snapshot.devices.filter((device) => device.transport === transport).length;
                    const state = connection?.state ?? "disconnected";
                    return (
                        <div className={`connection-overview-item connection-${state}`} key={transport}>
                            <Broadcast aria-hidden="true" size={24} weight="light" />
                            <div>
                                <strong>{transportLabel(transport)}</strong>
                                <span className="connection-overview-state">{connectionLabels[state]}</span>
                                <small>{count} 台设备</small>
                            </div>
                        </div>
                    );
                })}
                <div className={`connection-overview-item ${snapshot.devices.some((device) => device.transport === "ble") ? "connection-connected" : "connection-disconnected"}`}>
                    <Bluetooth aria-hidden="true" size={24} weight="light" />
                    <div>
                        <strong>BLE</strong>
                        <span className="connection-overview-state">{snapshot.devices.filter((device) => device.transport === "ble").length} 台</span>
                        <small>蓝牙直连</small>
                    </div>
                </div>
                <button className="secondary-button connection-overview-manage" onClick={() => setConnectionMode("manage")} type="button">
                    <LinkSimple aria-hidden="true" size={18} />管理连接
                </button>
            </section>

            <section className="overview-sync" aria-label="全设备强度同步">
                <div className="overview-sync-toggle">
                    <SlidersHorizontal aria-hidden="true" size={21} weight="light" />
                    <strong>全设备强度同步</strong>
                    <label className="toggle-switch">
                        <input
                            aria-label="同步所有设备"
                            checked={snapshot.syncAllDevices}
                            disabled={disabled || (!snapshot.syncAllDevices && (!baseDeviceId || snapshot.devices.filter((device) => device.initialization === "ready").length < 2))}
                            onChange={(event) => onSetSyncAllDevices(event.currentTarget.checked, baseDeviceId)}
                            type="checkbox"
                        />
                        <span className="toggle-track" aria-hidden="true"><span /></span>
                    </label>
                    <span className="overview-sync-state">{snapshot.syncAllDevices ? "开启" : "关闭"}</span>
                </div>
                <label className="overview-sync-base">
                    <span>对齐基准</span>
                    <select
                        aria-label="同步基准设备"
                        disabled={disabled || snapshot.devices.length === 0}
                        onChange={(event) => {
                            const deviceId = event.currentTarget.value;
                            onSelectSyncBaseDevice(deviceId);
                            if (snapshot.syncAllDevices && deviceId) onSetSyncAllDevices(true, deviceId);
                        }}
                        value={baseDeviceId}
                    >
                        <option disabled value="">选择基准设备</option>
                        {snapshot.devices.map((device) => (
                            <option disabled={device.initialization !== "ready"} key={device.controlId} value={device.controlId}>
                                {device.name} · {transportLabel(device.transport)} · {shortIdentifier(device)}
                            </option>
                        ))}
                    </select>
                </label>
                <p>开启或更换时按所选设备对齐 A/B 强度，遵守各通道上限。</p>
            </section>

            {snapshot.devices.length > 0 ? (
                <div className="overview-device-grid">
                    {snapshot.devices.map((device, index) => {
                        const headingId = `${pageId}-device-${index}`;
                        const statusA = displayChannelStatus(device.channelAStatus, device.capabilities.loadStatus);
                        const statusB = displayChannelStatus(device.channelBStatus, device.capabilities.loadStatus);
                        const loadCopy = !device.capabilities.loadStatus ? "回路状态未知"
                            : statusA === statusB ? statusCopy(statusA)
                                : `A ${statusCopy(statusA)} · B ${statusCopy(statusB)}`;
                        const selected = selectedDeviceId === device.controlId;
                        return (
                            <section
                                aria-labelledby={headingId}
                                className={`device-card overview-device-card${selected ? " is-selected" : ""}`}
                                key={device.controlId}
                                onClick={(event) => {
                                    if (!(event.target as HTMLElement).closest("button, input, select, a")) onSelectDevice(device.controlId);
                                }}
                            >
                                <div className="overview-device-heading">
                                    <div className="overview-device-icon"><DeviceMobile aria-hidden="true" size={26} weight="light" /></div>
                                    <div className="overview-device-identity">
                                        <div className="overview-device-title">
                                            <h2 aria-label={device.name} id={headingId}>
                                                <button
                                                    aria-label={`选择 ${device.name} · ${transportLabel(device.transport)} · ${shortIdentifier(device)}`}
                                                    aria-pressed={selected}
                                                    className="overview-device-select"
                                                    onClick={() => onSelectDevice(device.controlId)}
                                                    type="button"
                                                >{device.name}</button>
                                            </h2>
                                            <span className="overview-transport-chip">{transportLabel(device.transport)}</span>
                                        </div>
                                        <div className="overview-device-subtitle">
                                            <span className={`overview-initialization initialization-${device.initialization}`}>
                                                <Circle aria-hidden="true" size={6} weight="fill" />{initializationCopy(device)}
                                            </span>
                                            <span title={device.controlId}>{shortIdentifier(device)}</span>
                                        </div>
                                    </div>
                                    <div className="overview-device-metadata">
                                        <span className={device.power === null || !device.capabilities.battery ? "" : "battery-known"}>
                                            <BatteryCharging aria-hidden="true" size={16} />
                                            {device.power === null || !device.capabilities.battery ? "电量未知" : `${device.power}%`}
                                        </span>
                                        <span className="overview-load-state" title={`A：${statusCopy(statusA)}；B：${statusCopy(statusB)}`}>
                                            <Pulse aria-hidden="true" size={16} />{loadCopy}
                                        </span>
                                    </div>
                                    <button
                                        aria-label={`${device.name} 设备详情`}
                                        className="overview-details-button"
                                        onClick={() => setDetailsDeviceId(device.controlId)}
                                        title="设备详情"
                                        type="button"
                                    ><DotsThree aria-hidden="true" size={21} /></button>
                                </div>

                                <div className="overview-device-body">
                                    {(["a", "b"] as const).map((channel) => {
                                        const intensity = channel === "a" ? device.intensityA : device.intensityB;
                                        const limit = channel === "a" ? device.intensityLimitA : device.intensityLimitB;
                                        return (
                                            <div className="overview-channel" key={channel}>
                                                <div className="overview-channel-label"><strong>{channel.toUpperCase()}</strong><span>通道</span></div>
                                                <div className="overview-channel-value" aria-label={`${channel.toUpperCase()} 通道实际强度 ${intensity}，上限 ${limit}`}>
                                                    <Pulse aria-hidden="true" size={17} weight="light" />
                                                    <strong>{intensity}</strong><span>/ {limit}</span>
                                                </div>
                                                <SourceLabel
                                                    snapshot={snapshot}
                                                    sourceId={channel === "a" ? device.sourceIdA : device.sourceIdB}
                                                    waveformName={channel === "a" ? device.waveformNameA : device.waveformNameB}
                                                />
                                            </div>
                                        );
                                    })}
                                    <div className="overview-device-actions">
                                        <span className={`overview-output-state${device.outputActive ? " is-running" : ""}`}>
                                            {device.outputActive ? <Pulse aria-hidden="true" size={17} /> : <Square aria-hidden="true" size={12} weight="fill" />}
                                            {device.outputActive ? "输出中" : "输出已停止"}
                                        </span>
                                        {device.outputActive && (
                                            <button
                                                aria-label={`停止 ${device.name} 输出`}
                                                className="overview-stop-button"
                                                disabled={disabled}
                                                onClick={() => onStopOutput(device.controlId)}
                                                type="button"
                                            ><Square aria-hidden="true" size={12} weight="fill" />停止输出</button>
                                        )}
                                        <div className="overview-open-actions">
                                            <button
                                                aria-label="在新标签页中打开"
                                                className="secondary-button overview-open-button"
                                                disabled={disabled}
                                                onClick={() => onOpenInNewTab(device.controlId)}
                                                title={`打开 ${device.name} 控制`}
                                                type="button"
                                            ><Tabs aria-hidden="true" size={16} />打开控制</button>
                                            <button
                                                aria-label="在新窗口中打开"
                                                className="overview-window-button"
                                                disabled={disabled}
                                                onClick={() => onOpenInNewWindow(device.controlId)}
                                                title={`在新窗口中打开 ${device.name}`}
                                                type="button"
                                            ><ArrowSquareOut aria-hidden="true" size={16} /></button>
                                        </div>
                                    </div>
                                </div>
                            </section>
                        );
                    })}
                </div>
            ) : (
                <section className="empty-state overview-empty-state">
                    <DeviceMobile aria-hidden="true" size={38} weight="light" />
                    <h2>尚未发现设备</h2><p>配对 V4／V3 APP，或连接郊狼 3.0 蓝牙设备。</p>
                    <button className="primary-compact-button" onClick={() => setConnectionMode("add")} type="button">
                        <Plus aria-hidden="true" size={18} />添加设备
                    </button>
                </section>
            )}

            {connectionMode && (
                <ConnectionManager
                    mode={connectionMode}
                    onClose={() => setConnectionMode(null)}
                    onConnect={onConnect}
                    onConnectBluetooth={onConnectBluetooth}
                    onDisconnect={onDisconnect}
                    onOpenPairing={onOpenPairing}
                    onRefreshPairing={onRefreshPairing}
                    onScanBluetooth={onScanBluetooth}
                    onSetEndpoint={onSetEndpoint}
                    pendingAction={pendingAction}
                    snapshot={snapshot}
                />
            )}

            {detailsDevice && (
                <Dialog
                    className="device-details-dialog"
                    description={`${transportName(detailsDevice.transport)} · ${shortIdentifier(detailsDevice)}`}
                    onClose={() => setDetailsDeviceId(null)}
                    title={`${detailsDevice.name} · 设备详情`}
                >
                    <dl className="overview-device-details">
                        <div><dt>控制端 ID</dt><dd>{detailsDevice.controlId}</dd></div>
                        <div><dt>型号</dt><dd>{!detailsDevice.type || detailsDevice.type.toLowerCase() === "unknown" ? "未知" : detailsDevice.type}</dd></div>
                        <div><dt>初始化</dt><dd>{initializationCopy(detailsDevice)}{detailsDevice.initialization !== "ready" && "，输出不可用"}</dd></div>
                        <div><dt>A 通道回路</dt><dd>{statusCopy(displayChannelStatus(detailsDevice.channelAStatus, detailsDevice.capabilities.loadStatus))}</dd></div>
                        <div><dt>B 通道回路</dt><dd>{statusCopy(displayChannelStatus(detailsDevice.channelBStatus, detailsDevice.capabilities.loadStatus))}</dd></div>
                        <div><dt>操作反馈</dt><dd>{detailsDevice.capabilities.operationConfirmation ? "强度操作提供设备反馈" : "协议不提供完整操作确认"}</dd></div>
                    </dl>
                    {detailsDevice.transport === "ble" && (
                        <>
                            <BleDeviceSettings device={detailsDevice} disabled={disabled} onSave={onSaveBluetoothConfig} />
                            <button
                                className="secondary-button ble-disconnect"
                                disabled={disabled}
                                onClick={() => onDisconnectBluetooth(detailsDevice.controlId)}
                                type="button"
                            ><Plugs aria-hidden="true" size={17} />断开蓝牙设备</button>
                        </>
                    )}
                </Dialog>
            )}
        </div>
    );
};
