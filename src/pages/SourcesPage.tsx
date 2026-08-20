import {
    CirclesThreePlus,
    DeviceMobile,
    SlidersHorizontal,
    Waveform,
} from "@phosphor-icons/react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot } from "../lib/contracts";

interface SourcesPageProps {
    snapshot: HubSnapshot;
}

const sourceDescription = (kind: string): string => {
    if (kind === "builtin.manual") {
        return "当前输出预置静默帧，不提供参数编辑；作为后续手动波形参数化的扩展入口。";
    }
    return "循环生成 DG-LAB-VRCOSC 默认呼吸波形，以明显的渐强、保持和停顿验证设备链路。";
};

export const SourcesPage = ({ snapshot }: SourcesPageProps) => {
    const assignedDeviceCount = snapshot.sources.reduce(
        (total, source) => total + source.assignedDeviceCount,
        0,
    );

    return (
        <div className="standard-page">
            <PageHeader
                description="输入源通过 SourceFactory 注册表扩展；每台设备可在控制台标签中独立选择输入源，同一实例被多台设备使用时会保持同帧扇出。"
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
                    <span>设备绑定</span>
                    <strong>
                        {assignedDeviceCount} / {snapshot.devices.length}
                    </strong>
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
                            className={`source-card ${source.assignedDeviceCount > 0 ? "source-card-active" : ""}`}
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
                                    <dt>分配设备</dt>
                                    <dd>{source.assignedDeviceCount} 台</dd>
                                </div>
                                <div>
                                    <dt>目标</dt>
                                    <dd>A+B 双通道</dd>
                                </div>
                            </dl>
                            <div className="source-assignment-note">
                                <DeviceMobile aria-hidden="true" size={18} />
                                {source.assignedDeviceCount > 0
                                    ? `正由 ${source.assignedDeviceCount} 台设备使用`
                                    : "尚未分配；请在控制台设备标签中选择"}
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
