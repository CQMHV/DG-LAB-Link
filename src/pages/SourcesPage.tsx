import {
    ArrowLeft,
    ArrowRight,
    CirclesThreePlus,
    DeviceMobile,
    FileArrowDown,
    HandTap,
    Microphone,
    Star,
    Waveform,
} from "@phosphor-icons/react";
import { useState } from "react";

import { FixedWaveformSourceSettings } from "../components/FixedWaveformSourceSettings";
import { TouchSourceSettings } from "../components/TouchSourceSettings";
import { AudioPlayer } from "../components/AudioSourceControls";
import { PageHeader } from "../components/PageHeader";
import type {
    HubSnapshot,
    AudioAction,
    TouchConfig,
    SourceSnapshot,
    WaveformConfig,
} from "../lib/contracts";

interface SourcesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onDeleteCustomWaveform: (presetId: string) => void;
    onError: (message: string) => void;
    onImportCustomWaveforms: (configs: WaveformConfig[]) => void;
    onReorderCustomWaveforms: (presetIds: string[]) => void;
    onSetTouchConfig: (config: TouchConfig) => void;
    onAudioControl: (action: AudioAction) => Promise<void>;
}

const descriptions: Record<string, string> = {
    "builtin.fixed_waveform": "循环输出选定的固定波形；统一管理波形库，各设备通道分别选择使用项。",
    "builtin.touch": "按住自由触控板调整周期与相对强度，或在律动网格滑动切换波形；支持 A/B 路由与背景波形。",
    "builtin.audio": "支持本地音频或视频音轨、麦克风实时收音、录音回放及桌面音频四种模式，将音量与频谱映射为各设备通道的波形。",
};
const sourceIcon = (kind: string) => kind === "builtin.touch" ? HandTap : kind === "builtin.audio" ? Microphone : Waveform;

const getSourceAssignmentText = (
    snapshot: HubSnapshot,
    source: SourceSnapshot,
) => snapshot.defaultSourceId === source.id
    ? "当前为新设备的默认输入源"
    : source.assignedChannelCount > 0
      ? `正由 ${source.assignedChannelCount} 路设备通道使用`
      : "尚未分配给设备通道";

