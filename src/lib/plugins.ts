import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./tauri";
import { pluginDemoCall } from "./bridge";
import type { SourceActionParams, SourceInputParams, UiDocument } from "./contracts";
import type { ControlCommand } from "./generated/contracts";

export type PluginCommand = ControlCommand;
export const pluginCall = async <T = unknown>(command: PluginCommand): Promise<T> => isTauriRuntime()
    ? invoke<T>("plugin_call", { command }) : pluginDemoCall(command) as Promise<T>;
const uiCache = new Map<string, { sourceId: string; promise: Promise<UiDocument>; pending: boolean; expires: number }>();
const uiQueues = new Map<string, { tail: Promise<void>; count: number; generation: number }>();
let queuedUiRequests = 0;

export const invalidateSourceUi = (sourceId: string) => {
    for (const [key, entry] of uiCache) if (entry.sourceId === sourceId) uiCache.delete(key);
    const queue = uiQueues.get(sourceId);
    if (queue) queue.generation += 1;
};

export const getSourceUi = (sourceId: string, bindingId: string | undefined, surface: "settings" | "control", revision = "") => {
    const key = JSON.stringify([sourceId, bindingId, surface, revision]);
    const cached = uiCache.get(key);
    if (cached && (cached.pending || cached.expires > Date.now())) return cached.promise;
    if (cached) uiCache.delete(key);
    for (const [oldKey, entry] of uiCache) if (!entry.pending && entry.expires <= Date.now()) uiCache.delete(oldKey);
    const queue = uiQueues.get(sourceId) ?? { tail: Promise.resolve(), count: 0, generation: 0 };
    if (queue.count >= 8 || queuedUiRequests >= 64) return Promise.reject({ code: "queue_busy", message: "插件界面读取队列已满" });
    if (uiCache.size >= 128) {
        const oldest = [...uiCache].find(([, entry]) => !entry.pending);
        if (oldest) uiCache.delete(oldest[0]);
        else return Promise.reject({ code: "queue_busy", message: "插件界面缓存已满" });
    }
    queue.count += 1;
    queuedUiRequests += 1;
    const accepted = queue.generation;
    const promise = queue.tail.then(() => {
        if (accepted !== queue.generation) throw { code: "request_cancelled", message: "插件界面读取已取消" };
        return pluginCall<UiDocument>({ command: "get_source_ui", params: { sourceId, params: { bindingId, surface } } });
    });
    const entry = { sourceId, promise, pending: true, expires: 0 };
    uiCache.set(key, entry);
    queue.tail = promise.then(() => {}, () => {}).finally(() => {
        queue.count -= 1;
        queuedUiRequests -= 1;
        entry.pending = false;
        entry.expires = Date.now() + 150;
        if (queue.count === 0) uiQueues.delete(sourceId);
    });
    uiQueues.set(sourceId, queue);
    void promise.catch(() => { if (uiCache.get(key) === entry) uiCache.delete(key); });
    return promise;
};
export const sourceAction = (sourceId: string, params: SourceActionParams) =>
    pluginCall({ command: "source_action", params: { sourceId, params } });
export const sourceInput = (sourceId: string, params: SourceInputParams) =>
    pluginCall({ command: "source_input", params: { sourceId, params } });
export const setSourceConfig = (sourceId: string, config: unknown, expectedRevision: number, bindingId?: string) =>
    pluginCall({ command: "set_source_config", params: { sourceId, config, bindingId, expectedRevision } });
export const choosePluginPackage = async (): Promise<string | null> => isTauriRuntime()
    ? invoke("choose_plugin_package") : "C:/演示/示例.dglabplugin";
export const choosePluginFile = async (): Promise<string | null> => isTauriRuntime()
    ? invoke("choose_plugin_file") : "C:/演示/输入文件.txt";
export const choosePluginDestination = async (suggestedName?: string, extensions: string[] = []): Promise<string | null> => isTauriRuntime()
    ? invoke("choose_plugin_destination", { suggestedName, extensions }) : `C:/演示/${suggestedName ?? "输入文件"}`;
