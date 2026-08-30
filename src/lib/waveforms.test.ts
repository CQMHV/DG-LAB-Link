import { describe, expect, it } from "vitest";

import { parseWaveformFiles } from "./waveforms";

const textFile = (name: string, content: string): File =>
    ({
        name,
        size: new TextEncoder().encode(content).length,
        text: async () => content,
    }) as File;

describe("自定义波形导入", () => {
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
