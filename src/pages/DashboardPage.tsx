import {
    ArrowsLeftRight,
    BatteryHigh,
    Broadcast,
    Clock,
    LinkSimple,
    Play,
    ShieldCheck,
    SpinnerGap,
    Stop,
    WarningOctagon,
    Waveform,
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
    onOpenPairing: () => void;
    onSelectDevice: (deviceId: string) => void;
    onSetDeviceSource: (deviceId: string, sourceId: string) => void;
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
    onOpenPairing,
    onSelectDevice,
    onSetDeviceSource,
    onStartOutput,
    onStopOutput,
}: DashboardPageProps) => {
    const device =
        snapshot.devices.find(
            (candidate) => candidate.controlId === activeDeviceId,
        ) ?? (detached ? null : snapshot.device);
    const isConnected = snapshot.connection.state === "connected";
    const isRunning = snapshot.output.state === "running";
    const deviceSource = snapshot.sources.find(
        (source) => source.id === device?.sourceId,
    );
    const assignedSourceIds = new Set(
        snapshot.devices
            .map((candidate) => candidate.sourceId)
            .filter((sourceId): sourceId is string => Boolean(sourceId)),
    );
    const unassignedDeviceCount = snapshot.devices.filter(
        (candidate) => !candidate.sourceId,
    ).length;
    const globalSourceLabel = snapshot.devices.length === 0
        ? "未分配"
        : unassignedDeviceCount > 0
          ? `部分未分配 · ${unassignedDeviceCount} 台`
          : assignedSourceIds.size === 1
          ? snapshot.sources.find(
                (source) => source.id === [...assignedSourceIds][0],
            )?.name ?? "未知输入源"
          : `混合 · ${assignedSourceIds.size} 种`;
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
        snapshot.devices.length > 0 &&
        snapshot.devices.every((candidate) => Boolean(candidate.sourceId));
    const busy = pendingAction !== null || emergencyPending;

    return (
        <div className={`dashboard-page ${detached ? "dashboard-page-detached" : ""}`}>
            <section className="global-control-frame" aria-label="全局控制">
                <header className="global-control-heading">
                    <strong>全局控制</strong>
                    <span>·</span>
                    <span>全部设备</span>
                </header>
                <div className="global-control-items">
                    <div className="global-control-item global-connection-item">
                        <span
                            className={`status-dot status-${snapshot.connection.state}`}
                        />
                        <div>
                            <strong>{connectionCopy(snapshot)}</strong>
                            <small>{snapshot.connection.endpoint}</small>
                        </div>
                    </div>
                    <div className="global-control-item">
                        <WifiHigh aria-hidden="true" size={19} weight="light" />
                        <div>
                            <strong>
                                DG-LAB 4 APP · {snapshot.connection.appCount} 台
                            </strong>
                            <small>
                                {snapshot.connection.controllerId
                                    ? `控制端 ${snapshot.connection.controllerId}`
                                    : "连接后生成控制端 ID"}
                            </small>
                        </div>
                    </div>
                    <div className="global-control-item">
                        <Waveform aria-hidden="true" size={19} weight="light" />
                        <div>
                            <span>输入源</span>
                            <strong>{globalSourceLabel}</strong>
                        </div>
                    </div>
                    <div className="global-control-item">
                        <ArrowsLeftRight
                            aria-hidden="true"
                            size={19}
                            weight="light"
                        />
                        <div>
                            <span>输出路由</span>
                            <strong>
                                {outputRoute} × {snapshot.output.state === "running"
                                    ? `${snapshot.outputDeviceCount} 台`
                                    : `全部 ${snapshot.devices.length} 台`}
                            </strong>
                        </div>
                    </div>
                    <div className="global-control-item global-output-state">
                        <ShieldCheck aria-hidden="true" size={19} weight="light" />
                        <div>
                            <span>输出状态</span>
                            <strong className={`output-${snapshot.output.state}`}>
                                {outputLabel(snapshot.output.state)}
                            </strong>
                        </div>
                    </div>
                    <div className="connection-actions">
                        {canPair && !hasAppOrDevice ? (
                            <button
                                className="primary-compact-button"
                                onClick={onOpenPairing}
                                type="button"
                            >
                                <LinkSimple aria-hidden="true" size={18} />
                                连接设备
                            </button>
                        ) : !isConnected ? (
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
                        ) : null}
                    </div>
                </div>
            </section>

            <section className="device-workspace-shell" aria-label="当前设备仪表盘">
                {!detached && (
                    <DeviceTabs
                        activeDeviceId={device?.controlId ?? null}
                        devices={snapshot.devices}
                        onDetach={onDetachDevice}
                        onSelect={onSelectDevice}
                        pendingAction={pendingAction}
                    />
                )}
                <div
                    className={`device-workspace ${detached ? "device-workspace-detached" : ""}`}
                    id="active-device-workspace"
                    role="tabpanel"
                >
                    <header className="device-workspace-heading">
                        <div className="device-scope-title">
                            <strong>
                                {snapshot.syncAllDevices
                                    ? detached
                                        ? "此窗口显示"
                                        : "此标签显示"
                                    : detached
                                      ? "此窗口控制"
                                      : "此标签控制"}
                            </strong>
                            <span>·</span>
                            <span>{device?.name ?? "等待设备"}</span>
                        </div>
                        <label className="device-source-control">
                            <Waveform aria-hidden="true" size={17} weight="light" />
                            <span>输入源</span>
                            <select
                                aria-label={`选择 ${device?.name ?? "当前设备"} 的输入源`}
                                disabled={
                                    !device ||
                                    pendingAction === `source-${device.controlId}`
                                }
                                onChange={(event) => {
                                    if (device) {
                                        onSetDeviceSource(
                                            device.controlId,
                                            event.currentTarget.value,
                                        );
                                    }
                                }}
                                value={deviceSource?.id ?? ""}
                            >
                                {!deviceSource && <option value="">未分配</option>}
                                {snapshot.sources
                                    .filter((source) => source.enabled)
                                    .map((source) => (
                                        <option key={source.id} value={source.id}>
                                            {source.name}
                                        </option>
                                    ))}
                            </select>
                        </label>
                        <div className="device-scope-meta">
                            {snapshot.syncAllDevices && (
                                <span className="device-scope-effect">
                                    强度调节同步到全部设备
                                </span>
                            )}
                            {device ? (
                                <>
                                    <span className="device-battery">
                                        <BatteryHigh aria-hidden="true" size={18} />
                                        电量 {device.power}%
                                    </span>
                                    <span className="device-channel-status">
                                        {disabledChannels.length > 0
                                            ? disabledChannels.length === 2
                                                ? "A、B 通道已关闭，仍接收控制"
                                                : `${disabledChannels[0].toUpperCase()} 通道已关闭，仍接收控制`
                                            : "A、B 通道已就绪"}
                                    </span>
                                </>
                            ) : (
                                <span>
                                    {detached
                                        ? "该设备当前不在线"
                                        : "连接设备后可使用独立仪表盘"}
                                </span>
                            )}
                            <span className="device-frames-sent">
                                已发送 {snapshot.output.framesSent.toLocaleString("zh-CN")} 帧
                            </span>
                        </div>
                    </header>
                    <div className="realtime-stage" aria-label="双通道实时控制">
                        <ChannelControl
                            channel="a"
                            disabled={!isConnected || !device}
                            onAdjust={(channel, delta) => {
                                if (device) {
                                    onAdjust(channel, delta, device.controlId);
                                }
                            }}
                            pending={
                                pendingAction ===
                                `intensity-${device?.controlId}-a`
                            }
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
                            pending={
                                pendingAction ===
                                `intensity-${device?.controlId}-b`
                            }
                            snapshot={deviceChannels.b}
                        />
                    </div>
                </div>
            </section>

            <section className="control-footer" aria-label="安全限制与输出控制">
                <div className="footer-context">
                    <div className="global-output-scope">
                        <strong>全部设备输出</strong>
                        <small>应用到全部 {snapshot.devices.length} 台在线设备</small>
                    </div>
                    <div className="safety-summary">
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
