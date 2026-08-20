import {
    Check,
    Clock,
    Globe,
    ShieldCheck,
    SpinnerGap,
} from "@phosphor-icons/react";
import { useEffect, useState } from "react";

import { PageHeader } from "../components/PageHeader";
import type { HubSnapshot, SafetyUpdate } from "../lib/contracts";

interface SettingsPageProps {
    snapshot: HubSnapshot;
    pendingAction: string | null;
    onSaveSafety: (update: SafetyUpdate) => void;
}

export const SettingsPage = ({
    snapshot,
    pendingAction,
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
                description="管理全局安全阈值与 Relay 连接信息。限制由 Rust 后端强制执行。"
                eyebrow="PREFERENCES"
                title="设置"
            />

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
