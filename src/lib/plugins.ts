import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./tauri";
import { pluginDemoCall } from "./bridge";
import type { SourceActionParams, SourceInputParams, UiDocument } from "./contracts";

export type PluginCommand = { command: string; params?: Record<string, unknown> };
export const pluginCall = async <T = unknown>(command: PluginCommand): Promise<T> => isTauriRuntime()
    ? invoke<T>("plugin_call", { command }) : pluginDemoCall(command) as Promise<T>;
export const getSourceUi = (sourceId: string, bindingId: string | undefined, surface: "settings" | "control") =>
    pluginCall<UiDocument>({ command: "get_source_ui", params: { sourceId, params: { bindingId, surface } } });
export const sourceAction = (sourceId: string, params: SourceActionParams) =>
    pluginCall({ command: "source_action", params: { sourceId, params } });
export const sourceInput = (sourceId: string, params: SourceInputParams) =>
    pluginCall({ command: "source_input", params: { sourceId, params } });
export const setSourceConfig = (sourceId: string, config: unknown, bindingId?: string) =>
    pluginCall({ command: "set_source_config", params: { sourceId, config, bindingId } });
export const choosePluginPackage = async (): Promise<string | null> => isTauriRuntime()
    ? invoke("choose_plugin_package") : "C:/演示/示例.dglabplugin";
export const choosePluginFile = async (): Promise<string | null> => isTauriRuntime()
    ? invoke("choose_plugin_file") : "C:/演示/输入文件.txt";
