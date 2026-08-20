import { useEffect, useRef, useState } from "react";

import { Minus, Plus, Pulse } from "@phosphor-icons/react";
import {
    buildStyles,
    CircularProgressbarWithChildren,
} from "react-circular-progressbar";
import "react-circular-progressbar/dist/styles.css";

import type { ChannelSnapshot, HubChannel } from "../lib/contracts";

interface ChannelControlProps {
    channel: HubChannel;
    snapshot: ChannelSnapshot;
    disabled: boolean;
    pending: boolean;
    onAdjust: (channel: HubChannel, delta: number) => void;
}

const statusLabel = (status: ChannelSnapshot["status"]): string => {
    if (status === "active") {
        return "输出中";
    }
    if (status === "ready") {
        return "已就绪";
    }
    if (status === "fault") {
        return "通道异常";
    }
    if (status === "disabled") {
        return "被控端关闭";
    }
    if (status === "disconnected") {
        return "未连接";
    }
    return "待机";
};

export const ChannelControl = ({
    channel,
    snapshot,
    disabled,
    pending,
    onAdjust,
}: ChannelControlProps) => {
    const name = channel.toUpperCase();
    const [draftIntensity, setDraftIntensity] = useState(snapshot.intensity);
    const [numberValue, setNumberValue] = useState(String(snapshot.intensity));
    const draggingGauge = useRef(false);
    const percentage = snapshot.limit
        ? (draftIntensity / snapshot.limit) * 100
        : 0;

    useEffect(() => {
        if (!pending) {
            const intensity = Math.min(snapshot.limit, snapshot.intensity);
            setDraftIntensity(intensity);
            setNumberValue(String(intensity));
        }
    }, [pending, snapshot.intensity, snapshot.limit]);

    const clampIntensity = (value: number): number =>
        Math.min(snapshot.limit, Math.max(0, Math.round(value)));

    const previewIntensity = (value: number) => {
        const nextIntensity = clampIntensity(value);
        setDraftIntensity(nextIntensity);
        setNumberValue(String(nextIntensity));
    };

    const commitIntensity = (value: number) => {
        if (!Number.isFinite(value)) {
            setNumberValue(String(draftIntensity));
            return;
        }

        const nextIntensity = clampIntensity(value);
        setDraftIntensity(nextIntensity);
        setNumberValue(String(nextIntensity));
        const delta = nextIntensity - snapshot.intensity;
        if (!disabled && !pending && delta !== 0) {
            onAdjust(channel, delta);
        }
    };

    const intensityFromPointer = (
        element: HTMLElement,
        clientX: number,
        clientY: number,
    ): number => {
        const bounds = element.getBoundingClientRect();
        const centerX = bounds.left + bounds.width / 2;
        const centerY = bounds.top + bounds.height / 2;
        const angle = Math.atan2(clientY - centerY, clientX - centerX);
        const clockwiseFromTop = (angle + Math.PI / 2 + Math.PI * 2) % (Math.PI * 2);
        return (clockwiseFromTop / (Math.PI * 2)) * snapshot.limit;
    };

    const pointerIsOnGaugeTrack = (
        element: HTMLElement,
        clientX: number,
        clientY: number,
    ): boolean => {
        const progressRing = element.querySelector<SVGElement>(
            ".CircularProgressbar",
        );
        const progressBounds = progressRing?.getBoundingClientRect();
        const bounds =
            progressBounds && progressBounds.width > 0 && progressBounds.height > 0
                ? progressBounds
                : element.getBoundingClientRect();
        const centerX = bounds.left + bounds.width / 2;
        const centerY = bounds.top + bounds.height / 2;
        const size = Math.min(bounds.width, bounds.height);
        const halfStroke = size * 0.0175;
        const trackRadius = size / 2 - halfStroke;
        const pointerRadius = Math.hypot(clientX - centerX, clientY - centerY);
        const hitTolerance = halfStroke + 2;
        return Math.abs(pointerRadius - trackRadius) <= hitTolerance;
    };

    return (
        <section className="channel-control" aria-label={`${name} 通道控制`}>
            <div className="channel-title">
                <strong>{name}</strong>
                <span>通道</span>
                <span className={`channel-state state-${snapshot.status}`}>
                    {statusLabel(snapshot.status)}
                </span>
            </div>

            <div
                aria-disabled={disabled || pending}
                aria-label={`拖动设置 ${name} 通道强度`}
                className="gauge-shell gauge-interactive"
                data-testid={`channel-${channel}-gauge`}
                onPointerCancel={() => {
                    draggingGauge.current = false;
                    previewIntensity(snapshot.intensity);
                }}
                onPointerDown={(event) => {
                    if (
                        disabled ||
                        pending ||
                        !pointerIsOnGaugeTrack(
                            event.currentTarget,
                            event.clientX,
                            event.clientY,
                        )
                    ) {
                        return;
                    }
                    draggingGauge.current = true;
                    event.currentTarget.setPointerCapture?.(event.pointerId);
                    previewIntensity(
                        intensityFromPointer(
                            event.currentTarget,
                            event.clientX,
                            event.clientY,
                        ),
                    );
                }}
                onPointerMove={(event) => {
                    if (draggingGauge.current) {
                        previewIntensity(
                            intensityFromPointer(
                                event.currentTarget,
                                event.clientX,
                                event.clientY,
                            ),
                        );
                    }
                }}
                onPointerUp={(event) => {
                    if (!draggingGauge.current) {
                        return;
                    }
                    draggingGauge.current = false;
                    commitIntensity(
                        intensityFromPointer(
                            event.currentTarget,
                            event.clientX,
                            event.clientY,
                        ),
                    );
                }}
            >
                <CircularProgressbarWithChildren
                    strokeWidth={3.5}
                    value={Math.min(100, percentage)}
                    styles={buildStyles({
                        pathColor: "#f0cf6a",
                        trailColor: "#34332d",
                        pathTransitionDuration: 0.25,
                        strokeLinecap: "butt",
                    })}
                >
                    <div className="gauge-inner">
                        <Pulse
                            aria-hidden="true"
                            className="gauge-pulse"
                            size={24}
                            weight="light"
                        />
                        <input
                            aria-label={`输入 ${name} 通道强度`}
                            className="gauge-intensity-input"
                            data-testid={`channel-${channel}-intensity`}
                            disabled={disabled || pending}
                            inputMode="numeric"
                            max={snapshot.limit}
                            min={0}
                            onBlur={(event) =>
                                commitIntensity(Number(event.currentTarget.value))
                            }
                            onChange={(event) => {
                                setNumberValue(event.currentTarget.value);
                                const value = Number(event.currentTarget.value);
                                if (
                                    event.currentTarget.value !== "" &&
                                    Number.isFinite(value)
                                ) {
                                    setDraftIntensity(clampIntensity(value));
                                }
                            }}
                            onFocus={(event) => event.currentTarget.select()}
                            onKeyDown={(event) => {
                                if (event.key === "Enter") {
                                    commitIntensity(
                                        Number(event.currentTarget.value),
                                    );
                                }
                            }}
                            onPointerDown={(event) => event.stopPropagation()}
                            step={1}
                            type="number"
                            value={numberValue}
                        />
                        <span>强度</span>
                        <small>/ {snapshot.limit}</small>
                    </div>
                </CircularProgressbarWithChildren>
            </div>

            <div className="channel-adjuster">
                <button
                    aria-label={`降低 ${name} 通道强度`}
                    className="step-button"
                    disabled={disabled || pending || draftIntensity <= 0}
                    onClick={() => commitIntensity(draftIntensity - 1)}
                    type="button"
                >
                    <Minus aria-hidden="true" size={26} weight="light" />
                </button>
                <button
                    aria-label={`提高 ${name} 通道强度`}
                    className="step-button"
                    disabled={
                        disabled || pending || draftIntensity >= snapshot.limit
                    }
                    onClick={() => commitIntensity(draftIntensity + 1)}
                    type="button"
                >
                    <Plus aria-hidden="true" size={27} weight="light" />
                </button>
            </div>
        </section>
    );
};
