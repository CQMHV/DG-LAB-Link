// @vitest-environment jsdom

import {
    act,
    cleanup,
    createEvent,
    fireEvent,
    render,
    screen,
    waitFor,
    within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import { WindowChrome } from "./components/WindowChrome";
import {
    __emitMockSnapshot,
    __resetMockBridge,
    getAppPreferences,
    getHubSnapshot,
} from "./lib/bridge";
import { DEVICE_TAB_RETURN_EVENT } from "./lib/deviceWindows";
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
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    Reflect.deleteProperty(document, "elementFromPoint");
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

    it("标签页只显示设备名称", async () => {
        render(<App />);

        const tab = await screen.findByRole("tab", { name: "郊狼 3.0" });
        expect(tab.textContent).toBe("郊狼 3.0");
    });

    it("可以在设备标签页内独立开始和停止输出", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(
            await screen.findByRole("button", {
                name: "开始 郊狼 3.0 的波形输出",
            }),
        );
        expect(
            await screen.findByRole("button", {
                name: "停止 郊狼 3.0 的波形输出",
            }),
        ).toBeTruthy();
        expect((await getHubSnapshot()).devices[0].outputActive).toBe(true);

        await user.click(
            screen.getByRole("button", {
                name: "停止 郊狼 3.0 的波形输出",
            }),
        );
        expect(
            await screen.findByRole("button", {
                name: "开始 郊狼 3.0 的波形输出",
            }),
        ).toBeTruthy();
        expect((await getHubSnapshot()).devices[0].outputActive).toBe(false);
    });

    it("可以从标签页列表搜索并切换标签页", async () => {
        const user = userEvent.setup();
        render(<App />);

        const deviceTab = await screen.findByRole("tab", {
            name: "郊狼 3.0",
        });
        await user.click(screen.getByRole("button", { name: "新建标签页" }));
        expect(deviceTab.getAttribute("aria-selected")).toBe("false");

        await user.click(screen.getByRole("button", { name: "搜索标签页" }));
        const dialog = screen.getByRole("dialog", { name: "标签页列表" });
        await user.type(
            within(dialog).getByRole("searchbox", { name: "搜索标签页" }),
            "郊狼",
        );
        expect(
            within(dialog).queryByRole("button", {
                name: "切换到标签页：新标签页",
            }),
        ).toBeNull();
        await user.click(
            within(dialog).getByRole("button", {
                name: "切换到标签页：郊狼 3.0",
            }),
        );

        expect(deviceTab.getAttribute("aria-selected")).toBe("true");
        expect(screen.queryByRole("dialog", { name: "标签页列表" })).toBeNull();
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
                screen.getByRole("button", {
                    name: "提高 A 通道强度",
                }) as HTMLButtonElement
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
                screen.getByRole("button", {
                    name: "提高 A 通道强度",
                }) as HTMLButtonElement
            ).disabled,
        ).toBe(false);
        expect(
            (
                screen.getByRole("button", {
                    name: "提高 B 通道强度",
                }) as HTMLButtonElement
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
        const emergencyButton = screen.getByRole("button", { name: "紧急停止全部设备" });
        expect(document.activeElement).toBe(emergencyButton);
        await user.tab({ shift: true });
        expect(document.activeElement).toBe(copyButton);
        await user.tab();
        expect(document.activeElement).toBe(emergencyButton);
        await user.tab();
        expect(document.activeElement).toBe(closeButton);

        await user.click(closeButton);
        expect(screen.queryByRole("dialog", { name: "配对 APP" })).toBeNull();
        expect(document.activeElement).toBe(openButton);
    });

    it("仪表盘只显示当前设备控制并保留固定安全操作栏", async () => {
        const user = userEvent.setup();
        render(<App />);

        expect(
            await screen.findByRole("region", { name: "当前设备仪表盘" }),
        ).toBeTruthy();
        expect(screen.queryByRole("region", { name: "全局控制" })).toBeNull();
        expect(screen.getByRole("contentinfo", { name: "安全限制与输出控制" })).toBeTruthy();
        expect(
            screen.getByText("郊狼 3.0", {
                selector: ".device-scope-title strong",
            }),
        ).toBeTruthy();
        expect(screen.queryByText("全部设备输出")).toBeNull();
        expect(
            screen.queryByRole("region", { name: "安全限制与输出控制" }),
        ).toBeNull();
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
            ).toBe("25");
        });

        const numberInput = screen.getByRole("spinbutton", {
            name: "输入 A 通道强度",
        }) as HTMLInputElement;
        await waitFor(() => {
            expect(numberInput.disabled).toBe(false);
            expect(numberInput.value).toBe("25");
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

    it("所有设备通道共享同一个固定波形输入源", async () => {
        const user = userEvent.setup();
        render(<App />);

        const firstDeviceSourceA = await screen.findByRole("combobox", {
            name: "选择 郊狼 3.0 A 通道的输入源",
        }) as HTMLSelectElement;
        const firstDeviceSourceB = screen.getByRole("combobox", {
            name: "选择 郊狼 3.0 B 通道的输入源",
        }) as HTMLSelectElement;
        expect(firstDeviceSourceA.value).toBe("source-fixed-waveform");
        expect(firstDeviceSourceB.value).toBe("source-fixed-waveform");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdB).toBe("source-fixed-waveform");
            expect(
                snapshot.sources.find(
                    (source) => source.id === "source-fixed-waveform",
                )?.assignedChannelCount,
            ).toBe(4);
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
        expect(secondDeviceSourceB.value).toBe("source-fixed-waveform");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdB).toBe("source-fixed-waveform");
        });
    });

    it("新标签页可以重复打开同一设备并共享实时状态", async () => {
        const user = userEvent.setup();
        render(<App />);
        const initialIntensity = (await getHubSnapshot()).devices[0].intensityA;

        await screen.findByTestId("channel-a-gauge");
        await user.click(screen.getByRole("button", { name: "新建标签页" }));

        expect(
            await screen.findByRole("region", { name: "新设备标签页" }),
        ).toBeTruthy();
        expect(screen.getByRole("tab", { name: /新标签页/ })).toBeTruthy();
        expect(screen.queryByTestId("channel-a-gauge")).toBeNull();

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
        expect(sourceA.value).toBe("source-fixed-waveform");
        expect(sourceB.value).toBe("source-fixed-waveform");
        await user.click(sync);

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceSync).toBe(true);
            expect(snapshot.devices[0].sourceIdA).toBeNull();
            expect(snapshot.devices[0].sourceIdB).toBeNull();
        });
        await user.selectOptions(sourceB, "source-fixed-waveform");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
        });

        await user.click(sync);
        await user.selectOptions(sourceA, "source-fixed-waveform");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].sourceSync).toBe(false);
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
        });
    });

    it("设置页面随后端快照更新 Relay 连接状态", async () => {
        const user = userEvent.setup();
        const initial = await getHubSnapshot();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设置" }));

        const states = [
            ["disconnected", "Relay 未连接"],
            ["connecting", "正在连接 Relay"],
            ["waiting", "Relay 已连接"],
            ["connected", "Relay 已连接"],
            ["error", "Relay 连接异常"],
        ] as const;

        for (const [index, [state, label]] of states.entries()) {
            act(() => {
                __emitMockSnapshot({
                    ...initial,
                    revision: initial.revision + index + 1,
                    connection: { ...initial.connection, state },
                });
            });
            const status = await screen.findByText(label);
            expect(status.getAttribute("role")).toBe("status");
            expect(status.getAttribute("data-state")).toBe(state);
        }
    });

    it("可以在设置页面选择新设备的默认输入源", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设置" }));
        const defaultSource = screen.getByRole("combobox", {
            name: "选择默认输入源",
        }) as HTMLSelectElement;
        expect(defaultSource.value).toBe("");
        await user.selectOptions(defaultSource, "source-fixed-waveform");

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.defaultSourceId).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdB).toBe("source-fixed-waveform");
        });
        expect(defaultSource.value).toBe("source-fixed-waveform");

        await user.selectOptions(defaultSource, "");
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.defaultSourceId).toBeNull();
            expect(snapshot.devices[0].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdA).toBe("source-fixed-waveform");
            expect(snapshot.devices[1].sourceIdB).toBe("source-fixed-waveform");
        });
        expect(defaultSource.value).toBe("");
    });

    it("自定义波形资源只在固定波形输入源设置中管理", async () => {
        const user = userEvent.setup();
        render(<App />);

        await screen.findByRole("region", {
            name: "A 通道固定波形仪表盘",
        });
        expect(
            screen.queryByRole("region", { name: "固定波形输入源设置" }),
        ).toBeNull();
        expect(screen.queryByLabelText("导入自定义波形文件")).toBeNull();

        await user.click(await screen.findByRole("button", { name: "输入源" }));

        expect(screen.getByRole("heading", { name: "固定波形" })).toBeTruthy();
        expect(screen.getByText("按设备通道配置")).toBeTruthy();
        expect(screen.queryByRole("combobox")).toBeNull();
        expect(
            screen.queryByRole("region", { name: "固定波形输入源设置" }),
        ).toBeNull();
        await user.click(
            screen.getByRole("button", { name: "打开 固定波形 详情" }),
        );
        expect(
            screen.getByRole("region", { name: "固定波形 输入源详情" }),
        ).toBeTruthy();
        expect(
            screen.getByRole("region", { name: "固定波形输入源设置" }),
        ).toBeTruthy();
        expect(screen.getByLabelText("导入自定义波形文件")).toBeTruthy();
        expect(screen.getByRole("button", { name: "导入波形" })).toBeTruthy();
    });

    it("可以为固定波形选择 DG-LAB 官方内置波形", async () => {
        const user = userEvent.setup();
        render(<App />);

        const channelA = await screen.findByRole("button", {
            name: "A 通道选择 呼吸",
        });
        const channelB = screen.getByRole("button", {
            name: "B 通道选择 气泡",
        });
        expect(channelA.getAttribute("aria-pressed")).toBe("true");
        expect(channelB.getAttribute("aria-pressed")).toBe("true");

        await user.click(
            screen.getByRole("button", { name: "A 通道选择 气泡" }),
        );

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].waveformIdA).toBe("BUBBLE");
            expect(snapshot.devices[0].waveformNameA).toBe("气泡");
            expect(snapshot.devices[0].waveformIdB).toBe("BUBBLE");
        });
        expect(
            screen
                .getByRole("button", { name: "A 通道选择 气泡" })
                .getAttribute("aria-pressed"),
        ).toBe("true");

        await user.click(
            screen.getByRole("button", { name: "B 通道选择 呼吸" }),
        );
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].waveformIdA).toBe("BUBBLE");
            expect(snapshot.devices[0].waveformIdB).toBe("BREATHING");
        });
        expect(
            screen
                .getByRole("button", { name: "A 通道选择 气泡" })
                .getAttribute("aria-pressed"),
        ).toBe("true");
        expect(
            screen
                .getByRole("button", { name: "B 通道选择 呼吸" })
                .getAttribute("aria-pressed"),
        ).toBe("true");
    });

    it("可以导入、选择、排序和删除自定义波形", async () => {
        const user = userEvent.setup();
        render(<App />);

        await screen.findByRole("region", {
            name: "A 通道固定波形仪表盘",
        });
        expect(screen.queryByLabelText("导入自定义波形文件")).toBeNull();
        await user.click(screen.getByRole("button", { name: "输入源" }));
        await user.click(
            screen.getByRole("button", { name: "打开 固定波形 详情" }),
        );
        const file = new File(
            [JSON.stringify({ name: "导入波形", frames: ["0A0A0A0A64646464"] })],
            "导入.json",
            { type: "application/json" },
        );
        Object.defineProperty(file, "text", {
            value: async () =>
                JSON.stringify({
                    name: "导入波形",
                    frames: ["0A0A0A0A64646464"],
                }),
        });
        fireEvent.change(screen.getByLabelText("导入自定义波形文件"), {
            target: { files: [file] },
        });

        let importedId = "";
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.customWaveforms).toHaveLength(2);
            importedId = snapshot.customWaveforms[1].id;
        });
        await user.click(screen.getByRole("button", { name: "控制台" }));
        await user.click(
            screen.getByRole("button", { name: "A 通道选择 导入波形" }),
        );
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].waveformIdA).toBe(importedId);
            expect(snapshot.devices[0].waveformIdB).toBe("BUBBLE");
        });

        await user.click(screen.getByRole("button", { name: "输入源" }));
        await user.click(
            screen.getByRole("button", { name: "打开 固定波形 详情" }),
        );
        await user.click(screen.getByRole("button", { name: "前移 导入波形" }));
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.customWaveforms.map((waveform) => waveform.name)).toEqual([
                "导入波形",
                "演示波形",
            ]);
        });

        await user.click(screen.getByRole("button", { name: "删除 导入波形" }));
        await user.click(screen.getByRole("button", { name: "确认删除 导入波形" }));
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.customWaveforms.map((waveform) => waveform.name)).toEqual([
                "演示波形",
            ]);
            expect(snapshot.devices[0].waveformIdA).toBeNull();
            expect(snapshot.devices[0].waveformIdB).toBe("BUBBLE");
        });
        await user.click(screen.getByRole("button", { name: "控制台" }));
        const channelADashboard = screen.getByRole("region", {
            name: "A 通道固定波形仪表盘",
        });
        expect(within(channelADashboard).getByText("无波形")).toBeTruthy();

        await user.click(
            screen.getByRole("button", { name: "A 通道选择 演示波形" }),
        );
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].waveformIdA).toBe("custom-demo");
            expect(snapshot.devices[0].waveformIdB).toBe("BUBBLE");
        });

        await user.click(screen.getByRole("button", { name: "输入源" }));
        await user.click(
            screen.getByRole("button", { name: "打开 固定波形 详情" }),
        );
        await user.click(screen.getByRole("button", { name: "删除 演示波形" }));
        await user.click(screen.getByRole("button", { name: "确认删除 演示波形" }));
        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.customWaveforms).toEqual([]);
            expect(snapshot.devices[0].waveformIdA).toBeNull();
            expect(snapshot.devices[0].waveformIdB).toBe("BUBBLE");
            expect(snapshot.devices[0].sourceIdB).toBe("source-fixed-waveform");
        });
        expect(screen.getAllByText("尚未导入波形").length).toBeGreaterThan(0);
    });

    it("可以把视图标签拖出为独立窗口并从主窗口移除", async () => {
        const user = userEvent.setup();
        const focus = vi.fn();
        const open = vi
            .spyOn(window, "open")
            .mockReturnValue({ focus } as unknown as Window);
        render(<App />);

        const tab = await screen.findByRole("tab", { name: /郊狼 3\.0/ });
        const tabItem = tab.closest(".device-tab-item");
        expect(tabItem).toBeTruthy();
        fireEvent.pointerDown(tabItem as HTMLElement, {
            button: 0,
            clientX: 100,
            clientY: 100,
            pointerId: 1,
        });
        fireEvent.pointerMove(tabItem as HTMLElement, {
            buttons: 1,
            clientX: 100,
            clientY: 300,
            pointerId: 1,
        });
        fireEvent.pointerUp(tabItem as HTMLElement, {
            button: 0,
            clientX: 100,
            clientY: 300,
            pointerId: 1,
        });

        await waitFor(() => expect(open).toHaveBeenCalledOnce());
        expect(open.mock.calls[0][0]).toContain("detached=1");
        expect(open.mock.calls[0][0]).toContain("deviceId=demo-app%3Aslot-a1");
        expect(open.mock.calls[0][0]).toContain("tabId=device-view-");
        expect(open.mock.calls[0][2]).toContain("width=640,height=520");
        expect(focus).toHaveBeenCalledOnce();
        await waitFor(() => {
            expect(screen.queryByRole("tab", { name: /郊狼 3\.0/ })).toBeNull();
        });
        expect(screen.queryAllByRole("tab")).toHaveLength(0);
        expect(screen.getByRole("region", { name: "未打开标签页" })).toBeTruthy();
        expect(screen.getByRole("button", { name: "新建标签页" })).toBeTruthy();
        expect(
            screen.queryByRole("button", { name: /独立窗口打开/ }),
        ).toBeNull();

        await user.click(
            screen.getByRole("button", { name: "搜索标签页" }),
        );
        await user.click(
            screen.getByRole("button", {
                name: "聚焦窗口标签页：郊狼 3.0",
            }),
        );
        await waitFor(() => expect(focus).toHaveBeenCalledTimes(2));

        const detachedUrl = new URL(open.mock.calls[0][0] as string);
        const detachedTabId = detachedUrl.searchParams.get("tabId");
        expect(detachedTabId).toBeTruthy();
        window.dispatchEvent(
            new MessageEvent("message", {
                data: {
                    payload: {
                        deviceId: "demo-app:slot-a1",
                        tabId: detachedTabId,
                    },
                    type: DEVICE_TAB_RETURN_EVENT,
                },
                origin: window.location.origin,
            }),
        );

        expect(
            await screen.findByRole("tab", { name: /郊狼 3\.0/ }),
        ).toBeTruthy();
        expect(screen.queryByRole("region", { name: "未打开标签页" })).toBeNull();
    });

    it("分离窗口标题栏提供移回主窗口入口", async () => {
        const user = userEvent.setup();
        const onReturnToMain = vi.fn();
        render(<WindowChrome onReturnToMain={onReturnToMain} />);

        await user.click(
            screen.getByRole("button", { name: "移回主窗口标签页" }),
        );

        expect(onReturnToMain).toHaveBeenCalledOnce();
    });

    it("可以关闭所有标签并通过加号重新创建标签页", async () => {
        const user = userEvent.setup();
        render(<App />);

        const deviceTab = await screen.findByRole("tab", {
            name: /郊狼 3\.0/,
        });
        await user.click(screen.getByRole("button", { name: "新建标签页" }));
        expect(screen.getAllByRole("tab")).toHaveLength(2);

        await user.click(
            screen.getByRole("button", { name: "关闭标签页：新标签页" }),
        );
        expect(screen.getAllByRole("tab")).toHaveLength(1);
        expect(deviceTab.getAttribute("aria-selected")).toBe("true");

        await user.click(
            screen.getByRole("button", { name: "关闭标签页：郊狼 3.0" }),
        );
        expect(screen.queryAllByRole("tab")).toHaveLength(0);
        expect(screen.getByRole("region", { name: "未打开标签页" })).toBeTruthy();
        expect(screen.getByRole("img", { name: "DG-LAB Link" })).toBeTruthy();

        await user.click(screen.getByRole("button", { name: "创建新标签页" }));

        const replacementTab = screen.getByRole("tab", { name: /新标签页/ });
        expect(replacementTab.getAttribute("aria-selected")).toBe("true");
        expect(screen.getByRole("region", { name: "新设备标签页" })).toBeTruthy();
    });

    it("普通点击标签页时可以正常切换而不会触发拖拽捕获", async () => {
        const user = userEvent.setup();
        render(<App />);

        const deviceTab = await screen.findByRole("tab", {
            name: /郊狼 3\.0/,
        });
        await user.click(screen.getByRole("button", { name: "新建标签页" }));
        const newTab = screen.getByRole("tab", { name: /新标签页/ });
        expect(newTab.getAttribute("aria-selected")).toBe("true");

        await user.click(deviceTab);

        expect(deviceTab.getAttribute("aria-selected")).toBe("true");
        expect(newTab.getAttribute("aria-selected")).toBe("false");
        expect(screen.getByTestId("channel-a-gauge")).toBeTruthy();
    });

    it("可以横向拖动调整标签页顺序且不会拆出窗口", async () => {
        const open = vi.spyOn(window, "open");
        render(<App />);

        const deviceTab = await screen.findByRole("tab", {
            name: /郊狼 3\.0/,
        });
        fireEvent.click(screen.getByRole("button", { name: "新建标签页" }));
        const newTab = screen.getByRole("tab", { name: /新标签页/ });
        const deviceTabItem = deviceTab.closest(".device-tab-item");
        const newTabItem = newTab.closest(".device-tab-item");
        const tabTrack = deviceTabItem?.closest(".device-tabs");
        expect(deviceTabItem).toBeTruthy();
        expect(newTabItem).toBeTruthy();
        expect(tabTrack).toBeTruthy();

        const setPointerCapture = vi.fn();
        Object.defineProperty(deviceTabItem, "setPointerCapture", {
            configurable: true,
            value: setPointerCapture,
        });

        Object.defineProperty(deviceTabItem, "getBoundingClientRect", {
            configurable: true,
            value: vi.fn().mockReturnValue({
                bottom: 144,
                height: 58,
                left: 0,
                right: 230,
                top: 86,
                width: 230,
                x: 0,
                y: 86,
                toJSON: () => ({}),
            }),
        });
        Object.defineProperty(newTabItem, "getBoundingClientRect", {
            configurable: true,
            value: vi.fn().mockReturnValue({
                bottom: 144,
                height: 58,
                left: 235,
                right: 445,
                top: 86,
                width: 210,
                x: 235,
                y: 86,
                toJSON: () => ({}),
            }),
        });
        Object.defineProperty(tabTrack, "getBoundingClientRect", {
            configurable: true,
            value: vi.fn().mockReturnValue({
                bottom: 144,
                height: 64,
                left: 0,
                right: 800,
                top: 80,
                width: 800,
                x: 0,
                y: 80,
                toJSON: () => ({}),
            }),
        });

        const elementFromPoint = vi.fn().mockReturnValue(newTabItem);
        Object.defineProperty(document, "elementFromPoint", {
            configurable: true,
            value: elementFromPoint,
        });
        fireEvent.pointerDown(deviceTabItem as HTMLElement, {
            button: 0,
            clientX: 100,
            clientY: 100,
            pointerId: 1,
        });
        expect(deviceTab.getAttribute("aria-selected")).toBe("true");
        expect(newTab.getAttribute("aria-selected")).toBe("false");
        expect(setPointerCapture).not.toHaveBeenCalled();
        fireEvent.pointerMove(deviceTabItem as HTMLElement, {
            buttons: 1,
            clientX: 400,
            clientY: 130,
            pointerId: 1,
        });
        expect(setPointerCapture).toHaveBeenCalledWith(1);
        const dragPreview = document.querySelector<HTMLElement>(
            ".device-tab-drag-preview",
        );
        expect(dragPreview).toBeTruthy();
        expect(dragPreview?.style.transform).toBe(
            "translate3d(300px, 86px, 0)",
        );
        expect(
            screen.getAllByRole("tab").map((tab) => tab.textContent),
        ).toEqual([
            expect.stringContaining("新标签页"),
            expect.stringContaining("郊狼 3.0"),
        ]);

        fireEvent.pointerMove(window, {
            buttons: 1,
            clientX: 150,
            clientY: 130,
            pointerId: 1,
        });
        expect(dragPreview?.style.transform).toBe(
            "translate3d(50px, 86px, 0)",
        );
        fireEvent.pointerUp(window, {
            button: 0,
            clientX: 150,
            clientY: 130,
            pointerId: 1,
        });

        expect(
            screen.getAllByRole("tab").map((tab) => tab.textContent),
        ).toEqual([
            expect.stringContaining("郊狼 3.0"),
            expect.stringContaining("新标签页"),
        ]);
        expect(deviceTab.getAttribute("aria-selected")).toBe("true");
        expect(newTab.getAttribute("aria-selected")).toBe("false");
        expect(open).not.toHaveBeenCalled();
        expect(elementFromPoint).toHaveBeenNthCalledWith(1, 415, 115);
        expect(elementFromPoint).toHaveBeenNthCalledWith(2, 165, 115);
    });

    it("可以从标签栏横向拖出标签并创建独立窗口", async () => {
        const open = vi
            .spyOn(window, "open")
            .mockReturnValue({ focus: vi.fn() } as unknown as Window);
        render(<App />);

        const tab = await screen.findByRole("tab", { name: /郊狼 3\.0/ });
        const tabItem = tab.closest(".device-tab-item");
        expect(tabItem).toBeTruthy();

        fireEvent.pointerDown(tabItem as HTMLElement, {
            button: 0,
            clientX: 100,
            clientY: 100,
            pointerId: 1,
        });
        fireEvent.pointerMove(tabItem as HTMLElement, {
            buttons: 1,
            clientX: 600,
            clientY: 100,
            pointerId: 1,
        });
        fireEvent.pointerUp(tabItem as HTMLElement, {
            button: 0,
            clientX: 600,
            clientY: 100,
            pointerId: 1,
        });

        await waitFor(() => expect(open).toHaveBeenCalledOnce());
        expect(open.mock.calls[0][0]).toContain("detached=1");
        await waitFor(() => {
            expect(screen.queryByRole("tab", { name: /郊狼 3\.0/ })).toBeNull();
        });
    });

    it("独立设备窗口只显示自身配置", async () => {
        const snapshot = await getHubSnapshot();
        render(
            <DashboardPage
                activeTabId="detached-test-tab"
                activeDeviceId={snapshot.devices[0].controlId}
                detached
                detachedTabs={[]}
                onAdjust={vi.fn()}
                onCloseTab={vi.fn()}
                onConnect={vi.fn()}
                onDetachTab={vi.fn()}
                onMoveTab={vi.fn()}
                onNewDeviceTab={vi.fn()}
                onFocusDetachedTab={vi.fn()}
                onOpenPairing={vi.fn()}
                onSelectDevice={vi.fn()}
                onSelectCustomWaveform={vi.fn()}
                onSelectTab={vi.fn()}
                onSetDeviceChannelSource={vi.fn()}
                onSetDeviceChannelSourceSync={vi.fn()}
                onSetFixedWaveform={vi.fn()}
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
        expect(
            screen.getByRole("region", {
                name: "A 通道固定波形仪表盘",
            }),
        ).toBeTruthy();
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

    it("连接超时默认关闭、时长无数字微调按钮，并在应用设置后启用", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设置" }));
        const toggle = screen.getByRole("checkbox", {
            name: "连接超时自动断开",
        }) as HTMLInputElement;
        expect(toggle.checked).toBe(false);
        expect(screen.queryByRole("textbox", { name: "连接超时分钟数" })).toBeNull();
        expect((await getHubSnapshot()).safety.connectionTimeoutEnabled).toBe(false);

        await user.click(toggle);
        const minutes = screen.getByRole("textbox", {
            name: "连接超时分钟数",
        }) as HTMLInputElement;
        expect(minutes.value).toBe("60");
        await user.clear(minutes);
        await user.type(minutes, "45");
        expect((await getHubSnapshot()).safety.connectionTimeoutEnabled).toBe(false);
        await user.click(screen.getByRole("button", { name: "应用安全设置" }));
        await waitFor(async () => {
            const safety = (await getHubSnapshot()).safety;
            expect(safety.connectionTimeoutEnabled).toBe(true);
            expect(safety.connectionTimeoutMinutes).toBe(45);
        });
        await user.click(toggle);
        expect(screen.queryByRole("textbox", { name: "连接超时分钟数" })).toBeNull();
        await user.click(toggle);
        expect((screen.getByRole("textbox", {
            name: "连接超时分钟数",
        }) as HTMLInputElement).value).toBe("45");
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
        expect(autoStart.checked).toBe(false);
        expect(screen.queryByRole("checkbox", {
            name: "以最小化形式启动",
        })).toBeNull();
        expect((await getAppPreferences()).startMinimized).toBe(true);

        await user.click(autoStart);
        const startMinimized = await screen.findByRole("checkbox", {
            name: "以最小化形式启动",
        }) as HTMLInputElement;
        expect(startMinimized.checked).toBe(true);
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
        await user.click(autoStart);
        await waitFor(() => {
            expect(screen.queryByRole("checkbox", {
                name: "以最小化形式启动",
            })).toBeNull();
        });
        expect((await getAppPreferences()).startMinimized).toBe(false);
        await user.click(autoStart);
        expect((await screen.findByRole("checkbox", {
            name: "以最小化形式启动",
        }) as HTMLInputElement).checked).toBe(false);
    });

    it("设备页面只展示状态且不提供设备切换入口", async () => {
        const user = userEvent.setup();
        render(<App />);
        const selectedDeviceId = (await getHubSnapshot()).selectedDeviceId;

        await user.click(screen.getByRole("button", { name: "设备" }));
        expect(screen.getByText("郊狼 3.0")).toBeTruthy();
        expect(screen.getByText("郊狼 2.0")).toBeTruthy();
        expect(
            screen.queryByRole("button", { name: "设为控制设备" }),
        ).toBeNull();
        expect(
            screen.queryByRole("button", { name: "当前控制设备" }),
        ).toBeNull();
        expect((await getHubSnapshot()).selectedDeviceId).toBe(selectedDeviceId);
    });

    it("可以从设备页面在新标签页中打开设备", async () => {
        const user = userEvent.setup();
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设备" }));
        const deviceCard = screen
            .getByRole("heading", { name: "郊狼 2.0" })
            .closest(".device-card");
        expect(deviceCard).toBeTruthy();
        await user.click(
            within(deviceCard as HTMLElement).getByRole("button", {
                name: "在新标签页中打开",
            }),
        );

        expect(
            await screen.findByRole("tab", { name: /郊狼 2\.0/ }),
        ).toBeTruthy();
        expect(
            screen.getByText("郊狼 2.0", {
                selector: ".device-scope-title strong",
            }),
        ).toBeTruthy();
    });

    it("可以从设备页面在新窗口中打开设备", async () => {
        const user = userEvent.setup();
        const focus = vi.fn();
        const open = vi
            .spyOn(window, "open")
            .mockReturnValue({ focus } as unknown as Window);
        render(<App />);

        await user.click(await screen.findByRole("button", { name: "设备" }));
        const deviceCard = screen
            .getByRole("heading", { name: "郊狼 3.0" })
            .closest(".device-card");
        expect(deviceCard).toBeTruthy();
        await user.click(
            within(deviceCard as HTMLElement).getByRole("button", {
                name: "在新窗口中打开",
            }),
        );

        await waitFor(() => expect(open).toHaveBeenCalledOnce());
        expect(open.mock.calls[0][0]).toContain("detached=1");
        expect(open.mock.calls[0][0]).toContain(
            "deviceId=demo-app%3Aslot-a1",
        );
        expect(open.mock.calls[0][2]).toContain("width=640,height=520");
        expect(focus).toHaveBeenCalledOnce();
        expect(
            screen.getByRole("heading", { level: 1, name: "设备" }),
        ).toBeTruthy();
    });
});
