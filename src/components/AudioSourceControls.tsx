import { Copy, FileAudio, FloppyDisk, Microphone, Pause, Play, Record, SpeakerHigh, Stop } from "@phosphor-icons/react";
import { useEffect, useRef, useState } from "react";
import { chooseAudioFile, chooseRecordingDestination, isBrowserDemo } from "../lib/bridge";
import type { AudioAction, AudioChannelConfig, AudioSnapshot, HubChannel } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";
import { isValidAudioConfig } from "../lib/inputModes";
import { MappingCurveEditor } from "./MappingCurveEditor";

const formatTime = (ms: number): string => `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}`;
const stateNames: Record<AudioSnapshot["state"], string> = {
    idle: "未运行", loading: "正在加载", playing: "播放中", paused: "已暂停", capturing: "实时收音", recording: "录音中", error: "运行异常",
};

export const AudioPlayer = ({ audio, disabled, onControl, channel }: {
    audio: AudioSnapshot;
    disabled: boolean;
    onControl: (action: AudioAction) => Promise<void>;
    channel?: HubChannel;
}) => {
    const [error, setError] = useState<string | null>(null);
    const [seekPosition, setSeekPosition] = useState(audio.positionMs);
    const seeking = useRef(false);
    useEffect(() => { if (!seeking.current) setSeekPosition(audio.positionMs); }, [audio.positionMs, audio.durationMs, audio.fileName]);
    const control = async (action: AudioAction) => {
        setError(null);
        try { await onControl(action); } catch (failure) { setError(getErrorMessage(failure)); }
    };
    const chooseFile = async () => {
        try { const path = await chooseAudioFile(); if (path) await control({ type: "loadFile", path }); }
        catch (failure) { setError(getErrorMessage(failure)); }
    };
    const saveRecording = async () => {
        try { const path = await chooseRecordingDestination(); if (path) await control({ type: "saveRecording", path }); }
        catch (failure) { setError(getErrorMessage(failure)); }
    };
    const seek = () => {
        if (!seeking.current) return;
        seeking.current = false;
        void control({ type: "seek", positionMs: seekPosition });
    };
    const recording = audio.state === "recording";
    const capturing = audio.state === "capturing";
    const microphone = audio.mode === "microphone" && capturing;
    const desktop = audio.mode === "desktop" && capturing;
    const filePlayback = audio.mode === "file" || (audio.mode === "recording" && !recording);
    return (
        <section aria-label={channel ? `${channel.toUpperCase()} 通道音频控制` : "共享音频控制"} className="audio-player">
            <header className="input-mode-heading"><FileAudio aria-hidden="true" size={21} /><h3>{channel ? `${channel.toUpperCase()} 通道 · 音频输入` : "音频输入"}</h3><span className={recording || capturing || audio.state === "playing" ? "input-mode-running" : ""}>{desktop ? "桌面监听中" : stateNames[audio.state]}</span></header>
            <p className="input-mode-note">各设备共用声音输入与播放进度，通道映射独立配置。设备开始输出后才发送波形。</p>
            <p className="input-mode-note">支持 MP4 / M4V / MOV / MKV / WebM 视频，自动使用其中的音轨。视频上限 2 GB，音轨最长一小时；暂不支持 Opus、AC-3 / E-AC-3 音轨。</p>
            {audio.mode === "desktop" && <p className="input-mode-note">桌面音频监听 Windows 默认播放设备的声音。静音时保持监听；更换默认播放设备后请重新开启。</p>}
            {isBrowserDemo() && <p className="input-mode-demo">浏览器演示仅展示操作状态，实际播放、麦克风、录音和桌面音频请使用桌面应用。</p>}
            <div className="audio-input-options">
                <button className="secondary-button" disabled={disabled || recording} onClick={() => void chooseFile()} type="button"><FileAudio aria-hidden="true" size={17} />导入音频或视频</button>
                <button aria-pressed={microphone} className="secondary-button" disabled={disabled || recording} onClick={() => void control({ type: microphone ? "stop" : "startMicrophone" })} type="button"><Microphone aria-hidden="true" size={17} />{microphone ? "停止收音" : "麦克风实时输入"}</button>
                <button className={`secondary-button ${recording ? "is-recording" : ""}`} disabled={disabled} onClick={() => void control({ type: recording ? "stopRecording" : "startRecording" })} type="button">{recording ? <Stop aria-hidden="true" size={17} /> : <Record aria-hidden="true" size={17} />}{recording ? "完成录音并准备回放" : "开始录音"}</button>
                <button aria-pressed={desktop} className="secondary-button" disabled={disabled || recording} onClick={() => void control({ type: desktop ? "stop" : "startDesktop" })} type="button"><SpeakerHigh aria-hidden="true" size={17} />{desktop ? "停止桌面监听" : "桌面音频"}</button>
                {audio.hasRecording && <button className="secondary-button" disabled={disabled || recording} onClick={() => void saveRecording()} type="button"><FloppyDisk aria-hidden="true" size={17} />保存录音 WAV</button>}
            </div>
            <div className="audio-transport">
                <div className="audio-file-name"><strong>{recording ? "麦克风录音" : audio.mode === "desktop" ? "系统默认播放设备" : capturing ? "系统默认麦克风" : audio.fileName ?? "尚未选择音频或视频"}</strong><span>{formatTime(audio.positionMs)} / {formatTime(audio.durationMs)}</span></div>
                <input aria-label="音频播放进度" disabled={disabled || !filePlayback || audio.durationMs <= 0 || recording} min={0} max={Math.max(1, audio.durationMs)} onChange={(event) => { seeking.current = true; setSeekPosition(event.currentTarget.valueAsNumber); }} onPointerUp={seek} onKeyUp={seek} onBlur={seek} step={100} type="range" value={seekPosition} />
                <div className="input-mode-actions"><button className="primary-compact-button" disabled={disabled || !filePlayback || !audio.fileName || recording} onClick={() => void control({ type: audio.state === "playing" ? "pause" : "play" })} type="button">{audio.state === "playing" ? <Pause aria-hidden="true" size={16} /> : <Play aria-hidden="true" size={16} />}{audio.state === "playing" ? "暂停音频" : "播放音频"}</button><button className="secondary-button" disabled={disabled || recording || audio.state === "idle"} onClick={() => void control({ type: "stop" })} type="button"><Stop aria-hidden="true" size={16} />停止音频</button></div>
                {filePlayback && <fieldset className="audio-playback-options" disabled={disabled}>
                    <label><input aria-label="循环播放音频" type="checkbox" checked={audio.loop} onChange={(event) => void control({ type: "setPlaybackOptions", loop: event.currentTarget.checked, speakerEnabled: audio.speakerEnabled })} />循环播放</label>
                    <label><input aria-label="音频扬声器输出" type="checkbox" checked={audio.speakerEnabled} onChange={(event) => void control({ type: "setPlaybackOptions", loop: audio.loop, speakerEnabled: event.currentTarget.checked })} />扬声器输出</label>
                </fieldset>}
            </div>
            <div className="audio-levels" aria-label="音频分析状态">{(["Left", "Right"] as const).map((side) => {
                const level = side === "Left" ? audio.levelLeft : audio.levelRight;
                const peak = side === "Left" ? audio.peakLeftHz : audio.peakRightHz;
                return <div key={side}><span>{side === "Left" ? "左声道" : "右声道"}</span><meter aria-label={`${side === "Left" ? "左" : "右"}声道音量`} min={0} max={1} value={level} /><small>{Math.round(level * 100)}% · {Math.round(peak)} Hz</small></div>;
            })}</div>
            {(error || audio.lastError) && <p className="input-mode-error" role="alert">{error ?? audio.lastError}</p>}
        </section>
    );
};

