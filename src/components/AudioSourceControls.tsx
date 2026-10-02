import { FileAudio, FloppyDisk, Microphone, Pause, Play, Record, SpeakerHigh, Stop } from "@phosphor-icons/react";
import { useEffect, useRef, useState } from "react";
import { isBrowserDemo } from "../lib/bridge";
import { choosePluginDestination, choosePluginFile } from "../lib/plugins";
import type { AudioAction, AudioSnapshot, HubChannel } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";

const formatTime = (ms: number): string => `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}`;
const stateNames: Record<AudioSnapshot["state"], string> = {
    idle: "未运行", loading: "正在加载", playing: "播放中", paused: "已暂停", capturing: "实时收音", recording: "录音中", error: "运行异常",
};

export const AudioPlayer = ({ audio, disabled, onControl, channel, modes = ["file", "microphone", "recording", "desktop"], description }: {
    audio: AudioSnapshot;
    disabled: boolean;
    onControl: (action: AudioAction) => Promise<void>;
    channel?: HubChannel;
    modes?: AudioSnapshot["mode"][];
    description?: string;
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
        try { const path = await choosePluginFile(); if (path) await control({ type: "loadFile", path }); }
        catch (failure) { setError(getErrorMessage(failure)); }
    };
    const saveRecording = async () => {
        try { const path = await choosePluginDestination("DG-LAB录音.wav", ["wav"]); if (path) await control({ type: "saveRecording", path }); }
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
            {description && <p className="input-mode-note">{description}</p>}
            {isBrowserDemo() && <p className="input-mode-demo">浏览器演示仅展示操作状态，实际播放、麦克风、录音和桌面音频请使用桌面应用。</p>}
            <div className="audio-input-options">
                {modes.includes("file") && <button className="secondary-button" disabled={disabled || recording} onClick={() => void chooseFile()} type="button"><FileAudio aria-hidden="true" size={17} />导入音频或视频</button>}
                {modes.includes("microphone") && <button aria-pressed={microphone} className="secondary-button" disabled={disabled || recording} onClick={() => void control({ type: microphone ? "stop" : "startMicrophone" })} type="button"><Microphone aria-hidden="true" size={17} />{microphone ? "停止收音" : "麦克风实时输入"}</button>}
                {modes.includes("recording") && <button className={`secondary-button ${recording ? "is-recording" : ""}`} disabled={disabled} onClick={() => void control({ type: recording ? "stopRecording" : "startRecording" })} type="button">{recording ? <Stop aria-hidden="true" size={17} /> : <Record aria-hidden="true" size={17} />}{recording ? "完成录音并准备回放" : "开始录音"}</button>}
                {modes.includes("desktop") && <button aria-pressed={desktop} className="secondary-button" disabled={disabled || recording} onClick={() => void control({ type: desktop ? "stop" : "startDesktop" })} type="button"><SpeakerHigh aria-hidden="true" size={17} />{desktop ? "停止桌面监听" : "桌面音频"}</button>}
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
