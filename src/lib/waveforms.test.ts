import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import waveformImportFixtures from "../../shared/waveform-import-fixtures.json";

import { parseWaveformFiles } from "./waveforms";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
});

const textFile = (name: string, content: string): File =>
    ({
        name,
        size: new TextEncoder().encode(content).length,
        text: async () => content,
    }) as File;

describe("自定义波形导入", () => {
    it.each(waveformImportFixtures)("浏览器与核心共享 pulse 用例：$name", async (fixture) => {
        const [waveform] = await parseWaveformFiles([textFile(fixture.name, fixture.content)]);
        expect(waveform.presetName).toBe(fixture.presetName);
        expect(waveform.frames).toEqual(fixture.frames);
    });

    it("桌面端把文本交给共享核心解析并使用核心返回的 ID", async () => {
        vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
        const config = { presetId: "custom-core-id", presetName: "核心波形", frames: ["0A0A0A0A00643200"] };
        vi.mocked(invoke).mockResolvedValue([config]);
        const content = JSON.stringify({ name: "核心波形", frames: config.frames });

        expect(await parseWaveformFiles([textFile("核心.json", content)])).toEqual([config]);
        expect(invoke).toHaveBeenCalledWith("parse_waveform_files", {
            files: [{ name: "核心.json", content }],
        });
    });

    it("把 APP pulse 的 25ms 采样合并为 V3 100ms 帧", async () => {
        const [waveform] = await parseWaveformFiles([
            textFile(
                "呼吸.pulse",
                "Dungeonlab+pulse:自定义呼吸=0,0,0,1,1/0-0,100-0,50-0,0-0",
            ),
        ]);

        expect(waveform.presetName).toBe("自定义呼吸");
        expect(waveform.presetId).toMatch(/^custom-/);
        expect(waveform.frames).toEqual(["0A0A0A0A00643200"]);
    });

    it("允许一个 JSON 文件批量导入多个十六进制波形", async () => {
        const waveforms = await parseWaveformFiles([
            textFile(
                "组合.json",
                JSON.stringify([
                    { name: "波形一", frames: ["0A0A0A0A00643200"] },
                    { presetName: "波形二", pulseData: ["2D2D2D2D64646464"] },
                ]),
            ),
        ]);

        expect(waveforms.map((waveform) => waveform.presetName)).toEqual([
            "波形一",
            "波形二",
        ]);
        expect(new Set(waveforms.map((waveform) => waveform.presetId)).size).toBe(2);
    });

    it("在前端拒绝超范围的帧数据", async () => {
        await expect(
            parseWaveformFiles([
                textFile("错误.json", JSON.stringify(["090A0A0A00643200"])),
            ]),
        ).rejects.toThrow("频率必须在 10–240");
    });
});
