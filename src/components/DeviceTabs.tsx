import { ArrowSquareOut, DeviceMobile } from "@phosphor-icons/react";

import type { DeviceSnapshot } from "../lib/contracts";

interface DeviceTabsProps {
    activeDeviceId: string | null;
    devices: DeviceSnapshot[];
    onDetach: (device: DeviceSnapshot) => void;
    onSelect: (deviceId: string) => void;
    pendingAction: string | null;
}

export const DeviceTabs = ({
    activeDeviceId,
    devices,
    onDetach,
    onSelect,
    pendingAction,
}: DeviceTabsProps) => (
    <section className="device-tabs-shell" aria-label="设备仪表盘">
        <div className="device-tabs-heading">
            <DeviceMobile aria-hidden="true" size={18} weight="light" />
            <span>设备仪表盘</span>
        </div>
        <div className="device-tabs" role="tablist" aria-label="已连接设备">
            {devices.length === 0 ? (
                <span className="device-tabs-empty">连接设备后将在这里显示仪表盘</span>
            ) : (
                devices.map((device) => {
                    const active = device.controlId === activeDeviceId;
                    const pending = pendingAction === `device-${device.controlId}`;
                    return (
                        <div
                            className={`device-tab-item ${active ? "device-tab-active" : ""}`}
                            key={device.controlId}
                        >
                            <button
                                aria-selected={active}
                                className="device-tab-select"
                                disabled={pending}
                                onClick={() => onSelect(device.controlId)}
                                role="tab"
                                type="button"
                            >
                                <span
                                    aria-hidden="true"
                                    className={`device-tab-status ${device.outputActive ? "device-tab-status-active" : ""}`}
                                />
                                <span className="device-tab-copy">
                                    <strong>{device.name}</strong>
                                    <small>
                                        A {device.intensityA} · B {device.intensityB}
                                    </small>
                                </span>
                            </button>
                            <button
                                aria-label={`在独立窗口打开 ${device.name}`}
                                className="device-tab-detach"
                                onClick={() => onDetach(device)}
                                title="拉出独立窗口"
                                type="button"
                            >
                                <ArrowSquareOut aria-hidden="true" size={17} />
                            </button>
                        </div>
                    );
                })
            )}
        </div>
    </section>
);
