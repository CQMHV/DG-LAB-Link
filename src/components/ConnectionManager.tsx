import { Bluetooth, Broadcast, Check, MagnifyingGlass, SpinnerGap } from "@phosphor-icons/react";
import { useId, useRef, useState } from "react";

import type { BluetoothDevice, HubSnapshot, TransportKind } from "../lib/contracts";
import { ConnectionCard } from "./ConnectionCard";
import { Dialog } from "./Dialog";

import "./ConnectionManager.css";

export interface ConnectionManagerProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    mode: "add" | "manage";
    onClose: () => void;
    onOpenPairing: (connectionId?: string) => void;
    onConnect: (transport: Exclude<TransportKind, "ble">) => void;
    onDisconnect: (connectionId: string) => void;
    onRefreshPairing: (connectionId: string) => void;
    onSetEndpoint: (transport: Exclude<TransportKind, "ble">, endpoint: string) => void;
    onScanBluetooth: () => void;
    onConnectBluetooth: (deviceId: string) => void;
}

const transports: TransportKind[] = ["ws_v4", "ws_v3", "ble"];
const tabLabels: Record<TransportKind, string> = {
    ws_v4: "V4 APP",
    ws_v3: "V3 APP",
    ble: "蓝牙直连",
};

export const ConnectionManager = ({
    snapshot,
    pendingAction,
    mode,
    onClose,
    onOpenPairing,
    onConnect,
    onDisconnect,
    onRefreshPairing,
    onSetEndpoint,
    onScanBluetooth,
    onConnectBluetooth,
}: ConnectionManagerProps) => {
    const id = useId();
    const [transport, setTransport] = useState<TransportKind>(() =>
        mode === "manage"
            ? snapshot.connections.find((connection) =>
                !["disconnected", "error"].includes(connection.state))?.transport ?? "ws_v4"
            : "ws_v4");
    const tabRefs = useRef<Partial<Record<TransportKind, HTMLButtonElement | null>>>({});
    const disabled = pendingAction !== null;
    const scanning = pendingAction === "bluetooth-scan";
    const connection = snapshot.connections.find((candidate) => candidate.transport === transport);
    const bluetoothConnections = snapshot.connections.filter((candidate) => candidate.transport === "ble");
    const bluetoothDevices: BluetoothDevice[] = [...snapshot.bluetooth];

    for (const connected of bluetoothConnections) {
        if (["disconnected", "error"].includes(connected.state) ||
            bluetoothDevices.some((device) => device.deviceId === connected.endpoint)) {
            continue;
        }
        bluetoothDevices.push({
            deviceId: connected.endpoint,
            name: snapshot.devices.find((device) => device.connectionId === connected.connectionId)?.name ?? "郊狼 3.0",
            rssi: null,
        });
    }

    return (
        <Dialog
            className="connection-manager"
            description={mode === "add"
                ? "选择连接方式。连接成功后，设备会出现在设备页。"
                : "管理 APP 配对与蓝牙连接，设备控制在控制台中进行。"}
            onClose={onClose}
            title={mode === "add" ? "添加设备" : "管理连接"}
        >
            <div aria-label="连接方式" className="connection-manager-tabs" role="tablist">
                {transports.map((kind) => {
                    const Icon = kind === "ble" ? Bluetooth : Broadcast;
                    return (
                        <button
                            aria-controls={`${id}-${kind}-panel`}
                            aria-selected={transport === kind}
                            className={transport === kind ? "is-active" : undefined}
                            id={`${id}-${kind}-tab`}
                            key={kind}
                            onClick={() => setTransport(kind)}
                            onKeyDown={(event) => {
                                const index = transports.indexOf(kind);
                                const next = event.key === "ArrowRight" ? transports[(index + 1) % transports.length]
                                    : event.key === "ArrowLeft" ? transports[(index + transports.length - 1) % transports.length]
                                        : event.key === "Home" ? transports[0]
                                            : event.key === "End" ? transports[transports.length - 1]
                                                : null;
                                if (next) {
                                    event.preventDefault();
                                    setTransport(next);
                                    tabRefs.current[next]?.focus();
                                }
                            }}
                            ref={(element) => { tabRefs.current[kind] = element; }}
                            role="tab"
                            tabIndex={transport === kind ? 0 : -1}
                            type="button"
                        >
                            <Icon aria-hidden="true" size={19} weight="light" />
                            {tabLabels[kind]}
                        </button>
                    );
                })}
            </div>

            <div
                aria-labelledby={`${id}-${transport}-tab`}
                className="connection-manager-panel"
                id={`${id}-${transport}-panel`}
                role="tabpanel"
            >
                {transport !== "ble" ? (
                    <>
                        <p className="connection-manager-guide">
                            先连接服务，再使用 DG-LAB APP 扫描配对二维码。
                            {transport === "ws_v3" && "V3 每个连接支持一个双通道设备。"}
                        </p>
                        {connection ? (
                            <ConnectionCard
                                connection={connection}
                                disabled={disabled}
                                onConnect={onConnect}
                                onDisconnect={onDisconnect}
                                onOpenPairing={(connectionId) => {
                                    onClose();
                                    onOpenPairing(connectionId);
                                }}
                                onRefreshPairing={onRefreshPairing}
                                onSetEndpoint={onSetEndpoint}
                            />
                        ) : (
                            <p className="connection-manager-empty">连接服务尚未就绪，请稍后重试。</p>
                        )}
                    </>
                ) : (
                    <>
                        <div className="connection-manager-scan-heading">
                            <div>
                                <h3>发现郊狼 3.0</h3>
                                <p>开启设备，并断开手机 APP 的蓝牙连接后扫描。扫描不会自动连接或开启输出。</p>
                            </div>
                            <button
                                className="primary-compact-button"
                                disabled={disabled}
                                onClick={onScanBluetooth}
                                type="button"
                            >
                                {scanning ? <SpinnerGap aria-hidden="true" className="spin" size={18} />
                                    : <MagnifyingGlass aria-hidden="true" size={18} />}
                                {scanning ? "正在扫描…" : "扫描设备"}
                            </button>
                        </div>
                        <p aria-live="polite" className="connection-manager-result-count" role="status">
                            {scanning ? "正在查找附近的设备…" : bluetoothDevices.length > 0
                                ? `${bluetoothDevices.length} 台设备 · 连接后初始化完成才能输出`
                                : "尚无扫描结果，仅支持郊狼 3.0"}
                        </p>
                        {bluetoothDevices.length > 0 ? (
                            <ul aria-label="蓝牙设备" className="connection-manager-discovery-list">
                                {bluetoothDevices.map((device) => {
                                    const active = bluetoothConnections.find((candidate) =>
                                        candidate.endpoint === device.deviceId &&
                                        !["disconnected", "error"].includes(candidate.state));
                                    const connected = active?.state === "connected";
                                    return (
                                        <li className="connection-manager-discovery-row" key={device.deviceId}>
                                            <Bluetooth aria-hidden="true" className="connection-manager-device-icon" size={24} weight="light" />
                                            <div className="connection-manager-device-copy">
                                                <strong>{device.name}</strong>
                                                <code>{device.deviceId}</code>
                                                <span>信号 {device.rssi === null ? "未知" : `${device.rssi} dBm`}</span>
                                            </div>
                                            {active ? (
                                                <span className={`connection-manager-device-state${connected ? " is-connected" : ""}`}>
                                                    {connected ? <Check aria-hidden="true" size={16} />
                                                        : <SpinnerGap aria-hidden="true" className="spin" size={16} />}
                                                    {connected ? "已连接" : "正在连接"}
                                                </span>
                                            ) : (
                                                <button
                                                    aria-label={`连接蓝牙 ${device.name}`}
                                                    className="secondary-button"
                                                    disabled={disabled}
                                                    onClick={() => onConnectBluetooth(device.deviceId)}
                                                    type="button"
                                                >
                                                    连接
                                                </button>
                                            )}
                                        </li>
                                    );
                                })}
                            </ul>
                        ) : (
                            <div className="connection-manager-empty">
                                <Bluetooth aria-hidden="true" size={32} weight="light" />
                                <p>点击「扫描设备」开始查找</p>
                                <span>蓝牙不可用时，仍可通过 V4 / V3 APP 连接。</span>
                            </div>
                        )}
                        {bluetoothConnections.filter((candidate) => candidate.lastError).map((failed) => (
                            <p className="inline-error" key={failed.connectionId}>{failed.lastError}</p>
                        ))}
                    </>
                )}
            </div>
        </Dialog>
    );
};
