import {
    CheckCircle,
    CirclesThreePlus,
    SlidersHorizontal,
    SpinnerGap,
    Waveform,
} from "@phosphor-icons/react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot } from "../lib/contracts";

interface SourcesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onSetActiveSource: (sourceId: string) => void;
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
    onSetActiveSource,
}: SourcesPageProps) => (
    <div className="standard-page">
        <PageHeader
            description="输入源通过 SourceFactory 注册表扩展；首版同一时刻只有一个活动源，并固定输出到 A+B。"
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
                <Waveform aria-hidden="true" size={22} weight="light" />
                <span>当前活动源</span>
                <strong>
                    {snapshot.sources.find(
                        (source) => source.id === snapshot.activeSourceId,
                    )?.name ?? "未选择"}
                </strong>
            </div>
        </section>

        <div className="source-grid">
            {snapshot.sources.map((source) => {
                const isPending = pendingAction === `source-${source.id}`;
                const SourceIcon =
                    source.kind === "builtin.manual"
                        ? SlidersHorizontal
                        : Waveform;

                return (
                    <article
                        className={`source-card ${source.active ? "source-card-active" : ""}`}
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
                                {source.enabled ? "已启用" : "已停用"}
                            </span>
                        </div>
                        <p>{sourceDescription(source.kind)}</p>
                        <dl className="source-details">
                            <div>
                                <dt>实例 ID</dt>
                                <dd>{source.id}</dd>
                            </div>
                            <div>
                                <dt>目标</dt>
                                <dd>A+B 双通道</dd>
                            </div>
                        </dl>
                        <button
                            className={
                                source.active
                                    ? "active-source-button"
                                    : "secondary-button source-select-button"
                            }
                            disabled={source.active || !source.enabled || isPending}
                            onClick={() => onSetActiveSource(source.id)}
                            type="button"
                        >
                            {isPending ? (
                                <SpinnerGap
                                    aria-hidden="true"
                                    className="spin"
                                    size={18}
                                />
                            ) : (
                                <CheckCircle aria-hidden="true" size={18} />
                            )}
                            {source.active ? "当前活动源" : "设为活动源"}
                        </button>
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
