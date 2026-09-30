import {
    ArrowSquareOut,
    BatteryCharging,
    Broadcast,
    DeviceMobile,
    LinkSimple,
    PlugsConnected,
    Pulse,
    Tabs,
} from "@phosphor-icons/react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot } from "../lib/contracts";

interface DevicesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onOpenInNewTab: (deviceId: string) => void;
    onOpenInNewWindow: (deviceId: string) => void;
    onOpenPairing: () => void;
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
    return "未连接";
};

export const DevicesPage = ({
    snapshot,
    pendingAction,
    onOpenInNewTab,
    onOpenInNewWindow,
    onOpenPairing,
    onSetSyncAllDevices,
}: DevicesPageProps) => (
    <div className="standard-page">
        <PageHeader
            actions={
                snapshot.connection.pairingUrl ? (
                    <button
                        className="secondary-button"
                        onClick={onOpenPairing}
                        type="button"
                    >
                        <LinkSimple aria-hidden="true" size={18} />
                        配对新 APP
                    </button>
                ) : undefined
            }
            description="查看已接入的 DG-LAB APP、设备能力与双通道实时状态。"
            eyebrow="APP & DEVICE"
            title="设备"
        />

        <section className="device-app-card">
            <div className="device-app-icon">
                <DeviceMobile aria-hidden="true" size={30} weight="light" />
            </div>
            <div>
                <span className="eyebrow">DG-LAB 4 APP</span>
                <h2>
                    {snapshot.connection.appCount > 0 ? "APP 已连接" : "等待 APP 接入"}
                </h2>
                <p>{snapshot.connection.endpoint}</p>
            </div>
            <div className="device-app-meta">
                <span>
                    <Broadcast aria-hidden="true" size={18} />
                    {snapshot.connection.appCount} 个会话
                </span>
                <code>{snapshot.connection.controllerId ?? "尚无控制端 ID"}</code>
            </div>
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
                            status: device.channelAStatus,
                        },
                        b: {
                            intensity: device.intensityB,
                            limit: device.intensityLimitB,
                            status: device.channelBStatus,
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
                                        {device.type} · 插槽 {device.slotId}
                                    </p>
                                </div>
                                <div className="battery-badge">
                                    <BatteryCharging aria-hidden="true" size={21} />
                                    {device.power}%
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
                        </section>
                    );
                })}
            </div>
        ) : (
            <section className="empty-state">
                <DeviceMobile aria-hidden="true" size={38} weight="light" />
                <h2>尚未发现设备</h2>
                <p>在 DG-LAB APP 中连接设备后，设备与通道状态会自动出现在这里。</p>
            </section>
        )}
    </div>
);
