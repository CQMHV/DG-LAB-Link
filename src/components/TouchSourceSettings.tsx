import { ArrowCounterClockwise, HandTap, Shuffle } from "@phosphor-icons/react";
import { useEffect, useState } from "react";
import { getCustomWaveform } from "../lib/bridge";
import type { CustomWaveformSnapshot, TouchConfig, WaveformConfig } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";
import { defaultTouchConfig, isValidCurve, waveformConfig } from "../lib/inputModes";
import { findOfficialWaveform, OFFICIAL_WAVEFORMS } from "../lib/waveforms";
import { MappingCurveEditor } from "./MappingCurveEditor";

interface TouchSourceSettingsProps {
    config: TouchConfig;
    customWaveforms: CustomWaveformSnapshot[];
    disabled: boolean;
    onSave: (config: TouchConfig) => void;
    onError: (message: string) => void;
}

export const TouchSourceSettings = ({ config, customWaveforms, disabled, onSave, onError }: TouchSourceSettingsProps) => {
    const [draft, setDraft] = useState(config);
    const [loadingWaveform, setLoadingWaveform] = useState(false);
    const identity = JSON.stringify(config);
    useEffect(() => { setDraft(config); }, [identity]);
    const field = <K extends keyof TouchConfig>(key: K, value: TouchConfig[K]) => setDraft((current) => ({ ...current, [key]: value }));
    const selectWaveform = async (id: string, key: "freeWaveforms" | "rhythmWaveforms" | "background", index = 0) => {
        if (id === "" && key === "background") { field("background", null); return; }
        setLoadingWaveform(true);
        try {
            const waveform = OFFICIAL_WAVEFORMS.some((item) => item.presetId === id)
                ? waveformConfig(findOfficialWaveform(id)) : await getCustomWaveform(id);
            setDraft((current) => ({ ...current, [key]: key === "background" ? waveform : current[key].map((item, cell) => cell === index ? waveform : item) }));
        } catch (error) { onError(getErrorMessage(error)); }
        finally { setLoadingWaveform(false); }
    };
    const options = (selected: WaveformConfig | null) => <>
        {selected && !OFFICIAL_WAVEFORMS.some((item) => item.presetId === selected.presetId) && !customWaveforms.some((item) => item.id === selected.presetId) && <option value={selected.presetId}>{selected.presetName}</option>}
        <optgroup label="内置波形">{OFFICIAL_WAVEFORMS.map((item) => <option key={item.presetId} value={item.presetId}>{item.presetName}</option>)}</optgroup>
        {customWaveforms.length > 0 && <optgroup label="自定义波形">{customWaveforms.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</optgroup>}
    </>;
    const gridKey = draft.mode === "free" ? "freeWaveforms" : "rhythmWaveforms";
    const shuffle = () => {
        const choices = [...OFFICIAL_WAVEFORMS];
        for (let index = choices.length - 1; index > 0; index -= 1) {
            const other = Math.floor(Math.random() * (index + 1));
            [choices[index], choices[other]] = [choices[other], choices[index]];
        }
        field(gridKey, draft[gridKey].map((_, index) => waveformConfig(choices[index % choices.length])));
    };
    const valid = isValidCurve(draft.intensityCurve, 0, 100) && isValidCurve(draft.periodCurve, 10, 100);
    return (
        <section aria-label="触控模式配置" className="input-mode-settings">
            <header className="input-mode-heading"><HandTap aria-hidden="true" size={21} /><h3>触控配置</h3></header>
            <p className="input-mode-note">配置由所有设备共用，触点按设备独立控制。相对强度不会改变通道的基础强度。</p>
            <fieldset className="input-mode-fields" disabled={disabled || loadingWaveform}>
                <label>触控面板<select aria-label="触控面板" value={draft.mode} onChange={(event) => field("mode", event.currentTarget.value as TouchConfig["mode"])}><option value="free">自由触控</option><option value="rhythm">律动触控</option></select></label>
                <label>通道分配<select aria-label="触控通道分配" value={draft.routing} onChange={(event) => field("routing", event.currentTarget.value as TouchConfig["routing"])}><option value="a">A 单通道</option><option value="b">B 单通道</option><option value="sync">AB 同步</option><option value="separate">AB 双指</option><option value="alternate">AB 轮替</option></select></label>
                {draft.mode === "rhythm" && <label>网格大小<select aria-label="律动网格大小" value={draft.gridSize} onChange={(event) => field("gridSize", Number(event.currentTarget.value))}>{[2, 3, 4].map((size) => <option key={size} value={size}>{size} × {size}</option>)}</select></label>}
                {draft.mode === "free" && <>
                    <label>强度映射<select aria-label="触控强度映射" value={draft.intensityMode} onChange={(event) => field("intensityMode", event.currentTarget.value as TouchConfig["intensityMode"])}><option value="classic">经典映射</option><option value="gradient">渐变映射</option></select></label>
                    {draft.intensityMode === "gradient" && <label>渐变方向<select aria-label="触控渐变方向" value={draft.gradientDirection} onChange={(event) => field("gradientDirection", event.currentTarget.value as TouchConfig["gradientDirection"])}><option value="left">左侧渐变</option><option value="right">右侧渐变</option><option value="both">两侧渐变</option></select></label>}
                    <label className="input-mode-checkbox"><input aria-label="交换触控两轴" checked={draft.swapAxes} onChange={(event) => field("swapAxes", event.currentTarget.checked)} type="checkbox" />交换横轴与纵轴</label>
                </>}
                <label>松手后的背景波形<select aria-label="触控背景波形" value={draft.background?.presetId ?? ""} onChange={(event) => void selectWaveform(event.currentTarget.value, "background")}><option value="">无背景（松手结束触控输出）</option>{options(draft.background)}</select></label>
            </fieldset>
            {draft.mode === "free" && <div className="mapping-editor-pair">
                {draft.intensityMode === "classic" ? <MappingCurveEditor disabled={disabled} label="触控相对强度" min={0} max={100} onChange={(points) => field("intensityCurve", points)} points={draft.intensityCurve} unit="相对强度" /> : <div className="input-mode-help"><strong>{draft.gradientDirection === "left" ? "左侧渐变" : draft.gradientDirection === "right" ? "右侧渐变" : "两侧渐变"}</strong><p>{draft.gradientDirection === "left" ? "左侧随时间由高到低，其余区域保持最高相对强度。" : draft.gradientDirection === "right" ? "右侧随时间由低到高，其余区域保持最高相对强度。" : "左侧随时间由高到低，右侧由低到高，中部保持最高相对强度。"}位置决定变化速度。</p></div>}
                <MappingCurveEditor disabled={disabled} label="触控周期" min={10} max={100} onChange={(points) => field("periodCurve", points)} points={draft.periodCurve} unit="周期 ms" />
            </div>}
            <header className="input-mode-heading"><h3>快捷波形区域</h3><button className="secondary-button" disabled={disabled || loadingWaveform} onClick={shuffle} type="button"><Shuffle aria-hidden="true" size={16} />随机换批</button></header>
            <div className="touch-cell-settings" style={{ gridTemplateColumns: `repeat(${draft.mode === "free" ? 4 : draft.gridSize}, minmax(0, 1fr))` }}>
                {draft[gridKey].slice(0, draft.mode === "free" ? 8 : draft.gridSize ** 2).map((waveform, index) => <label key={index}><span>区域 {index + 1}</span><select aria-label={`触控区域 ${index + 1} 波形`} disabled={disabled || loadingWaveform} value={waveform.presetId} onChange={(event) => void selectWaveform(event.currentTarget.value, gridKey, index)}>{options(waveform)}</select></label>)}
            </div>
            {!valid && <p className="input-mode-error" role="alert">请检查节点：位置必须递增，相对强度为 0–100，周期为 10–100 ms。</p>}
            <div className="input-mode-actions"><button className="secondary-button" disabled={disabled || loadingWaveform} onClick={() => setDraft(defaultTouchConfig())} type="button"><ArrowCounterClockwise aria-hidden="true" size={16} />恢复默认</button><button className="primary-compact-button" disabled={disabled || loadingWaveform || !valid} onClick={() => onSave(draft)} type="button">应用触控配置</button></div>
        </section>
    );
};
