import {
    CirclesThreePlus,
    DeviceMobile,
    SlidersHorizontal,
    Star,
    Waveform,
} from "@phosphor-icons/react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot } from "../lib/contracts";

interface SourcesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onSetDefaultSource: (sourceId: string | null) => void;
}

const sourceDescription = (kind: string): string => {
    if (kind === "builtin.manual") {
        return "当前输出预置静默帧，不提供参数编辑；作为后续手动波形参数化的扩展入口。";
    }
    return "循环生成 DG-LAB-VRCOSC 默认呼吸波形，以明显的渐强、保持和停顿验证设备链路。";
};

export const SourcesPage = ({
    snapshot,
    pendingAction,
    onSetDefaultSource,
}: SourcesPageProps) => {
    const assignedChannelCount = snapshot.sources.reduce(
        (total, source) => total + source.assignedChannelCount,
        0,
    );

    return (
        <div className="standard-page">
            <PageHeader
                description="输入源通过 SourceFactory 注册表扩展；每台设备的 A、B 通道可在控制台标签中分别选择输入源，同一实例被多路使用时会保持同帧扇出。"
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
                <div className="source-default-control">
                    <Star aria-hidden="true" size={22} weight="light" />
                    <label htmlFor="default-source-select">
                        <span>默认输入源</span>
                        <select
                            aria-label="选择默认输入源"
                            disabled={pendingAction !== null}
                            id="default-source-select"
                            onChange={(event) =>
                                onSetDefaultSource(event.target.value || null)
                            }
                            value={snapshot.defaultSourceId ?? ""}
                        >
                            <option value="">每次询问</option>
                            {snapshot.sources
                                .filter((source) => source.enabled)
                                .map((source) => (
                                    <option key={source.id} value={source.id}>
                                        {source.name}
                                    </option>
                                ))}
                        </select>
                    </label>
                </div>
            </section>

            <div className="source-grid">
                {snapshot.sources.map((source) => {
                    const SourceIcon =
                        source.kind === "builtin.manual"
                            ? SlidersHorizontal
                            : Waveform;

                    return (
                        <article
                            className={`source-card ${source.assignedChannelCount > 0 ? "source-card-active" : ""}`}
                            key={source.id}
                        >
                            <div className="source-card-heading">
                                <div className="source-icon">
                                    <SourceIcon
                                        aria-hidden="true"
                                        size={25}
                                        weight="light"
                                    />
                                </div>
                                <div>
                                    <h2>{source.name}</h2>
                                    <code>{source.kind}</code>
                                </div>
                                <span
                                    className={`source-enabled ${source.enabled ? "enabled" : ""}`}
                                >
                                    {snapshot.defaultSourceId === source.id
                                    ? "默认"
                                        : source.enabled
                                          ? "已启用"
                                          : "已停用"}
                                </span>
                            </div>
                            <p>{sourceDescription(source.kind)}</p>
                            <dl className="source-details">
                                <div>
                                    <dt>实例 ID</dt>
                                    <dd>{source.id}</dd>
                                </div>
                                <div>
                                    <dt>分配通道</dt>
                                    <dd>{source.assignedChannelCount} 路</dd>
                                </div>
                                <div>
                                    <dt>目标</dt>
                                    <dd>A 或 B 通道</dd>
                                </div>
                            </dl>
                            <div className="source-assignment-note">
                                {snapshot.defaultSourceId === source.id ? (
                                    <Star aria-hidden="true" size={18} weight="fill" />
                                ) : (
                                    <DeviceMobile aria-hidden="true" size={18} />
                                )}
                                {snapshot.defaultSourceId === source.id
                                    ? "新接入设备的 A/B 将默认使用此输入源"
                                    : source.assignedChannelCount > 0
                                      ? `正由 ${source.assignedChannelCount} 路设备通道使用`
                                      : "尚未分配；请在控制台设备标签中选择 A/B 来源"}
                            </div>
                        </article>
                    );
                })}
            </div>

            <div className="extension-note">
                <CirclesThreePlus aria-hidden="true" size={21} />
                <div>
                    <strong>可扩展输入架构</strong>
                    <p>
                        新输入类型在 Rust 侧注册 SourceFactory 后即可作为独立实例接入；实时调度不会经过 React。
                    </p>
                </div>
            </div>
        </div>
    );
};
