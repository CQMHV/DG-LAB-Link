import { ArrowLeft, ArrowRight, CirclesThreePlus, DeviceMobile, DownloadSimple, Plus, PuzzlePiece, Star, Trash, Waveform } from "@phosphor-icons/react";
import { useState } from "react";
import { Dialog } from "../components/Dialog";
import { FixedWaveformSourceSettings } from "../components/FixedWaveformSourceSettings";
import { PageHeader } from "../components/PageHeader";
import { PluginSourcePanel } from "../components/PluginSourcePanel";
import type { HubSnapshot, InstalledPlugin, SourceSnapshot, WaveformConfig } from "../lib/contracts";
import { getErrorMessage } from "../lib/errors";
import { choosePluginPackage, pluginCall } from "../lib/plugins";
import "./SourcesPage.css";

interface SourcesPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onDeleteCustomWaveform: (presetId: string) => void;
    onError: (message: string) => void;
    onImportCustomWaveforms: (configs: WaveformConfig[]) => void;
    onReorderCustomWaveforms: (presetIds: string[]) => void;
}

const runtimeNames: Record<string, string> = { stopped: "未运行", starting: "启动中", running: "运行中", faulted: "运行异常", disabled: "已停用" };
const assignmentText = (snapshot: HubSnapshot, source: SourceSnapshot) => snapshot.defaultSourceId === source.id
    ? "当前为新设备的默认输入源" : source.assignedChannelCount ? `正由 ${source.assignedChannelCount} 路设备通道使用` : "尚未分配给设备通道";

