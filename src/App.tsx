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
import { SafetyActionBar } from "./components/SafetyActionBar";
import type { DeviceViewTab } from "./components/DeviceTabs";
import { WindowChrome } from "./components/WindowChrome";
import { useHubSnapshot } from "./hooks/useHubSnapshot";
import {
    adjustIntensity,
    audioControl,
    connectRelay,
    connectTransport,
    disconnectConnection,
    refreshConnectionPairing,
    setRelayEndpoint,
    scanBluetooth,
    connectBluetooth,
    disconnectBluetooth,
    emergencyStop,
    setBluetoothConfig,
    deleteCustomWaveform,
    getAppPreferences,
    importCustomWaveforms,
    isBrowserDemo,
    reorderCustomWaveforms,
    selectDevice,
    selectCustomWaveform,
    setAutoStart,
    setDefaultSource,
    setFixedWaveform,
    setAudioConfig,
    setTouchConfig,
    setCloseToTray,
    setDeviceChannelSource,
    setDeviceChannelSourceSync,
    setSyncAllDevices,
    setStartMinimized,
    startOutput,
    stopOutput,
    updateSafety,
} from "./lib/bridge";
import type {
    AppPreferences,
    HubChannel,
    SafetyUpdate,
} from "./lib/contracts";
import {
    focusDeviceWindow,
    getDeviceWindowContext,
    listenForDeviceTabReturn,
    openDeviceWindow,
    returnDeviceTabToMain,
    updateDetachedWindowDevice,
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

interface DashboardTabsState {
    activeTabId: string;
    tabs: DeviceViewTab[];
}

let nextDeviceViewTabId = 0;

const createDeviceViewTab = (deviceId: string | null = null): DeviceViewTab => ({
    id: `device-view-${Date.now()}-${++nextDeviceViewTabId}`,
    deviceId,
});

export default function App() {
    const { snapshot, loading, error: snapshotError, refresh } =
        useHubSnapshot();
    const [page, setPage] = useState<PageId>("dashboard");
    const [appPreferences, setAppPreferences] = useState<AppPreferences>({
        closeToTray: true,
        autoStart: false,
        startMinimized: true,
    });
    const [dashboardTabs, setDashboardTabs] = useState<DashboardTabsState>(() => {
        const initialTab = createDeviceViewTab();
        return {
            activeTabId: initialTab.id,
            tabs: [initialTab],
        };
    });
    const [detachedTabs, setDetachedTabs] = useState<DeviceViewTab[]>([]);
    const [detachedDeviceId, setDetachedDeviceId] = useState(
        windowContext.deviceId,
    );
    const [pendingAction, setPendingAction] = useState<string | null>(null);
    const [overviewDeviceId, setOverviewDeviceId] = useState<string | null>(null);
    const [syncBaseDeviceId, setSyncBaseDeviceId] = useState<string | null>(null);
    const [emergencyPending, setEmergencyPending] = useState(false);
    const emergencyInFlight = useRef(false);
    const actionEpoch = useRef(0);
    const [actionError, setActionError] = useState<string | null>(null);
    const [dismissedExternalError, setDismissedExternalError] = useState<
        string | null
    >(null);
    const [pairingConnectionId, setPairingConnectionId] = useState<string | null>(null);
    const dashboardTabsInitialized = useRef(false);
    const demoMode = isBrowserDemo();

    const runAction = useCallback(
        async (name: string, action: () => Promise<void>) => {
            if (pendingAction || emergencyInFlight.current) {
                return;
            }
            setPendingAction(name);
            setActionError(null);
            const acceptedEpoch = actionEpoch.current;
            try {
                await action();
                if (acceptedEpoch === actionEpoch.current) {
                    await refresh();
                }
            } catch (actionFailure) {
                if (acceptedEpoch === actionEpoch.current) {
                    setActionError(getErrorMessage(actionFailure));
                }
            } finally {
                setPendingAction(null);
            }
        },
        [pendingAction, refresh],
    );

    const closePairing = useCallback(() => setPairingConnectionId(null), []);
    const pairingConnection = pairingConnectionId === "ws-v4"
        ? snapshot?.connection
        : snapshot?.connections.find((connection) => connection.connectionId === pairingConnectionId);

    const handleAdjust = (
        channel: HubChannel,
        delta: number,
        deviceId: string,
    ) => {
        void runAction(`intensity-${deviceId}-${channel}`, () =>
            adjustIntensity(channel, delta, deviceId),
        );
    };

    const handleStartOutput = (deviceId: string) => {
        void runAction(`output-${deviceId}`, () => startOutput(deviceId));
    };

    const handleStopOutput = (deviceId: string) => {
        void runAction(`output-${deviceId}`, () => stopOutput(deviceId));
    };

    const handleEmergencyStop = async () => {
        if (emergencyInFlight.current) {
            return;
        }
        emergencyInFlight.current = true;
        actionEpoch.current += 1;
        setEmergencyPending(true);
        setActionError(null);
        try {
            await emergencyStop();
            await refresh();
        } catch (failure) {
            setActionError(getErrorMessage(failure));
        } finally {
            emergencyInFlight.current = false;
            setEmergencyPending(false);
        }
    };

    const handleSafetySave = (update: SafetyUpdate) => {
        void runAction("safety", () => updateSafety(update));
    };

    const handleCloseToTrayChange = (enabled: boolean) => {
        if (pendingAction || emergencyInFlight.current) {
            return;
        }
        const previous = appPreferences;
        setAppPreferences({ ...previous, closeToTray: enabled });
        void runAction("close-to-tray", async () => {
            try {
                setAppPreferences(await setCloseToTray(enabled));
            } catch (error) {
                setAppPreferences(previous);
                throw error;
            }
        });
    };

    const handleAutoStartChange = (enabled: boolean) => {
        if (pendingAction || emergencyInFlight.current) {
            return;
        }
        const previous = appPreferences;
        setAppPreferences({ ...previous, autoStart: enabled });
        void runAction("auto-start", async () => {
            try {
                setAppPreferences(await setAutoStart(enabled));
            } catch (error) {
                setAppPreferences(previous);
                throw error;
            }
        });
    };

    const handleStartMinimizedChange = (enabled: boolean) => {
        if (pendingAction || emergencyInFlight.current) {
            return;
        }
        const previous = appPreferences;
        setAppPreferences({ ...previous, startMinimized: enabled });
        void runAction("start-minimized", async () => {
            try {
                setAppPreferences(await setStartMinimized(enabled));
            } catch (error) {
                setAppPreferences(previous);
                throw error;
            }
        });
    };

    const handleSelectTab = (tabId: string) => {
        if (dashboardTabs.activeTabId === tabId) {
            return;
        }
        const tab = dashboardTabs.tabs.find((candidate) => candidate.id === tabId);
        setDashboardTabs((current) => ({
            ...current,
            activeTabId: tabId,
        }));
        const deviceId = tab?.deviceId;
        if (deviceId) {
            void runAction(`device-${deviceId}`, () => selectDevice(deviceId));
        }
    };

    const handleSelectDevice = (deviceId: string) => {
        if (windowContext.detached) {
            setDetachedDeviceId(deviceId);
            updateDetachedWindowDevice(deviceId);
        } else {
            setDashboardTabs((current) => ({
                ...current,
                tabs: current.tabs.map((tab) =>
                    tab.id === current.activeTabId ? { ...tab, deviceId } : tab,
                ),
            }));
        }
        void runAction(`device-${deviceId}`, () => selectDevice(deviceId));
    };

    const handleNewDeviceTab = () => {
        const tab = createDeviceViewTab();
        setDashboardTabs((current) => ({
            activeTabId: tab.id,
            tabs: [...current.tabs, tab],
        }));
    };

    const handleCloseDeviceTab = (tabId: string) => {
        const closingIndex = dashboardTabs.tabs.findIndex(
            (tab) => tab.id === tabId,
        );
        if (closingIndex < 0) {
            return;
        }

        const closingActiveTab = dashboardTabs.activeTabId === tabId;
        const remainingTabs = dashboardTabs.tabs.filter(
            (tab) => tab.id !== tabId,
        );
        if (remainingTabs.length === 0) {
            setDashboardTabs({
                activeTabId: "",
                tabs: [],
            });
            return;
        }

        const nextActiveTab = closingActiveTab
            ? remainingTabs[Math.min(closingIndex, remainingTabs.length - 1)]
            : remainingTabs.find(
                  (tab) => tab.id === dashboardTabs.activeTabId,
              ) ?? remainingTabs[0];
        setDashboardTabs({
            activeTabId: nextActiveTab.id,
            tabs: remainingTabs,
        });
        const nextDeviceId = nextActiveTab.deviceId;
        if (closingActiveTab && nextDeviceId) {
            void runAction(`device-${nextDeviceId}`, () =>
                selectDevice(nextDeviceId),
            );
        }
    };

    const handleMoveDeviceTab = (tabId: string, targetTabId: string) => {
        setDashboardTabs((current) => {
            const tabIndex = current.tabs.findIndex(
                (tab) => tab.id === tabId,
            );
            const targetIndex = current.tabs.findIndex(
                (tab) => tab.id === targetTabId,
            );
            if (tabIndex < 0 || targetIndex < 0 || tabIndex === targetIndex) {
                return current;
            }

            const tabs = [...current.tabs];
            const [movedTab] = tabs.splice(tabIndex, 1);
            tabs.splice(targetIndex, 0, movedTab);
            return {
                ...current,
                tabs,
            };
        });
    };

    const handleOpenDeviceInNewTab = (deviceId: string) => {
        const tab = createDeviceViewTab(deviceId);
        setDashboardTabs((current) => ({
            activeTabId: tab.id,
            tabs: [...current.tabs, tab],
        }));
        setPage("dashboard");
        void runAction(`device-${deviceId}`, () => selectDevice(deviceId));
    };

    const handleOpenDeviceInNewWindow = (deviceId: string) => {
        const device = snapshot?.devices.find(
            (candidate) => candidate.controlId === deviceId,
        );
        if (!device) {
            setActionError("设备不存在或已断开");
            return;
        }
        const tab = createDeviceViewTab(deviceId);
        void runAction(`window-${tab.id}`, async () => {
            await openDeviceWindow({
                device,
                onClosed: () =>
                    setDetachedTabs((current) =>
                        current.filter((candidate) => candidate.id !== tab.id),
                    ),
                tabId: tab.id,
            });
            setDetachedTabs((current) => [...current, tab]);
        });
    };

    const handleDetachTab = (tab: DeviceViewTab) => {
        const device = snapshot?.devices.find(
            (candidate) => candidate.controlId === tab.deviceId,
        );
        void runAction(`window-${tab.id}`, async () => {
            await openDeviceWindow({
                device,
                onClosed: () =>
                    setDetachedTabs((current) =>
                        current.filter((candidate) => candidate.id !== tab.id),
                    ),
                tabId: tab.id,
            });
            setDetachedTabs((current) => [...current, tab]);
            setDashboardTabs((current) => {
                const detachedIndex = current.tabs.findIndex(
                    (candidate) => candidate.id === tab.id,
                );
                const remainingTabs = current.tabs.filter(
                    (candidate) => candidate.id !== tab.id,
                );
                if (remainingTabs.length === 0) {
                    return {
                        activeTabId: "",
                        tabs: [],
                    };
                }
                return {
                    activeTabId:
                        current.activeTabId === tab.id
                            ? remainingTabs[
                                  Math.min(detachedIndex, remainingTabs.length - 1)
                              ].id
                            : current.activeTabId,
                    tabs: remainingTabs,
                };
            });
        });
    };

    const handleFocusDetachedTab = (tabId: string) => {
        void runAction(`window-${tabId}`, async () => {
            const focused = await focusDeviceWindow(tabId);
            if (!focused) {
                setDetachedTabs((current) =>
                    current.filter((candidate) => candidate.id !== tabId),
                );
                throw new Error("标签页所在窗口已关闭");
            }
        });
    };

    const handleReturnToMain = () => {
        const tabId = windowContext.tabId;
        if (!tabId) {
            setActionError("当前窗口缺少标签页标识，无法移回主窗口");
            return;
        }
        void runAction(`return-${tabId}`, () =>
            returnDeviceTabToMain({
                deviceId: detachedDeviceId,
                tabId,
            }),
        );
    };

    const runtimeError =
        snapshot?.output.lastError ?? snapshot?.connection.lastError ?? snapshot?.connections.find((connection) => connection.lastError)?.lastError ?? null;
    const externalError = snapshotError ?? runtimeError;
    const visibleExternalError =
        externalError === dismissedExternalError ? null : externalError;
    const appError = actionError ?? visibleExternalError;
    const activeDashboardTab = dashboardTabs.tabs.find(
        (tab) => tab.id === dashboardTabs.activeTabId,
    );
    const detachedDevice = snapshot?.devices.find(
        (device) => device.controlId === detachedDeviceId,
    );
    const detachedWindowTitle = detachedDevice?.name ??
        (detachedDeviceId ? "设备已离线" : "新标签页");
    const activeDashboardDeviceId = windowContext.detached
        ? detachedDeviceId
        : dashboardTabsInitialized.current
          ? activeDashboardTab?.deviceId ?? null
          : snapshot?.selectedDeviceId ?? snapshot?.devices[0]?.controlId ?? null;
    const selectedOverviewDeviceId = snapshot?.devices.some((device) => device.controlId === overviewDeviceId)
        ? overviewDeviceId
        : snapshot?.devices[0]?.controlId ?? null;
    const safetyDeviceId = !windowContext.detached && page === "devices"
        ? selectedOverviewDeviceId
        : activeDashboardDeviceId;
    const ordinaryPendingAction = emergencyPending ? "emergency-stop" : pendingAction;

    useEffect(() => {
        if (windowContext.detached) {
            return;
        }

        let disposed = false;
        let stopListening: (() => void) | undefined;
        void listenForDeviceTabReturn((tab) => {
            setDetachedTabs((current) =>
                current.filter((candidate) => candidate.id !== tab.tabId),
            );
            setDashboardTabs((current) => {
                const existing = current.tabs.some(
                    (candidate) => candidate.id === tab.tabId,
                );
                return {
                    activeTabId: tab.tabId,
                    tabs: existing
                        ? current.tabs.map((candidate) =>
                              candidate.id === tab.tabId
                                  ? { ...candidate, deviceId: tab.deviceId }
                                  : candidate,
                          )
                        : [
                              ...current.tabs,
                              { id: tab.tabId, deviceId: tab.deviceId },
                          ],
                };
            });
            setPage("dashboard");
        })
            .then((unlisten) => {
                if (disposed) {
                    unlisten();
                } else {
                    stopListening = unlisten;
                }
            })
            .catch((error) => {
                if (!disposed) {
                    setActionError(
                        getErrorMessage(error, "监听标签页窗口失败"),
                    );
                }
            });

        return () => {
            disposed = true;
            stopListening?.();
        };
    }, []);

    useEffect(() => {
        setDismissedExternalError(null);
    }, [externalError]);

    useEffect(() => {
        let active = true;
        void getAppPreferences()
            .then((preferences) => {
                if (active) {
                    setAppPreferences(preferences);
                }
            })
            .catch((error) => {
                if (active) {
                    setActionError(
                        getErrorMessage(error, "读取应用偏好设置失败"),
                    );
                }
            });
        return () => {
            active = false;
        };
    }, []);

    useEffect(() => {
        if (
            windowContext.detached ||
            !snapshot ||
            dashboardTabsInitialized.current
        ) {
            return;
        }
        dashboardTabsInitialized.current = true;
        const initialDeviceId =
            snapshot.selectedDeviceId ?? snapshot.devices[0]?.controlId ?? null;
        if (!initialDeviceId) {
            return;
        }
        setDashboardTabs((current) => ({
            ...current,
            tabs: current.tabs.map((tab) =>
                tab.id === current.activeTabId
                    ? { ...tab, deviceId: initialDeviceId }
                    : tab,
            ),
        }));
    }, [snapshot]);

    return (
        <div
            className={`app-shell ${windowContext.detached ? "app-shell-detached" : ""}`}
        >
            <WindowChrome
                disableReturnToMain={pendingAction !== null}
                onReturnToMain={
                    windowContext.detached ? handleReturnToMain : undefined
                }
                title={
                    windowContext.detached
                        ? `DG-LAB Link · ${detachedWindowTitle}`
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
                                activeTabId={
                                    dashboardTabs.activeTabId
                                }
                                activeDeviceId={
                                    activeDashboardDeviceId
                                }
                                detached={windowContext.detached}
                                outputControlsInFooter
                                detachedTabs={detachedTabs}
                                onAdjust={handleAdjust}
                                onCloseTab={handleCloseDeviceTab}
                                onConnect={() =>
                                    void runAction("connect", connectRelay)
                                }
                                onDetachTab={handleDetachTab}
                                onMoveTab={handleMoveDeviceTab}
                                onNewDeviceTab={handleNewDeviceTab}
                                onFocusDetachedTab={handleFocusDetachedTab}
                                onOpenPairing={() => setPairingConnectionId("ws-v4")}
                                onSelectDevice={handleSelectDevice}
                                onSelectCustomWaveform={(deviceId, channel, presetId) =>
                                    void runAction(
                                        `select-custom-waveform-${deviceId}-${channel}`,
                                        () =>
                                            selectCustomWaveform(
                                                deviceId,
                                                channel,
                                                presetId,
                                            ),
                                    )
                                }
                                onSelectTab={handleSelectTab}
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
                                onStopOutput={handleStopOutput}
                                onAudioControl={(action) => runAction("audio-control", () => audioControl(action))}
                                onSetAudioConfig={(deviceId, channel, config) => void runAction(`audio-config-${deviceId}-${channel}`, () => setAudioConfig(deviceId, channel, config))}
                                onSetFixedWaveform={(deviceId, channel, config) =>
                                    void runAction(
                                        `fixed-waveform-${deviceId}-${channel}`,
                                        () =>
                                            setFixedWaveform(
                                                deviceId,
                                                channel,
                                                config,
                                            ),
                                    )
                                }
                                pendingAction={ordinaryPendingAction}
                                snapshot={snapshot}
                                tabs={dashboardTabs.tabs}
                            />
                        )}
                        {!windowContext.detached && page === "sources" && (
                            <SourcesPage
                                onSetTouchConfig={(config) => void runAction("touch-config", () => setTouchConfig(config))}
                                onAudioControl={(action) => runAction("audio-control", () => audioControl(action))}
                                onDeleteCustomWaveform={(presetId) =>
                                    void runAction("delete-custom-waveform", () =>
                                        deleteCustomWaveform(presetId),
                                    )
                                }
                                onError={setActionError}
                                onImportCustomWaveforms={(configs) =>
                                    void runAction("import-custom-waveforms", () =>
                                        importCustomWaveforms(configs),
                                    )
                                }
                                onReorderCustomWaveforms={(presetIds) =>
                                    void runAction("reorder-custom-waveforms", () =>
                                        reorderCustomWaveforms(presetIds),
                                    )
                                }
                                pendingAction={ordinaryPendingAction}
                                snapshot={snapshot}
                            />
                        )}
                        {!windowContext.detached && page === "devices" && (
                            <DevicesPage
                                syncBaseDeviceId={syncBaseDeviceId ?? snapshot.devices.find((device) => device.initialization === "ready")?.controlId ?? null}
                                onSelectSyncBaseDevice={setSyncBaseDeviceId}
                                selectedDeviceId={selectedOverviewDeviceId}
                                onSelectDevice={setOverviewDeviceId}
                                onStopOutput={handleStopOutput}
                                onOpenInNewTab={handleOpenDeviceInNewTab}
                                onOpenInNewWindow={handleOpenDeviceInNewWindow}
                                onSetSyncAllDevices={(enabled, baseDeviceId) =>
                                    void runAction("sync-devices", () =>
                                        setSyncAllDevices(enabled, baseDeviceId),
                                    )
                                }
                                onOpenPairing={(connectionId = "ws-v4") => setPairingConnectionId(connectionId)}
                                onConnect={(transport) => void runAction(`connect-${transport}`, () => connectTransport(transport))}
                                onDisconnect={(connectionId) => void runAction(`disconnect-${connectionId}`, () => disconnectConnection(connectionId))}
                                onRefreshPairing={(connectionId) => void runAction(`pairing-${connectionId}`, () => refreshConnectionPairing(connectionId))}
                                onSetEndpoint={(transport, endpoint) => void runAction(`endpoint-${transport}`, () => setRelayEndpoint(transport, endpoint))}
                                onScanBluetooth={() => void runAction("bluetooth-scan", async () => { await scanBluetooth(); })}
                                onConnectBluetooth={(deviceId) => void runAction(`bluetooth-connect-${deviceId}`, () => connectBluetooth(deviceId))}
                                onDisconnectBluetooth={(deviceId) => void runAction(`bluetooth-disconnect-${deviceId}`, () => disconnectBluetooth(deviceId))}
                                onSaveBluetoothConfig={(deviceId, config) => void runAction(`bluetooth-config-${deviceId}`, () => setBluetoothConfig(deviceId, config))}
                                pendingAction={ordinaryPendingAction}
                                snapshot={snapshot}
                            />
                        )}
                        {!windowContext.detached && page === "logs" && (
                            <LogsPage snapshot={snapshot} />
                        )}
                        {!windowContext.detached && page === "settings" && (
                            <SettingsPage
                                appPreferences={appPreferences}
                                onSetAutoStart={handleAutoStartChange}
                                onSetCloseToTray={handleCloseToTrayChange}
                                onSetDefaultSource={(sourceId) =>
                                    void runAction("default-source", () =>
                                        setDefaultSource(sourceId),
                                    )
                                }
                                onSetStartMinimized={handleStartMinimizedChange}
                                onSaveSafety={handleSafetySave}
                                pendingAction={ordinaryPendingAction}
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

            <SafetyActionBar
                deviceId={safetyDeviceId}
                emergencyPending={emergencyPending}
                onEmergencyStop={() => void handleEmergencyStop()}
                onStartOutput={handleStartOutput}
                onStopOutput={handleStopOutput}
                pendingAction={pendingAction}
                showOutputControl={windowContext.detached || page === "dashboard"}
                snapshot={snapshot}
            />

            {pairingConnectionId && pairingConnection?.pairingUrl && (
                <PairingModal
                    controllerId={pairingConnection.controllerId}
                    protocol={pairingConnectionId === "ws-v3" ? "v3" : "v4"}
                    onClose={closePairing}
                    pairingUrl={pairingConnection.pairingUrl}
                />
            )}
        </div>
    );
}
