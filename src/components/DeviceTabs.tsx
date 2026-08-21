import { Plus } from "@phosphor-icons/react";
import { useRef, useState } from "react";

import type { DeviceSnapshot } from "../lib/contracts";

export interface DeviceViewTab {
    id: string;
    deviceId: string | null;
}

interface DeviceTabsProps {
    activeTabId: string;
    devices: DeviceSnapshot[];
    onDetach: (tab: DeviceViewTab) => void;
    onNewTab: () => void;
    onSelect: (tabId: string) => void;
    pendingAction: string | null;
    tabs: DeviceViewTab[];
}

const DETACH_DISTANCE = 72;

export const DeviceTabs = ({
    activeTabId,
    devices,
    onDetach,
    onNewTab,
    onSelect,
    pendingAction,
    tabs,
}: DeviceTabsProps) => {
    const [draggingTabId, setDraggingTabId] = useState<string | null>(null);
    const dragOriginY = useRef(0);

    return (
        <div className="device-tabs-shell">
            <div className="device-tabs" role="tablist" aria-label="设备视图">
                {tabs.map((tab) => {
                    const device = devices.find(
                        (candidate) => candidate.controlId === tab.deviceId,
                    );
                    const active = tab.id === activeTabId;
                    const pending =
                        pendingAction === `window-${tab.id}` ||
                        (device
                            ? pendingAction === `device-${device.controlId}`
                            : false);
                    const dragging = draggingTabId === tab.id;

                    return (
                        <div
                            className={`device-tab-item ${active ? "device-tab-active" : ""} ${dragging ? "device-tab-dragging" : ""} ${device ? "" : "device-new-tab-item"}`}
                            draggable={!pending}
                            key={tab.id}
                            onDragEnd={(event) => {
                                const pointerY = event.screenY || event.clientY;
                                const shouldDetach =
                                    Math.abs(pointerY - dragOriginY.current) >=
                                    DETACH_DISTANCE;
                                setDraggingTabId(null);
                                if (shouldDetach) {
                                    onDetach(tab);
                                }
                            }}
                            onDragStart={(event) => {
                                dragOriginY.current = event.screenY || event.clientY;
                                event.dataTransfer.effectAllowed = "move";
                                event.dataTransfer.setData("text/plain", tab.id);
                                setDraggingTabId(tab.id);
                            }}
                            title="拖出标签栏以打开独立窗口"
                        >
                            <button
                                aria-controls="active-device-workspace"
                                aria-selected={active}
                                className="device-tab-select"
                                disabled={pending}
                                onClick={() => onSelect(tab.id)}
                                role="tab"
                                type="button"
                            >
                                {device ? (
                                    <span
                                        aria-hidden="true"
                                        className={`device-tab-status ${device.outputActive ? "device-tab-status-active" : ""}`}
                                    />
                                ) : (
                                    <Plus aria-hidden="true" size={17} />
                                )}
                                <span className="device-tab-copy">
                                    <strong>{device?.name ?? "新标签页"}</strong>
                                    <small>
                                        {device
                                            ? `A ${device.intensityA} · B ${device.intensityB}`
                                            : "选择或连接设备"}
                                    </small>
                                </span>
                            </button>
                        </div>
                    );
                })}
                <button
                    aria-label="新建标签页"
                    className="device-new-tab-button"
                    onClick={onNewTab}
                    title="新建标签页"
                    type="button"
                >
                    <Plus aria-hidden="true" size={19} />
                </button>
            </div>
        </div>
    );
};
