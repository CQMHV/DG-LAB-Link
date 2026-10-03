// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import { __emitMockSnapshot, __resetMockBridge, connectBluetooth, getHubSnapshot, scanBluetooth } from "../lib/bridge";
import { asObject } from "../lib/json";
import type { AudioSnapshot } from "../lib/contracts";
import { DashboardPage } from "../pages/DashboardPage";

beforeEach(() => {
    __resetMockBridge();
    vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
    vi.spyOn(HTMLElement.prototype, "getClientRects").mockReturnValue([{ width: 100, height: 40 }] as unknown as DOMRectList);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
describe("设备输出控制", () => {
    it("其他普通请求未完成时仍可停止当前设备", async () => {
        const snapshot = await getHubSnapshot();
        snapshot.devices[0].outputActive = true;
        const stop = vi.fn();
        const user = userEvent.setup();
        const noop = () => {};
        render(<DashboardPage
            activeTabId="test-tab"
            activeDeviceId={snapshot.devices[0].controlId}
            detachedTabs={[]}
            tabs={[{ id: "test-tab", deviceId: snapshot.devices[0].controlId }]}
            snapshot={snapshot}
            pendingAction="source-config"
            onAdjust={noop}
            onCloseTab={noop}
            onConnect={noop}
            onDetachTab={noop}
            onMoveTab={noop}
            onNewDeviceTab={noop}
            onFocusDetachedTab={noop}
            onOpenPairing={noop}
            onSelectDevice={noop}
            onSelectCustomWaveform={noop}
            onSelectTab={noop}
            onSetDeviceChannelSource={noop}
            onSetDeviceChannelSourceSync={noop}
            onSetFixedWaveform={noop}
            onStartOutput={noop}
            onStopOutput={stop}
        />);
        const button = screen.getByRole("button", { name: "停止 郊狼 3.0 的波形输出" });
        expect((button as HTMLButtonElement).disabled).toBe(false);
        await user.click(button);
        expect(stop).toHaveBeenCalledWith(snapshot.devices[0].controlId);
    });
    it("普通停止保留强度及音频采集，其他设备继续输出", async () => {
        const initial = await getHubSnapshot();
        initial.devices[0].outputActive = true; initial.devices[1].outputActive = true;
        initial.outputDeviceCount = 2; (asObject(initial.sources.find((source) => source.id === "source-audio")!.state).audio as AudioSnapshot).state = "capturing";
        __emitMockSnapshot(initial);
        const user = userEvent.setup(); render(<App />);
        await user.click(await screen.findByRole("button", { name: "停止 郊狼 3.0 的波形输出" }));
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].outputActive).toBe(false);
            expect(snapshot.devices[1].outputActive).toBe(true);
            expect(snapshot.devices[0].intensityA).toBe(initial.devices[0].intensityA);
            expect((asObject(snapshot.sources.find((source) => source.id === "source-audio")!.state).audio as AudioSnapshot).state).toBe("capturing");
        });
    });
    it("设备详情的键盘导航包含蓝牙参数折叠入口", async () => {
        await scanBluetooth(); await connectBluetooth("ble-demo-030");
        const user = userEvent.setup(); render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: "郊狼 3.0 蓝牙 设备详情" }));
        const close = screen.getByRole("button", { name: "关闭郊狼 3.0 蓝牙 · 设备详情窗口" });
        expect(document.activeElement).toBe(close);
        await user.tab(); expect(document.activeElement).toBe(screen.getByText("蓝牙参数"));
        await user.tab({ shift: true }); expect(document.activeElement).toBe(close);
        await user.keyboard("{Escape}"); expect(screen.queryByRole("dialog")).toBeNull();
    });
});
