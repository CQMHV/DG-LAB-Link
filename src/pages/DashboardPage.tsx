import {
    ArrowRight,
    Broadcast,
    Clock,
    DeviceMobile,
    LinkSimple,
    Play,
    ShieldCheck,
    SpinnerGap,
    Stop,
    WarningOctagon,
    WifiHigh,
} from "@phosphor-icons/react";

import { ChannelControl } from "../components/ChannelControl";
import { DeviceTabs } from "../components/DeviceTabs";
import { WaveformPreview } from "../components/WaveformPreview";
import type {
    DeviceSnapshot,
    HubChannel,
    HubSnapshot,
} from "../lib/contracts";

interface DashboardPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    emergencyPending: boolean;
    activeDeviceId: string | null;
    detached?: boolean;
    onAdjust: (channel: HubChannel, delta: number, deviceId: string) => void;
    onConnect: () => void;
    onDetachDevice: (device: DeviceSnapshot) => void;
    onEmergencyStop: () => void;
    onOpenDevices: () => void;
    onOpenPairing: () => void;
    onSelectDevice: (deviceId: string) => void;
    onStartOutput: () => void;
    onStopOutput: () => void;
}

const connectionCopy = (snapshot: HubSnapshot): string => {
    if (snapshot.connection.state === "connecting") {
        return "Relay 连接中";
    }
    if (snapshot.connection.state === "waiting") {
        return "Relay 已连接，等待 APP";
    }
    if (snapshot.connection.state === "connected") {
        return "WebSocket 服务运行中";
    }
    if (snapshot.connection.state === "error") {
        return "Relay 连接异常";
    }
    return "Relay 未连接";
};

const outputLabel = (state: HubSnapshot["output"]["state"]): string => {
    if (state === "running") {
        return "输出中";
    }
    if (state === "error") {
        return "输出异常";
    }
    if (state === "stopped") {
        return "已停止";
    }
    return "待机";
};

