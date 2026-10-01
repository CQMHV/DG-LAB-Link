import {
    ArrowClockwise,
    Bluetooth,
    Broadcast,
    FloppyDisk,
    LinkSimple,
    Plugs,
    PlugsConnected,
} from "@phosphor-icons/react";
import { useEffect, useState } from "react";

import type { TransportConnectionSnapshot, TransportKind } from "../lib/contracts";
import { transportName } from "../lib/transports";

interface ConnectionCardProps {
    connection: TransportConnectionSnapshot;
    disabled: boolean;
    onConnect: (transport: Exclude<TransportKind, "ble">) => void;
    onDisconnect: (connectionId: string) => void;
    onOpenPairing: (connectionId: string) => void;
    onRefreshPairing: (connectionId: string) => void;
    onSetEndpoint: (transport: Exclude<TransportKind, "ble">, endpoint: string) => void;
}

const stateLabels = {
    disconnected: "未连接",
    connecting: "正在连接",
    waiting: "等待 APP 配对",
    connected: "已连接",
    error: "连接异常",
};

export const ConnectionCard = ({
    connection,
    disabled,
    onConnect,
    onDisconnect,
    onOpenPairing,
    onRefreshPairing,
    onSetEndpoint,
}: ConnectionCardProps) => {
    const [endpoint, setEndpoint] = useState(connection.endpoint);
    useEffect(() => setEndpoint(connection.endpoint), [connection.endpoint]);
    const offline = ["disconnected", "error"].includes(connection.state);
    const wsTransport = connection.transport === "ble" ? null : connection.transport;
    const ConnectionIcon = wsTransport ? Broadcast : Bluetooth;
    const validEndpoint = (() => {
        try {
            return ["ws:", "wss:"].includes(new URL(endpoint).protocol);
        } catch {
            return false;
        }
    })();

    return (
        <section className="device-app-card transport-connection-card" aria-label={`${transportName(connection.transport)} 连接`}>
            <div className="device-app-icon">
                <ConnectionIcon aria-hidden="true" size={27} weight="light" />
            </div>
            <div className="transport-connection-copy">
                <span className="eyebrow">{transportName(connection.transport)}</span>
                <h2>{stateLabels[connection.state]}</h2>
                <p>{connection.transport === "ble" ? "郊狼 3.0 直接连接" : `${connection.appCount} 个 APP 会话`}</p>
                <code>{connection.controllerId ?? connection.connectionId}</code>
            </div>
            <div className="transport-connection-actions">
                {wsTransport && offline && (
                    <button
                        className="primary-compact-button"
                        disabled={disabled || endpoint !== connection.endpoint || !validEndpoint}
                        onClick={() => onConnect(wsTransport)}
                        title={endpoint !== connection.endpoint ? "请先保存修改后的端点" : undefined}
                        type="button"
                    >
                        <PlugsConnected aria-hidden="true" size={17} />
                        连接
                    </button>
                )}
                {!offline && (
                    <button
                        className="secondary-button"
                        disabled={disabled}
                        onClick={() => onDisconnect(connection.connectionId)}
                        type="button"
                    >
                        <Plugs aria-hidden="true" size={17} />
                        断开
                    </button>
                )}
                {connection.pairingUrl && (
                    <button
                        className="secondary-button"
                        disabled={disabled}
                        onClick={() => onOpenPairing(connection.connectionId)}
                        type="button"
                    >
                        <LinkSimple aria-hidden="true" size={17} />
                        配对 APP
                    </button>
                )}
                {wsTransport && connection.pairingUrl && (
                    <button
                        aria-label={`刷新 ${transportName(connection.transport)} 配对`}
                        className="icon-button"
                        disabled={disabled}
                        onClick={() => onRefreshPairing(connection.connectionId)}
                        title="刷新配对会断开该连接的 APP"
                        type="button"
                    >
                        <ArrowClockwise aria-hidden="true" size={18} />
                    </button>
                )}
            </div>
            {wsTransport && (
                <div className="transport-endpoint-row">
                    <label className="visually-hidden" htmlFor={`endpoint-${connection.connectionId}`}>
                        {transportName(connection.transport)} 端点
                    </label>
                    <input
                        disabled={disabled || !offline}
                        id={`endpoint-${connection.connectionId}`}
                        onChange={(event) => setEndpoint(event.currentTarget.value)}
                        spellCheck={false}
                        value={endpoint}
                    />
                    <button
                        aria-label={`保存 ${transportName(connection.transport)} 端点`}
                        className="secondary-button"
                        disabled={disabled || !offline || !validEndpoint || endpoint === connection.endpoint}
                        onClick={() => onSetEndpoint(wsTransport, endpoint)}
                        title="连接运行时请先断开再修改端点"
                        type="button"
                    >
                        <FloppyDisk aria-hidden="true" size={17} />
                        保存端点
                    </button>
                </div>
            )}
            {connection.lastError && <p className="inline-error transport-connection-error">{connection.lastError}</p>}
        </section>
    );
};
