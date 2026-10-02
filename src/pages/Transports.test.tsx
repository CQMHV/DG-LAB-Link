// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import App from "../App";
import { __emitMockSnapshot, __resetMockBridge, connectBluetooth, getHubSnapshot, scanBluetooth } from "../lib/bridge";

beforeEach(() => __resetMockBridge());
afterEach(() => cleanup());

describe("多传输设备入口", () => {
    it("V3 配对使用自己的控制端和二维码，同时保留 V4", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: "管理连接" }));
        const manager = screen.getByRole("dialog", { name: "管理连接" });
        await user.click(within(manager).getByRole("tab", { name: "V3 APP" }));
        const v3 = within(manager).getByRole("region", { name: "Socket V3 连接" });
        await user.click(within(v3).getByRole("button", { name: "连接" }));
        await waitFor(() => expect(within(v3).getByText("等待 APP 配对")).toBeTruthy());
        await user.click(within(v3).getByRole("button", { name: "配对 APP" }));
        expect(screen.queryByRole("dialog", { name: "管理连接" })).toBeNull();
        const modal = screen.getByRole("dialog", { name: "配对 APP" });
        expect(within(modal).getByText("DG-LAB SOCKET V3")).toBeTruthy();
        expect(within(modal).getByText("v3-demo-controller")).toBeTruthy();
        const snapshot = await getHubSnapshot();
        expect(snapshot.connections.find((connection) => connection.transport === "ws_v4")?.state).toBe("connected");
        expect(snapshot.connections.find((connection) => connection.transport === "ws_v3")?.pairingUrl).toContain("DGLAB-SOCKET");
    });

    it("扫描主动连接 BLE，断开 V4 后仍能在仪表盘控制 BLE", async () => {
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        expect((await getHubSnapshot()).bluetooth).toEqual([]);
        await user.click(screen.getByRole("button", { name: "添加设备" }));
        const addDevice = screen.getByRole("dialog", { name: "添加设备" });
        await user.click(within(addDevice).getByRole("tab", { name: "蓝牙直连" }));
        await user.click(within(addDevice).getByRole("button", { name: "扫描设备" }));
        const connect = await screen.findByRole("button", { name: "连接蓝牙 47L121000 演示设备" });
        expect((await getHubSnapshot()).devices).toHaveLength(2);
        await user.click(connect);
        await screen.findByRole("heading", { name: /郊狼 3\.0 蓝牙/, level: 2 });
        expect((await getHubSnapshot()).devices.find((device) => device.transport === "ble")?.outputActive).toBe(false);
        await user.click(within(addDevice).getByRole("button", { name: "关闭添加设备窗口" }));
        const bleCard = screen.getByRole("heading", { name: /郊狼 3\.0 蓝牙/, level: 2 }).closest(".device-card") as HTMLElement;
        expect(within(bleCard).getByText("电量未知")).toBeTruthy();
        expect(within(bleCard).getByText("回路状态未知")).toBeTruthy();
        await user.click(screen.getByRole("button", { name: "管理连接" }));
        const manager = screen.getByRole("dialog", { name: "管理连接" });
        await user.click(within(manager).getByRole("tab", { name: "V4 APP" }));
        await user.click(within(manager).getByRole("button", { name: "断开" }));
        await waitFor(async () => expect((await getHubSnapshot()).devices).toHaveLength(1));
        await user.click(within(manager).getByRole("button", { name: "关闭管理连接窗口" }));
        await user.click(screen.getByRole("button", { name: "在新标签页中打开" }));
        const start = await screen.findByRole("button", { name: "开始 郊狼 3.0 蓝牙 的波形输出" });
        expect((start as HTMLButtonElement).disabled).toBe(false);
        await user.click(start);
        await waitFor(async () => expect((await getHubSnapshot()).devices[0].outputActive).toBe(true));
        expect((await getHubSnapshot()).connections.find((connection) => connection.transport === "ws_v4")?.state).toBe("disconnected");
    });

    it("蓝牙参数显示无回执状态，并保存有效设备参数", async () => {
        await scanBluetooth();
        await connectBluetooth("ble-demo-030");
        const user = userEvent.setup();
        render(<App />);
        await user.click(await screen.findByRole("button", { name: "设备" }));
        await user.click(screen.getByRole("button", { name: "郊狼 3.0 蓝牙 设备详情" }));
        const details = screen.getByRole("dialog", { name: "郊狼 3.0 蓝牙 · 设备详情" });
        await user.click(within(details).getByText("蓝牙参数"));
        expect(within(details).getByText("参数状态：已下发（BF 无设备回执）")).toBeTruthy();
        const limit = within(details).getByRole("spinbutton", { name: "郊狼 3.0 蓝牙 A 通道软上限" });
        fireEvent.change(limit, { target: { value: "201" } });
        expect((within(details).getByRole("button", { name: "应用并保存参数" }) as HTMLButtonElement).disabled).toBe(true);
        expect((await getHubSnapshot()).devices.find((device) => device.transport === "ble")?.bleParameters?.maxStrengthA).toBe(100);
        fireEvent.change(limit, { target: { value: "90" } });
        const nextSnapshot = await getHubSnapshot();
        act(() => __emitMockSnapshot({ ...nextSnapshot, revision: nextSnapshot.revision + 1 }));
        expect((limit as HTMLInputElement).value).toBe("90");
        await user.click(within(details).getByRole("button", { name: "应用并保存参数" }));
        await waitFor(async () => expect((await getHubSnapshot()).devices.find((device) => device.transport === "ble")?.bleParameters?.maxStrengthA).toBe(90));
    });
});
