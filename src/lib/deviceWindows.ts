import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

import type { DeviceSnapshot } from "./contracts";

export interface DeviceWindowContext {
    detached: boolean;
    deviceId: string | null;
}

const hashDeviceId = (deviceId: string): string => {
    let hash = 0x811c9dc5;
    for (let index = 0; index < deviceId.length; index += 1) {
        hash ^= deviceId.charCodeAt(index);
        hash = Math.imul(hash, 0x01000193);
    }
    return (hash >>> 0).toString(16).padStart(8, "0");
};

export const deviceWindowLabel = (deviceId: string): string =>
    `device-${deviceId.length}-${hashDeviceId(deviceId)}`;

export const getDeviceWindowContext = (): DeviceWindowContext => {
    const query = new URLSearchParams(window.location.search);
    const deviceId = query.get("deviceId");
    return {
        detached: query.get("detached") === "1" && Boolean(deviceId),
        deviceId,
    };
};

const deviceWindowQuery = (deviceId: string): URLSearchParams => {
    const query = new URLSearchParams();
    query.set("detached", "1");
    query.set("deviceId", deviceId);
    return query;
};

const browserDeviceWindowUrl = (deviceId: string): string => {
    const url = new URL(window.location.href);
    url.search = "";
    url.hash = "";
    url.search = deviceWindowQuery(deviceId).toString();
    return url.toString();
};

export const openDeviceWindow = async (
    device: DeviceSnapshot,
): Promise<void> => {
    const label = deviceWindowLabel(device.controlId);

    if (!("__TAURI_INTERNALS__" in window)) {
        const url = browserDeviceWindowUrl(device.controlId);
        const popup = window.open(
            url,
            label,
            "popup,width=1080,height=760,left=120,top=90",
        );
        if (!popup) {
            throw new Error("浏览器阻止了设备窗口，请允许此站点打开弹出窗口");
        }
        popup.focus();
        return;
    }

    const existing = await WebviewWindow.getByLabel(label);
    if (existing) {
        await existing.show();
        await existing.setFocus();
        return;
    }

    await new Promise<void>((resolve, reject) => {
        const window = new WebviewWindow(label, {
            url: `index.html?${deviceWindowQuery(device.controlId).toString()}`,
            title: `DG-LAB Link · ${device.name}`,
            width: 1080,
            height: 760,
            minWidth: 1080,
            minHeight: 620,
            center: true,
            decorations: false,
            resizable: true,
            shadow: true,
            focus: true,
        });

        void window.once("tauri://created", () => resolve());
        void window.once("tauri://error", (event) => {
            reject(new Error(`无法创建设备窗口：${String(event.payload)}`));
        });
    });
};
