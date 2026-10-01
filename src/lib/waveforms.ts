import { parsePulseText } from "@dg-kit/waveforms";
import { invoke } from "@tauri-apps/api/core";
import officialWaveforms from "../../shared/official-waveforms.json";

import { isTauriRuntime } from "./tauri";
import type { WaveformConfig } from "./contracts";

export interface OfficialWaveform extends WaveformConfig {
    englishName: string;
    durationMs: number;
}

export const OFFICIAL_WAVEFORMS: OfficialWaveform[] = officialWaveforms;

export const DEFAULT_WAVEFORM_ID = "BREATHING";

export const findOfficialWaveform = (presetId: string | null): OfficialWaveform =>
    OFFICIAL_WAVEFORMS.find((waveform) => waveform.presetId === presetId) ??
    OFFICIAL_WAVEFORMS.find(
        (waveform) => waveform.presetId === DEFAULT_WAVEFORM_ID,
    )!;

const MAX_IMPORT_FILE_BYTES = 2 * 1024 * 1024;
const V3_FRAME_PATTERN = /^[0-9a-f]{16}$/i;
let fallbackIdCounter = 0;

interface ImportedWaveformData {
    name: string;
    frames: string[];
}

export const parseWaveformFiles = async (
    files: readonly File[],
): Promise<WaveformConfig[]> => {
    if (isTauriRuntime()) {
        const payload = await Promise.all(files.map(async (file) => {
            if (file.size > MAX_IMPORT_FILE_BYTES) {
                throw new Error(`${file.name} 超过 2 MB，无法导入`);
            }
            return { name: file.name, content: await file.text() };
        }));
        return invoke<WaveformConfig[]>("parse_waveform_files", { files: payload });
    }
    const parsed = await Promise.all(files.map(parseWaveformFile));
    return parsed.flat();
};

const parseWaveformFile = async (file: File): Promise<WaveformConfig[]> => {
    if (file.size > MAX_IMPORT_FILE_BYTES) {
        throw new Error(`${file.name} 超过 2 MB，无法导入`);
    }
    const text = await file.text();
    const extension = file.name.split(".").pop()?.toLowerCase();
    const imported = extension === "pulse"
        ? [parsePulseFile(file.name, text)]
        : parseJsonFile(file.name, text);
    return imported.map(({ name, frames }) => ({
        presetId: createCustomWaveformId(),
        presetName: validateName(name, file.name),
        frames: validateFrames(frames, file.name),
    }));
};

const parsePulseFile = (fileName: string, text: string): ImportedWaveformData => {
    const parsed = parsePulseText(text);
    const samples = parsed.frames;
    const frames: string[] = [];
    for (let offset = 0; offset < samples.length; offset += 4) {
        const group = samples.slice(offset, offset + 4);
        const fallback = group.at(-1)!;
        while (group.length < 4) {
            group.push(fallback);
        }
        frames.push(
            group.map(([frequency]) => toHexByte(frequency)).join("") +
                group.map(([, intensity]) => toHexByte(intensity)).join(""),
        );
    }
    return {
        name: parsed.name || fileName.replace(/\.pulse$/i, ""),
        frames,
    };
};

const parseJsonFile = (fileName: string, text: string): ImportedWaveformData[] => {
    let value: unknown;
    try {
        value = JSON.parse(text);
    } catch {
        throw new Error(`${fileName} 不是有效的 JSON 波形文件`);
    }
    const fallbackName = fileName.replace(/\.(json|pulses)$/i, "");
    if (isStringArray(value)) {
        return [{ name: fallbackName, frames: value }];
    }
    if (Array.isArray(value)) {
        return value.map((entry, index) =>
            parseJsonWaveform(entry, `${fallbackName} ${index + 1}`, fileName),
        );
    }
    return [parseJsonWaveform(value, fallbackName, fileName)];
};

const parseJsonWaveform = (
    value: unknown,
    fallbackName: string,
    fileName: string,
): ImportedWaveformData => {
    if (!value || typeof value !== "object") {
        throw new Error(`${fileName} 中的波形必须是对象或十六进制帧数组`);
    }
    const record = value as Record<string, unknown>;
    const frames = record.frames ?? record.pulseData;
    if (!isStringArray(frames)) {
        throw new Error(`${fileName} 中的波形缺少 frames 或 pulseData 数组`);
    }
    const name = [record.name, record.presetName].find(
        (candidate): candidate is string => typeof candidate === "string",
    );
    return { name: name?.trim() || fallbackName, frames };
};

const validateName = (name: string, fileName: string): string => {
    const trimmed = name.trim();
    if (!trimmed || new TextEncoder().encode(trimmed).length > 64) {
        throw new Error(`${fileName} 中的波形名称必须为 1–64 字节`);
    }
    return trimmed;
};

const validateFrames = (frames: string[], fileName: string): string[] => {
    if (frames.length === 0 || frames.length > 16_384) {
        throw new Error(`${fileName} 中的波形必须包含 1–16384 帧`);
    }
    return frames.map((frame, index) => {
        const normalized = frame.trim().toUpperCase();
        if (!V3_FRAME_PATTERN.test(normalized)) {
            throw new Error(`${fileName} 的第 ${index + 1} 帧不是 16 位十六进制数据`);
        }
        const bytes = normalized.match(/../g)!.map((byte) => Number.parseInt(byte, 16));
        if (bytes.slice(0, 4).some((frequency) => frequency < 10 || frequency > 240)) {
            throw new Error(`${fileName} 的第 ${index + 1} 帧频率必须在 10–240`);
        }
        if (bytes.slice(4).some((intensity) => intensity > 100)) {
            throw new Error(`${fileName} 的第 ${index + 1} 帧强度必须在 0–100`);
        }
        return normalized;
    });
};

const createCustomWaveformId = (): string => {
    if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
        return `custom-${crypto.randomUUID()}`;
    }
    fallbackIdCounter += 1;
    return `custom-${Date.now().toString(36)}-${fallbackIdCounter.toString(36)}`;
};

const isStringArray = (value: unknown): value is string[] =>
    Array.isArray(value) && value.every((entry) => typeof entry === "string");

const toHexByte = (value: number): string =>
    Math.round(value).toString(16).padStart(2, "0").toUpperCase();
