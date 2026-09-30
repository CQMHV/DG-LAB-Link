import { HandTap } from "@phosphor-icons/react";
import { useCallback, useEffect, useRef, useState, type PointerEvent } from "react";
import { updateTouchInput } from "../lib/bridge";
import type { TouchConfig, TouchInput } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";

interface TouchBoardProps {
    config: TouchConfig;
    deviceId: string;
    disabled: boolean;
    onInput?: (input: TouchInput) => Promise<void>;
}

const routeNames: Record<TouchConfig["routing"], string> = {
    a: "A 单通道", b: "B 单通道", sync: "AB 同步", separate: "AB 双指", alternate: "AB 轮替",
};
const clamp = (value: number) => Math.max(0, Math.min(1, value));

export const TouchBoard = ({ config, deviceId, disabled, onInput = updateTouchInput }: TouchBoardProps) => {
    const board = useRef<HTMLDivElement>(null);
    const pad = useRef<HTMLDivElement>(null);
    const pointers = useRef(new Map<number, TouchInput["pointers"][number]>());
    const ownerId = useRef(`touch-${globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`}`);
    const sequence = useRef(0);
    const sending = useRef(false);
    const latest = useRef<TouchInput | null>(null);
    const critical = useRef<TouchInput[]>([]);
    const alive = useRef(true);
    const disabledRef = useRef(disabled);
    disabledRef.current = disabled;
    const [visiblePointers, setVisiblePointers] = useState<TouchInput["pointers"]>([]);
    const [error, setError] = useState<string | null>(null);
    const inputCallback = useRef(onInput);
    inputCallback.current = onInput;

    // Preserve press/release/cell transitions; coalesce only moves and leases.
    const flush = useCallback(async () => {
        if (sending.current || (!latest.current && critical.current.length === 0)) return;
        sending.current = true;
        while (latest.current || critical.current.length > 0) {
            const input = critical.current.shift() ?? latest.current!;
            if (input === latest.current) latest.current = null;
            try { await inputCallback.current(input); }
            catch (failure) {
                if (alive.current && !disabledRef.current && input.pointers.length > 0) setError(getErrorMessage(failure));
                pointers.current.clear();
                if (alive.current) setVisiblePointers([]);
                latest.current = null;
                critical.current = [];
            }
        }
        sending.current = false;
    }, []);
    const publish = useCallback((isCritical = false, immediate = false) => {
        const input = { deviceId, ownerId: ownerId.current, sequence: ++sequence.current, pointers: [...pointers.current.values()] };
        if (isCritical) {
            latest.current = null;
            if (critical.current.length >= 8) {
                // Overflow withdraws intent instead of discarding a release edge.
                pointers.current.clear();
                input.pointers = [];
                critical.current = [];
                if (alive.current) {
                    setVisiblePointers([]);
                    setError("触控事件过快，已释放触点，请重新按下。");
                }
            }
            critical.current.push(input);
        } else {
            latest.current = input;
        }
        if (isCritical || immediate) void flush();
    }, [deviceId, flush]);
    const cancel = useCallback(() => {
        pointers.current.clear();
        setVisiblePointers([]);
        publish(true);
    }, [publish]);
    useEffect(() => {
        alive.current = true;
        const coalesce = window.setInterval(() => { void flush(); }, 40);
        const heartbeat = window.setInterval(() => {
            if (pointers.current.size > 0) publish(false, true);
        }, 200);
        const visibility = () => { if (document.hidden) cancel(); };
        window.addEventListener("blur", cancel);
        document.addEventListener("visibilitychange", visibility);
        return () => {
            alive.current = false;
            pointers.current.clear();
            publish(true);
            window.clearInterval(coalesce);
            window.clearInterval(heartbeat);
            window.removeEventListener("blur", cancel);
            document.removeEventListener("visibilitychange", visibility);
        };
    }, [cancel, flush, publish]);
    const configIdentity = JSON.stringify(config);
    useEffect(() => { cancel(); }, [disabled, configIdentity, cancel]);
    const location = (event: PointerEvent<HTMLDivElement>) => {
        const freeRect = pad.current?.getBoundingClientRect();
        const boardRect = board.current!.getBoundingClientRect();
        const rect = config.mode === "free" && freeRect ? freeRect : boardRect;
        let cell: number | null = null;
        const cells = board.current!.querySelectorAll<HTMLElement>("[data-touch-cell]");
        cells.forEach((item) => {
            const bounds = item.getBoundingClientRect();
            if (event.clientX >= bounds.left && event.clientX < bounds.right && event.clientY >= bounds.top && event.clientY < bounds.bottom) cell = Number(item.dataset.touchCell);
        });
        return {
            id: event.pointerId,
            x: clamp((event.clientX - rect.left) / Math.max(1, rect.width)),
            y: clamp((event.clientY - rect.top) / Math.max(1, rect.height)),
            cell,
        };
    };
    const move = (event: PointerEvent<HTMLDivElement>) => {
        if (disabled || !pointers.current.has(event.pointerId)) return;
        const previous = pointers.current.get(event.pointerId)!;
        const current = location(event);
        pointers.current.set(event.pointerId, current);
        setVisiblePointers([...pointers.current.values()]);
        publish(previous.cell !== current.cell);
    };
    const down = (event: PointerEvent<HTMLDivElement>) => {
        if (disabled || event.button !== 0 || pointers.current.size >= (config.routing === "separate" ? 2 : 1)) return;
        event.preventDefault();
        setError(null);
        event.currentTarget.setPointerCapture?.(event.pointerId);
        pointers.current.set(event.pointerId, location(event));
        setVisiblePointers([...pointers.current.values()]);
        publish(true);
    };
    const up = (event: PointerEvent<HTMLDivElement>) => {
        if (!pointers.current.delete(event.pointerId)) return;
        setVisiblePointers([...pointers.current.values()]);
        publish(true);
    };
    const waveforms = config.mode === "free" ? config.freeWaveforms : config.rhythmWaveforms.slice(0, config.gridSize ** 2);
    return (
        <section aria-label="设备触控面板" className="touch-console">
            <header className="input-mode-heading"><HandTap aria-hidden="true" size={21} /><h3>{config.mode === "free" ? "自由触控" : "律动触控"}</h3><span>{routeNames[config.routing]}</span></header>
            <p className="input-mode-note">{disabled ? "先开始此设备的输出，再按住面板控制。" : "按住持续输出，滑动切换区域；松手结束触控。"} {config.background ? `背景：${config.background.presetName}` : "无背景波形"}</p>
            <div aria-label="触控区域" aria-disabled={disabled} className={`touch-board ${disabled ? "is-disabled" : ""}`} onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onLostPointerCapture={up} ref={board}>
                {config.mode === "free" && <div className="touch-free-pad" ref={pad}>
                    <span className="touch-axis-x">{config.swapAxes ? "周期 ms" : "相对强度"} →</span>
                    <span className="touch-axis-y">{config.swapAxes ? "相对强度" : "周期 ms"} ↓</span>
                    <HandTap aria-hidden="true" size={32} weight="light" />
                    {visiblePointers.filter((pointer) => pointer.cell === null).map((pointer, index) => <span aria-hidden="true" className="touch-pointer" key={pointer.id} style={{ left: `${pointer.x * 100}%`, top: `${pointer.y * 100}%` }}>{index === 0 ? "A" : "B"}</span>)}
                </div>}
                <div className="touch-live-grid" style={{ gridTemplateColumns: `repeat(${config.mode === "free" ? 4 : config.gridSize}, minmax(0, 1fr))` }}>
                    {waveforms.map((waveform, index) => <div className={`touch-live-cell ${visiblePointers.some((pointer) => pointer.cell === index) ? "is-held" : ""}`} data-touch-cell={index} key={index}><small>{String(index + 1).padStart(2, "0")}</small><strong>{waveform.presetName}</strong></div>)}
                </div>
            </div>
            {error && <p className="input-mode-error" role="alert">{error}</p>}
        </section>
    );
};
