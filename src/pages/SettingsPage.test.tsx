// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getHubSnapshot, getMcpConfig, getRuntimeInfo } from "../lib/bridge";
import { SettingsPage } from "./SettingsPage";

vi.mock("../lib/bridge", async (importOriginal) => ({
    ...await importOriginal<typeof import("../lib/bridge")>(),
    getRuntimeInfo: vi.fn(),
    getMcpConfig: vi.fn(),
}));

const renderSettings = async () => render(
    <SettingsPage
        appPreferences={{ autoStart: false, closeToTray: true, startMinimized: true }}
        snapshot={await getHubSnapshot()}
        pendingAction={null}
        onSetAutoStart={vi.fn()}
        onSetCloseToTray={vi.fn()}
        onSetDefaultSource={vi.fn()}
        onSetStartMinimized={vi.fn()}
        onSaveSafety={vi.fn()}
    />,
);

beforeEach(() => {
    vi.mocked(getRuntimeInfo).mockResolvedValue({
        instanceId: "test-core", pid: 123, holderCount: 3,
        mcpUrl: "http://127.0.0.1:17846/mcp",
    });
    vi.mocked(getMcpConfig).mockResolvedValue({
        url: "http://127.0.0.1:17846/mcp", token: "private-local-token",
    });
});

afterEach(() => {
    cleanup();
    vi.clearAllMocks();
});

describe("共享核心设置", () => {
    it("显示持有者与 MCP 地址，只在点击复制时读取令牌", async () => {
        const user = userEvent.setup();
        await renderSettings();

        expect(await screen.findByText("3 个")).toBeTruthy();
        expect(screen.getByText("http://127.0.0.1:17846/mcp")).toBeTruthy();
        expect(getMcpConfig).not.toHaveBeenCalled();
        expect(screen.queryByText("private-local-token")).toBeNull();

        await user.click(screen.getByRole("button", { name: "复制连接令牌" }));
        expect(await screen.findByText("连接令牌已复制")).toBeTruthy();
        expect(await navigator.clipboard.readText()).toBe("private-local-token");
        expect(getMcpConfig).toHaveBeenCalledOnce();
        expect(screen.queryByText("private-local-token")).toBeNull();
    });

    it("浏览器演示不提供核心令牌", async () => {
        vi.mocked(getRuntimeInfo).mockResolvedValue(null);
        await renderSettings();

        expect(await screen.findByText("浏览器演示")).toBeTruthy();
        const button = screen.getByRole("button", { name: "复制连接令牌" }) as HTMLButtonElement;
        expect(button.disabled).toBe(true);
        expect(getMcpConfig).not.toHaveBeenCalled();
    });

    it("核心断开后显示错误并禁用令牌复制", async () => {
        vi.mocked(getRuntimeInfo).mockRejectedValue({ code: "core_disconnected", message: "共享核心已断开" });
        await renderSettings();

        expect((await screen.findByRole("alert")).textContent).toContain("共享核心已断开");
        expect(screen.getByText("核心已断开")).toBeTruthy();
        expect((screen.getByRole("button", { name: "复制连接令牌" }) as HTMLButtonElement).disabled).toBe(true);
    });
});
