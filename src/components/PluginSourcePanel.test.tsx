// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import { __resetMockBridge, getHubSnapshot } from "../lib/bridge";
import type { SourceSnapshot, UiDocument } from "../lib/contracts";
import * as plugins from "../lib/plugins";
import { PluginSourcePanel } from "./PluginSourcePanel";

beforeEach(() => {
    __resetMockBridge();
    vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({ left: 0, top: 0, right: 400, bottom: 200, width: 400, height: 200, x: 0, y: 0, toJSON() {} });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const source = (pluginId: string): SourceSnapshot => ({ id: `instance-${pluginId}`, pluginId, kind: "plugin", name: "插件实例", enabled: true, assignedChannelCount: 0, selectedPresetId: null, selectedPresetName: null, runtimeStatus: "running", config: { gain: 2, hidden: { preserve: true } }, state: {} });
const doc = (): UiDocument => ({ title: "插件设置", nodes: [{ id: "config", type: "form", action: "configure", label: "插件设置", children: [{ id: "gain", type: "number_field", configKey: "gain", label: "增益参数", value: 2 }] }] });

describe("插件管理与公开语义界面", () => {
    it("本地安装、创建多个实例、更新与卸载关联实例", async () => {
        const user = userEvent.setup(); render(<App />);
        await user.click(await screen.findByRole("button", { name: "输入源" }));
        await user.click(screen.getByRole("button", { name: "安装本地插件" }));
        await waitFor(async () => expect((await getHubSnapshot()).plugins?.some((item) => item.manifest.id === "example.sample")).toBe(true));
        for (const name of ["外部输入一", "外部输入二"]) {
            await user.click(screen.getByRole("button", { name: "创建输入源" }));
            await user.selectOptions(screen.getByRole("combobox", { name: "输入源插件" }), "example.sample");
            await user.type(screen.getByRole("textbox", { name: "输入源实例名称" }), name);
            await user.click(screen.getByRole("button", { name: "创建实例" }));
            await screen.findByRole("heading", { name });
        }
        await user.click(screen.getByRole("tab", { name: /插件管理/ }));
        await user.click(screen.getByRole("button", { name: "从本地包更新" }));
        await waitFor(async () => expect((await getHubSnapshot()).plugins?.find((item) => item.manifest.id === "example.sample")?.manifest.version).toBe("0.2.0"));
        expect((await getHubSnapshot()).sources.filter((item) => item.pluginId === "example.sample")).toHaveLength(2);
        await user.click(screen.getByRole("button", { name: "卸载 示例插件" }));
        expect((screen.getByRole("checkbox", { name: "同时清除配置和数据" }) as HTMLInputElement).checked).toBe(false);
        await user.click(screen.getByRole("button", { name: "卸载插件" }));
        await waitFor(async () => expect((await getHubSnapshot()).sources.filter((item) => item.pluginId === "example.sample")).toHaveLength(0));
    });

    it.each(["cn.dglab.link.touch", "thirdparty.custom"])("%s 使用相同表单与配置事务，保存隐藏字段", async (pluginId) => {
        vi.spyOn(plugins, "getSourceUi").mockResolvedValue(doc());
        const save = vi.spyOn(plugins, "setSourceConfig").mockResolvedValue({});
        const user = userEvent.setup(); const instance = source(pluginId);
        render(<PluginSourcePanel source={instance} />);
        const field = await screen.findByRole("spinbutton", { name: "增益参数" });
        fireEvent.change(field, { target: { value: "4" } });
        await user.click(screen.getByRole("button", { name: "应用配置" }));
        await waitFor(() => expect(save).toHaveBeenCalledWith(instance.id, { gain: 4, hidden: { preserve: true } }, undefined));
    });

    it("后台状态更新不覆盖未提交表单", async () => {
        const ui = vi.spyOn(plugins, "getSourceUi").mockResolvedValue(doc());
        const instance = source("thirdparty.custom");
        const { rerender } = render(<PluginSourcePanel source={instance} />);
        const field = await screen.findByRole("spinbutton", { name: "增益参数" });
        fireEvent.change(field, { target: { value: "7" } });
        const next = doc(); next.nodes[0].children![0].value = 3; ui.mockResolvedValue(next);
        rerender(<PluginSourcePanel source={{ ...instance, state: { tick: 1 } }} />);
        await waitFor(() => expect(ui).toHaveBeenCalledTimes(3));
        expect((field as HTMLInputElement).value).toBe("7");
    });

    it("第三方触控控件包含绑定、所有者和递增序号，失焦释放触点", async () => {
        vi.spyOn(plugins, "getSourceUi").mockResolvedValue({ title: "外部触控", nodes: [{ id: "pad", type: "xy_pad", label: "第三方触控", input: "pointer" }] });
        const input = vi.spyOn(plugins, "sourceInput").mockResolvedValue({});
        render(<PluginSourcePanel source={source("thirdparty.custom")} surface="control" bindingId="opaque-binding" />);
        const board = await screen.findByLabelText("触控区域");
        fireEvent.pointerDown(board, { pointerId: 9, button: 0, clientX: 200, clientY: 100 });
        await waitFor(() => expect(input.mock.calls.at(-1)?.[1].value).toEqual({ pointers: [{ id: 9, x: 0.5, y: 0.5, cell: null }] }));
        const first = input.mock.calls.at(-1)![1];
        fireEvent.blur(window);
        await waitFor(() => expect(input.mock.calls.at(-1)?.[1].value).toEqual({ pointers: [] }));
        const last = input.mock.calls.at(-1)![1];
        expect(last.bindingId).toBe("opaque-binding"); expect(last.owner).toBe(first.owner); expect(last.sequence).toBeGreaterThan(first.sequence);
    });
    it("显式停止插件后状态刷新不会重新懒启动", async () => {
        const ui = vi.spyOn(plugins, "getSourceUi").mockResolvedValue(doc());
        const instance = source("thirdparty.custom");
        const { rerender } = render(<PluginSourcePanel source={instance} />);
        await screen.findByRole("spinbutton", { name: "增益参数" });
        await waitFor(() => expect(ui).toHaveBeenCalledTimes(2));
        rerender(<PluginSourcePanel source={{ ...instance, runtimeStatus: "stopped", state: { stopped: true } }} />);
        await screen.findByText("插件进程已停止");
        expect(ui).toHaveBeenCalledTimes(2);
        expect(screen.queryByRole("spinbutton")).toBeNull();
    });

});
