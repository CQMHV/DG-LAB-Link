import { Bluetooth, Play, Pulse, ShieldCheck, SpinnerGap, Stop } from "@phosphor-icons/react";

import type { HubSnapshot } from "../lib/contracts";
import "./SafetyActionBar.css";

interface SafetyActionBarProps {
    snapshot: HubSnapshot | null;
    deviceId: string | null;
    pendingAction: string | null;
    emergencyPending: boolean;
    showOutputControl: boolean;
    onStartOutput: (deviceId: string) => void;
    onStopOutput: (deviceId: string) => void;
    onEmergencyStop: () => void;
}

export const SafetyActionBar = ({
    snapshot,
    deviceId,
    pendingAction,
    emergencyPending,
    showOutputControl,
    onStartOutput,
    onStopOutput,
    onEmergencyStop,
}: SafetyActionBarProps) => {
    const device = snapshot?.devices.find((item) => item.controlId === deviceId);
    const connection = snapshot?.connections.find((item) => item.connectionId === device?.connectionId);
    const sourceA = snapshot?.sources.find((item) => item.id === device?.sourceIdA);
    const sourceB = snapshot?.sources.find((item) => item.id === device?.sourceIdB);
    const canStart = Boolean(device && device.initialization === "ready" &&
        connection?.state === "connected" && sourceA?.enabled && sourceB?.enabled);
    const outputCount = snapshot?.outputDeviceCount ?? 0;
    const protocol = device?.transport === "ble" ? "BLE" : device?.transport === "ws_v3" ? "V3" : "V4";

    return (
        <footer className="safety-action-bar" aria-label="安全限制与输出控制">
            <div className="safety-current-device">
                {device?.transport === "ble" ? <Bluetooth aria-hidden="true" size={23} /> : <ShieldCheck aria-hidden="true" size={23} />}
                <div>
                    <span title={device?.controlId}>{device?.transport === "ble" ? `当前设备 · ${device.id}` : "当前设备"}</span>
                    <strong title={device?.controlId}>{device ? `${device.name} · ${protocol}` : "未选择设备"}</strong>
                </div>
            </div>
            <div className="safety-device-limits">
                <span>安全上限</span>
                <strong>A {device?.intensityLimitA ?? "—"} <span>/</span> B {device?.intensityLimitB ?? "—"}</strong>
            </div>
            <div className={`safety-output-status ${outputCount > 0 ? "is-active" : ""}`} role="status">
                <Pulse aria-hidden="true" size={20} />
                <span>{snapshot ? outputCount > 0 ? `${outputCount} 台设备输出中` : "全部输出已停止" : "正在读取状态"}</span>
            </div>
            {showOutputControl && device && (
                <button
                    aria-label={`${device.outputActive ? "停止" : "开始"} ${device.name} 的波形输出`}
                    className={`safety-output-action ${device.outputActive ? "is-running" : ""}`}
                    disabled={pendingAction !== null || emergencyPending || (!device.outputActive && !canStart)}
                    onClick={() => device.outputActive ? onStopOutput(device.controlId) : onStartOutput(device.controlId)}
                    title={!device.outputActive && !canStart ? "设备就绪并为 A/B 通道分配输入源后可开始输出" : undefined}
                    type="button"
                >
                    {pendingAction === `output-${device.controlId}` ? <SpinnerGap aria-hidden="true" className="spin" size={18} /> : device.outputActive ? <Stop aria-hidden="true" size={17} weight="fill" /> : <Play aria-hidden="true" size={17} weight="fill" />}
                    {device.outputActive ? "停止输出" : "开始输出"}
                </button>
            )}
            <button
                aria-label="紧急停止全部设备"
                className="emergency-stop-button"
                data-emergency-stop
                disabled={emergencyPending}
                onClick={onEmergencyStop}
                title="清空全部设备波形、归零 A/B 强度并停止音频"
                type="button"
            >
                {emergencyPending ? <SpinnerGap aria-hidden="true" className="spin" size={18} /> : <Stop aria-hidden="true" size={18} weight="fill" />}
                <strong>{emergencyPending ? "正在停止…" : "紧急停止"}</strong>
                <span>全部设备</span>
            </button>
        </footer>
    );
};
