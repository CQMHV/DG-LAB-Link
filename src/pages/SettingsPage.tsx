import {
    Check,
    Clock,
    Globe,
    ShieldCheck,
    SpinnerGap,
    Tray,
    Waveform,
} from "@phosphor-icons/react";
import { useEffect, useState } from "react";

import { PageHeader } from "../components/PageHeader";
import type {
    AppPreferences,
    HubSnapshot,
    SafetyUpdate,
} from "../lib/contracts";

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
    const [channelLimit, setChannelLimit] = useState(
        snapshot.safety.channelLimit,
    );
    const [maxDurationMinutes, setMaxDurationMinutes] = useState(
        snapshot.safety.maxDurationMinutes,
    );
    const [allowAppIntensityControl, setAllowAppIntensityControl] = useState(
        snapshot.safety.allowAppIntensityControl,
    );

    useEffect(() => {
        setChannelLimit(snapshot.safety.channelLimit);
        setMaxDurationMinutes(snapshot.safety.maxDurationMinutes);
        setAllowAppIntensityControl(
            snapshot.safety.allowAppIntensityControl,
        );
    }, [
        snapshot.safety.allowAppIntensityControl,
        snapshot.safety.channelLimit,
        snapshot.safety.maxDurationMinutes,
    ]);

    const dirty =
        channelLimit !== snapshot.safety.channelLimit ||
        maxDurationMinutes !== snapshot.safety.maxDurationMinutes ||
        allowAppIntensityControl !==
            snapshot.safety.allowAppIntensityControl;

    return (
        <div className="standard-page settings-page">
            <PageHeader
                description="管理应用行为、全局安全阈值与 Relay 连接信息。"
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

                <div className="setting-toggle-row setting-toggle-row-first">
                    <div>
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

                <div className="setting-toggle-row setting-toggle-row-dependent">
                    <div>
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
                            disabled={
                                pendingAction !== null ||
                                !appPreferences.autoStart
                            }
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

                <div className="setting-toggle-row setting-toggle-row-last">
                    <div>
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

                <div className="setting-select-row">
                    <div>
                        <strong>新设备默认输入源</strong>
                        <span>选择“每次询问”时，新设备接入后由仪表盘决定。</span>
                    </div>
                    <select
                        aria-label="选择默认输入源"
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
                            应用安全设置后仅在当前运行期间生效；重启应用会恢复默认上限
                            80、最长输出 30 分钟。
                        </p>
                    </div>
                </div>

                <div className="setting-control">
                    <label htmlFor="channel-limit">
                        <span>双通道强度上限</span>
                        <strong>{channelLimit}</strong>
                    </label>
                    <input
                        id="channel-limit"
                        max={200}
                        min={1}
                        onChange={(event) =>
                            setChannelLimit(Number(event.currentTarget.value))
                        }
                        type="range"
                        value={channelLimit}
                    />
                    <div className="range-labels">
                        <span>1</span>
                        <span>200</span>
                    </div>
                </div>

                <div className="duration-setting duration-setting-editable">
                    <Clock aria-hidden="true" size={21} weight="light" />
                    <div>
                        <span>最长连续输出</span>
                        <strong>{maxDurationMinutes} 分钟</strong>
                    </div>
                    <label htmlFor="max-duration">时长限制</label>
                    <input
                        aria-label="最长连续输出分钟数"
                        id="max-duration"
                        max={120}
                        min={1}
                        onChange={(event) =>
                            setMaxDurationMinutes(Number(event.currentTarget.value))
                        }
                        type="number"
                        value={maxDurationMinutes}
                    />
                </div>

                <div className="setting-toggle-row">
                    <div>
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
                    disabled={!dirty || pendingAction === "safety"}
                    onClick={() =>
                        onSaveSafety({
                            channelLimit,
                            maxDurationMinutes,
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

            <section className="settings-card relay-card">
                <div className="settings-heading">
                    <div className="settings-icon">
                        <Globe aria-hidden="true" size={25} weight="light" />
                    </div>
                    <div>
                        <h2>Relay 端点</h2>
                        <p>中枢通过 Socket V4 Relay 与 DG-LAB APP 建立配对和消息通道。</p>
                    </div>
                </div>
                <div className="endpoint-field">
                    <span>当前端点</span>
                    <code>{snapshot.connection.endpoint}</code>
                </div>
                <div className="relay-note">
                    前端不会持续承载 WebSocket 或实时波形调度；窗口刷新不会中断 Rust
                    后端中的会话。
                </div>
            </section>
        </div>
    );
};