export const DashboardPage = ({
    snapshot,
    pendingAction,
    emergencyPending,
    activeDeviceId,
    detached = false,
    onAdjust,
    onConnect,
    onDetachDevice,
    onEmergencyStop,
    onOpenDevices,
    onOpenPairing,
    onSelectDevice,
    onStartOutput,
    onStopOutput,
}: DashboardPageProps) => {
    const device =
        snapshot.devices.find(
            (candidate) => candidate.controlId === activeDeviceId,
        ) ?? (detached ? null : snapshot.device);
    const isConnected = snapshot.connection.state === "connected";
    const isRunning = snapshot.output.state === "running";
    const activeSource = snapshot.sources.find(
        (source) => source.id === snapshot.activeSourceId,
    );
    const canPair =
        (snapshot.connection.state === "waiting" || isConnected) &&
        Boolean(snapshot.connection.pairingUrl);
    const hasAppOrDevice =
        snapshot.connection.appCount > 0 || snapshot.devices.length > 0;
    const disabledChannels = (["a", "b"] as const).filter(
        (channel) =>
            (channel === "a"
                ? device?.channelAStatus
                : device?.channelBStatus) === "disabled",
    );
    const deviceChannels = {
        a: {
            intensity: device?.intensityA ?? 0,
            limit: Math.min(
                snapshot.safety.channelLimit,
                device?.intensityLimitA ?? snapshot.safety.channelLimit,
            ),
            status: device?.channelAStatus ?? "disconnected",
        },
        b: {
            intensity: device?.intensityB ?? 0,
            limit: Math.min(
                snapshot.safety.channelLimit,
                device?.intensityLimitB ?? snapshot.safety.channelLimit,
            ),
            status: device?.channelBStatus ?? "disconnected",
        },
    } satisfies HubSnapshot["channels"];
    const outputRoute = "A+B";
    const canOutput =
        isConnected &&
        Boolean(device) &&
        Boolean(activeSource);
    const busy = pendingAction !== null || emergencyPending;

    return (
        <div className={`dashboard-page ${detached ? "dashboard-page-detached" : ""}`}>
            <section className="connection-strip" aria-label="连接状态">
                <div className="connection-item">
                    <span
                        className={`status-dot status-${snapshot.connection.state}`}
                    />
                    <div>
                        <strong>{connectionCopy(snapshot)}</strong>
                        <small>{snapshot.connection.endpoint}</small>
                    </div>
                </div>
                <div className="connection-divider" />
                <div className="connection-item">
                    <WifiHigh aria-hidden="true" size={19} weight="light" />
                    <div>
                        <strong>
                            DG-LAB 4 APP
                            {snapshot.connection.appCount > 0
                                ? ` · ${snapshot.connection.appCount} 台`
                                : " · 未连接"}
                        </strong>
                        <small>
                            {snapshot.connection.controllerId
                                ? `控制端 ${snapshot.connection.controllerId}`
                                : "连接 Relay 后生成配对 ID"}
                        </small>
                    </div>
                </div>
                <div className="connection-divider" />
                <div className="connection-item connection-device">
                    <DeviceMobile aria-hidden="true" size={20} weight="light" />
                    <div>
                        <strong>
                            {device
                                ? `${detached ? "独立仪表盘" : "当前控制"}：${device.name}`
                                : detached
                                  ? "设备已断开"
                                  : "设备：未发现"}
                        </strong>
                        <small>
                            {device
                                ? disabledChannels.length > 0
                                    ? disabledChannels.length === 2
                                        ? "被控端 A、B 通道均已关闭；仍接收控制信息，但当前不会实际输出"
                                        : `被控端 ${disabledChannels[0].toUpperCase()} 通道已关闭；仍接收控制信息，但该通道不会实际输出`
                                    : snapshot.output.state === "running"
                                      ? `${snapshot.outputDeviceCount} 台正在同步输出 · 当前电量 ${device.power}%`
                                      : `${snapshot.devices.length} 台设备在线 · 当前电量 ${device.power}%`
                                : detached
                                  ? "该设备当前不在 APP 在线设备列表中"
                                  : "请先在 APP 中连接设备"}
                        </small>
                    </div>
                </div>
                <div className="connection-actions">
                    {detached ? (
                        <span className="detached-window-badge">
                            此窗口仅控制当前设备
                        </span>
                    ) : canPair ? (
                        <button
                            className="primary-compact-button"
                            onClick={hasAppOrDevice ? onOpenDevices : onOpenPairing}
                            type="button"
                        >
                            {hasAppOrDevice ? (
                                <DeviceMobile aria-hidden="true" size={18} />
                            ) : (
                                <LinkSimple aria-hidden="true" size={18} />
                            )}
                            {hasAppOrDevice ? "切换设备" : "连接设备"}
                        </button>
                    ) : (
                        <button
                            className="primary-compact-button"
                            disabled={busy}
                            onClick={onConnect}
                            type="button"
                        >
                            {pendingAction === "connect" ? (
                                <SpinnerGap
                                    aria-hidden="true"
                                    className="spin"
                                    size={18}
                                />
                            ) : (
                                <Broadcast aria-hidden="true" size={18} />
                            )}
                            连接服务
                        </button>
                    )}
                </div>
            </section>

            {!detached && (
                <DeviceTabs
                    activeDeviceId={device?.controlId ?? null}
                    devices={snapshot.devices}
                    onDetach={onDetachDevice}
                    onSelect={onSelectDevice}
                    pendingAction={pendingAction}
                />
            )}

            <section className="session-strip" aria-label="当前会话">
                <div className="session-label">当前会话</div>
                <div className="session-block">
                    <span>输入源</span>
                    <strong>{activeSource?.name ?? "未选择"}</strong>
                </div>
                <div className="session-divider" />
                <div className="session-block session-route">
                    <span>输出路由</span>
                    <strong>
                        {outputRoute} × {snapshot.output.state === "running"
                            ? `${snapshot.outputDeviceCount} 台`
                            : `全部 ${snapshot.devices.length} 台`}
                    </strong>
                    <ArrowRight aria-hidden="true" size={17} />
                    <strong>
                        {snapshot.syncAllDevices
                            ? `同步调节：全部 ${snapshot.devices.length} 台`
                            : device
                              ? `当前调节：${device.name}`
                            : "等待设备"}
                    </strong>
                </div>
                <div className="session-meta">
                    已发送 {snapshot.output.framesSent.toLocaleString("zh-CN")} 帧
                </div>
            </section>

            <section className="realtime-stage" aria-label="双通道实时控制">
                <ChannelControl
                    channel="a"
                    disabled={!isConnected || !device}
                    onAdjust={(channel, delta) => {
                        if (device) {
                            onAdjust(channel, delta, device.controlId);
                        }
                    }}
                    pending={pendingAction === `intensity-${device?.controlId}-a`}
                    snapshot={deviceChannels.a}
                />
                <WaveformPreview outputState={snapshot.output.state} />
                <ChannelControl
                    channel="b"
                    disabled={!isConnected || !device}
                    onAdjust={(channel, delta) => {
                        if (device) {
                            onAdjust(channel, delta, device.controlId);
                        }
                    }}
                    pending={pendingAction === `intensity-${device?.controlId}-b`}
                    snapshot={deviceChannels.b}
                />
            </section>

            <section className="control-footer" aria-label="安全限制与输出控制">
                <div className="safety-summary">
                    <strong>安全限制</strong>
                    <span>
                        <ShieldCheck aria-hidden="true" size={20} weight="light" />
                        通道上限 {snapshot.safety.channelLimit}
                    </span>
                    <span className="bullet-separator">·</span>
                    <span>
                        <Clock aria-hidden="true" size={20} weight="light" />
                        最长输出 {snapshot.safety.maxDurationMinutes} 分钟
                    </span>
                </div>
                <div className="output-summary">
                    <span>输出状态</span>
                    <strong className={`output-${snapshot.output.state}`}>
                        <span className="output-status-dot" />
                        {outputLabel(snapshot.output.state)}
                    </strong>
                </div>
                <div className="output-actions">
                    <button
                        className={`output-button ${isRunning ? "output-button-stop" : ""}`}
                        disabled={busy || (!isRunning && !canOutput)}
                        onClick={isRunning ? onStopOutput : onStartOutput}
                        type="button"
                    >
                        {pendingAction === "output" ? (
                            <SpinnerGap
                                aria-hidden="true"
                                className="spin"
                                size={21}
                            />
                        ) : isRunning ? (
                            <Stop aria-hidden="true" size={20} weight="fill" />
                        ) : (
                            <Play aria-hidden="true" size={20} weight="fill" />
                        )}
                        {isRunning ? "停止输出" : "开始输出"}
                    </button>
                    <button
                        className="emergency-button"
                        disabled={emergencyPending}
                        onClick={onEmergencyStop}
                        type="button"
                    >
                        {emergencyPending ? (
                            <SpinnerGap
                                aria-hidden="true"
                                className="spin"
                                size={20}
                            />
                        ) : (
                            <WarningOctagon
                                aria-hidden="true"
                                size={20}
                                weight="fill"
                            />
                        )}
                        {emergencyPending ? "正在停止" : "紧急停止"}
                    </button>
                </div>
            </section>
        </div>
    );
};
