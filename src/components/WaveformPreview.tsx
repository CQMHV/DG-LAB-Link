import { Circle, ShieldCheck } from "@phosphor-icons/react";
import { useEffect, useMemo, useState } from "react";
import {
    Line,
    LineChart,
    ResponsiveContainer,
    XAxis,
    YAxis,
} from "recharts";

import type { OutputState } from "../lib/contracts";

interface WaveformPreviewProps {
    outputState: OutputState;
}

const makeWaveform = (phase: number) =>
    Array.from({ length: 150 }, (_, index) => {
        const position = index / 149;
        const envelope =
            0.12 +
            0.85 *
                Math.max(
                    Math.exp(-Math.pow((position - 0.29) * 20, 2)),
                    Math.exp(-Math.pow((position - 0.61) * 23, 2)),
                    Math.exp(-Math.pow((position - 0.81) * 26, 2)),
                );
        const value =
            Math.sin(index * 2.46 + phase) * envelope * 37 +
            Math.sin(index * 4.82 - phase * 0.5) * envelope * 8 +
            Math.sin(index * 0.63 + phase * 0.2) * 3;

        return { index, value };
    });

export const WaveformPreview = ({ outputState }: WaveformPreviewProps) => {
    const [phase, setPhase] = useState(0);

    useEffect(() => {
        const interval = window.setInterval(() => {
            setPhase((current) => current + (outputState === "running" ? 0.7 : 0.14));
        }, 180);
        return () => window.clearInterval(interval);
    }, [outputState]);

    const waveform = useMemo(() => makeWaveform(phase), [phase]);

    return (
        <section className="waveform-panel" aria-label="实时波形预览">
            <div className="waveform-security">
                <ShieldCheck
                    aria-hidden="true"
                    className="security-icon"
                    size={21}
                    weight="fill"
                />
                <span>安全保护已启用</span>
            </div>

            <div className="waveform-chart" data-testid="waveform-chart">
                <ResponsiveContainer height="100%" minHeight={120} width="100%">
                    <LineChart data={waveform} margin={{ top: 10, right: 2, bottom: 10, left: 2 }}>
                        <XAxis dataKey="index" hide />
                        <YAxis domain={[-50, 50]} hide />
                        <Line
                            dataKey="value"
                            dot={false}
                            isAnimationActive={false}
                            stroke="#efcf69"
                            strokeWidth={1.15}
                            type="linear"
                        />
                    </LineChart>
                </ResponsiveContainer>
            </div>

            <div className={`preview-state preview-${outputState}`}>
                <Circle aria-hidden="true" size={10} weight="fill" />
                {outputState === "running" ? "实时输出中" : "实时输出预览"}
            </div>
        </section>
    );
};
