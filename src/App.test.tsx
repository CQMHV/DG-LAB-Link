// @vitest-environment jsdom

import {
    act,
    cleanup,
    createEvent,
    fireEvent,
    render,
    screen,
    waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import {
    __emitMockSnapshot,
    __resetMockBridge,
    __setMockStartOutputCompletion,
    getAppPreferences,
    getHubSnapshot,
} from "./lib/bridge";
import { DashboardPage } from "./pages/DashboardPage";

class ResizeObserverMock {
    observe() {}

    unobserve() {}

    disconnect() {}
}

beforeEach(() => {
    __resetMockBridge();
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
        bottom: 240,
        height: 240,
        left: 0,
        right: 480,
        top: 0,
        width: 480,
        x: 0,
        y: 0,
        toJSON: () => ({}),
    });
    Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
            writeText: vi.fn().mockResolvedValue(undefined),
        },
    });
});

afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
});

describe("DG-LAB Link 前端", () => {
    it("在普通浏览器中明确标记演示模式", async () => {
        render(<App />);

        expect(
            await screen.findByText(
                "演示模式 · 当前为浏览器模拟数据，不会连接 Relay、APP 或设备",
            ),
        ).toBeTruthy();
    });

    it("可以在五个主页面之间切换", async () => {
        const user = userEvent.setup();
        render(<App />);

        await screen.findByRole("region", { name: "当前设备仪表盘" });
        await user.click(screen.getByRole("button", { name: "输入源" }));
        expect(
            screen.getByRole("heading", { level: 1, name: "输入源" }),
        ).toBeTruthy();

        await user.click(screen.getByRole("button", { name: "设备" }));
        expect(
            screen.getByRole("heading", { level: 1, name: "设备" }),
        ).toBeTruthy();

        await user.click(screen.getByRole("button", { name: "运行记录" }));
        expect(
            screen.getByRole("heading", { level: 1, name: "运行记录" }),
        ).toBeTruthy();

        await user.click(screen.getByRole("button", { name: "设置" }));
        expect(
            screen.getByRole("heading", { level: 1, name: "设置" }),
        ).toBeTruthy();

        await user.click(screen.getByRole("button", { name: "控制台" }));
        expect(
            screen.getByRole("region", { name: "当前设备仪表盘" }),
        ).toBeTruthy();
    });

    it("可以开始并停止波形输出", async () => {
        const user = userEvent.setup();
        render(<App />);

        const startButton = await screen.findByRole("button", {
            name: "开始输出",
        });
        await user.click(startButton);

        await waitFor(async () => {
            expect(
                screen.getByRole("button", { name: "停止输出" }),
            ).toBeTruthy();
            expect((await getHubSnapshot()).output.state).toBe("running");
        });

        await user.click(screen.getByRole("button", { name: "停止输出" }));
        await waitFor(() => {
            expect(
                screen.getByRole("button", { name: "开始输出" }),
            ).toBeTruthy();
        });
    });

    it("普通操作等待时仍可立即执行紧急停止", async () => {
        const user = userEvent.setup();
        let completeStartOutput: (() => void) | undefined;
        const startOutputCompletion = new Promise<void>((resolve) => {
            completeStartOutput = resolve;
        });
        __setMockStartOutputCompletion(startOutputCompletion);
        render(<App />);

        await user.click(
            await screen.findByRole("button", { name: "开始输出" }),
        );

        const emergencyButton = screen.getByRole("button", {
            name: "紧急停止",
        });
        expect((emergencyButton as HTMLButtonElement).disabled).toBe(false);
        await user.click(emergencyButton);

        await waitFor(() => {
            expect(screen.getAllByText("已停止").length).toBeGreaterThan(0);
        });

        await act(async () => {
            completeStartOutput?.();
            await startOutputCompletion;
        });
        await waitFor(async () => {
            expect((await getHubSnapshot()).output.state).toBe("stopped");
        });
    });

    it("展示后端快照中的输出错误", async () => {
        const initial = await getHubSnapshot();
        render(<App />);
        await screen.findByRole("region", { name: "当前设备仪表盘" });

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: initial.revision + 1,
                output: {
                    ...initial.output,
                    lastError: "APP 拒绝了波形操作",
                },
            });
        });

        expect(await screen.findByText("APP 拒绝了波形操作")).toBeTruthy();
    });

    it("展示后端快照中的连接错误", async () => {
        const initial = await getHubSnapshot();
        render(<App />);
        await screen.findByRole("region", { name: "当前设备仪表盘" });

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: initial.revision + 1,
                connection: {
                    ...initial.connection,
                    lastError: "Relay 握手超时",
                },
            });
        });

        expect(await screen.findByText("Relay 握手超时")).toBeTruthy();
    });

    it("被控端关闭通道时仍允许发送控制信息", async () => {
        const initial = await getHubSnapshot();
        render(<App />);
        await screen.findByRole("region", { name: "当前设备仪表盘" });

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: initial.revision + 1,
                device: initial.device
                    ? { ...initial.device, channelAStatus: "disabled" }
                    : null,
                devices: initial.devices.map((device, index) =>
                    index === 0
                        ? { ...device, channelAStatus: "disabled" }
                        : device,
                ),
                channels: {
                    ...initial.channels,
                    a: { ...initial.channels.a, status: "disabled" },
                },
            });
        });

        expect(await screen.findByText("被控端关闭")).toBeTruthy();
        expect(
            await screen.findByText("A 通道已关闭，仍接收控制"),
        ).toBeTruthy();
        expect(
            (
                screen.getByRole("button", { name: "开始输出" }) as HTMLButtonElement
            ).disabled,
        ).toBe(false);

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: initial.revision + 2,
                device: initial.device
                    ? {
                          ...initial.device,
                          channelAStatus: "disabled",
                          channelBStatus: "disabled",
                      }
                    : null,
                devices: initial.devices.map((device, index) =>
                    index === 0
                        ? {
                              ...device,
                              channelAStatus: "disabled",
                              channelBStatus: "disabled",
                          }
                        : device,
                ),
                channels: {
                    a: { ...initial.channels.a, status: "disabled" },
                    b: { ...initial.channels.b, status: "disabled" },
                },
            });
        });
        expect(
            await screen.findByText("A、B 通道已关闭，仍接收控制"),
        ).toBeTruthy();
        expect(
            (
                screen.getByRole("button", { name: "开始输出" }) as HTMLButtonElement
            ).disabled,
        ).toBe(false);
    });

    it("Relay 等待 APP 时可以通过连接设备打开并关闭二维码弹窗", async () => {
        const user = userEvent.setup();
        const initial = await getHubSnapshot();
        __emitMockSnapshot({
            ...initial,
            revision: initial.revision + 1,
            connection: {
                ...initial.connection,
                state: "waiting",
                appCount: 0,
            },
            device: null,
            devices: [],
            selectedDeviceId: null,
        });
        render(<App />);

        expect(
            await screen.findByRole("tab", { name: /新标签页/ }),
        ).toBeTruthy();
        expect(
            screen.getByRole("region", { name: "新设备标签页" }),
        ).toBeTruthy();
        expect(screen.getByRole("heading", { name: "连接设备" })).toBeTruthy();
        expect(screen.queryByTestId("channel-a-gauge")).toBeNull();
        expect(screen.queryByTestId("channel-b-gauge")).toBeNull();
        expect(
            screen.queryByRole("group", { name: "选择已连接设备" }),
        ).toBeNull();

        const openButton = await screen.findByRole("button", {
            name: "连接新设备",
        });
        await user.click(openButton);
        expect(
            screen.getByRole("dialog", { name: "配对 APP" }),
        ).toBeTruthy();
        expect(screen.getByText("控制端 ID")).toBeTruthy();
        const closeButton = screen.getByRole("button", {
            name: "关闭配对窗口",
        });
        expect(
            screen.getByRole("button", { name: "复制控制端 ID" }),
        ).toBeTruthy();
        const copyButton = screen.getByRole("button", {
            name: "复制配对链接",
        });
        expect(initial.connection.pairingUrl).toBeTruthy();
        expect(
            screen.queryByText(initial.connection.pairingUrl as string),
        ).toBeNull();

        await waitFor(() => {
            expect(document.activeElement).toBe(closeButton);
        });
        await user.tab({ shift: true });
        expect(document.activeElement).toBe(copyButton);
        await user.tab();
        expect(document.activeElement).toBe(closeButton);

        await user.click(closeButton);
        expect(screen.queryByRole("dialog", { name: "配对 APP" })).toBeNull();
        expect(document.activeElement).toBe(openButton);
    });

    it("明确区分当前设备和全部设备输出", async () => {
        const user = userEvent.setup();
        render(<App />);

        expect(
            await screen.findByRole("region", { name: "当前设备仪表盘" }),
        ).toBeTruthy();
        expect(screen.queryByRole("region", { name: "全局控制" })).toBeNull();
        expect(
            screen.getByText("郊狼 3.0", {
                selector: ".device-scope-title strong",
            }),
        ).toBeTruthy();
        expect(screen.getByText("全部设备输出")).toBeTruthy();
        expect(screen.getByText("应用到全部 2 台在线设备")).toBeTruthy();
        expect(screen.queryByRole("button", { name: "切换设备" })).toBeNull();

        await user.click(screen.getByRole("button", { name: "设备" }));
        expect(
            screen.getByRole("heading", { level: 1, name: "设备" }),
        ).toBeTruthy();
        expect(
            screen.queryByRole("button", { name: "刷新连接状态" }),
        ).toBeNull();
        expect(screen.queryByRole("dialog", { name: "配对 APP" })).toBeNull();
    });

    it("可以开启所有设备同步并保持相同实际强度", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设备" }));
        const syncToggle = screen.getByRole("checkbox", {
            name: "同步所有设备",
        });
        expect((syncToggle as HTMLInputElement).checked).toBe(false);
        await user.click(syncToggle);
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.syncAllDevices).toBe(true);
            expect(snapshot.devices[0].intensityA).toBe(5);
            expect(snapshot.devices[1].intensityA).toBe(5);
            expect(snapshot.devices[0].intensityB).toBe(11);
            expect(snapshot.devices[1].intensityB).toBe(11);
        });

        await user.click(screen.getByRole("button", { name: "控制台" }));
        expect(
            screen.getByText("强度调节同步到全部设备"),
        ).toBeTruthy();
        await user.click(
            screen.getByRole("button", { name: "提高 A 通道强度" }),
        );

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].intensityA).toBe(6);
            expect(snapshot.devices[1].intensityA).toBe(6);
        });
    });

    it("可以调整 A 通道强度", async () => {
        const user = userEvent.setup();
        render(<App />);

        expect(
            (await screen.findByTestId("channel-a-intensity") as HTMLInputElement)
                .value,
        ).toBe("5");
        await user.click(
            screen.getByRole("button", { name: "提高 A 通道强度" }),
        );
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("6");
        });

        await user.click(
            screen.getByRole("button", { name: "降低 A 通道强度" }),
        );
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("5");
        });

        const gauge = screen.getByTestId("channel-a-gauge");
        fireEvent.pointerDown(gauge, { clientX: 240, clientY: 120, pointerId: 1 });
        fireEvent.pointerUp(gauge, { clientX: 240, clientY: 120, pointerId: 1 });
        expect(
            (screen.getByTestId("channel-a-intensity") as HTMLInputElement).value,
        ).toBe("5");

        fireEvent.pointerDown(gauge, { clientX: 356, clientY: 120, pointerId: 2 });
        fireEvent.pointerUp(gauge, { clientX: 356, clientY: 120, pointerId: 2 });
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("20");
        });

        const numberInput = screen.getByRole("spinbutton", {
            name: "输入 A 通道强度",
        }) as HTMLInputElement;
        await waitFor(() => {
            expect(numberInput.disabled).toBe(false);
            expect(numberInput.value).toBe("20");
        });
        fireEvent.change(numberInput, { target: { value: "27" } });
        fireEvent.keyDown(numberInput, { key: "Enter" });
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("27");
        });
    });

    it("可以在新标签页选择另一台设备并只调整该设备", async () => {
        const user = userEvent.setup();
        render(<App />);

        await screen.findByTestId("channel-a-gauge");
        await user.click(screen.getByRole("button", { name: "新建标签页" }));
        await user.click(
            screen.getByRole("button", {
                name: "在当前标签页打开 郊狼 2.0",
            }),
        );

        await waitFor(() => {
            expect(screen.getByRole("tab", { name: /郊狼 2\.0/ })).toBeTruthy();
            expect(
                screen.getByText("郊狼 2.0", {
                    selector: ".device-scope-title strong",
                }),
            ).toBeTruthy();
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("3");
        });

        await user.click(
            screen.getByRole("button", { name: "提高 A 通道强度" }),
        );
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].intensityA).toBe(5);
            expect(snapshot.devices[1].intensityA).toBe(4);
        });
    });

    it("可以为每台设备的 A/B 通道独立选择输入源", async () => {
        const user = userEvent.setup();
        render(<App />);

        const firstDeviceSourceA = await screen.findByRole("combobox", {
            name: "选择 郊狼 3.0 A 通道的输入源",
        }) as HTMLSelectElement;
        const firstDeviceSourceB = screen.getByRole("combobox", {
            name: "选择 郊狼 3.0 B 通道的输入源",
        }) as HTMLSelectElement;
        expect(firstDeviceSourceA.value).toBe("source-test-pattern");
        expect(firstDeviceSourceB.value).toBe("source-manual");
        await user.selectOptions(firstDeviceSourceA, "source-manual");

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdB).toBe("source-test-pattern");
            expect(
                snapshot.sources.find(
                    (source) => source.id === "source-manual",
                )?.assignedChannelCount,
            ).toBe(3);
        });
        await user.click(screen.getByRole("button", { name: "新建标签页" }));
        await user.click(
            screen.getByRole("button", {
                name: "在当前标签页打开 郊狼 2.0",
            }),
        );
        const secondDeviceSourceB = screen.getByRole("combobox", {
            name: "选择 郊狼 2.0 B 通道的输入源",
        }) as HTMLSelectElement;
        expect(secondDeviceSourceB.value).toBe("source-test-pattern");
        await user.selectOptions(secondDeviceSourceB, "source-manual");

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdB).toBe("source-manual");
        });
    });

    it("新标签页可以重复打开同一设备并共享实时状态", async () => {
        const user = userEvent.setup();
        render(<App />);
        const initialIntensity = (await getHubSnapshot()).devices[0].intensityA;

        await user.click(
            await screen.findByRole("button", { name: "开始输出" }),
        );
        await waitFor(async () => {
            expect((await getHubSnapshot()).output.state).toBe("running");
        });
        await user.click(screen.getByRole("button", { name: "新建标签页" }));

        expect(
            await screen.findByRole("region", { name: "新设备标签页" }),
        ).toBeTruthy();
        expect(screen.getByRole("tab", { name: /新标签页/ })).toBeTruthy();
        expect(screen.queryByTestId("channel-a-gauge")).toBeNull();
        expect((await getHubSnapshot()).output.state).toBe("running");
        expect(screen.getByRole("button", { name: "停止输出" })).toBeTruthy();

        await user.click(
            screen.getByRole("button", {
                name: "在当前标签页打开 郊狼 3.0",
            }),
        );
        expect(await screen.findByTestId("channel-a-gauge")).toBeTruthy();
        expect(screen.queryByRole("tab", { name: /新标签页/ })).toBeNull();
        expect(screen.getAllByRole("tab", { name: /郊狼 3\.0/ })).toHaveLength(2);

        await user.click(
            screen.getByRole("button", { name: "提高 A 通道强度" }),
        );
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe(String(initialIntensity + 1));
        });
        await user.click(screen.getAllByRole("tab", { name: /郊狼 3\.0/ })[0]);
        expect(
            (await screen.findByTestId("channel-a-intensity") as HTMLInputElement)
                .value,
        ).toBe(String(initialIntensity + 1));
        expect((await getHubSnapshot()).output.state).toBe("running");
    });

    it("可以按设备同步 A/B 输入源并在关闭后恢复独立选择", async () => {
        const user = userEvent.setup();
        render(<App />);

        const sourceA = await screen.findByRole("combobox", {
            name: "选择 郊狼 3.0 A 通道的输入源",
        }) as HTMLSelectElement;
        const sourceB = screen.getByRole("combobox", {
            name: "选择 郊狼 3.0 B 通道的输入源",
        }) as HTMLSelectElement;
        const sync = screen.getByRole("checkbox", {
            name: "同步 郊狼 3.0 的 A/B 输入源",
        }) as HTMLInputElement;

        expect(sync.checked).toBe(false);
        expect(sourceA.value).toBe("source-test-pattern");
        expect(sourceB.value).toBe("source-manual");
        await user.click(sync);

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceSync).toBe(true);
            expect(snapshot.devices[0].sourceIdA).toBeNull();
            expect(snapshot.devices[0].sourceIdB).toBeNull();
        });
        await user.selectOptions(sourceB, "source-manual");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
        });

        await user.click(sync);
        await user.selectOptions(sourceA, "source-test-pattern");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceSync).toBe(false);
            expect(snapshot.devices[0].sourceIdA).toBe("source-test-pattern");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
        });
    });

    it("可以在输入源页面选择新设备的默认输入源", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "输入源" }));
        const defaultSource = screen.getByRole("combobox", {
            name: "选择默认输入源",
        }) as HTMLSelectElement;
        expect(defaultSource.value).toBe("");
        await user.selectOptions(defaultSource, "source-manual");

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.defaultSourceId).toBe("source-manual");
            expect(snapshot.devices[0].sourceIdA).toBe("source-test-pattern");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdB).toBe("source-test-pattern");
        });
        expect(
            screen.getByText("新接入设备的 A/B 将默认使用此输入源"),
        ).toBeTruthy();

        await user.selectOptions(defaultSource, "");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.defaultSourceId).toBeNull();
            expect(snapshot.devices[0].sourceIdA).toBe("source-test-pattern");
            expect(snapshot.devices[0].sourceIdB).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdA).toBe("source-manual");
            expect(snapshot.devices[1].sourceIdB).toBe("source-test-pattern");
        });
        expect(defaultSource.value).toBe("");
    });

    it("可以把视图标签拖出为独立窗口并从主窗口移除", async () => {
        const focus = vi.fn();
        const open = vi
            .spyOn(window, "open")
            .mockReturnValue({ focus } as unknown as Window);
        render(<App />);

        const tab = await screen.findByRole("tab", { name: /郊狼 3\.0/ });
        const tabItem = tab.closest(".device-tab-item");
        expect(tabItem).toBeTruthy();
        const dragStart = createEvent.dragStart(tabItem as HTMLElement, {
            dataTransfer: {
                effectAllowed: "none",
                setData: vi.fn(),
            },
        });
        Object.defineProperty(dragStart, "screenY", { value: 100 });
        fireEvent(tabItem as HTMLElement, dragStart);
        const dragEnd = createEvent.dragEnd(tabItem as HTMLElement);
        Object.defineProperty(dragEnd, "screenY", { value: 190 });
        fireEvent(tabItem as HTMLElement, dragEnd);

        await waitFor(() => expect(open).toHaveBeenCalledOnce());
        expect(open.mock.calls[0][0]).toContain("detached=1");
        expect(open.mock.calls[0][0]).toContain("deviceId=demo-app%3Aslot-a1");
        expect(open.mock.calls[0][0]).toContain("tabId=device-view-");
        expect(open.mock.calls[0][2]).toContain("width=640,height=520");
        expect(focus).toHaveBeenCalledOnce();
        await waitFor(() => {
            expect(screen.queryByRole("tab", { name: /郊狼 3\.0/ })).toBeNull();
        });
        expect(screen.getByRole("region", { name: "新设备标签页" })).toBeTruthy();
        expect(
            screen.getByRole("button", {
                name: "在当前标签页打开 郊狼 3.0",
            }),
        ).toBeTruthy();
        expect(
            screen.queryByRole("button", { name: /独立窗口打开/ }),
        ).toBeNull();
    });

    it("独立设备窗口只显示自身配置", async () => {
        const snapshot = await getHubSnapshot();
        render(
            <DashboardPage
                activeTabId="detached-test-tab"
                activeDeviceId={snapshot.devices[0].controlId}
                detached
                emergencyPending={false}
                onAdjust={vi.fn()}
                onConnect={vi.fn()}
                onDetachTab={vi.fn()}
                onEmergencyStop={vi.fn()}
                onNewDeviceTab={vi.fn()}
                onOpenPairing={vi.fn()}
                onSelectDevice={vi.fn()}
                onSelectTab={vi.fn()}
                onSetDeviceChannelSource={vi.fn()}
                onSetDeviceChannelSourceSync={vi.fn()}
                onStartOutput={vi.fn()}
                onStopOutput={vi.fn()}
                pendingAction={null}
                snapshot={snapshot}
                tabs={[
                    {
                        id: "detached-test-tab",
                        deviceId: snapshot.devices[0].controlId,
                    },
                ]}
            />,
        );

        expect(
            screen.getByText("郊狼 3.0", {
                selector: ".device-scope-title strong",
            }),
        ).toBeTruthy();
        expect(screen.getByTestId("channel-a-gauge")).toBeTruthy();
        expect(screen.getByTestId("channel-b-gauge")).toBeTruthy();
        expect(screen.queryByRole("region", { name: "全局控制" })).toBeNull();
        expect(
            screen.queryByRole("region", { name: "安全限制与输出控制" }),
        ).toBeNull();
        expect(screen.queryByRole("tablist", { name: "设备视图" })).toBeNull();
        expect(screen.queryByRole("button", { name: "新建标签页" })).toBeNull();
    });

    it("忽略 revision 更旧的乱序快照", async () => {
        const initial = await getHubSnapshot();
        render(<App />);
        await screen.findByTestId("channel-a-intensity");

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: 10,
                device: initial.device
                    ? { ...initial.device, intensityA: 42 }
                    : null,
                devices: initial.devices.map((device, index) =>
                    index === 0 ? { ...device, intensityA: 42 } : device,
                ),
                channels: {
                    ...initial.channels,
                    a: { ...initial.channels.a, intensity: 42 },
                },
            });
        });
        await waitFor(() => {
            expect(
                (screen.getByTestId("channel-a-intensity") as HTMLInputElement)
                    .value,
            ).toBe("42");
        });

        act(() => {
            __emitMockSnapshot({
                ...initial,
                revision: 9,
                device: initial.device
                    ? { ...initial.device, intensityA: 7 }
                    : null,
                devices: initial.devices.map((device, index) =>
                    index === 0 ? { ...device, intensityA: 7 } : device,
                ),
                channels: {
                    ...initial.channels,
                    a: { ...initial.channels.a, intensity: 7 },
                },
            });
        });
        expect(
            (screen.getByTestId("channel-a-intensity") as HTMLInputElement).value,
        ).toBe("42");
    });

    it("手机端反向控制只在应用安全设置后生效", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设置" }));
        const toggle = screen.getByRole("checkbox", {
            name: "允许手机端反向控制",
        }) as HTMLInputElement;
        expect(toggle.checked).toBe(false);

        await user.click(toggle);
        await waitFor(() => expect(toggle.checked).toBe(true));
        expect((await getHubSnapshot()).safety.allowAppIntensityControl).toBe(
            false,
        );
        expect(
            screen.getByText(/开启后，手机调整会同步到电脑端/),
        ).toBeTruthy();

        await user.click(
            screen.getByRole("button", { name: "应用安全设置" }),
        );
        await waitFor(async () => {
            expect(
                (await getHubSnapshot()).safety.allowAppIntensityControl,
            ).toBe(true);
        });
    });

    it("默认关闭主窗口时保留在托盘并立即保存设置", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设置" }));
        const toggle = screen.getByRole("checkbox", {
            name: "关闭主窗口时保留在托盘",
        }) as HTMLInputElement;
        expect(toggle.checked).toBe(true);
        expect((await getAppPreferences()).closeToTray).toBe(true);

        await user.click(toggle);
        await waitFor(async () => {
            expect(toggle.checked).toBe(false);
            expect((await getAppPreferences()).closeToTray).toBe(false);
        });
    });

    it("开机自启默认关闭且最小化启动默认开启", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设置" }));
        const autoStart = screen.getByRole("checkbox", {
            name: "开机自启",
        }) as HTMLInputElement;
        const startMinimized = screen.getByRole("checkbox", {
            name: "以最小化形式启动",
        }) as HTMLInputElement;

        expect(autoStart.checked).toBe(false);
        expect(startMinimized.checked).toBe(true);
        expect(startMinimized.disabled).toBe(true);

        await user.click(autoStart);
        await waitFor(async () => {
            expect(autoStart.checked).toBe(true);
            expect(startMinimized.disabled).toBe(false);
            expect((await getAppPreferences()).autoStart).toBe(true);
        });

        await user.click(startMinimized);
        await waitFor(async () => {
            expect(startMinimized.checked).toBe(false);
            expect((await getAppPreferences()).startMinimized).toBe(false);
        });
    });

    it("输出中切换控制焦点不会停止其他设备", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(
            await screen.findByRole("button", { name: "开始输出" }),
        );
        await waitFor(async () => {
            expect((await getHubSnapshot()).output.state).toBe("running");
        });

        await user.click(screen.getByRole("button", { name: "设备" }));
        expect(screen.getByText("郊狼 3.0")).toBeTruthy();
        expect(screen.getByText("郊狼 2.0")).toBeTruthy();
        await user.click(
            screen.getByRole("button", { name: "设为控制设备" }),
        );

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.device?.name).toBe("郊狼 2.0");
            expect(snapshot.output.state).toBe("running");
            expect(snapshot.devices).toHaveLength(2);
        });
    });
});
