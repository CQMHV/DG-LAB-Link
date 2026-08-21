import {
    ArrowsLeftRight,
    BatteryHigh,
    Broadcast,
    Clock,
    DeviceMobile,
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
import {
    DeviceTabs,
    type DeviceViewTab,
} from "../components/DeviceTabs";
import type {
    HubChannel,
    HubSnapshot,
} from "../lib/contracts";

interface DashboardPageProps {
    snapshot: HubSnapshot;
    tabs: DeviceViewTab[];
    pendingAction: string | null;
    emergencyPending: boolean;
    activeTabId: string;
    activeDeviceId: string | null;
    detached?: boolean;
    onAdjust: (channel: HubChannel, delta: number, deviceId: string) => void;
    onConnect: () => void;
    onDetachTab: (tab: DeviceViewTab) => void;
    onEmergencyStop: () => void;
    onNewDeviceTab: () => void;
    onOpenPairing: () => void;
    onSelectDevice: (deviceId: string) => void;
    onSelectTab: (tabId: string) => void;
    onSetDeviceChannelSource: (
        deviceId: string,
        channel: HubChannel,
        sourceId: string,
    ) => void;
    onSetDeviceChannelSourceSync: (
        deviceId: string,
        enabled: boolean,
    ) => void;
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
    tabs,
    pendingAction,
    emergencyPending,
    activeTabId,
    activeDeviceId,
    detached = false,
    onAdjust,
    onConnect,
    onDetachTab,
    onEmergencyStop,
    onNewDeviceTab,
    onOpenPairing,
    onSelectDevice,
    onSelectTab,
    onSetDeviceChannelSource,
    onSetDeviceChannelSourceSync,
    onStartOutput,
    onStopOutput,
}: DashboardPageProps) => {
    const device = activeDeviceId
        ? snapshot.devices.find(
              (candidate) => candidate.controlId === activeDeviceId,
          ) ?? null
        : null;
    const deviceUnavailable = Boolean(activeDeviceId) && !device;
    const isConnected = snapshot.connection.state === "connected";
    const isRunning = snapshot.output.state === "running";
    const deviceSources = {
        a: snapshot.sources.find((source) => source.id === device?.sourceIdA),
        b: snapshot.sources.find((source) => source.id === device?.sourceIdB),
    };
    const assignedSourceIds = new Set(
        snapshot.devices
            .flatMap((candidate) => [candidate.sourceIdA, candidate.sourceIdB])
            .filter((sourceId): sourceId is string => Boolean(sourceId)),
    );
    const unassignedChannelCount = snapshot.devices.reduce(
        (count, candidate) =>
            count + Number(!candidate.sourceIdA) + Number(!candidate.sourceIdB),
        0,
    );
    const globalSourceLabel = snapshot.devices.length === 0
        ? "未分配"
        : unassignedChannelCount > 0
          ? `部分未分配 · ${unassignedChannelCount} 路`
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
    const outputRoute = "A / B 独立";
    const canOutput =
        isConnected &&
        Boolean(device) &&
        snapshot.devices.length > 0 &&
        snapshot.devices.every(
            (candidate) => Boolean(candidate.sourceIdA && candidate.sourceIdB),
        );
    const busy = pendingAction !== null || emergencyPending;
    const sourceBusy = Boolean(
        device &&
            (pendingAction?.startsWith(`source-${device.controlId}-`) ||
                pendingAction === `source-sync-${device.controlId}`),
    );

    return (
        <div className={`dashboard-page ${detached ? "dashboard-page-detached" : ""}`}>
            {!detached && (
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
            )}

            <section className="device-workspace-shell" aria-label="当前设备仪表盘">
                {!detached && (
                    <DeviceTabs
                        activeTabId={activeTabId}
                        devices={snapshot.devices}
                        onDetach={onDetachTab}
                        onNewTab={onNewDeviceTab}
                        onSelect={onSelectTab}
                        pendingAction={pendingAction}
                        tabs={tabs}
                    />
                )}
                <div
                    className={`device-workspace ${detached ? "device-workspace-detached" : ""} ${!device ? "device-workspace-empty" : ""}`}
                    id="active-device-workspace"
                    role="tabpanel"
                >
                    {device ? (
                        <>
                            <header className="device-workspace-heading">
                                <div className="device-scope-title">
                                    <strong>{device.name}</strong>
                                </div>
                                <div className="device-source-control">
                                    <Waveform aria-hidden="true" size={17} weight="light" />
                                    <span>输入源</span>
                                    {(["a", "b"] as const).map((channel) => (
                                        <label key={channel}>
                                            <b>{channel.toUpperCase()}</b>
                                            <select
                                                aria-label={`选择 ${device.name} ${channel.toUpperCase()} 通道的输入源`}
                                                disabled={sourceBusy}
                                                onChange={(event) =>
                                                    onSetDeviceChannelSource(
                                                        device.controlId,
                                                        channel,
                                                        event.currentTarget.value,
                                                    )
                                                }
                                                value={deviceSources[channel]?.id ?? ""}
                                            >
                                                {!deviceSources[channel] && (
                                                    <option value="">未分配</option>
                                                )}
                                                {snapshot.sources
                                                    .filter((source) => source.enabled)
                                                    .map((source) => (
                                                        <option key={source.id} value={source.id}>
                                                            {source.name}
                                                        </option>
                                                    ))}
                                            </select>
                                        </label>
                                    ))}
                                    <label className="source-sync-toggle">
                                        <input
                                            aria-label={`同步 ${device.name} 的 A/B 输入源`}
                                            checked={device.sourceSync}
                                            disabled={sourceBusy}
                                            onChange={(event) =>
                                                onSetDeviceChannelSourceSync(
                                                    device.controlId,
                                                    event.currentTarget.checked,
                                                )
                                            }
                                            type="checkbox"
                                        />
                                        <span>A/B 同步</span>
                                    </label>
                                </div>
                                <div className="device-scope-meta">
                                    {snapshot.syncAllDevices && (
                                        <span className="device-scope-effect">
                                            强度调节同步到全部设备
                                        </span>
                                    )}
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
                                    <span className="device-frames-sent">
                                        已发送 {snapshot.output.framesSent.toLocaleString("zh-CN")} 帧
                                    </span>
                                </div>
                            </header>
                            <div className="realtime-stage" aria-label="双通道实时控制">
                                <ChannelControl
                                    channel="a"
                                    disabled={!isConnected}
                                    onAdjust={(channel, delta) =>
                                        onAdjust(channel, delta, device.controlId)
                                    }
                                    pending={
                                        pendingAction ===
                                        `intensity-${device.controlId}-a`
                                    }
                                    snapshot={deviceChannels.a}
                                />
                                <ChannelControl
                                    channel="b"
                                    disabled={!isConnected}
                                    onAdjust={(channel, delta) =>
                                        onAdjust(channel, delta, device.controlId)
                                    }
                                    pending={
                                        pendingAction ===
                                        `intensity-${device.controlId}-b`
                                    }
                                    snapshot={deviceChannels.b}
                                />
                            </div>
                        </>
                    ) : deviceUnavailable ? (
                        <section className="device-new-tab-page" aria-label="设备已离线">
                            <div className="device-new-tab-icon device-offline-icon">
                                <DeviceMobile aria-hidden="true" size={42} weight="light" />
                            </div>
                            <h2>设备已离线</h2>
                            <p>此设备重新上线后，仪表盘会自动恢复。</p>
                        </section>
                    ) : (
                        <section className="device-new-tab-page" aria-label="新设备标签页">
                            <div className="device-new-tab-icon">
                                <DeviceMobile aria-hidden="true" size={42} weight="light" />
                            </div>
                            <h2>
                                {snapshot.devices.length > 0
                                    ? "选择设备"
                                    : "连接设备"}
                            </h2>
                            <p>
                                {snapshot.devices.length > 0
                                    ? "选择此标签页要显示的设备；同一设备可以在多个标签页或窗口中打开，并共享实时状态。"
                                    : isConnected
                                      ? "通过 DG-LAB APP 连接新设备后，即可在此标签页打开。"
                                      : "先连接 Relay 服务，再通过 DG-LAB APP 接入设备。"}
                            </p>
                            {snapshot.devices.length > 0 && (
                                <div
                                    className="device-picker-options"
                                    aria-label="选择已连接设备"
                                    role="group"
                                >
                                    {snapshot.devices.map((candidate) => (
                                        <button
                                            aria-label={`在当前标签页打开 ${candidate.name}`}
                                            className="device-picker-option"
                                            key={candidate.controlId}
                                            onClick={() =>
                                                onSelectDevice(candidate.controlId)
                                            }
                                            type="button"
                                        >
                                            <span className="device-picker-option-icon">
                                                <DeviceMobile
                                                    aria-hidden="true"
                                                    size={25}
                                                    weight="light"
                                                />
                                            </span>
                                            <span className="device-picker-option-copy">
                                                <strong>{candidate.name}</strong>
                                                <small>
                                                    A {candidate.intensityA} · B {candidate.intensityB}
                                                    <span>·</span>
                                                    电量 {candidate.power}%
                                                </small>
                                            </span>
                                            <span
                                                aria-hidden="true"
                                                className={`device-picker-option-status ${candidate.outputActive ? "is-active" : ""}`}
                                            />
                                        </button>
                                    ))}
                                </div>
                            )}
                            {canPair ? (
                                <button
                                    className={
                                        snapshot.devices.length > 0
                                            ? "secondary-button device-picker-connect"
                                            : "primary-compact-button"
                                    }
                                    onClick={onOpenPairing}
                                    type="button"
                                >
                                    <LinkSimple aria-hidden="true" size={18} />
                                    连接新设备
                                </button>
                            ) : !isConnected ? (
                                <button
                                    className="primary-compact-button"
                                    disabled={busy}
                                    onClick={onConnect}
                                    type="button"
                                >
                                    <Broadcast aria-hidden="true" size={18} />
                                    连接 Relay 服务
                                </button>
                            ) : (
                                <span className="device-new-tab-waiting">等待设备接入…</span>
                            )}
                        </section>
                    )}
                </div>
            </section>

            {!detached && (
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
            )}
        </div>
    );
};
