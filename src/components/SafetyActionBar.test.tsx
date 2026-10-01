// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "../App";
import {
    __emitMockSnapshot,
    __resetMockBridge,
    __setMockStartOutputCompletion,
    connectBluetooth,
    getHubSnapshot,
    scanBluetooth,
} from "../lib/bridge";
import { SafetyActionBar } from "./SafetyActionBar";

beforeEach(() => {
    __resetMockBridge();
    vi.stubGlobal("ResizeObserver", class {
        observe() {}
        unobserve() {}
        disconnect() {}
    });
    vi.spyOn(HTMLElement.prototype, "getClientRects").mockReturnValue([
        { width: 100, height: 40 },
    ] as unknown as DOMRectList);
});

afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
});

describe("固定安全操作栏", () => {
    it("普通输出请求未完成时仍可紧急停止，迟到的开始请求不能恢复输出", async () => {
        let complete!: () => void;
        const delayed = new Promise<void>((resolve) => { complete = resolve; });
        __setMockStartOutputCompletion(delayed);
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "开始 郊狼 3.0 的波形输出" }));
        const stop = screen.getByRole("button", { name: "紧急停止全部设备" });
        expect((stop as HTMLButtonElement).disabled).toBe(false);
        await user.click(stop);
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.output.state).toBe("stopped");
            expect(snapshot.devices.every((device) => device.intensityA === 0 && device.intensityB === 0 && !device.outputActive)).toBe(true);
        });

        await act(async () => { complete(); await delayed; });
        await waitFor(() => expect((screen.getByRole("button", { name: "开始 郊狼 3.0 的波形输出" }) as HTMLButtonElement).disabled).toBe(false));
        expect((await getHubSnapshot()).outputDeviceCount).toBe(0);
        expect((await getHubSnapshot()).devices.every((device) => !device.outputActive)).toBe(true);
        expect(screen.queryByText("输出请求已被紧急停止取消")).toBeNull();
    });

    it("连接弹窗内可用键盘直接到达紧急停止，归零设备并停止音频", async () => {
        const initial = await getHubSnapshot();
        __emitMockSnapshot({
            ...initial,
            revision: initial.revision + 1,
            inputModes: { ...initial.inputModes, audio: { ...initial.inputModes.audio, state: "playing", levelLeft: 0.8 } },
        });
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: "添加设备" }));
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "关闭添加设备窗口" }));
        await user.tab({ shift: true });
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "紧急停止全部设备" }));
        await user.keyboard("{Enter}");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.inputModes.audio.state).toBe("idle");
            expect(snapshot.inputModes.audio.levelLeft).toBe(0);
            expect(snapshot.devices.every((device) => device.intensityA === 0 && device.intensityB === 0)).toBe(true);
        });
        expect(screen.getByRole("dialog", { name: "添加设备" })).toBeTruthy();
    });

    it("设备详情的键盘导航包含蓝牙参数折叠入口", async () => {
        await scanBluetooth();
        await connectBluetooth("ble-demo-030");
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: "郊狼 3.0 蓝牙 设备详情" }));
        const close = screen.getByRole("button", { name: "关闭郊狼 3.0 蓝牙 · 设备详情窗口" });
        expect(document.activeElement).toBe(close);
        await user.tab();
        expect(document.activeElement).toBe(screen.getByText("蓝牙参数"));
        await user.tab({ shift: true });
        expect(document.activeElement).toBe(close);
        await user.keyboard("{Escape}");
        expect(screen.queryByRole("dialog")).toBeNull();
    });

    it("未读到快照时保留紧急停止入口，安全上限显示未知", async () => {
        const emergency = vi.fn();
        const user = userEvent.setup();
        render(<SafetyActionBar
            deviceId={null}
            emergencyPending={false}
            onEmergencyStop={emergency}
            onStartOutput={vi.fn()}
            onStopOutput={vi.fn()}
            pendingAction="connect"
            showOutputControl
            snapshot={null}
        />);
        await user.click(screen.getByRole("button", { name: "紧急停止全部设备" }));
        expect(emergency).toHaveBeenCalledOnce();
        expect(screen.getByText("未选择设备")).toBeTruthy();
        expect(screen.queryByRole("button", { name: /开始.*输出/ })).toBeNull();
    });
});
