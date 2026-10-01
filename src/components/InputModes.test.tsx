// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import * as bridge from "../lib/bridge";
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
    ])("控制面板放在各自通道内：A %s / B %s", async (sourceA, sourceB, panelA, panelB) => {
        const user = userEvent.setup();
        render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), sourceA);
        await user.selectOptions(screen.getByRole("combobox", { name: "选择 郊狼 3.0 B 通道的输入源" }), sourceB);
        const channelA = screen.getByRole("region", { name: "A 通道控制" });
        const channelB = screen.getByRole("region", { name: "B 通道控制" });
        expect(within(channelA).getByRole("region", { name: `A 通道${panelA}` })).toBeTruthy();
        expect(within(channelB).getByRole("region", { name: `B 通道${panelB}` })).toBeTruthy();
        expect(within(channelA).queryByRole("region", { name: `B 通道${panelB}` })).toBeNull();
        expect(within(channelB).queryByRole("region", { name: `A 通道${panelA}` })).toBeNull();
    });

    it("两路音频各有控制面板并同步共享输入状态", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-audio");
        await user.selectOptions(screen.getByRole("combobox", { name: "选择 郊狼 3.0 B 通道的输入源" }), "source-audio");
        const channelA = within(screen.getByRole("region", { name: "A 通道控制" }));
        const channelB = within(screen.getByRole("region", { name: "B 通道控制" }));
        const playerA = within(channelA.getByRole("region", { name: "A 通道音频控制" }));
        const playerB = within(channelB.getByRole("region", { name: "B 通道音频控制" }));
        await user.click(playerB.getByRole("button", { name: "桌面音频" }));
        await waitFor(() => {
            expect(playerA.getByRole("button", { name: "停止桌面监听" }).getAttribute("aria-pressed")).toBe("true");
            expect(playerB.getByRole("button", { name: "停止桌面监听" }).getAttribute("aria-pressed")).toBe("true");
        });
        expect(channelA.getByRole("region", { name: "A 通道音频映射" })).toBeTruthy();
        expect(channelB.getByRole("region", { name: "B 通道音频映射" })).toBeTruthy();
        await user.click(playerA.getByRole("button", { name: "停止桌面监听" }));
        await waitFor(() => expect(playerB.getByRole("button", { name: "桌面音频" }).getAttribute("aria-pressed")).toBe("false"));
    });

    it("双通道分别触控，先按 B 不串到 A，撤下 A 只释放其触点", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-touch");
        await user.selectOptions(screen.getByRole("combobox", { name: "选择 郊狼 3.0 B 通道的输入源" }), "source-touch");
        await user.click(screen.getByRole("button", { name: "开始 郊狼 3.0 的波形输出" }));
        const boardA = within(screen.getByRole("region", { name: "A 通道控制" })).getByLabelText("触控区域");
        const boardB = within(screen.getByRole("region", { name: "B 通道控制" })).getByLabelText("触控区域");
        fireEvent.pointerDown(boardB, { pointerId: 12, button: 0, clientX: 300, clientY: 100 });
        await waitFor(() => expect(__getMockTouchInput()?.pointers).toEqual([{ id: 12, x: 0.75, y: 0.5, cell: null, channel: "b" }]));
        expect(boardA.querySelectorAll(".touch-pointer")).toHaveLength(0);
        expect(boardB.querySelectorAll(".touch-pointer")).toHaveLength(1);
        const firstInput = __getMockTouchInput()!;
        fireEvent.pointerDown(boardB, { pointerId: 13, button: 0, clientX: 200, clientY: 100 });
        expect(__getMockTouchInput()?.pointers).toHaveLength(1);
        fireEvent.pointerDown(boardA, { pointerId: 11, button: 0, clientX: 100, clientY: 100 });
        await waitFor(() => expect(__getMockTouchInput()?.pointers.map(({ id, channel }) => ({ id, channel }))).toEqual([{ id: 12, channel: "b" }, { id: 11, channel: "a" }]));
        expect(boardA.querySelectorAll(".touch-pointer")).toHaveLength(1);
        expect(boardB.querySelectorAll(".touch-pointer")).toHaveLength(1);
        expect(__getMockTouchInput()?.ownerId).toBe(firstInput.ownerId);
        expect(__getMockTouchInput()!.sequence).toBeGreaterThan(firstInput.sequence);
        await user.selectOptions(screen.getByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-audio");
        await waitFor(() => expect(__getMockTouchInput()?.pointers.map(({ id }) => id)).toEqual([12]));
        expect(within(screen.getByRole("region", { name: "B 通道控制" })).getByLabelText("触控区域")).toBe(boardB);
        expect(__getMockTouchInput()?.ownerId).toBe(firstInput.ownerId);
        fireEvent.pointerUp(boardB, { pointerId: 12 });
        await waitFor(() => expect(__getMockTouchInput()?.pointers).toHaveLength(0));
    });

    it("注册两源并允许保存触控面板与路由配置", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        expect(screen.getByRole("heading", { name: "触控模式" })).toBeTruthy();
        expect(screen.getByRole("heading", { name: "音频模式" })).toBeTruthy();
        await user.click(screen.getByRole("button", { name: "打开 触控模式 详情" }));
        await user.selectOptions(screen.getByRole("combobox", { name: "触控面板" }), "rhythm");
        await user.selectOptions(screen.getByRole("combobox", { name: "律动网格大小" }), "3");
        await user.selectOptions(screen.getByRole("combobox", { name: "触控通道分配" }), "separate");
        expect(screen.getAllByRole("combobox", { name: /触控区域.*波形/ })).toHaveLength(9);
        await user.click(screen.getByRole("button", { name: "应用触控配置" }));
        await waitFor(async () => {
            const config = (await getHubSnapshot()).inputModes.touchConfig;
            expect(config.mode).toBe("rhythm");
            expect(config.gridSize).toBe(3);
            expect(config.routing).toBe("separate");
        });
    });

    it("触控仅在开始设备输出后可用，切换来源不会影响另一设备", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-touch");
        const board = await screen.findByLabelText("触控区域");
        expect(board.getAttribute("aria-disabled")).toBe("true");
        await user.click(screen.getByRole("button", { name: "开始 郊狼 3.0 的波形输出" }));
        await waitFor(() => expect(board.getAttribute("aria-disabled")).toBe("false"));
        const snapshot = await getHubSnapshot();
        expect(snapshot.devices[0].sourceIdA).toBe("source-touch");
        expect(snapshot.devices[1].sourceIdA).toBe("source-fixed-waveform");
    });

    it("按设备通道保存音频映射并复制到另一通道", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.selectOptions(await screen.findByRole("combobox", { name: "选择 郊狼 3.0 A 通道的输入源" }), "source-audio");
        const settings = screen.getByRole("region", { name: "A 通道音频映射" });
        expect(settings.querySelector("details")?.open).toBe(false);
        await user.click(within(settings).getByText("映射设置"));
        expect(settings.querySelector("details")?.open).toBe(true);
        await user.selectOptions(screen.getByRole("combobox", { name: "A 通道输入声道" }), "left");
        const gain = screen.getByRole("spinbutton", { name: "A 通道数据增益" });
        fireEvent.change(gain, { target: { value: "4" } });
        await user.click(screen.getByRole("button", { name: "应用 A 音频配置" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audioBindings[0].config.gain).toBe(4));
        await user.click(screen.getByRole("button", { name: "复制到 B" }));
        await waitFor(async () => {
            const bindings = (await getHubSnapshot()).inputModes.audioBindings;
            expect(bindings[1].config.inputChannel).toBe("left");
            expect(bindings[1].config.gain).toBe(4);
            expect(bindings[2].config.gain).toBe(2.5);
        });
        await user.click(screen.getByRole("checkbox", { name: "A 通道音频输出" }));
        await waitFor(async () => {
            const bindings = (await getHubSnapshot()).inputModes.audioBindings;
            expect(bindings[0].config.enabled).toBe(false);
            expect(bindings[1].config.enabled).toBe(true);
        });
    });

    it("共享音频提供文件播放、麦克风和录音回放", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "打开 音频模式 详情" }));
        const player = screen.getByRole("region", { name: "共享音频控制" });
        expect(within(player).getByText(/浏览器演示仅展示操作状态/)).toBeTruthy();
        await user.click(within(player).getByRole("button", { name: "导入音频或视频" }));
        await user.click(await within(player).findByRole("button", { name: "播放音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("playing"));
        await user.click(within(player).getByRole("button", { name: "麦克风实时输入" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("capturing"));
        await user.click(within(player).getByRole("button", { name: "开始录音" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("recording"));
        await user.click(within(player).getByRole("button", { name: "完成录音并准备回放" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.hasRecording).toBe(true));
        expect(within(player).getByRole("button", { name: "保存录音 WAV" })).toBeTruthy();
        expect((await getHubSnapshot()).inputModes.audio.mode).toBe("recording");
        expect((within(player).getByRole("slider", { name: "音频播放进度" }) as HTMLInputElement).disabled).toBe(false);
        await user.click(within(player).getByRole("checkbox", { name: "循环播放音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.loop).toBe(true));
        await user.click(within(player).getByRole("checkbox", { name: "音频扬声器输出" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.speakerEnabled).toBe(false));
        await user.click(within(player).getByRole("button", { name: "播放音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("playing"));
    });

    it("视频通过同一导入操作加载音轨并使用音频播放控制", async () => {
        vi.spyOn(bridge, "chooseAudioFile").mockResolvedValue("C:/媒体/演示视频.MP4");
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "打开 音频模式 详情" }));
        const player = screen.getByRole("region", { name: "共享音频控制" });
        expect(within(player).getByText(/自动使用其中的音轨/)).toBeTruthy();
        await user.click(within(player).getByRole("button", { name: "导入音频或视频" }));
        expect(await within(player).findByText("演示视频.MP4")).toBeTruthy();
        await user.click(within(player).getByRole("button", { name: "播放音频" }));
        await waitFor(async () => {
            const audio = (await getHubSnapshot()).inputModes.audio;
            expect(audio.mode).toBe("file");
            expect(audio.state).toBe("playing");
            expect(audio.fileName).toBe("演示视频.MP4");
        });
        await user.click(within(player).getByRole("button", { name: "暂停音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("paused"));
    });

    it("第四种桌面音频模式可独立启停并与麦克风互相切换", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "打开 音频模式 详情" }));
        const player = screen.getByRole("region", { name: "共享音频控制" });
        await user.click(within(player).getByRole("button", { name: "导入音频或视频" }));
        await user.click(within(player).getByRole("button", { name: "桌面音频" }));
        await waitFor(async () => {
            const audio = (await getHubSnapshot()).inputModes.audio;
            expect(audio.mode).toBe("desktop");
            expect(audio.state).toBe("capturing");
            expect(audio.fileName).toBeNull();
            expect(audio.durationMs).toBe(0);
        });
        expect(within(player).getByText("桌面监听中")).toBeTruthy();
        expect(within(player).getByText("系统默认播放设备")).toBeTruthy();
        expect(within(player).getByRole("button", { name: "停止桌面监听" }).getAttribute("aria-pressed")).toBe("true");
        expect(within(player).getByRole("button", { name: "麦克风实时输入" }).getAttribute("aria-pressed")).toBe("false");
        expect((within(player).getByRole("slider", { name: "音频播放进度" }) as HTMLInputElement).disabled).toBe(true);
        expect(within(player).queryByRole("checkbox", { name: "音频扬声器输出" })).toBeNull();
        await user.click(within(player).getByRole("button", { name: "停止桌面监听" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("idle"));
        await user.click(within(player).getByRole("button", { name: "桌面音频" }));
        await user.click(within(player).getByRole("button", { name: "麦克风实时输入" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.mode).toBe("microphone"));
        expect(within(player).getByRole("button", { name: "停止收音" }).getAttribute("aria-pressed")).toBe("true");
        expect(within(player).getByRole("button", { name: "桌面音频" }).getAttribute("aria-pressed")).toBe("false");
        await user.click(within(player).getByRole("button", { name: "桌面音频" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.mode).toBe("desktop"));
        await user.click(within(player).getByRole("button", { name: "开始录音" }));
        await waitFor(async () => expect((await getHubSnapshot()).inputModes.audio.state).toBe("recording"));
        expect((within(player).getByRole("button", { name: "桌面音频" }) as HTMLButtonElement).disabled).toBe(true);
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
        const board = screen.getByLabelText("触控区域");
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
