import {
    FileText,
    DeviceMobile,
    Gear,
    Pulse,
    WarningCircle,
    Waveform,
    X,
} from "@phosphor-icons/react";
import { useCallback, useEffect, useRef, useState } from "react";

import { PairingModal } from "./components/PairingModal";
import { WindowChrome } from "./components/WindowChrome";
import { useHubSnapshot } from "./hooks/useHubSnapshot";
import {
    adjustIntensity,
    connectRelay,
    emergencyStop,
    isBrowserDemo,
    selectDevice,
    setDefaultSource,
    setDeviceChannelSource,
    setDeviceChannelSourceSync,
    setSyncAllDevices,
    startOutput,
    stopOutput,
    updateSafety,
} from "./lib/bridge";
import type { HubChannel, SafetyUpdate } from "./lib/contracts";
import {
    getDeviceWindowContext,
    openDeviceWindow,
} from "./lib/deviceWindows";
import { getErrorMessage } from "./lib/errors";
import { DashboardPage } from "./pages/DashboardPage";
import { DevicesPage } from "./pages/DevicesPage";
import { LogsPage } from "./pages/LogsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { SourcesPage } from "./pages/SourcesPage";

type PageId = "dashboard" | "sources" | "devices" | "logs" | "settings";

const navigation = [
    { id: "dashboard", label: "控制台", icon: Pulse },
    { id: "sources", label: "输入源", icon: Waveform },
    { id: "devices", label: "设备", icon: DeviceMobile },
    { id: "logs", label: "运行记录", icon: FileText },
    { id: "settings", label: "设置", icon: Gear },
] satisfies Array<{
    id: PageId;
    label: string;
    icon: typeof Pulse;
}>;

const windowContext = getDeviceWindowContext();

