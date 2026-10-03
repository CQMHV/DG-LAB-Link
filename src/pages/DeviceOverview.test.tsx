// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import App from "../App";
import {
    __resetMockBridge,
    connectBluetooth,
    disconnectBluetooth,
    getHubSnapshot,
    scanBluetooth,
    startOutput,
} from "../lib/bridge";

beforeEach(() => __resetMockBridge());
afterEach(() => cleanup());

describe("设备总览", () => {
    it("首先展示已连接设备，按需打开连接管理并可用键盘返回", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));

        expect(screen.getByRole("heading", { level: 2, name: /郊狼 3\.0/ })).toBeTruthy();
        expect(screen.getByRole("heading", { level: 2, name: /郊狼 2\.0/ })).toBeTruthy();
        expect(screen.queryByRole("dialog")).toBeNull();
        expect(screen.queryByRole("region", { name: "Socket V4 连接" })).toBeNull();
        expect(screen.queryByRole("textbox", { name: "Socket V4 端点" })).toBeNull();
        expect(screen.queryByRole("button", { name: "扫描设备" })).toBeNull();

        const manage = screen.getByRole("button", { name: "管理连接" });
        await user.click(manage);
        const dialog = screen.getByRole("dialog", { name: "管理连接" });
        expect(within(dialog).getByRole("textbox", { name: "Socket V4 端点" })).toBeTruthy();
        const v4 = within(dialog).getByRole("tab", { name: "V4 APP" });
        await user.click(v4);
        await user.keyboard("{ArrowRight}");
        expect(within(dialog).getByRole("tab", { name: "V3 APP" }).getAttribute("aria-selected")).toBe("true");
        expect(within(dialog).getByRole("textbox", { name: "Socket V3 端点" })).toBeTruthy();
        await user.keyboard("{Escape}");

        expect(screen.queryByRole("dialog")).toBeNull();
        expect(document.activeElement).toBe(manage);
        expect((await getHubSnapshot()).connections.find((connection) => connection.transport === "ws_v3")?.state).toBe("disconnected");
    });

    it("同步使用显式基准设备，不改变控制台焦点", async () => {
        const initial = await getHubSnapshot();
        const base = initial.devices[1];
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));

        await user.selectOptions(screen.getByRole("combobox", { name: "同步基准设备" }), base.controlId);
        await user.click(screen.getByRole("checkbox", { name: "同步所有设备" }));

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.syncAllDevices).toBe(true);
            expect(snapshot.devices.map((device) => [device.intensityA, device.intensityB]))
                .toEqual([[base.intensityA, base.intensityB], [base.intensityA, base.intensityB]]);
        });
        await user.click(screen.getByRole("button", { name: "控制台" }));
        expect(screen.getByText("郊狼 3.0", { selector: ".device-scope-title strong" })).toBeTruthy();
    });

    it("普通停止只停止所选设备并保留两台设备的基础强度", async () => {
        const initial = await getHubSnapshot();
        await startOutput(initial.devices[0].controlId);
        await startOutput(initial.devices[1].controlId);
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: /^选择 郊狼 2\.0 · V4/ }));
        const deviceCard = screen.getByRole("heading", { level: 2, name: /郊狼 2\.0/ }).closest(".device-card") as HTMLElement;
        await user.click(within(deviceCard).getByRole("button", { name: "停止 郊狼 2.0 输出" }));

        await waitFor(async () => {
            const snapshot = await getHubSnapshot();
            expect(snapshot.devices[0].outputActive).toBe(true);
            expect(snapshot.devices[1].outputActive).toBe(false);
            expect(snapshot.devices.map((device) => [device.intensityA, device.intensityB]))
                .toEqual(initial.devices.map((device) => [device.intensityA, device.intensityB]));
            expect(snapshot.outputDeviceCount).toBe(1);
        });
        expect(within(deviceCard).queryByRole("button", { name: "停止 郊狼 2.0 输出" })).toBeNull();
        expect(screen.getByRole("button", { name: "停止 郊狼 3.0 输出" })).toBeTruthy();
    });

    it("离开设备页再返回时保留选定的对齐基准", async () => {
        const initial = await getHubSnapshot();
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.selectOptions(screen.getByRole("combobox", { name: "同步基准设备" }), initial.devices[1].controlId);
        await user.click(screen.getByRole("button", { name: "设置" }));
        await user.click(screen.getByRole("button", { name: "设备" }));
        expect((screen.getByRole("combobox", { name: "同步基准设备" }) as HTMLSelectElement).value).toBe(initial.devices[1].controlId);
        expect((await getHubSnapshot()).syncAllDevices).toBe(false);
    });

    it("对齐基准设备断开后仍可关闭全设备同步", async () => {
        await scanBluetooth();
        await connectBluetooth("ble-demo-030");
        const ble = (await getHubSnapshot()).devices.find((device) => device.transport === "ble")!;
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.selectOptions(screen.getByRole("combobox", { name: "同步基准设备" }), ble.controlId);
        await user.click(screen.getByRole("checkbox", { name: "同步所有设备" }));
        await waitFor(async () => expect((await getHubSnapshot()).syncAllDevices).toBe(true));
        await act(async () => { await disconnectBluetooth(ble.controlId); });
        const toggle = screen.getByRole("checkbox", { name: "同步所有设备" }) as HTMLInputElement;
        expect(toggle.disabled).toBe(false);
        await user.click(toggle);
        await waitFor(async () => expect((await getHubSnapshot()).syncAllDevices).toBe(false));
    });

    it("断开所选蓝牙设备后关闭详情，现存设备仍可操作", async () => {
        await scanBluetooth();
        await connectBluetooth("ble-demo-030");
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: /^选择 郊狼 3\.0 蓝牙 · BLE/ }));
        await user.click(screen.getByRole("button", { name: "郊狼 3.0 蓝牙 设备详情" }));
        const details = screen.getByRole("dialog", { name: "郊狼 3.0 蓝牙 · 设备详情" });
        await user.click(within(details).getByRole("button", { name: "断开蓝牙设备" }));

        await waitFor(() => {
            expect(screen.queryByRole("heading", { name: /郊狼 3\.0 蓝牙/, level: 2 })).toBeNull();
            expect(screen.queryByRole("dialog", { name: "郊狼 3.0 蓝牙 · 设备详情" })).toBeNull();
        });
        const existingDevice = screen.getByRole("heading", { level: 2, name: /郊狼 3\.0/ }).closest(".device-card") as HTMLElement;
        await user.click(within(existingDevice).getByRole("button", { name: "在新标签页中打开" }));
        expect(screen.queryByRole("button", { name: "开始 郊狼 3.0 蓝牙 的波形输出" })).toBeNull();
        await user.click(screen.getByRole("button", { name: "开始 郊狼 3.0 的波形输出" }));
        await waitFor(async () => expect((await getHubSnapshot()).devices[0].outputActive).toBe(true));
    });
});
