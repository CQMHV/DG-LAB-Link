import { afterEach, describe, expect, it, vi } from "vitest";
import * as bridge from "./bridge";
import { getSourceUi, invalidateSourceUi } from "./plugins";

afterEach(() => vi.restoreAllMocks());

describe("共享插件界面读取", () => {
    it("相同面板合并请求，不同绑定在同实例串行读取", async () => {
        let finish: (() => void) | undefined;
        const call = vi.spyOn(bridge, "pluginDemoCall").mockImplementationOnce(() => new Promise((resolve) => { finish = () => resolve({ title: "A", nodes: [] }); })).mockResolvedValue({ title: "B", nodes: [] });
        const a = getSourceUi("ui-shared", "a", "control");
        const same = getSourceUi("ui-shared", "a", "control");
        const b = getSourceUi("ui-shared", "b", "control");
        expect(a).toBe(same);
        await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(1));
        finish!();
        expect((await a).title).toBe("A");
        expect((await b).title).toBe("B");
        expect(call).toHaveBeenCalledTimes(2);
        invalidateSourceUi("ui-shared");
    });

    it("有界读取队列并且停止后废弃未发送请求", async () => {
        let finish: (() => void) | undefined;
        const call = vi.spyOn(bridge, "pluginDemoCall").mockImplementation(() => new Promise((resolve) => { finish = () => resolve({ title: "active", nodes: [] }); }));
        const first = getSourceUi("ui-capacity", "0", "control");
        const queued = Array.from({ length: 7 }, (_, index) => getSourceUi("ui-capacity", String(index + 1), "control").catch((error) => error.code));
        await expect(getSourceUi("ui-capacity", "overflow", "control")).rejects.toMatchObject({ code: "queue_busy" });
        await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(1));
        invalidateSourceUi("ui-capacity");
        finish!();
        await first;
        expect(await Promise.all(queued)).toEqual(Array(7).fill("request_cancelled"));
        expect(call).toHaveBeenCalledTimes(1);
    });
});
