import {
    ArrowSquareOut,
    BatteryCharging,
    Bluetooth,
    Broadcast,
    DeviceMobile,
    LinkSimple,
    MagnifyingGlass,
    Plugs,
    PlugsConnected,
    Pulse,
    Tabs,
} from "@phosphor-icons/react";

import { PageHeader } from "../components/PageHeader";
import { ConnectionCard } from "../components/ConnectionCard";
import { BleDeviceSettings } from "../components/BleDeviceSettings";
import type { BleParameters, HubSnapshot, TransportKind } from "../lib/contracts";
import { displayChannelStatus, transportName } from "../lib/transports";

interface DevicesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
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
    onSetSyncAllDevices: (enabled: boolean) => void;
}

const statusCopy = (status: string): string => {
    if (status === "active") {
        return "输出中";
    }
    if (status === "ready") {
        return "回路正常";
    }
    if (status === "fault") {
        return "需要检查";
    }
    if (status === "disabled") {
        return "被控端已关闭";
    }
    if (status === "unknown") {
        return "回路状态未知";
    }
    return "未连接";
};

export const DevicesPage = ({
    snapshot,
    pendingAction,
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
}: DevicesPageProps) => (
    <div className="standard-page">
        <PageHeader
            actions={
                snapshot.connection.pairingUrl ? (
                    <button
                        className="secondary-button"
                        onClick={() => onOpenPairing("ws-v4")}
                        type="button"
                    >
                        <LinkSimple aria-hidden="true" size={18} />
                        配对新 APP
                    </button>
                ) : undefined
            }
            description="管理 Socket V4、Socket V3 和郊狼 3.0 蓝牙连接，共享设备与双通道实时状态。"
            eyebrow="APP & DEVICE"
            title="设备"
        />

        <div className="transport-connections">
            {snapshot.connections.map((connection) => (
                <ConnectionCard
                    connection={connection}
                    disabled={pendingAction !== null}
                    key={connection.connectionId}
                    onConnect={onConnect}
                    onDisconnect={onDisconnect}
                    onOpenPairing={onOpenPairing}
                    onRefreshPairing={onRefreshPairing}
                    onSetEndpoint={onSetEndpoint}
                />
            ))}
        </div>

        <section className="bluetooth-discovery-card" aria-label="蓝牙设备发现">
            <div className="bluetooth-discovery-heading">
                <div>
                    <span className="eyebrow">BLUETOOTH LE</span>
                    <h2><Bluetooth aria-hidden="true" size={24} />郊狼 3.0 蓝牙直连</h2>
                    <p>开启设备并断开手机 APP 的蓝牙连接后扫描。扫描不会自动连接或开启输出。</p>
                </div>
                <button
                    className="secondary-button"
                    disabled={pendingAction !== null}
                    onClick={onScanBluetooth}
                    type="button"
                >
                    <MagnifyingGlass aria-hidden="true" size={18} />
                    {pendingAction === "bluetooth-scan" ? "正在扫描…" : "扫描蓝牙设备"}
                </button>
            </div>
            {snapshot.bluetooth.length > 0 ? (
                <div className="bluetooth-discovery-list">
                    {snapshot.bluetooth.map((device) => (
                        <div className="bluetooth-discovery-row" key={device.deviceId}>
                            <div>
                                <strong>{device.name}</strong>
                                <code>{device.deviceId}</code>
                                <span>信号 {device.rssi === null ? "未知" : `${device.rssi} dBm`}</span>
                            </div>
                            <button
                                aria-label={`连接蓝牙 ${device.name}`}
                                className="primary-compact-button"
                                disabled={pendingAction !== null || snapshot.connections.some((connection) =>
                                    connection.transport === "ble" && connection.endpoint === device.deviceId &&
                                    !["disconnected", "error"].includes(connection.state))}
                                onClick={() => onConnectBluetooth(device.deviceId)}
                                type="button"
                            >
                                连接
                            </button>
                        </div>
                    ))}
                </div>
            ) : (
                <p className="bluetooth-empty">扫描结果会出现在这里，仅支持郊狼 3.0。</p>
            )}
        </section>

        <section className="device-sync-card">
            <div className="device-sync-icon">
                <Broadcast aria-hidden="true" size={26} weight="light" />
            </div>
            <div>
                <strong>同步所有设备</strong>
                <p>
                    开启时立即以当前仪表盘标签中的设备为基准对齐 A/B 强度；之后所有设备保持相同目标值，
                    并分别遵守自己的通道上限。
                </p>
            </div>
            <label className="toggle-switch">
                <span className="visually-hidden">同步所有设备</span>
                <input
                    aria-label="同步所有设备"
                    checked={snapshot.syncAllDevices}
                    disabled={
                        pendingAction !== null ||
                        (!snapshot.syncAllDevices && snapshot.devices.length < 2)
                    }
                    onChange={(event) =>
                        onSetSyncAllDevices(event.currentTarget.checked)
                    }
                    type="checkbox"
                />
                <span className="toggle-track" aria-hidden="true">
                    <span />
                </span>
            </label>
        </section>

        {snapshot.devices.length > 0 ? (
            <div className="device-list">
                {snapshot.devices.map((device) => {
                    const channels = {
                        a: {
                            intensity: device.intensityA,
                            limit: device.intensityLimitA,
                            status: displayChannelStatus(device.channelAStatus, device.capabilities.loadStatus),
                        },
                        b: {
                            intensity: device.intensityB,
                            limit: device.intensityLimitB,
                            status: displayChannelStatus(device.channelBStatus, device.capabilities.loadStatus),
                        },
                    };
                    return (
                        <section className="device-card" key={device.controlId}>
                            <div className="device-card-heading">
                                <div>
                                    <span className="eyebrow">
                                        {device.outputActive
                                            ? "OUTPUT ACTIVE"
                                            : "CONNECTED DEVICE"}
                                    </span>
                                    <h2>{device.name}</h2>
                                    <p>
                                        {transportName(device.transport)} · {device.type.toLowerCase() === "unknown" || !device.type ? "型号未知" : device.type} · 插槽 {device.slotId}
                                    </p>
                                    <p className={`device-initialization state-${device.initialization}`}>{device.initialization === "ready" ? "设备已就绪" : device.initialization === "initializing" ? "正在初始化，输出不可用" : "初始化失败，输出不可用"}</p>
                                </div>
                                <div className="battery-badge">
                                    <BatteryCharging aria-hidden="true" size={21} />
                                    {device.power === null ? "电量未知" : `${device.power}%`}
                                </div>
                            </div>
                            <div className="device-channel-grid">
                                {(["a", "b"] as const).map((channel) => {
                                    const upper = channel.toUpperCase();
                                    const data = channels[channel];
                                    return (
                                        <article key={channel}>
                                            <div className="device-channel-title">
                                                <strong>{upper}</strong>
                                                <span>{upper} 通道</span>
                                                <small
                                                    className={`state-${data.status}`}
                                                >
                                                    <PlugsConnected
                                                        aria-hidden="true"
                                                        size={16}
                                                    />
                                                    {statusCopy(data.status)}
                                                </small>
                                            </div>
                                            <div className="device-channel-value">
                                                <Pulse
                                                    aria-hidden="true"
                                                    size={22}
                                                    weight="light"
                                                />
                                                <strong>{data.intensity}</strong>
                                                <span>/ {data.limit}</span>
                                            </div>
                                            <div
                                                className="mini-progress"
                                                aria-hidden="true"
                                            >
                                                <span
                                                    style={{
                                                        width: `${Math.min(
                                                            100,
                                                            (data.intensity /
                                                                Math.max(
                                                                    1,
                                                                    data.limit,
                                                                )) *
                                                                100,
                                                        )}%`,
                                                    }}
                                                />
                                            </div>
                                        </article>
                                    );
                                })}
                            </div>
                            <p className="device-capability-note">{device.capabilities.loadStatus ? "支持回路状态" : "回路状态未知"} · {device.capabilities.operationConfirmation ? "强度操作提供设备反馈" : "协议不提供完整操作确认"}</p>
                            <div className="device-card-actions">
                                <button
                                    className="secondary-button device-card-action device-card-action-primary"
                                    disabled={pendingAction !== null}
                                    onClick={() =>
                                        onOpenInNewTab(device.controlId)
                                    }
                                    type="button"
                                >
                                    <Tabs aria-hidden="true" size={18} />
                                    在新标签页中打开
                                </button>
                                <button
                                    className="secondary-button device-card-action"
                                    disabled={pendingAction !== null}
                                    onClick={() =>
                                        onOpenInNewWindow(device.controlId)
                                    }
                                    type="button"
                                >
                                    <ArrowSquareOut
                                        aria-hidden="true"
                                        size={18}
                                    />
                                    在新窗口中打开
                                </button>
                            </div>
                            {device.transport === "ble" && (
                                <>
                                    <BleDeviceSettings
                                        device={device}
                                        disabled={pendingAction !== null}
                                        onSave={onSaveBluetoothConfig}
                                    />
                                    <button
                                        className="secondary-button ble-disconnect"
                                        disabled={pendingAction !== null}
                                        onClick={() => onDisconnectBluetooth(device.controlId)}
                                        type="button"
                                    >
                                        <Plugs aria-hidden="true" size={17} />
                                        断开蓝牙设备
                                    </button>
                                </>
                            )}
                        </section>
                    );
                })}
            </div>
        ) : (
            <section className="empty-state">
                <DeviceMobile aria-hidden="true" size={38} weight="light" />
                <h2>尚未发现设备</h2>
                <p>通过 Socket V4／V3 配对 APP，或扫描并连接郊狼 3.0 蓝牙设备。</p>
            </section>
        )}
    </div>
);