export default function App() {
    const { snapshot, loading, error: snapshotError, refresh } =
        useHubSnapshot();
    const [page, setPage] = useState<PageId>("dashboard");
    const [pendingAction, setPendingAction] = useState<string | null>(null);
    const [actionError, setActionError] = useState<string | null>(null);
    const [emergencyPending, setEmergencyPending] = useState(false);
    const [emergencyError, setEmergencyError] = useState<string | null>(null);
    const [dismissedExternalError, setDismissedExternalError] = useState<
        string | null
    >(null);
    const [pairingOpen, setPairingOpen] = useState(false);
    const emergencyGeneration = useRef(0);
    const demoMode = isBrowserDemo();

    const runAction = useCallback(
        async (name: string, action: () => Promise<void>) => {
            if (pendingAction || emergencyPending) {
                return;
            }
            setPendingAction(name);
            setActionError(null);
            try {
                await action();
                await refresh();
            } catch (actionFailure) {
                setActionError(getErrorMessage(actionFailure));
            } finally {
                setPendingAction(null);
            }
        },
        [emergencyPending, pendingAction, refresh],
    );

    const closePairing = useCallback(() => setPairingOpen(false), []);

    const runEmergencyStop = useCallback(async () => {
        if (emergencyPending) {
            return;
        }
        emergencyGeneration.current += 1;
        setEmergencyPending(true);
        setEmergencyError(null);
        try {
            await emergencyStop();
            await refresh();
        } catch (stopFailure) {
            setEmergencyError(getErrorMessage(stopFailure, "紧急停止失败"));
        } finally {
            setEmergencyPending(false);
        }
    }, [emergencyPending, refresh]);

    const handleAdjust = (
        channel: HubChannel,
        delta: number,
        deviceId: string,
    ) => {
        void runAction(`intensity-${deviceId}-${channel}`, () =>
            adjustIntensity(channel, delta, deviceId),
        );
    };

    const handleSafetySave = (update: SafetyUpdate) => {
        void runAction("safety", () => updateSafety(update));
    };

    const handleStartOutput = () => {
        const startGeneration = emergencyGeneration.current;
        void runAction("output", async () => {
            await startOutput();
            if (startGeneration !== emergencyGeneration.current) {
                await emergencyStop();
            }
        });
    };

    const runtimeError =
        snapshot?.output.lastError ?? snapshot?.connection.lastError ?? null;
    const externalError = snapshotError ?? runtimeError;
    const visibleExternalError =
        externalError === dismissedExternalError ? null : externalError;
    const appError =
        emergencyError ?? actionError ?? visibleExternalError;
    const detachedDevice = snapshot?.devices.find(
        (device) => device.controlId === windowContext.deviceId,
    );

    useEffect(() => {
        setDismissedExternalError(null);
    }, [externalError]);

    return (
        <div
            className={`app-shell ${windowContext.detached ? "app-shell-detached" : ""}`}
        >
            <WindowChrome
                title={
                    windowContext.detached
                        ? `DG-LAB Link · ${detachedDevice?.name ?? "设备窗口"}`
                        : undefined
                }
            />

            {demoMode && (
                <div className="demo-mode-banner" role="status">
                    演示模式 · 当前为浏览器模拟数据，不会连接 Relay、APP 或设备
                </div>
            )}

            {!windowContext.detached && <aside className="sidebar" aria-label="主导航">
                <nav>
                    {navigation.map((item) => {
                        const Icon = item.icon;
                        const active = page === item.id;
                        return (
                            <button
                                aria-current={active ? "page" : undefined}
                                className={active ? "nav-active" : ""}
                                key={item.id}
                                onClick={() => setPage(item.id)}
                                type="button"
                            >
                                <Icon
                                    aria-hidden="true"
                                    size={27}
                                    weight={active ? "regular" : "light"}
                                />
                                <span>{item.label}</span>
                            </button>
                        );
                    })}
                </nav>
            </aside>}

            <main className="app-main">
                {loading && !snapshot ? (
                    <div className="app-loading" role="status">
                        <Pulse aria-hidden="true" className="loading-pulse" size={34} />
                        <strong>正在连接中枢</strong>
                        <span>读取 Rust 后端状态…</span>
                    </div>
                ) : snapshot ? (
                    <>
                        {(windowContext.detached || page === "dashboard") && (
                            <DashboardPage
                                activeDeviceId={
                                    windowContext.detached
                                        ? windowContext.deviceId
                                        : snapshot.selectedDeviceId
                                }
                                detached={windowContext.detached}
                                onAdjust={handleAdjust}
                                onConnect={() =>
                                    void runAction("connect", connectRelay)
                                }
                                emergencyPending={emergencyPending}
                                onDetachDevice={(device) =>
                                    void runAction(
                                        `window-${device.controlId}`,
                                        () => openDeviceWindow(device),
                                    )
                                }
                                onEmergencyStop={() => void runEmergencyStop()}
                                onOpenPairing={() => setPairingOpen(true)}
                                onSelectDevice={(deviceId) =>
                                    void runAction(`device-${deviceId}`, () =>
                                        selectDevice(deviceId),
                                    )
                                }
                                onSetDeviceChannelSource={(deviceId, channel, sourceId) =>
                                    void runAction(`source-${deviceId}-${channel}`, () =>
                                        setDeviceChannelSource(
                                            deviceId,
                                            channel,
                                            sourceId,
                                        ),
                                    )
                                }
                                onSetDeviceChannelSourceSync={(deviceId, enabled) =>
                                    void runAction(`source-sync-${deviceId}`, () =>
                                        setDeviceChannelSourceSync(deviceId, enabled),
                                    )
                                }
                                onStartOutput={handleStartOutput}
                                onStopOutput={() =>
                                    void runAction("output", stopOutput)
                                }
                                pendingAction={pendingAction}
                                snapshot={snapshot}
                            />
                        )}
                        {!windowContext.detached && page === "sources" && (
                            <SourcesPage
                                onSetDefaultSource={(sourceId) =>
                                    void runAction("default-source", () =>
                                        setDefaultSource(sourceId),
                                    )
                                }
                                pendingAction={pendingAction}
                                snapshot={snapshot}
                            />
                        )}
                        {!windowContext.detached && page === "devices" && (
                            <DevicesPage
                                onSelectDevice={(deviceId) =>
                                    void runAction(`device-${deviceId}`, () =>
                                        selectDevice(deviceId),
                                    )
                                }
                                onSetSyncAllDevices={(enabled) =>
                                    void runAction("sync-devices", () =>
                                        setSyncAllDevices(enabled),
                                    )
                                }
                                onOpenPairing={() => setPairingOpen(true)}
                                pendingAction={pendingAction}
                                snapshot={snapshot}
                            />
                        )}
                        {!windowContext.detached && page === "logs" && (
                            <LogsPage snapshot={snapshot} />
                        )}
                        {!windowContext.detached && page === "settings" && (
                            <SettingsPage
                                onSaveSafety={handleSafetySave}
                                pendingAction={pendingAction}
                                snapshot={snapshot}
                            />
                        )}
                    </>
                ) : (
                    <div className="fatal-state" role="alert">
                        <WarningCircle aria-hidden="true" size={40} weight="light" />
                        <h1>无法读取中枢状态</h1>
                        <p>{snapshotError ?? "请确认 Tauri 后端已正常启动。"}</p>
                        <button
                            className="secondary-button"
                            onClick={() => void refresh()}
                            type="button"
                        >
                            重试
                        </button>
                    </div>
                )}

                {appError && snapshot && (
                    <div className="error-toast" role="alert">
                        <WarningCircle aria-hidden="true" size={19} weight="fill" />
                        <span>{appError}</span>
                        <button
                            aria-label="关闭错误提示"
                            onClick={() => {
                                setEmergencyError(null);
                                setActionError(null);
                                setDismissedExternalError(externalError);
                            }}
                            type="button"
                        >
                            <X aria-hidden="true" size={17} />
                        </button>
                    </div>
                )}
            </main>

            {pairingOpen && snapshot?.connection.pairingUrl && (
                <PairingModal
                    controllerId={snapshot.connection.controllerId}
                    onClose={closePairing}
                    pairingUrl={snapshot.connection.pairingUrl}
                />
            )}
        </div>
    );
}
