import type { AudioChannelConfig, MappingPoint, TouchConfig, WaveformConfig } from "./contracts";
import { OFFICIAL_WAVEFORMS } from "./waveforms";

const curve = (values: number[]): MappingPoint[] =>
    [0, 0.33, 0.66, 1].map((x, index) => ({ x, y: values[index] }));

export const waveformConfig = (waveform: WaveformConfig): WaveformConfig => ({
    presetId: waveform.presetId,
    presetName: waveform.presetName,
    frames: [...waveform.frames],
});

export const defaultTouchConfig = (): TouchConfig => ({
    mode: "free",
    routing: "sync",
    gridSize: 4,
    swapAxes: false,
    intensityMode: "classic",
    gradientDirection: "both",
    intensityCurve: curve([0, 100, 100, 0]),
    periodCurve: curve([100, 10, 10, 100]),
    freeWaveforms: Array.from({ length: 8 }, (_, index) => waveformConfig(OFFICIAL_WAVEFORMS[index % OFFICIAL_WAVEFORMS.length])),
    rhythmWaveforms: Array.from({ length: 16 }, (_, index) => waveformConfig(OFFICIAL_WAVEFORMS[index % OFFICIAL_WAVEFORMS.length])),
    background: null,
});

export const defaultAudioConfig = (): AudioChannelConfig => ({
    enabled: true,
    inputChannel: "mix",
    gain: 2.5,
    volumeLower: 0.05,
    volumeUpper: 1,
    adaptive: true,
    adaptiveLower: 0.1,
    adaptiveUpper: 0.1,
    hysteresisMs: 300,
    frequencyMin: 100,
    frequencyMax: 1000,
    periodCurve: curve([10, 100, 10, 100]),
});

export const isValidCurve = (points: MappingPoint[], min: number, max: number): boolean =>
    points.length >= 2 && points.length <= 6 && points[0].x === 0 && points.at(-1)?.x === 1 &&
    points.every((point, index) => Number.isFinite(point.x) && Number.isFinite(point.y) &&
        point.y >= min && point.y <= max && (index === 0 || point.x > points[index - 1].x));

export const isValidAudioConfig = (config: AudioChannelConfig): boolean =>
    Object.values(config).every((value) => typeof value !== "number" || Number.isFinite(value)) &&
    config.gain >= 1 && config.gain <= 10 && config.volumeLower >= 0 && config.volumeUpper <= 1 &&
    config.volumeUpper - config.volumeLower >= 0.001 && config.adaptiveLower >= 0 && config.adaptiveLower <= 0.5 &&
    config.adaptiveUpper >= 0 && config.adaptiveUpper <= 0.5 && Number.isInteger(config.hysteresisMs) && config.hysteresisMs >= 0 && config.hysteresisMs <= 2000 &&
    config.frequencyMin >= 50 && config.frequencyMax <= 10000 && config.frequencyMin < config.frequencyMax &&
    isValidCurve(config.periodCurve, 10, 100);
