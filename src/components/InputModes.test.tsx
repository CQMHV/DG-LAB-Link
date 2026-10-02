// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import { __emitMockSnapshot, __getMockTouchInput, __resetMockBridge, getHubSnapshot } from "../lib/bridge";
import type { TouchInput } from "../lib/contracts";
import { defaultTouchConfig } from "../lib/inputModes";
import { DashboardPage } from "../pages/DashboardPage";
import { TouchBoard } from "./TouchBoard";

beforeEach(() => {
    __resetMockBridge();
    vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
        const cell = this.hasAttribute("data-touch-cell");
        return { bottom: cell ? 400 : 200, height: 200, left: 0, right: 400, top: cell ? 200 : 0, width: 400, x: 0, y: cell ? 200 : 0, toJSON: () => ({}) };
    });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("动态输入源", () => {
    it.each([
        ["source-touch", "source-audio", "触控面板", "音频控制"],
        ["source-audio", "source-touch", "音频控制", "触控面板"],
    ])("公共插件面板独立显示于 A %s / B %s", async (sourceA, sourceB, panelA, panelB) => {
        const user = userEvent.setup(); render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), sourceA);
        await user.selectOptions(screen.getByRole("combobox", { name: "选择 郊狼 3.0 B 通道的输入源" }), sourceB);
        const channelA = within(screen.getByRole("region", { name: "A 通道控制" }));
        const channelB = within(screen.getByRole("region", { name: "B 通道控制" }));
        expect(await channelA.findByRole("region", { name: `A 通道${panelA}` })).toBeTruthy();
        expect(await channelB.findByRole("region", { name: `B 通道${panelB}` })).toBeTruthy();
    });

    it("声明式配置保留完整波形与曲线，保存触控参数", async () => {
        const original = (await getHubSnapshot()).inputModes.touchConfig;
        const user = userEvent.setup(); render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "打开 触控模式 详情" }));
        await user.selectOptions(await screen.findByRole("combobox", { name: "触控面板" }), "rhythm");
        await user.selectOptions(screen.getByRole("combobox", { name: "律动网格大小" }), "3");
        await user.selectOptions(screen.getByRole("combobox", { name: "触控通道分配" }), "separate");
        await user.click(screen.getByRole("button", { name: "应用触控配置" }));
        await waitFor(async () => {
            const config = (await getHubSnapshot()).inputModes.touchConfig;
            expect(config.mode).toBe("rhythm"); expect(config.gridSize).toBe(3); expect(config.routing).toBe("separate");
            expect(config.freeWaveforms).toEqual(original.freeWaveforms); expect(config.intensityCurve).toEqual(original.intensityCurve);
        });
    });

    it("设备输出控制仅控制插件波形路由", async () => {
        const user = userEvent.setup(); render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-touch");
        const board = await screen.findByLabelText("触控区域");
        expect(board.getAttribute("aria-disabled")).toBe("true");
        await user.click(screen.getByRole("button", { name: "开始 郊狼 3.0 的波形输出" }));
        await waitFor(() => expect(board.getAttribute("aria-disabled")).toBe("false"));
        await user.click(screen.getByRole("button", { name: "停止 郊狼 3.0 的波形输出" }));
        await waitFor(() => expect(board.getAttribute("aria-disabled")).toBe("true"));
        expect((await getHubSnapshot()).sources.find((source) => source.id === "source-touch")?.enabled).toBe(true);
    });

    it("公开音频播放器支持文件、麦克风、录音与桌面输入", async () => {
        const user = userEvent.setup(); render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "打开 音频模式 详情" }));
        const player = within(await screen.findByRole("region", { name: "共享音频控制" }));
        await user.click(player.getByRole("button", { name: "导入音频或视频" }));
        await user.click(await player.findByRole("button", { name: "播放音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("playing"));
        await user.click(player.getByRole("button", { name: "麦克风实时输入" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("capturing"));
        await user.click(player.getByRole("button", { name: "开始录音" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("recording"));
        await user.click(player.getByRole("button", { name: "完成录音并准备回放" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.hasRecording).toBe(true));
        await user.click(player.getByRole("button", { name: "桌面音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.mode).toBe("desktop"));
        await user.click(player.getByRole("button", { name: "停止桌面监听" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("idle"));
    });

    it("通道映射通过公共表单分别配置", async () => {
        const user = userEvent.setup(); render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-audio");
        const form = within(await screen.findByRole("form", { name: "A 通道音频映射" }));
        await user.selectOptions(form.getByRole("combobox", { name: "音频声道" }), "left");
        fireEvent.change(form.getByRole("slider", { name: "增益" }), { target: { value: "4" } });
        await user.click(form.getByRole("button", { name: "应用配置" }));
        await waitFor(async () => {
            const bindings = (await getHubSnapshot()).inputModes.audioBindings;
            expect(bindings[0].config.gain).toBe(4); expect(bindings[0].config.inputChannel).toBe("left"); expect(bindings[1].config.gain).toBe(2.5);
        });
    });
    it("触点移动合并，松手与失焦及时发布空输入", async () => {
        const input = vi.fn<(input: TouchInput) => Promise<void>>().mockResolvedValue(undefined);
        const { unmount } = render(<TouchBoard config={defaultTouchConfig()} deviceId="device-a" disabled={false} onInput={input} />);
        const board = screen.getByLabelText("触控区域");
        await waitFor(() => expect(input).toHaveBeenCalled());
        fireEvent.pointerDown(board, { pointerId: 10, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(1));
        fireEvent.pointerMove(board, { pointerId: 10, clientX: 300, clientY: 50 });
        fireEvent.pointerMove(board, { pointerId: 10, clientX: 200, clientY: 50 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers[0].x).toBe(0.5));
        fireEvent.pointerUp(board, { pointerId: 10 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0));
        fireEvent.pointerDown(board, { pointerId: 11, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(1));
        fireEvent.blur(window);
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0));
        const sequences = input.mock.calls.map(([call]) => call.sequence);
        expect(sequences.every((sequence, index) => index === 0 || sequence > sequences[index - 1])).toBe(true);
        unmount();
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0));
    });

    it("改变触控配置或停止输出会释放既有触点", async () => {
        const input = vi.fn<(input: TouchInput) => Promise<void>>().mockResolvedValue(undefined);
        const config = defaultTouchConfig();
        const { rerender } = render(<TouchBoard config={config} deviceId="device-a" disabled={false} onInput={input} />);
        const board = screen.getByLabelText("触控区域");
        fireEvent.pointerDown(board, { pointerId: 1, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(1));
        rerender(<TouchBoard config={{ ...config, routing: "separate" }} deviceId="device-a" disabled={false} onInput={input} />);
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0));
        fireEvent.pointerDown(board, { pointerId: 2, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(1));
        rerender(<TouchBoard config={config} deviceId="device-a" disabled onInput={input} />);
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0));
        fireEvent.pointerDown(board, { pointerId: 3, button: 0, clientX: 100, clientY: 100 });
        expect(input.mock.calls.at(-1)?.[0].pointers).toHaveLength(0);
    });

    it("IPC 未完成时快速松手再按下仍保留释放边沿", async () => {
        let finishFirstPress: (() => void) | undefined;
        const input = vi.fn<(input: TouchInput) => Promise<void>>().mockImplementation((state) => {
            if (state.pointers[0]?.id === 1) return new Promise<void>((resolve) => { finishFirstPress = resolve; });
            return Promise.resolve();
        });
        render(<TouchBoard config={defaultTouchConfig()} deviceId="device-a" disabled={false} onInput={input} />);
        await waitFor(() => expect(input).toHaveBeenCalled());
        const board = screen.getByLabelText("触控区域");
        fireEvent.pointerDown(board, { pointerId: 1, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(finishFirstPress).toBeTypeOf("function"));
        fireEvent.pointerMove(board, { pointerId: 1, clientX: 200, clientY: 100 });
        fireEvent.pointerUp(board, { pointerId: 1 });
        fireEvent.pointerDown(board, { pointerId: 2, button: 0, clientX: 300, clientY: 100 });
        finishFirstPress!();
        await waitFor(() => expect(input.mock.calls.at(-1)?.[0].pointers[0]?.id).toBe(2));
        const calls = input.mock.calls.map(([state]) => state);
        const firstPress = calls.findIndex((state) => state.pointers[0]?.id === 1);
        expect(calls[firstPress + 1].pointers).toHaveLength(0);
        expect(calls[firstPress + 2].pointers[0].id).toBe(2);
        expect(calls.every((state, index) => index === 0 || state.sequence > calls[index - 1].sequence)).toBe(true);
    });

    it("另一通道切换来源或保存音频配置不会中断已有触点与续租", async () => {
        const snapshot = await getHubSnapshot();
        const deviceId = snapshot.devices[0].controlId;
        snapshot.devices[0].sourceIdA = "source-touch";
        snapshot.devices[0].bindingIdA = `${deviceId}/a`;
        snapshot.devices[0].outputActive = true;
        __emitMockSnapshot(snapshot);
        const props = {
            activeTabId: "touch-channel-independence",
            activeDeviceId: deviceId,
            detachedTabs: [],
            tabs: [{ id: "touch-channel-independence", deviceId }],
            pendingAction: null,
            snapshot,
            onAdjust: vi.fn(),
            onCloseTab: vi.fn(),
            onConnect: vi.fn(),
            onDetachTab: vi.fn(),
            onMoveTab: vi.fn(),
            onNewDeviceTab: vi.fn(),
            onFocusDetachedTab: vi.fn(),
            onOpenPairing: vi.fn(),
            onSelectDevice: vi.fn(),
            onSelectCustomWaveform: vi.fn(),
            onSelectTab: vi.fn(),
            onSetFixedWaveform: vi.fn(),
            onStartOutput: vi.fn(),
            onStopOutput: vi.fn(),
            onSetDeviceChannelSource: vi.fn(),
            onSetDeviceChannelSourceSync: vi.fn(),
            onSetAudioConfig: vi.fn(),
        };
        const { rerender } = render(<DashboardPage {...props} />);
        const board = await screen.findByLabelText("触控区域");
        fireEvent.pointerDown(board, { pointerId: 5, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(__getMockTouchInput()?.pointers[0]?.id).toBe(5));
        const firstInput = __getMockTouchInput()!;
        const next = structuredClone(snapshot);
        next.devices[0].sourceIdB = "source-audio";
        __emitMockSnapshot(next);
        rerender(<DashboardPage {...props} snapshot={next} pendingAction={`source-${deviceId}-b`} />);
        expect(screen.getByLabelText("触控区域")).toBe(board);
        expect(board.getAttribute("aria-disabled")).toBe("false");
        await waitFor(() => {
            const renewed = __getMockTouchInput()!;
            expect(renewed.sequence).toBeGreaterThan(firstInput.sequence);
            expect(renewed.ownerId).toBe(firstInput.ownerId);
            expect(renewed.pointers[0].id).toBe(5);
        });
        const beforeAudioSave = __getMockTouchInput()!;
        rerender(<DashboardPage {...props} snapshot={next} pendingAction={`audio-config-${deviceId}-b`} />);
        expect(screen.getByLabelText("触控区域")).toBe(board);
        await waitFor(() => {
            const renewed = __getMockTouchInput()!;
            expect(renewed.sequence).toBeGreaterThan(beforeAudioSave.sequence);
            expect(renewed.ownerId).toBe(firstInput.ownerId);
            expect(renewed.pointers[0].id).toBe(5);
        });
    });
});