export const SourcesPage = ({ snapshot, pendingAction, onDeleteCustomWaveform, onError, onImportCustomWaveforms, onReorderCustomWaveforms }: SourcesPageProps) => {
    const [tab, setTab] = useState<"sources" | "plugins">("sources");
    const [openSourceId, setOpenSourceId] = useState<string | null>(null);
    const [newInstancePluginId, setNewInstancePluginId] = useState<string | null>(null);
    const [instanceName, setInstanceName] = useState("");
    const [busy, setBusy] = useState(false);
    const [removal, setRemoval] = useState<{ source?: SourceSnapshot; plugin?: InstalledPlugin } | null>(null);
    const [deleteData, setDeleteData] = useState(false);
    const plugins = snapshot.plugins ?? [];
    const openSource = snapshot.sources.find((source) => source.id === openSourceId);
    const disabled = busy || pendingAction !== null;
    const run = async (action: () => Promise<unknown>) => {
        if (disabled) return;
        setBusy(true);
        try { await action(); } catch (failure) { onError(getErrorMessage(failure)); } finally { setBusy(false); }
    };
    const install = (update = false) => void run(async () => {
        const path = await choosePluginPackage();
        if (path) await pluginCall({ command: update ? "update_plugin" : "install_plugin", params: { path } });
    });
    const remove = () => void run(async () => {
        if (!removal) return;
        await pluginCall(removal.source
            ? { command: "delete_source", params: { sourceId: removal.source.id, deleteData } }
            : { command: "uninstall_plugin", params: { pluginId: removal.plugin!.manifest.id, deleteData } });
        setRemoval(null); setOpenSourceId(null);
    });
    const pluginFor = (source: SourceSnapshot) => plugins.find((plugin) => plugin.manifest.id === source.pluginId);
    const sourceIcon = (source: SourceSnapshot) => source.kind === "builtin.fixed_waveform" ? Waveform : PuzzlePiece;

    if (openSource) {
        const Icon = sourceIcon(openSource);
        const plugin = pluginFor(openSource);
        return <div className="standard-page">
            <PageHeader eyebrow="SOURCE DETAIL" title={openSource.name} description="配置输入源实例；设备输出由控制台单独管理。" actions={<button className="secondary-button source-detail-back" onClick={() => setOpenSourceId(null)} type="button"><ArrowLeft aria-hidden="true" size={17} />返回输入源</button>} />
            <section aria-label={`${openSource.name} 输入源详情`} className="source-detail-shell">
                <div className="source-card-heading"><div className="source-icon"><Icon aria-hidden="true" size={25} weight="light" /></div><div><h2>{openSource.name}</h2><code>{plugin?.manifest.name ?? "核心基础能力"}</code></div><span className={`source-enabled ${openSource.enabled ? "enabled" : ""}`}>{openSource.kind === "builtin.fixed_waveform" ? "可用" : !openSource.enabled ? "已停用" : runtimeNames[openSource.runtimeStatus ?? "stopped"]}</span></div>
                <dl className="source-details source-detail-meta"><div><dt>实例 ID</dt><dd>{openSource.id}</dd></div><div><dt>配置范围</dt><dd>{openSource.kind === "builtin.fixed_waveform" ? "按设备通道配置" : "实例独立配置"}</dd></div><div><dt>分配通道</dt><dd>{openSource.assignedChannelCount} 路</dd></div>{plugin && <div><dt>插件版本</dt><dd>{plugin.manifest.version}</dd></div>}</dl>
                <div className="source-assignment-note"><DeviceMobile aria-hidden="true" size={18} />{assignmentText(snapshot, openSource)}</div>
                {openSource.kind === "builtin.fixed_waveform"
                    ? <FixedWaveformSourceSettings customWaveforms={snapshot.customWaveforms} disabled={disabled} onDeleteCustomWaveform={onDeleteCustomWaveform} onError={onError} onImportCustomWaveforms={onImportCustomWaveforms} onReorderCustomWaveforms={onReorderCustomWaveforms} />
                    : <><div className="plugin-instance-actions"><button className="secondary-button" disabled={disabled} type="button" onClick={() => void run(() => pluginCall({ command: "set_source_enabled", params: { sourceId: openSource.id, enabled: !openSource.enabled } }))}>{openSource.enabled ? "停用实例" : "启用实例"}</button><button className="secondary-button" disabled={disabled || !openSource.enabled} type="button" onClick={() => void run(() => pluginCall({ command: openSource.runtimeStatus === "running" ? "stop_source" : "start_source", params: { sourceId: openSource.id } }))}>{openSource.runtimeStatus === "running" ? "停止插件进程" : "启动插件进程"}</button><button className="secondary-button" disabled={disabled} type="button" onClick={() => { setDeleteData(false); setRemoval({ source: openSource }); }}><Trash aria-hidden="true" size={16} />删除实例</button></div><PluginSourcePanel source={openSource} disabled={disabled} /></>}
            </section>
            {removal && <Dialog title="删除输入源实例" onClose={() => setRemoval(null)}><p>将解除此实例的设备通道绑定并停止其进程。</p><label className="plugin-remove-data"><input type="checkbox" checked={deleteData} onChange={(event) => setDeleteData(event.currentTarget.checked)} />同时清除配置和数据</label><button className="primary-compact-button" disabled={disabled} type="button" onClick={remove}>删除实例</button></Dialog>}
        </div>;
    }

    return <div className="standard-page">
        <PageHeader eyebrow="INPUT SOURCES" title="输入源" description="安装插件并创建输入源实例，将输出分配给设备通道。" actions={<><button className="secondary-button" disabled={disabled} type="button" onClick={() => install()}><DownloadSimple aria-hidden="true" size={17} />安装本地插件</button><button className="primary-compact-button" disabled={disabled || !plugins.length} type="button" onClick={() => { setNewInstancePluginId(plugins[0]?.manifest.id ?? null); setInstanceName(""); }}><Plus aria-hidden="true" size={17} />创建输入源</button></>} />
        <div className="source-page-tabs" role="tablist" aria-label="输入源管理"><button role="tab" aria-selected={tab === "sources"} type="button" onClick={() => setTab("sources")}>我的输入源 <span>{snapshot.sources.length}</span></button><button role="tab" aria-selected={tab === "plugins"} type="button" onClick={() => setTab("plugins")}>插件管理 <span>{plugins.length}</span></button></div>
        {tab === "sources" ? <>
            <section className="source-overview"><div><CirclesThreePlus aria-hidden="true" size={22} weight="light" /><span>输入源实例</span><strong>{snapshot.sources.length}</strong></div><div><DeviceMobile aria-hidden="true" size={22} weight="light" /><span>通道绑定</span><strong>{snapshot.sources.reduce((total, source) => total + source.assignedChannelCount, 0)} / {snapshot.devices.length * 2}</strong></div><div><PuzzlePiece aria-hidden="true" size={22} weight="light" /><span>已安装插件</span><strong>{plugins.length}</strong></div></section>
            <div className="source-grid">{snapshot.sources.map((source) => { const Icon = sourceIcon(source); const plugin = pluginFor(source); return <article key={source.id} className={`source-card ${source.assignedChannelCount ? "source-card-active" : ""}`}><div className="source-card-heading"><div className="source-icon"><Icon aria-hidden="true" size={25} weight="light" /></div><div><h2>{source.name}</h2><span className="source-plugin-attribution">{plugin?.manifest.name ?? (source.kind === "builtin.fixed_waveform" ? "核心基础能力" : "插件输入源")}</span></div><span className={`source-enabled ${source.enabled ? "enabled" : ""}`}>{source.kind === "builtin.fixed_waveform" ? "可用" : !source.enabled ? "已停用" : runtimeNames[source.runtimeStatus ?? "stopped"]}</span></div><p>{source.kind === "builtin.fixed_waveform" ? "循环输出选定波形；管理统一波形库，各设备通道分别选择。" : plugin ? `${plugin.manifest.publisher} · ${plugin.manifest.version}${plugin.preinstalled ? " · 预装" : ""}` : "通过插件提供波形和控制界面。"}</p>{source.lastError && <p className="input-mode-error">{source.lastError}</p>}<dl className="source-details"><div><dt>实例 ID</dt><dd>{source.id}</dd></div><div><dt>配置范围</dt><dd>{source.kind === "builtin.fixed_waveform" ? "按设备通道配置" : "实例独立配置"}</dd></div><div><dt>分配通道</dt><dd>{source.assignedChannelCount} 路</dd></div></dl><div className="source-card-footer"><div className="source-assignment-note">{snapshot.defaultSourceId === source.id ? <Star aria-hidden="true" size={18} weight="fill" /> : <DeviceMobile aria-hidden="true" size={18} />}{assignmentText(snapshot, source)}</div><button aria-label={`打开 ${source.name} 详情`} className="source-open-details" onClick={() => setOpenSourceId(source.id)} type="button">打开详情<ArrowRight aria-hidden="true" size={16} /></button></div></article>; })}</div>
        </> : <section className="plugin-manager" role="tabpanel" aria-label="插件管理"><header><p>选择本地 .dglabplugin 安装包；更新使用相同插件 ID 的新版包。</p><button className="secondary-button" disabled={disabled} type="button" onClick={() => install(true)}>从本地包更新</button></header>{plugins.map((plugin) => <article className="plugin-package-row" key={plugin.manifest.id}><PuzzlePiece aria-hidden="true" size={26} weight="light" /><div><h2>{plugin.manifest.name}{plugin.preinstalled && <small>预装</small>}</h2><p>{plugin.manifest.publisher} · {plugin.manifest.version} · {plugin.manifest.license}</p><code>{plugin.manifest.id}</code></div><span>{snapshot.sources.filter((source) => source.pluginId === plugin.manifest.id).length} 个实例</span><button className="secondary-button" disabled={disabled} type="button" onClick={() => { setNewInstancePluginId(plugin.manifest.id); setInstanceName(""); }}>创建实例</button><button className="icon-button" aria-label={`卸载 ${plugin.manifest.name}`} disabled={disabled} type="button" onClick={() => { setDeleteData(false); setRemoval({ plugin }); }}><Trash aria-hidden="true" size={18} /></button></article>)}{plugins.length === 0 && <p>尚未安装插件。固定波形始终可用。</p>}</section>}
        {newInstancePluginId !== null && <Dialog title="创建输入源" description="同一插件可创建多个独立运行的实例。" onClose={() => setNewInstancePluginId(null)}><form className="plugin-instance-form" onSubmit={(event) => { event.preventDefault(); void run(async () => { await pluginCall({ command: "create_source", params: { pluginId: newInstancePluginId, name: instanceName.trim() || plugins.find((plugin) => plugin.manifest.id === newInstancePluginId)?.manifest.name || "新输入源" } }); setNewInstancePluginId(null); }); }}><label>插件<select aria-label="输入源插件" value={newInstancePluginId} onChange={(event) => setNewInstancePluginId(event.currentTarget.value)}>{plugins.map((plugin) => <option key={plugin.manifest.id} value={plugin.manifest.id}>{plugin.manifest.name} · {plugin.manifest.version}</option>)}</select></label><label>实例名称<input aria-label="输入源实例名称" maxLength={128} value={instanceName} onChange={(event) => setInstanceName(event.currentTarget.value)} placeholder="例如：桌面音频、远程触控" /></label><button className="primary-compact-button" disabled={disabled} type="submit">创建实例</button></form></Dialog>}
        {removal && <Dialog title="卸载插件" description="将停止所有关联实例并解除设备通道绑定。" onClose={() => setRemoval(null)}><p>{removal.plugin?.manifest.name}</p><label className="plugin-remove-data"><input type="checkbox" checked={deleteData} onChange={(event) => setDeleteData(event.currentTarget.checked)} />同时清除配置和数据</label><button className="primary-compact-button" disabled={disabled} type="button" onClick={remove}>卸载插件</button></Dialog>}
    </div>;
};
