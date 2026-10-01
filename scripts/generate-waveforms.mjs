import { readFile, writeFile } from "node:fs/promises";
import { COYOTE_WAVEFORM, COYOTE_WAVEFORMS } from "dglab-kit";

const destination = new URL("../shared/official-waveforms.json", import.meta.url);
const waveforms = Object.values(COYOTE_WAVEFORM).map((presetId) => {
    const preset = COYOTE_WAVEFORMS[presetId];
    return {
        presetId,
        presetName: preset.label.cn,
        englishName: preset.label.en,
        frames: [...preset.raw],
        durationMs: preset.raw.length * 100,
    };
});
const expected = `${JSON.stringify(waveforms, null, 4)}\n`;
if (process.argv.includes("--check")) {
    const actual = await readFile(destination, "utf8");
    if (actual.replaceAll("\r\n", "\n") !== expected) {
        console.error("内置波形目录与 dglab-kit 不一致，请运行 npm run generate:waveforms");
        process.exitCode = 1;
    } else {
        console.log(`内置波形目录一致：${waveforms.length} 项`);
    }
} else {
    await writeFile(destination, expected);
    console.log(`已生成 ${waveforms.length} 项内置波形；来源见 THIRD_PARTY_NOTICES.md`);
}
