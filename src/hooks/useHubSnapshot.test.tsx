// @vitest-environment jsdom

import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HubSnapshot } from "../lib/contracts";
import { getHubSnapshot, listenHubSnapshot, listenRuntimeError } from "../lib/bridge";
import { useHubSnapshot } from "./useHubSnapshot";

vi.mock("../lib/bridge", () => ({
    getHubSnapshot: vi.fn(),
    listenHubSnapshot: vi.fn(),
    listenRuntimeError: vi.fn(),
}));

afterEach(() => {
    cleanup();
    vi.clearAllMocks();
});

describe("共享核心快照", () => {
    it("核心断开后移除旧控制状态，延迟快照不能恢复控制界面", async () => {
        let onSnapshot: ((snapshot: HubSnapshot) => void) | undefined;
        let onError: ((message: string) => void) | undefined;
        const snapshot = { revision: 1 } as HubSnapshot;
        const unlisten = vi.fn();
        const unlistenError = vi.fn();
        vi.mocked(listenHubSnapshot).mockImplementation(async (listener) => {
            onSnapshot = listener;
            return unlisten;
        });
        vi.mocked(listenRuntimeError).mockImplementation(async (listener) => {
            onError = listener;
            return unlistenError;
        });
        vi.mocked(getHubSnapshot).mockResolvedValue(snapshot);

        const hook = renderHook(useHubSnapshot);
        await waitFor(() => expect(hook.result.current.loading).toBe(false));
        expect(hook.result.current.snapshot).toEqual(snapshot);

        act(() => onError!("共享核心已断开"));
        expect(hook.result.current.snapshot).toBeNull();
        expect(hook.result.current.error).toBe("共享核心已断开");

        act(() => onSnapshot!({ ...snapshot, revision: 2 }));
        await act(() => hook.result.current.refresh());
        expect(hook.result.current.snapshot).toBeNull();
        expect(hook.result.current.error).toBe("共享核心已断开");

        hook.unmount();
        expect(unlisten).toHaveBeenCalledOnce();
        expect(unlistenError).toHaveBeenCalledOnce();
    });
});
