import {
    ArrowLeft,
    ArrowRight,
    CirclesThreePlus,
    DeviceMobile,
    FileArrowDown,
    Star,
    Waveform,
} from "@phosphor-icons/react";
import { useState } from "react";

import { FixedWaveformSourceSettings } from "../components/FixedWaveformSourceSettings";
import { PageHeader } from "../components/PageHeader";
import type {
    HubSnapshot,
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
}

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
}: SourcesPageProps) => {
    const [openSourceId, setOpenSourceId] = useState<string | null>(null);
    const openSource = snapshot.sources.find((source) => source.id === openSourceId);
    const assignedChannelCount = snapshot.sources.reduce(
        (total, source) => total + source.assignedChannelCount,
        0,
    );

    if (openSource) {
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
                            <Waveform aria-hidden="true" size={25} weight="light" />
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
                        循环输出选定的固定波形；波形资源由此输入源统一管理，设备通道分别选择使用项。
                    </p>

                    <dl className="source-details source-detail-meta">
                        <div>
                            <dt>实例 ID</dt>
                            <dd>{openSource.id}</dd>
                        </div>
                        <div>
                            <dt>波形配置</dt>
                            <dd>按设备通道配置</dd>
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
                {snapshot.sources.map((source) => (
                    <article
                        className={`source-card ${source.assignedChannelCount > 0 ? "source-card-active" : ""}`}
                        key={source.id}
                    >
                        <div className="source-card-heading">
                            <div className="source-icon">
                                <Waveform aria-hidden="true" size={25} weight="light" />
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
                            循环输出选定的固定波形；打开详情可查看和修改此输入源的专属配置。
                        </p>

                        <dl className="source-details">
                            <div>
                                <dt>实例 ID</dt>
                                <dd>{source.id}</dd>
                            </div>
                            <div>
                                <dt>波形配置</dt>
                                <dd>按设备通道配置</dd>
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
                ))}
            </div>
        </div>
    );
};