interface AudioChannelSettingsProps {
    channel: HubChannel;
    config: AudioChannelConfig;
    disabled: boolean;
    onSave: (config: AudioChannelConfig) => void;
    onCopy: (config: AudioChannelConfig) => void;
}

export const AudioChannelSettings = ({ channel, config, disabled, onSave, onCopy }: AudioChannelSettingsProps) => {
    const [draft, setDraft] = useState(config);
    const identity = JSON.stringify(config);
    useEffect(() => { setDraft(config); }, [identity]);
    const field = <K extends keyof AudioChannelConfig>(key: K, value: AudioChannelConfig[K]) => setDraft((current) => ({ ...current, [key]: value }));
    const numberField = (key: "gain" | "volumeLower" | "volumeUpper" | "adaptiveLower" | "adaptiveUpper" | "hysteresisMs" | "frequencyMin" | "frequencyMax", label: string, min: number, max: number, step: number) => <label>{label}<input aria-label={`${channel.toUpperCase()} 通道${label}`} min={min} max={max} step={step} type="number" value={Number.isFinite(draft[key]) ? draft[key] : ""} onChange={(event) => field(key, event.currentTarget.valueAsNumber)} /></label>;
    const valid = isValidAudioConfig(draft);
    return (
        <section aria-label={`${channel.toUpperCase()} 通道音频映射`} className="audio-channel-settings">
            <header className="input-mode-heading"><FileAudio aria-hidden="true" size={18} /><h3>音频映射</h3></header>
            <fieldset className="audio-channel-switch" disabled={disabled}>
                <label className="input-mode-checkbox"><input aria-label={`${channel.toUpperCase()} 通道音频输出`} checked={config.enabled} type="checkbox" onChange={(event) => onSave({ ...config, enabled: event.currentTarget.checked })} />启用此通道音频波形</label>
            </fieldset>
            <p className="input-mode-note">{config.inputChannel === "left" ? "左声道" : config.inputChannel === "right" ? "右声道" : "左右混合"} · 增益 {config.gain} · {config.adaptive ? "自适应" : "固定范围"}</p>
            <details className="audio-mapping-details">
                <summary aria-label={`${channel.toUpperCase()} 通道音频映射设置`}>映射设置</summary>
                <fieldset className="input-mode-fields" disabled={disabled}>
                    <label>输入声道<select aria-label={`${channel.toUpperCase()} 通道输入声道`} value={draft.inputChannel} onChange={(event) => field("inputChannel", event.currentTarget.value as AudioChannelConfig["inputChannel"])}><option value="mix">左右混合</option><option value="left">左声道</option><option value="right">右声道</option></select></label>
                    <label>强度映射<select aria-label={`${channel.toUpperCase()} 通道音频强度映射`} value={draft.adaptive ? "adaptive" : "fixed"} onChange={(event) => field("adaptive", event.currentTarget.value === "adaptive")}><option value="adaptive">自适应</option><option value="fixed">固定范围</option></select></label>
                    {numberField("gain", "数据增益", 1, 10, 0.1)}
                    {numberField("volumeLower", "音量下限", 0, 1, 0.01)}
                    {numberField("volumeUpper", "音量上限", 0, 1, 0.01)}
                    {draft.adaptive && <>{numberField("adaptiveLower", "低适应系数", 0, 0.5, 0.01)}{numberField("adaptiveUpper", "高适应系数", 0, 0.5, 0.01)}{numberField("hysteresisMs", "迟滞 ms", 0, 2000, 100)}</>}
                    {numberField("frequencyMin", "观察频段下限 Hz", 50, 10000, 10)}
                    {numberField("frequencyMax", "观察频段上限 Hz", 50, 10000, 10)}
                </fieldset>
                <MappingCurveEditor disabled={disabled} label={`${channel.toUpperCase()} 音频周期`} min={10} max={100} onChange={(points) => field("periodCurve", points)} points={draft.periodCurve} unit="周期 ms" />
                <p className="input-mode-note">频段位置按对数归一化映射为周期；周期越小，输出频率越高。</p>
                {!valid && <p className="input-mode-error" role="alert">请检查参数范围：音量及频段下限需小于上限，周期节点位置需递增。</p>}
                <div className="input-mode-actions"><button className="secondary-button" disabled={disabled || !valid} onClick={() => onCopy(draft)} type="button"><Copy aria-hidden="true" size={15} />复制到 {channel === "a" ? "B" : "A"}</button><button className="primary-compact-button" disabled={disabled || !valid} onClick={() => onSave(draft)} type="button">应用 {channel.toUpperCase()} 音频配置</button></div>
            </details>
        </section>
    );
};
