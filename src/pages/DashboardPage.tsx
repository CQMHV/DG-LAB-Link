import {
    BatteryHigh,
    Broadcast,
    DeviceMobile,
    LinkSimple,
    Plus,
    Waveform,
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
    activeTabId: string;
    activeDeviceId: string | null;
    detached?: boolean;
    detachedTabs: DeviceViewTab[];
    onAdjust: (channel: HubChannel, delta: number, deviceId: string) => void;
    onCloseTab: (tabId: string) => void;
    onConnect: () => void;
    onDetachTab: (tab: DeviceViewTab) => void;
    onMoveTab: (tabId: string, targetTabId: string) => void;
    onNewDeviceTab: () => void;
    onFocusDetachedTab: (tabId: string) => void;
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
}

export const DashboardPage = ({
    snapshot,
    tabs,
    pendingAction,
    activeTabId,
    activeDeviceId,
    detached = false,
    detachedTabs,
    onAdjust,
    onCloseTab,
    onConnect,
    onDetachTab,
    onMoveTab,
    onNewDeviceTab,
    onFocusDetachedTab,
    onOpenPairing,
    onSelectDevice,
    onSelectTab,
    onSetDeviceChannelSource,
    onSetDeviceChannelSourceSync,
}: DashboardPageProps) => {
    const device = activeDeviceId
        ? snapshot.devices.find(
              (candidate) => candidate.controlId === activeDeviceId,
          ) ?? null
        : null;
    const hasOpenTab = detached || tabs.length > 0;
    const deviceUnavailable = Boolean(activeDeviceId) && !device;
    const isConnected = snapshot.connection.state === "connected";
    const deviceSources = {
        a: snapshot.sources.find((source) => source.id === device?.sourceIdA),
        b: snapshot.sources.find((source) => source.id === device?.sourceIdB),
    };
    const canPair =
        (snapshot.connection.state === "waiting" || isConnected) &&
        Boolean(snapshot.connection.pairingUrl);
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
    const busy = pendingAction !== null;
    const sourceBusy = Boolean(
        device &&
            (pendingAction?.startsWith(`source-${device.controlId}-`) ||
                pendingAction === `source-sync-${device.controlId}`),
    );

    return (
        <div className={`dashboard-page ${detached ? "dashboard-page-detached" : ""}`}>
            <section className="device-workspace-shell" aria-label="当前设备仪表盘">
                {!detached && (
                    <DeviceTabs
                        activeTabId={activeTabId}
                        devices={snapshot.devices}
                        detachedTabs={detachedTabs}
                        onClose={onCloseTab}
                        onDetach={onDetachTab}
                        onMove={onMoveTab}
                        onNewTab={onNewDeviceTab}
                        onFocusDetached={onFocusDetachedTab}
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
                    {!hasOpenTab ? (
                        <section
                            aria-label="未打开标签页"
                            className="device-new-tab-page"
                        >
                            <div
                                aria-label="DG-LAB Link"
                                className="device-empty-tabs-wordmark"
                                role="img"
                            >
                                DG-LAB Link
                            </div>
                            <button
                                aria-label="创建新标签页"
                                className="device-new-tab-icon device-empty-tabs-add"
                                onClick={onNewDeviceTab}
                                title="新建标签页"
                                type="button"
                            >
                                <Plus aria-hidden="true" size={42} weight="light" />
                            </button>
                            <h2>未打开标签页</h2>
                            <p>点击“＋”新建标签页。</p>
                        </section>
                    ) : device ? (
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

        </div>
    );
};
