import {
    Check,
    Copy,
    Globe,
    ShieldCheck,
    PlugsConnected,
    SpinnerGap,
    Tray,
    Waveform,
} from "@phosphor-icons/react";
import { useEffect, useState } from "react";

import { PageHeader } from "../components/PageHeader";
import type {
    AppPreferences,
    HubSnapshot,
    RuntimeInfo,
    SafetyUpdate,
} from "../lib/contracts";
import { getMcpConfig, getRuntimeInfo } from "../lib/bridge";
import { getErrorMessage } from "../lib/errors";

const RuntimeSettingsCard = () => {
    const [info, setInfo] = useState<RuntimeInfo | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [copying, setCopying] = useState(false);
    const [copyMessage, setCopyMessage] = useState<string | null>(null);

    useEffect(() => {
        let active = true;
        let timer: ReturnType<typeof setTimeout> | undefined;
        const refresh = async () => {
            try {
                const current = await getRuntimeInfo();
                if (active) {
                    setInfo(current);
                    setError(null);
                }
            } catch (refreshError) {
                if (active) {
                    setInfo(null);
                    setError(getErrorMessage(refreshError, "无法读取共享核心状态"));
                }
            } finally {
                if (active) {
                    setLoading(false);
                    timer = setTimeout(() => void refresh(), 3000);
                }
            }
        };
        void refresh();
        return () => {
            active = false;
            clearTimeout(timer);
        };
    }, []);

    const copyToken = async () => {
        setCopying(true);
        setCopyMessage(null);
        try {
            const config = await getMcpConfig();
            await navigator.clipboard.writeText(config.token);
            setCopyMessage("连接令牌已复制");
        } catch (copyError) {
            setCopyMessage(getErrorMessage(copyError, "无法复制连接令牌"));
        } finally {
            setCopying(false);
        }
    };

    return (
        <section className="settings-card">
            <div className="settings-heading">
                <div className="settings-icon">
                    <PlugsConnected aria-hidden="true" size={25} weight="light" />
                </div>
                <div>
                    <h2>共享核心与 MCP</h2>
                    <p>GUI、CLI 和 MCP 共用设备会话；持有者全部退出后核心停止。</p>
                </div>
            </div>
            <div className="setting-row">
                <div className="setting-row-copy"><strong>核心状态</strong></div>
                <span role="status">
                    {loading ? "正在读取" : error ? "核心已断开" : info ? "运行中" : "浏览器演示"}
                </span>
            </div>
            {info && (
                <>
                    <div className="setting-row">
                        <div className="setting-row-copy"><strong>当前持有者</strong></div>
                        <span>{info.holderCount} 个</span>
                    </div>
                    <div className="setting-row">
                        <div className="setting-row-copy"><strong>MCP 地址</strong></div>
                        <code className="setting-endpoint">{info.mcpUrl}</code>
                    </div>
                </>
            )}
            <div className="setting-row">
                <div className="setting-row-copy">
                    <strong>连接令牌</strong>
                    <span>客户端使用 Bearer 令牌连接本机 Streamable HTTP MCP。</span>
                </div>
                <button className="secondary-button" disabled={!info || copying} onClick={() => void copyToken()} type="button">
                    <Copy aria-hidden="true" size={18} />
                    {copying ? "正在复制" : "复制连接令牌"}
                </button>
            </div>
            {error && <p className="relay-note" role="alert">{error}</p>}
            {copyMessage && <p className="relay-note" role="status">{copyMessage}</p>}
        </section>
    );
};

interface SettingsPageProps {
    appPreferences: AppPreferences;
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onSetAutoStart: (enabled: boolean) => void;
    onSetCloseToTray: (enabled: boolean) => void;
    onSetDefaultSource: (sourceId: string | null) => void;
    onSetStartMinimized: (enabled: boolean) => void;
    onSaveSafety: (update: SafetyUpdate) => void;
}

const relayStateLabels = {
    disconnected: "Relay 未连接",
    connecting: "正在连接 Relay",
    waiting: "Relay 已连接",
    connected: "Relay 已连接",
    error: "Relay 连接异常",
};

