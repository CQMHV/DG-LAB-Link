import { Component, useEffect, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { FileArrowUp, PuzzlePiece, SpinnerGap } from "@phosphor-icons/react";
import type { AudioSnapshot, HubChannel, MappingPoint, SourceSnapshot, TouchConfig, UiDocument, UiNode, WaveformConfig } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";
import { asObject } from "../lib/json";
import { choosePluginFile, getSourceUi, invalidateSourceUi, pluginCall, setSourceConfig, sourceAction, sourceInput } from "../lib/plugins";
import { AudioPlayer } from "./AudioSourceControls";
import { MappingCurveEditor } from "./MappingCurveEditor";
import { TouchBoard } from "./TouchBoard";
import "./PluginSourcePanel.css";

type Values = Record<string, unknown>;
class PluginRenderBoundary extends Component<{ children: ReactNode; identity: string }, { error: boolean }> {
    state = { error: false };
    static getDerivedStateFromError() { return { error: true }; }
    componentDidUpdate(previous: Readonly<{ children: ReactNode; identity: string }>) {
        if (previous.identity !== this.props.identity && this.state.error) this.setState({ error: false });
    }
    render() { return this.state.error ? <p role="alert" className="input-mode-error">插件界面数据无效，请检查插件或更新版本。</p> : this.props.children; }
}
const object = asObject;
const string = (value: unknown, fallback = "") => typeof value === "string" ? value : fallback;
const numeric = (value: unknown, fallback = 0) => typeof value === "number" && Number.isFinite(value) ? value : fallback;
const fieldValues = (nodes: UiNode[], base: Values): Values => {
    const values = { ...base };
    for (const node of nodes) {
        if (node.configKey && node.value !== undefined && !Object.hasOwn(values, node.configKey)) values[node.configKey] = node.value;
        Object.assign(values, fieldValues(node.children ?? [], values));
    }
    return values;
};
interface RenderContext {
    disabled: boolean;
    bindingId?: string;
    channel?: HubChannel;
    outputDisabled?: boolean;
    onAction: (action: string, value: unknown, expectedRevision?: number) => Promise<void>;
    onInput: (action: string, value: unknown, owner: string, sequence: number) => Promise<void>;
    values?: Values;
    change?: (key: string, value: unknown) => void;
    baseConfig?: Values;
    configRevision: number;
}

const WaveformPicker = ({ node, value, disabled, onChange }: { node: UiNode; value: unknown; disabled: boolean; onChange: (value: unknown) => void }) => {
    const [options, setOptions] = useState<WaveformConfig[]>([]);
    const [error, setError] = useState<string | null>(null);
    useEffect(() => {
        let alive = true;
        void pluginCall<{ official: WaveformConfig[]; custom: WaveformConfig[] }>({ command: "list_waveforms" }).then((result) => { if (alive) setOptions([...result.official, ...result.custom]); }).catch((failure) => { if (alive) setError(getErrorMessage(failure)); });
        return () => { alive = false; };
    }, []);
    const multiple = node.props?.multiple === true;
    const selected = multiple ? (Array.isArray(value) ? value as WaveformConfig[] : []) : [value as WaveformConfig | null];
    const count = multiple ? Math.max(1, Math.min(64, numeric(node.props?.count, selected.length || 1))) : 1;
    return <fieldset className="plugin-waveform-picker" disabled={disabled}><legend>{node.label}</legend>{Array.from({ length: count }, (_, index) => {
        const current = selected[index];
        const available = current && !options.some((item) => item.presetId === current.presetId) ? [current, ...options] : options;
        return <label key={index}>{multiple ? `区域 ${index + 1}` : node.label}<select aria-label={multiple ? `${node.label} ${index + 1}` : node.label} value={current?.presetId ?? ""} onChange={(event) => {
            const next = available.find((item) => item.presetId === event.currentTarget.value) ?? null;
            if (multiple) { if (!next) return; const updated = [...selected]; updated[index] = next; onChange(updated); }
            else onChange(next);
        }}>{(node.props?.nullable === true || !current) && <option value="">无波形</option>}{available.map((item) => <option key={item.presetId} value={item.presetId}>{item.presetName}</option>)}</select></label>;
    })}{error && <p role="alert">{error}</p>}</fieldset>;
};

const PointerPad = ({ node, context }: { node: UiNode; context: RenderContext }) => {
    const pointers = useRef(new Map<number, { id: number; x: number; y: number; cell: number | null }>());
    const owner = useRef(globalThis.crypto?.randomUUID?.() ?? `pointer-${Date.now()}-${Math.random()}`);
    const sequence = useRef(0);
    const sending = useRef(false);
    const latest = useRef<unknown>(null);
    const critical = useRef<unknown[]>([]);
    const inputCallback = useRef(context.onInput);
    inputCallback.current = context.onInput;
    const [visible, setVisible] = useState<{ id: number; x: number; y: number; cell: number | null }[]>([]);
    const action = node.input ?? node.action ?? "input";
    const flush = async () => {
        if (sending.current) return;
        sending.current = true;
        try {
            while (critical.current.length || latest.current !== null) {
                const value = critical.current.shift() ?? latest.current;
                if (value === latest.current) latest.current = null;
                await inputCallback.current(action, value, owner.current, ++sequence.current);
            }
        } catch {
            pointers.current.clear();
            latest.current = null;
            critical.current = [];
            setVisible([]);
        } finally { sending.current = false; }
    };
    const publish = (edge: boolean) => {
        const value = { pointers: [...pointers.current.values()] };
        if (edge) {
            latest.current = null;
            if (critical.current.length >= 8) { pointers.current.clear(); value.pointers = []; critical.current = []; setVisible([]); }
            critical.current.push(value);
        } else latest.current = value;
        void flush();
    };
    const cancel = () => { pointers.current.clear(); setVisible([]); publish(true); };
    useEffect(() => {
        const renew = window.setInterval(() => { if (pointers.current.size) publish(false); }, 200);
        const blur = () => cancel();
        const visibility = () => { if (document.hidden) cancel(); };
        window.addEventListener("blur", blur);
        document.addEventListener("visibilitychange", visibility);
        return () => {
            window.clearInterval(renew);
            window.removeEventListener("blur", blur);
            document.removeEventListener("visibilitychange", visibility);
            cancel();
        };
    }, [action]);
    const disabled = context.disabled || context.outputDisabled;
    useEffect(() => { if (disabled) cancel(); }, [disabled]);
    const point = (event: PointerEvent<HTMLElement>, cell: number | null = null) => {
        const rect = event.currentTarget.getBoundingClientRect();
        return { id: event.pointerId, x: Math.max(0, Math.min(1, (event.clientX - rect.left) / Math.max(1, rect.width))), y: Math.max(0, Math.min(1, (event.clientY - rect.top) / Math.max(1, rect.height))), cell };
    };
    const props = node.props ?? {};
    const cells = Array.isArray(props.cells) ? props.cells : Array.from({ length: Math.min(64, numeric(props.rows, 3) * numeric(props.columns, 3)) }, (_, index) => index + 1);
    const handlers = (cell: number | null) => ({
        onPointerDown: (event: PointerEvent<HTMLDivElement>) => {
            if (disabled || event.button !== 0 || pointers.current.size >= 2) return;
            event.preventDefault();
            event.currentTarget.setPointerCapture?.(event.pointerId);
            pointers.current.set(event.pointerId, point(event, cell)); setVisible([...pointers.current.values()]); publish(true);
        },
        onPointerMove: (event: PointerEvent<HTMLDivElement>) => { if (disabled || !pointers.current.has(event.pointerId)) return; pointers.current.set(event.pointerId, point(event, cell)); setVisible([...pointers.current.values()]); publish(false); },
        onPointerUp: (event: PointerEvent<HTMLDivElement>) => { if (pointers.current.delete(event.pointerId)) { setVisible([...pointers.current.values()]); publish(true); } },
        onPointerCancel: cancel,
        onLostPointerCapture: (event: PointerEvent<HTMLDivElement>) => { if (pointers.current.delete(event.pointerId)) { setVisible([...pointers.current.values()]); publish(true); } },
    });
    return <section className="plugin-pointer-control" aria-label={node.label ?? "触控面板"}>
        <h3>{node.label ?? "触控面板"}</h3>
        {node.type === "grid"
            ? <div className="plugin-grid-control" style={{ gridTemplateColumns: `repeat(${Math.max(1, Math.min(8, numeric(props.columns, 3)))}, 1fr)` }}>{cells.slice(0, 64).map((cell, index) => <div key={index} className={visible.some((p) => p.cell === index) ? "is-held" : ""} aria-disabled={disabled} {...handlers(index)}>{typeof cell === "object" ? string(object(cell).label, String(index + 1)) : String(cell)}</div>)}</div>
            : <div className="plugin-xy-pad" aria-label={string(props.areaLabel, "触控区域")} aria-disabled={disabled} {...handlers(null)}>{visible.map((p) => <span key={p.id} style={{ left: `${p.x * 100}%`, top: `${p.y * 100}%` }} />)}<small>{string(props.xLabel, "X")} → · {string(props.yLabel, "Y")} ↓</small></div>}
    </section>;
};

const SemanticForm = ({ node, context }: { node: UiNode; context: RenderContext }) => {
    const prefix = string(node.props?.configPrefix);
    const base = prefix ? object(context.baseConfig?.[prefix]) : context.baseConfig ?? {};
    const initial = fieldValues(node.children ?? [], { ...base, ...object(node.value) });
    const [values, setValues] = useState(initial);
    const [dirty, setDirty] = useState(false);
    const [draftRevision, setDraftRevision] = useState(context.configRevision);
    const [conflict, setConflict] = useState(false);
    const identity = JSON.stringify(initial);
    useEffect(() => { if (!dirty) { setValues(initial); setDraftRevision(context.configRevision); } }, [identity, dirty, context.configRevision]);
    return <form className="plugin-form" noValidate aria-label={node.label} onSubmit={(event) => { event.preventDefault(); if (context.disabled) return; const payload = prefix ? { ...context.baseConfig, [prefix]: values } : values; void context.onAction(node.action ?? "configure", payload, draftRevision).then(() => { setDirty(false); setConflict(false); }).catch((failure) => { if (object(failure).code === "config_conflict") setConflict(true); }); }}>
        {node.label && <h3>{node.label}</h3>}
        <div className="plugin-form-fields">{(node.children ?? []).map((child) => <SemanticNode key={child.id} node={child} context={{ ...context, values, change: (key, value) => { setDirty(true); setValues((current) => ({ ...current, [key]: value })); } }} />)}</div>
        <button className="primary-compact-button" disabled={context.disabled} type="submit">{string(node.props?.submitLabel, "应用配置")}</button>
        {conflict && <p role="alert">配置已由其他入口修改。<button className="secondary-button" type="button" disabled={context.disabled} onClick={() => { setValues(initial); setDraftRevision(context.configRevision); setDirty(false); setConflict(false); }}>重新载入配置</button></p>}
    </form>;
};

const SemanticNode = ({ node, context }: { node: UiNode; context: RenderContext }) => {
    const props = node.props ?? {};
    const label = node.label ?? node.id;
    const value = node.configKey && context.values && Object.hasOwn(context.values, node.configKey) ? context.values[node.configKey] : node.value;
    const change = (next: unknown) => { if (node.configKey && context.change) context.change(node.configKey, next); else if (node.action) void context.onAction(node.action, next).catch(() => {}); };
    const options = Array.isArray(props.options) ? props.options as { value: string | number; label: string }[] : [];
    const disabled = context.disabled || props.disabled === true;
    switch (node.type) {
        case "form": return <SemanticForm node={node} context={{ ...context, disabled }} />;
        case "page": case "section": case "group": case "stack": case "list": return <section className={`plugin-node plugin-${node.type}`} aria-label={node.label}>{node.label && <h3>{node.label}</h3>}{(node.children ?? []).map((child) => <SemanticNode key={child.id} node={child} context={{ ...context, disabled }} />)}</section>;
        case "text": return <p>{string(value, label)}</p>;
        case "status": return <output className="plugin-status">{label}: {string(value, String(value ?? "未知"))}</output>;
        case "key_value": return <dl className="plugin-key-value">{Object.entries(object(value)).map(([key, item]) => <div key={key}><dt>{key}</dt><dd>{String(item ?? "未知")}</dd></div>)}</dl>;
        case "divider": return <hr />;
        case "button": return <button className="secondary-button" disabled={disabled} type="button" onClick={() => void context.onAction(node.action ?? node.id, value ?? {}).catch(() => {})}>{label}</button>;
        case "switch": return <label className="plugin-switch"><input aria-label={label} checked={Boolean(value)} disabled={disabled} onChange={(event) => change(event.currentTarget.checked)} type="checkbox" />{label}</label>;
        case "text_field": return <label>{label}<input aria-label={label} type="text" value={string(value)} disabled={disabled} maxLength={numeric(props.maxLength, 4096)} onChange={(event) => change(event.currentTarget.value)} /></label>;
        case "integer_field": case "number_field": return <label>{label}<input aria-label={label} type="number" value={numeric(value)} min={typeof props.min === "number" ? props.min : undefined} max={typeof props.max === "number" ? props.max : undefined} step={numeric(props.step, node.type === "integer_field" ? 1 : 0.1)} disabled={disabled} onChange={(event) => { if (Number.isFinite(event.currentTarget.valueAsNumber)) change(event.currentTarget.valueAsNumber); }} /></label>;
        case "select": return <label>{label}<select aria-label={label} disabled={disabled} value={String(value ?? "")} onChange={(event) => { const option = options.find((item) => String(item.value) === event.currentTarget.value); change(option?.value ?? event.currentTarget.value); }}>{options.map((item) => <option key={String(item.value)} value={item.value}>{item.label}</option>)}</select></label>;
        case "waveform_picker": return <WaveformPicker node={node} value={value} disabled={disabled} onChange={change} />;
        case "slider": return <label>{label} <output>{String(value ?? "")}</output><input aria-label={label} disabled={disabled} min={numeric(props.min)} max={numeric(props.max, 100)} step={numeric(props.step, 1)} type="range" value={numeric(value)} onChange={(event) => change(event.currentTarget.valueAsNumber)} /></label>;
        case "progress": return <label>{label}<progress aria-label={label} value={numeric(value)} max={numeric(props.max, 100)} /></label>;
        case "meter": return Array.isArray(value) ? <div className="plugin-meter-array" aria-label={label}>{value.map((item, index) => { const itemLabel = `${label} ${Array.isArray(props.labels) ? String(props.labels[index] ?? index + 1) : index + 1}`; return <label key={index}>{itemLabel}<meter aria-label={itemLabel} value={numeric(item)} min={numeric(props.min)} max={numeric(props.max, 1)} /></label>; })}</div> : <label>{label}<meter aria-label={label} value={numeric(value)} min={numeric(props.min)} max={numeric(props.max, 1)} /></label>;
        case "curve": return <MappingCurveEditor label={label} points={Array.isArray(value) && value.length >= 2 ? value as MappingPoint[] : [{ x: 0, y: numeric(props.min) }, { x: 1, y: numeric(props.max, 100) }]} min={numeric(props.min)} max={numeric(props.max, 100)} unit={string(props.unit)} disabled={disabled} onChange={change} />;
        case "file_field": return <label>{label}<span className="plugin-file-field"><input aria-label={label} value={string(value)} readOnly /><button className="secondary-button" type="button" disabled={disabled} onClick={() => void choosePluginFile().then((path) => { if (path) { if (node.configKey) change(path); else void context.onAction(node.action ?? node.id, { ...object(props.payload), path }).catch(() => {}); } }).catch(() => {})}><FileArrowUp aria-hidden="true" size={17} />选择文件</button></span></label>;
        case "audio_player": return <AudioPlayer audio={value as AudioSnapshot} disabled={disabled} channel={props.channel === "a" || props.channel === "b" ? props.channel as HubChannel : context.channel} modes={Array.isArray(props.modes) ? props.modes.filter((mode): mode is AudioSnapshot["mode"] => ["file", "microphone", "recording", "desktop"].includes(String(mode))) : undefined} description={typeof props.description === "string" ? props.description : undefined} onControl={async (action) => context.onAction(node.action ?? "audio_control", action)} />;
        case "xy_pad": case "grid": return props.touchConfig ? <TouchBoard config={props.touchConfig as TouchConfig} deviceId={context.bindingId ?? node.id} channel={props.channel === "a" || props.channel === "b" ? props.channel as HubChannel : context.channel} disabled={Boolean(disabled || context.outputDisabled)} onInput={(input) => context.onInput(node.input ?? "update_touch_input", { pointers: input.pointers }, input.ownerId, input.sequence)} /> : <PointerPad node={node} context={{ ...context, disabled }} />;
        default: return <p role="alert">不支持的插件控件：{String(node.type)}</p>;
    }
};

export const PluginSourcePanel = ({ source, bindingId, bindingConfig, bindingRevision, channel, surface = "settings", disabled = false }: { source: SourceSnapshot; bindingId?: string; bindingConfig?: unknown; bindingRevision?: number; channel?: HubChannel; surface?: "settings" | "control"; disabled?: boolean }) => {
    const [document, setDocument] = useState<UiDocument | null>(null);
    const [documentScope, setDocumentScope] = useState(`${source.id}/${bindingId ?? "settings"}`);
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState(false);
    const generation = useRef(0);
    const alive = useRef(true);
    const fetching = useRef(false);
    const fetchAgain = useRef(false);
    const refreshCallback = useRef<(explicit?: boolean) => Promise<void>>(async () => {});
    const lifecycle = useRef({ identity: `${source.id}/${bindingId ?? "settings"}`, hasRun: source.runtimeStatus === "running" || source.runtimeStatus === "starting" });
    const identity = `${source.id}/${bindingId ?? "settings"}`;
    if (lifecycle.current.identity !== identity) lifecycle.current = { identity, hasRun: source.runtimeStatus === "running" || source.runtimeStatus === "starting" };
    if (source.runtimeStatus === "running" || source.runtimeStatus === "starting") lifecycle.current.hasRun = true;
    const suspended = !source.enabled || source.runtimeStatus === "faulted" || (lifecycle.current.hasRun && source.runtimeStatus === "stopped");
    const suspendedRef = useRef(suspended);
    suspendedRef.current = suspended;
    const stateIdentity = JSON.stringify([source.state, source.config, source.runtimeStatus, source.revision, bindingConfig, bindingRevision]);
    const refresh = async (explicitStart = false) => {
        if (!alive.current || (suspendedRef.current && !explicitStart)) return;
        if (fetching.current) { fetchAgain.current = true; return; }
        fetching.current = true;
        const current = ++generation.current;
        try { const doc = await getSourceUi(source.id, bindingId, surface, stateIdentity); if (current === generation.current && (!suspendedRef.current || explicitStart)) { setDocumentScope(identity); setDocument(doc); setError(null); } }
        catch (failure) { if (object(failure).code === "request_cancelled") { if (!suspendedRef.current) fetchAgain.current = true; } else if (current === generation.current) setError(getErrorMessage(failure)); }
        finally {
            fetching.current = false;
            if (fetchAgain.current) { fetchAgain.current = false; void refreshCallback.current(); }
        }
    };
    refreshCallback.current = refresh;
    useEffect(() => { alive.current = true; return () => { alive.current = false; generation.current += 1; }; }, []);
    useEffect(() => { if (suspended) { invalidateSourceUi(source.id); generation.current += 1; setError(null); } }, [suspended, source.id]);
    useEffect(() => { setDocument(null); return () => { generation.current += 1; }; }, [source.id, bindingId, surface]);
    useEffect(() => { void refresh(); }, [source.id, bindingId, surface, stateIdentity]);
    const onAction = async (action: string, value: unknown, expectedRevision?: number) => {
        setBusy(true); setError(null);
        try { if (action === "configure") await setSourceConfig(source.id, value, expectedRevision ?? (bindingId ? bindingRevision! : source.revision), bindingId); else await sourceAction(source.id, { action, value, bindingId }); invalidateSourceUi(source.id); await refresh(); }
        catch (failure) { setError(getErrorMessage(failure)); throw failure; }
        finally { setBusy(false); }
    };
    const onInput = async (action: string, value: unknown, owner: string, sequence: number) => {
        try { await sourceInput(source.id, { action, value, bindingId, owner, sequence }); }
        catch (failure) { setError(getErrorMessage(failure)); throw failure; }
    };
    const visibleDocument = documentScope === identity ? document : null;
    return <div className="plugin-source-panel" aria-label={`${source.name} 插件控制`}>
        {!visibleDocument && !error && !suspended && <p role="status"><SpinnerGap className="spin" aria-hidden="true" size={17} />读取插件界面…</p>}
        {suspended ? <div className="plugin-suspended"><p>{!source.enabled ? "输入源实例已停用" : source.runtimeStatus === "faulted" ? "插件进程运行异常" : "插件进程已停止"}</p>{source.enabled && <button className="secondary-button" type="button" disabled={busy} onClick={() => { setBusy(true); void pluginCall({ command: "start_source", params: { sourceId: source.id } }).then(() => refresh(true)).catch((failure) => setError(getErrorMessage(failure))).finally(() => setBusy(false)); }}>启动插件进程</button>}</div>
            : visibleDocument && <PluginRenderBoundary identity={JSON.stringify([identity, visibleDocument])}>{visibleDocument.nodes.map((node) => <SemanticNode key={node.id} node={node} context={{ disabled: !source.enabled || busy || (surface === "settings" && disabled), outputDisabled: surface === "control" && disabled, bindingId, channel, baseConfig: object(bindingId ? bindingConfig : source.config), configRevision: bindingId ? bindingRevision! : source.revision, onAction, onInput }} />)}</PluginRenderBoundary>}
        {!suspended && visibleDocument && visibleDocument.nodes.length === 0 && <p><PuzzlePiece aria-hidden="true" size={18} />此插件未提供界面，可通过 CLI 或 MCP 调用动作。</p>}
        {(error || source.lastError) && <p className="input-mode-error" role="alert">{error ?? source.lastError}<button className="secondary-button" type="button" onClick={() => void refresh()}>重试</button></p>}
    </div>;
};
