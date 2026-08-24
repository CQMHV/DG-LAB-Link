import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

import type { DeviceSnapshot } from "./contracts";

export interface DeviceWindowContext {
    detached: boolean;
    deviceId: string | null;
    tabId: string | null;
}

interface DeviceWindowOptions {
    device?: DeviceSnapshot;
    onClosed?: () => void;
    tabId: string;
}

const browserDeviceWindows = new Map<string, Window>();

const bringDeviceWindowToFront = async (
    deviceWindow: WebviewWindow,
): Promise<void> => {
    await deviceWindow.show();
    await deviceWindow.unminimize();
    await deviceWindow.setFocus();
};

const hashTabId = (tabId: string): string => {
    let hash = 0x811c9dc5;
    for (let index = 0; index < tabId.length; index += 1) {
        hash ^= tabId.charCodeAt(index);
        hash = Math.imul(hash, 0x01000193);
    }
    return (hash >>> 0).toString(16).padStart(8, "0");
};

export const deviceWindowLabel = (tabId: string): string =>
    `device-view-${tabId.length}-${hashTabId(tabId)}`;

export const getDeviceWindowContext = (): DeviceWindowContext => {
    const query = new URLSearchParams(window.location.search);
    return {
        detached: query.get("detached") === "1",
        deviceId: query.get("deviceId"),
        tabId: query.get("tabId"),
    };
};

export const updateDetachedWindowDevice = (deviceId: string | null): void => {
    const query = new URLSearchParams(window.location.search);
    if (deviceId) {
        query.set("deviceId", deviceId);
    } else {
        query.delete("deviceId");
    }
    window.history.replaceState(null, "", `${window.location.pathname}?${query}`);
};

const deviceWindowQuery = ({
    device,
    tabId,
}: DeviceWindowOptions): URLSearchParams => {
    const query = new URLSearchParams();
    query.set("detached", "1");
    query.set("tabId", tabId);
    if (device) {
        query.set("deviceId", device.controlId);
    }
    return query;
};

const browserDeviceWindowUrl = (options: DeviceWindowOptions): string => {
    const url = new URL(window.location.href);
    url.search = deviceWindowQuery(options).toString();
    url.hash = "";
    return url.toString();
};

export const openDeviceWindow = async (
    options: DeviceWindowOptions,
): Promise<void> => {
    const label = deviceWindowLabel(options.tabId);
    const title = options.device?.name ?? "新标签页";

    if (!("__TAURI_INTERNALS__" in window)) {
        const popup = window.open(
            browserDeviceWindowUrl(options),
            label,
            "popup,width=640,height=520,left=120,top=90",
        );
        if (!popup) {
            throw new Error("浏览器阻止了设备窗口，请允许此站点打开弹出窗口");
        }
        browserDeviceWindows.set(options.tabId, popup);
        if (typeof popup.addEventListener === "function") {
            popup.addEventListener(
                "beforeunload",
                () => {
                    browserDeviceWindows.delete(options.tabId);
                    options.onClosed?.();
                },
                { once: true },
            );
        }
        popup.focus();
        return;
    }

    const existing = await WebviewWindow.getByLabel(label);
    if (existing) {
        if (options.onClosed) {
            void existing.once("tauri://destroyed", options.onClosed);
        }
        await bringDeviceWindowToFront(existing);
        return;
    }

    await new Promise<void>((resolve, reject) => {
        const detachedWindow = new WebviewWindow(label, {
            url: `index.html?${deviceWindowQuery(options).toString()}`,
            title: `DG-LAB Link · ${title}`,
            width: 640,
            height: 520,
            minWidth: 640,
            minHeight: 520,
            center: true,
            decorations: false,
            resizable: true,
            shadow: true,
            focus: true,
        });

        if (options.onClosed) {
            void detachedWindow.once("tauri://destroyed", options.onClosed);
        }
        void detachedWindow.once("tauri://created", () => resolve());
        void detachedWindow.once("tauri://error", (event) => {
            reject(new Error(`无法创建设备窗口：${String(event.payload)}`));
        });
    });
};

export const focusDeviceWindow = async (tabId: string): Promise<boolean> => {
    if (!("__TAURI_INTERNALS__" in window)) {
        const popup = browserDeviceWindows.get(tabId);
        if (!popup || popup.closed === true) {
            browserDeviceWindows.delete(tabId);
            return false;
        }
        popup.focus();
        return true;
    }

    const existing = await WebviewWindow.getByLabel(deviceWindowLabel(tabId));
    if (!existing) {
        return false;
    }
    await bringDeviceWindowToFront(existing);
    return true;
};