export const SettingsPage = ({
    appPreferences,
    snapshot,
    pendingAction,
    onSetAutoStart,
    onSetCloseToTray,
    onSetDefaultSource,
    onSetStartMinimized,
    onSaveSafety,
}: SettingsPageProps) => {
    const [connectionTimeoutEnabled, setConnectionTimeoutEnabled] = useState(
        snapshot.safety.connectionTimeoutEnabled,
    );
    const [connectionTimeoutMinutes, setConnectionTimeoutMinutes] = useState(
        String(snapshot.safety.connectionTimeoutMinutes),
    );
    const [allowAppIntensityControl, setAllowAppIntensityControl] = useState(
        snapshot.safety.allowAppIntensityControl,
    );

    useEffect(() => {
        setConnectionTimeoutEnabled(snapshot.safety.connectionTimeoutEnabled);
        setConnectionTimeoutMinutes(String(snapshot.safety.connectionTimeoutMinutes));
        setAllowAppIntensityControl(
            snapshot.safety.allowAppIntensityControl,
        );
    }, [
        snapshot.safety.allowAppIntensityControl,
        snapshot.safety.connectionTimeoutEnabled,
        snapshot.safety.connectionTimeoutMinutes,
    ]);

    const timeoutMinutes = Number(connectionTimeoutMinutes);
    const timeoutMinutesValid =
        /^\d+$/.test(connectionTimeoutMinutes) &&
        Number.isInteger(timeoutMinutes) &&
        timeoutMinutes >= 1 &&
        timeoutMinutes <= 1440;
    const dirty =
        connectionTimeoutEnabled !== snapshot.safety.connectionTimeoutEnabled ||
        connectionTimeoutMinutes !== String(snapshot.safety.connectionTimeoutMinutes) ||
        allowAppIntensityControl !==
            snapshot.safety.allowAppIntensityControl;

    return (
        <div className="standard-page settings-page">
            <PageHeader
                description="管理应用行为、连接超时与 Relay 连接信息。"
                eyebrow="PREFERENCES"
                title="设置"
            />

            <section className="settings-card">
                <div className="settings-heading">
                    <div className="settings-icon">
                        <Tray aria-hidden="true" size={25} weight="light" />
                    </div>
                    <div>
                        <h2>应用行为</h2>
                        <p>控制应用启动与主窗口关闭行为，修改后会自动保存。</p>
                    </div>
                </div>

                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>开机自启</strong>
                        <span>登录系统后自动启动 DG-LAB Link。</span>
                    </div>
                    <label className="toggle-switch">
                        <span className="visually-hidden">开机自启</span>
                        <input
                            aria-label="开机自启"
                            checked={appPreferences.autoStart}
                            disabled={pendingAction !== null}
                            onChange={(event) =>
                                onSetAutoStart(event.currentTarget.checked)
                            }
                            type="checkbox"
                        />
                        <span className="toggle-track" aria-hidden="true">
                            <span />
                        </span>
                    </label>
                </div>

                {appPreferences.autoStart && (
                    <div className="setting-row">
                        <div className="setting-row-copy">
                            <strong>以最小化形式启动</strong>
                            <span>
                                仅在开机自启时生效；启动后不打开主窗口，只在系统托盘中运行。
                            </span>
                        </div>
                        <label className="toggle-switch">
                            <span className="visually-hidden">以最小化形式启动</span>
                            <input
                                aria-label="以最小化形式启动"
                                checked={appPreferences.startMinimized}
                                disabled={pendingAction !== null}
                                onChange={(event) =>
                                    onSetStartMinimized(
                                        event.currentTarget.checked,
                                    )
                                }
                                type="checkbox"
                            />
                            <span className="toggle-track" aria-hidden="true">
                                <span />
                            </span>
                        </label>
                    </div>
                )}

                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>关闭主窗口时保留在托盘</strong>
                        <span>
                            开启后关闭按钮只隐藏主窗口，Relay、设备连接与波形输出会继续运行；可从托盘重新打开或退出。
                        </span>
                    </div>
                    <label className="toggle-switch">
                        <span className="visually-hidden">
                            关闭主窗口时保留在托盘
                        </span>
                        <input
                            aria-label="关闭主窗口时保留在托盘"
                            checked={appPreferences.closeToTray}
                            disabled={pendingAction !== null}
                            onChange={(event) =>
                                onSetCloseToTray(event.currentTarget.checked)
                            }
                            type="checkbox"
                        />
                        <span className="toggle-track" aria-hidden="true">
                            <span />
                        </span>
                    </label>
                </div>
            </section>

            <section className="settings-card">
                <div className="settings-heading">
                    <div className="settings-icon">
                        <Waveform aria-hidden="true" size={25} weight="light" />
                    </div>
                    <div>
                        <h2>输入源默认值</h2>
                        <p>设置新接入设备的 A/B 通道初始输入源。</p>
                    </div>
                </div>

                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>新设备默认输入源</strong>
                        <span>选择“每次询问”时，新设备接入后由仪表盘决定。</span>
                    </div>
                    <select
                        aria-label="选择默认输入源"
                        className="setting-select"
                        disabled={pendingAction !== null}
                        id="default-source-select"
                        onChange={(event) =>
                            onSetDefaultSource(event.currentTarget.value || null)
                        }
                        value={snapshot.defaultSourceId ?? ""}
                    >
                        <option value="">每次询问</option>
                        {snapshot.sources.map((source) => (
                            <option key={source.id} value={source.id}>
                                {source.name}{!source.enabled ? "（未配置）" : ""}
                            </option>
                        ))}
                    </select>
                </div>
            </section>

            <section className="settings-card">
                <div className="settings-heading">
                    <div className="settings-icon">
                        <ShieldCheck aria-hidden="true" size={25} weight="light" />
                    </div>
                    <div>
                        <h2>安全限制</h2>
                        <p>
                            连接超时自动断开默认关闭，设置会在重启后保留。A/B 强度上限以设备上报值为准。
                        </p>
                    </div>
                </div>

                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>连接超时自动断开</strong>
                        <span>Relay 建立连接后开始计时；到期停止所有输出并断开所有连接，不自动重连。</span>
                    </div>
                    <label className="toggle-switch">
                        <span className="visually-hidden">连接超时自动断开</span>
                        <input
                            aria-label="连接超时自动断开"
                            checked={connectionTimeoutEnabled}
                            disabled={pendingAction !== null}
                            onChange={(event) => {
                                const enabled = event.currentTarget.checked;
                                setConnectionTimeoutEnabled(enabled);
                                if (!enabled && !timeoutMinutesValid) {
                                    setConnectionTimeoutMinutes(
                                        String(snapshot.safety.connectionTimeoutMinutes),
                                    );
                                }
                            }}
                            type="checkbox"
                        />
                        <span className="toggle-track" aria-hidden="true">
                            <span />
                        </span>
                    </label>
                </div>

                {connectionTimeoutEnabled && (
                    <div className="setting-row">
                        <label className="setting-row-copy" htmlFor="connection-timeout">
                            <strong>连接超时时长</strong>
                            <span>可设置 1–1440 分钟。</span>
                        </label>
                        <div className="setting-input-with-unit">
                            <input
                                aria-label="连接超时分钟数"
                                aria-invalid={!timeoutMinutesValid}
                                className="setting-input"
                                disabled={pendingAction !== null}
                                id="connection-timeout"
                                inputMode="numeric"
                                onChange={(event) => {
                                    const value = event.currentTarget.value;
                                    if (/^\d{0,4}$/.test(value)) {
                                        setConnectionTimeoutMinutes(value);
                                    }
                                }}
                                pattern="[0-9]*"
                                type="text"
                                value={connectionTimeoutMinutes}
                            />
                            <span>分钟</span>
                        </div>
                    </div>
                )}

                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>允许手机端反向控制</strong>
                        <span>
                            关闭时锁定电脑端认可的 A/B 强度；开启后，手机调整会同步到电脑端。
                        </span>
                    </div>
                    <label className="toggle-switch">
                        <span className="visually-hidden">
                            允许手机端反向控制
                        </span>
                        <input
                            aria-label="允许手机端反向控制"
                            checked={allowAppIntensityControl}
                            disabled={pendingAction !== null}
                            onChange={(event) =>
                                setAllowAppIntensityControl(
                                    event.currentTarget.checked,
                                )
                            }
                            type="checkbox"
                        />
                        <span className="toggle-track" aria-hidden="true">
                            <span />
                        </span>
                    </label>
                </div>

                <button
                    className="save-button"
                    disabled={
                        !dirty ||
                        pendingAction === "safety" ||
                        !timeoutMinutesValid
                    }
                    onClick={() =>
                        onSaveSafety({
                            connectionTimeoutEnabled,
                            connectionTimeoutMinutes: timeoutMinutes,
                            allowAppIntensityControl,
                        })
                    }
                    type="button"
                >
                    {pendingAction === "safety" ? (
                        <SpinnerGap
                            aria-hidden="true"
                            className="spin"
                            size={18}
                        />
                    ) : (
                        <Check aria-hidden="true" size={18} />
                    )}
                    应用安全设置
                </button>
            </section>

            <section className="settings-card">
                <div className="settings-heading">
                    <div className="settings-icon">
                        <Globe aria-hidden="true" size={25} weight="light" />
                    </div>
                    <div>
                        <h2>Relay 端点</h2>
                        <p>中枢通过 Socket V4 Relay 与 DG-LAB APP 建立配对和消息通道。</p>
                    </div>
                </div>
                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>当前端点</strong>
                    </div>
                    <code className="setting-endpoint">{snapshot.connection.endpoint}</code>
                </div>
                <div className="setting-row">
                    <div className="setting-row-copy">
                        <strong>连接状态</strong>
                    </div>
                    <span
                        className="relay-connection-status"
                        data-state={snapshot.connection.state}
                        role="status"
                    >
                        {relayStateLabels[snapshot.connection.state]}
                    </span>
                </div>
                <div className="relay-note">
                    前端不会持续承载 WebSocket 或实时波形调度；窗口刷新不会中断 Rust
                    后端中的会话。
                </div>
            </section>
            <RuntimeSettingsCard />
        </div>
    );
};