export const SourcesPage = ({
    snapshot,
    pendingAction,
    onDeleteCustomWaveform,
    onError,
    onImportCustomWaveforms,
    onReorderCustomWaveforms,
    onSetTouchConfig,
    onAudioControl,
}: SourcesPageProps) => {
    const [openSourceId, setOpenSourceId] = useState<string | null>(null);
    const openSource = snapshot.sources.find((source) => source.id === openSourceId);
    const assignedChannelCount = snapshot.sources.reduce(
        (total, source) => total + source.assignedChannelCount,
        0,
    );

    if (openSource) {
        const Icon = sourceIcon(openSource.kind);
        return (
            <div className="standard-page">
                <PageHeader
                    actions={
                        <button
                            className="secondary-button source-detail-back"
                            onClick={() => setOpenSourceId(null)}
                            type="button"
                        >
                            <ArrowLeft aria-hidden="true" size={17} />
                            返回输入源
                        </button>
                    }
                    description="查看输入源状态，并修改此输入源独有的配置。"
                    eyebrow="SOURCE DETAIL"
                    title={openSource.name}
                />

                <section
                    aria-label={`${openSource.name} 输入源详情`}
                    className="source-detail-shell"
                >
                    <div className="source-card-heading">
                        <div className="source-icon">
                            <Icon aria-hidden="true" size={25} weight="light" />
                        </div>
                        <div>
                            <h2>{openSource.name}</h2>
                            <code>{openSource.kind}</code>
                        </div>
                        <span
                            className={`source-enabled ${openSource.enabled ? "enabled" : ""}`}
                        >
                            {openSource.enabled ? "可用" : "未配置"}
                        </span>
                    </div>

                    <p>
                        {descriptions[openSource.kind] ?? "打开详情查看此输入源的配置。"}
                    </p>

                    <dl className="source-details source-detail-meta">
                        <div>
                            <dt>实例 ID</dt>
                            <dd>{openSource.id}</dd>
                        </div>
                        <div>
                            <dt>配置范围</dt>
                            <dd>{openSource.kind === "builtin.touch" ? "共用面板，设备独立触点" : openSource.kind === "builtin.audio" ? "共用声音，通道独立映射" : "按设备通道配置"}</dd>
                        </div>
                        <div>
                            <dt>分配通道</dt>
                            <dd>{openSource.assignedChannelCount} 路</dd>
                        </div>
                    </dl>

                    <div className="source-assignment-note">
                        {snapshot.defaultSourceId === openSource.id ? (
                            <Star aria-hidden="true" size={18} weight="fill" />
                        ) : (
                            <DeviceMobile aria-hidden="true" size={18} />
                        )}
                        {getSourceAssignmentText(snapshot, openSource)}
                    </div>

                    {openSource.kind === "builtin.fixed_waveform" && (
                        <FixedWaveformSourceSettings
                            customWaveforms={snapshot.customWaveforms}
                            disabled={pendingAction !== null}
                            onDeleteCustomWaveform={onDeleteCustomWaveform}
                            onError={onError}
                            onImportCustomWaveforms={onImportCustomWaveforms}
                            onReorderCustomWaveforms={onReorderCustomWaveforms}
                        />
                    )}
                    {openSource.kind === "builtin.touch" && <TouchSourceSettings config={snapshot.inputModes.touchConfig} customWaveforms={snapshot.customWaveforms} disabled={pendingAction !== null} onSave={onSetTouchConfig} onError={onError} />}
                    {openSource.kind === "builtin.audio" && <AudioPlayer audio={snapshot.inputModes.audio} disabled={pendingAction !== null} onControl={onAudioControl} />}
                    {openSource.kind === "builtin.audio" && <p className="input-mode-note">在控制台将通道设为音频模式，即可编辑该通道的音量、频段和周期映射，并复制 A/B 配置。</p>}
                </section>
            </div>
        );
    }

    return (
        <div className="standard-page">
            <PageHeader
                description="查看中枢已注册的输入源、运行状态及设备通道绑定情况。"
                eyebrow="SOURCE REGISTRY"
                title="输入源"
            />

            <section className="source-overview">
                <div>
                    <CirclesThreePlus aria-hidden="true" size={22} weight="light" />
                    <span>已注册实例</span>
                    <strong>{snapshot.sources.length}</strong>
                </div>
                <div>
                    <DeviceMobile aria-hidden="true" size={22} weight="light" />
                    <span>通道绑定</span>
                    <strong>
                        {assignedChannelCount} / {snapshot.devices.length * 2}
                    </strong>
                </div>
                <div>
                    <FileArrowDown aria-hidden="true" size={22} weight="light" />
                    <span>已导入波形</span>
                    <strong>{snapshot.customWaveforms.length}</strong>
                </div>
            </section>

            <div className="source-grid">
                {snapshot.sources.map((source) => {
                    const Icon = sourceIcon(source.kind);
                    return (
                    <article
                        className={`source-card ${source.assignedChannelCount > 0 ? "source-card-active" : ""}`}
                        key={source.id}
                    >
                        <div className="source-card-heading">
                            <div className="source-icon">
                                <Icon aria-hidden="true" size={25} weight="light" />
                            </div>
                            <div>
                                <h2>{source.name}</h2>
                                <code>{source.kind}</code>
                            </div>
                            <span
                                className={`source-enabled ${source.enabled ? "enabled" : ""}`}
                            >
                                {source.enabled ? "可用" : "未配置"}
                            </span>
                        </div>
                        <p>
                            {descriptions[source.kind] ?? "打开详情查看此输入源的配置。"}
                        </p>

                        <dl className="source-details">
                            <div>
                                <dt>实例 ID</dt>
                                <dd>{source.id}</dd>
                            </div>
                            <div>
                                <dt>配置范围</dt>
                                <dd>{source.kind === "builtin.touch" ? "共用面板，设备独立触点" : source.kind === "builtin.audio" ? "共用声音，通道独立映射" : "按设备通道配置"}</dd>
                            </div>
                            <div>
                                <dt>分配通道</dt>
                                <dd>{source.assignedChannelCount} 路</dd>
                            </div>
                        </dl>
                        <div className="source-card-footer">
                            <div className="source-assignment-note">
                                {snapshot.defaultSourceId === source.id ? (
                                    <Star aria-hidden="true" size={18} weight="fill" />
                                ) : (
                                    <DeviceMobile aria-hidden="true" size={18} />
                                )}
                                {getSourceAssignmentText(snapshot, source)}
                            </div>
                            <button
                                aria-label={`打开 ${source.name} 详情`}
                                className="source-open-details"
                                onClick={() => setOpenSourceId(source.id)}
                                type="button"
                            >
                                打开详情
                                <ArrowRight aria-hidden="true" size={16} />
                            </button>
                        </div>
                    </article>
                    );
                })}
            </div>
        </div>
    );
};
