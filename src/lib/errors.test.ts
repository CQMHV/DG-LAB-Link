import { describe, expect, it } from "vitest";

import { getErrorMessage } from "./errors";

describe("getErrorMessage", () => {
    it("解析 Tauri 序列化的命令错误", () => {
        expect(
            getErrorMessage({
                code: "not_connected",
                message: "尚未连接 DG-LAB Relay",
            }),
        ).toBe("尚未连接 DG-LAB Relay（not_connected）");
    });

    it("对未知拒绝值使用指定回退文案", () => {
        expect(getErrorMessage({ unexpected: true }, "紧急停止失败")).toBe(
            "紧急停止失败",
        );
    });
});
