import {
    Broadcast,
    CaretDown,
    CheckCircle,
    DeviceMobile,
    Link,
    Plus,
    Plugs,
    PlugsConnected,
    Trash,
    Warning,
    WaveSine,
    XCircle,
} from "@phosphor-icons/react";
import { useEffect, useRef, useState } from "react";
import {
    cloneDevices,
    ControlledAppEngine,
    createVirtualDevice,
    DEFAULT_RELAY_URL,
    MAX_VIRTUAL_DEVICES,
    resolveControlledEndpoint,
    type Channel,
    type ChannelStatus,
    type LogLevel,
    type VirtualDevice,
} from "./protocol";

type ConnectionState = "idle" | "connecting" | "connected" | "attached" | "error";

interface DisplayLog {
    id: number;
    level: LogLevel;
    message: string;
    time: string;
}

const STATUS_OPTIONS: Array<{ label: string; value: ChannelStatus }> = [
    { label: "无输出", value: 0 },
    { label: "未形成回路", value: 1 },
    { label: "输出正常", value: 2 },
    { label: "输出损坏", value: 3 },
    { label: "通道屏蔽", value: 4 },
];

const CONNECTION_LABEL: Record<ConnectionState, string> = {
    attached: "控制端已接入",
    connected: "Relay 已连接",
    connecting: "正在连接",
    error: "连接异常",
    idle: "尚未连接",
};

function channelName(channel: Channel): "A" | "B" {
    return channel === 0 ? "A" : "B";
}

function logIcon(level: LogLevel) {
    if (level === "success") {
        return <CheckCircle aria-hidden="true" />;
    }
    if (level === "error") {
        return <XCircle aria-hidden="true" />;
    }
    if (level === "warning") {
        return <Warning aria-hidden="true" />;
    }
    return <Broadcast aria-hidden="true" />;
}

function ChannelPanel({
    channel,
    device,
    engine,
}: {
    channel: Channel;
    device: VirtualDevice;
    engine: ControlledAppEngine;
}) {
    const state = device.channels[channel];
    const label = channelName(channel);

    return (
        <section className="channel-panel" aria-label={`${device.name} ${label} 通道`}>
            <div className="channel-panel__title">
                <span className={`channel-letter channel-letter--${label.toLowerCase()}`}>{label}</span>
                <div>
                    <strong>{label} 通道</strong>
                    <span>{state.lastOperation}</span>
                </div>
                <output>{state.intensity}</output>
            </div>

            <label className="field-label" htmlFor={`${device.slotId}-${label}-intensity`}>
                手机端上报强度
                <span>拖动后立即发送 slots.patch</span>
            </label>
            <div className="range-row">
                <input
                    id={`${device.slotId}-${label}-intensity`}
                    max={state.intensityMax}
                    min="0"
                    onChange={(event) => engine.setChannelIntensity(device.slotId, channel, Number(event.target.value))}
                    type="range"
                    value={state.intensity}
                />
                <input
                    aria-label={`${label} 通道手机端强度数值`}
                    className="number-input"
                    max={state.intensityMax}
                    min="0"
                    onChange={(event) => engine.setChannelIntensity(device.slotId, channel, Number(event.target.value))}
                    type="number"
                    value={state.intensity}
                />
            </div>

            <div className="channel-settings">
                <label>
                    <span>设备强度上限</span>
                    <input
                        max="200"
                        min="1"
                        onChange={(event) => engine.setChannelLimit(device.slotId, channel, Number(event.target.value))}
                        type="number"
                        value={state.intensityMax}
                    />
                </label>
                <label>
                    <span>输出状态</span>
                    <select
                        onChange={(event) => engine.setChannelStatus(
                            device.slotId,
                            channel,
                            Number(event.target.value) as ChannelStatus,
                        )}
                        value={state.status}
                    >
                        {STATUS_OPTIONS.map((option) => (
                            <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                    </select>
                </label>
            </div>

            <label className="switch-row">
                <input
                    checked={state.muted}
                    onChange={(event) => engine.setMuted(device.slotId, channel, event.target.checked)}
                    type="checkbox"
                />
                <span aria-hidden="true" className="switch-track"><span /></span>
                <span>模拟手机端屏蔽该通道</span>
            </label>

            <div className="pulse-stats">
                <div>
                    <span>波形包</span>
                    <strong>{state.pulsePackets}</strong>
                </div>
                <div>
                    <span>波形帧</span>
                    <strong>{state.pulseFrames}</strong>
                </div>
                <div className="pulse-stats__frame">
                    <span>最后一帧</span>
                    <code title={state.lastFrame}>{state.lastFrame}</code>
                </div>
            </div>
        </section>
    );
}

function DeviceCard({
    device,
    engine,
}: {
    device: VirtualDevice;
    engine: ControlledAppEngine;
}) {
    return (
        <article className={`device-card ${device.hasDevice ? "" : "device-card--offline"}`}>
            <header className="device-card__header">
                <div className="device-mark"><DeviceMobile aria-hidden="true" weight="light" /></div>
                <div className="device-heading">
                    <input
                        aria-label={`${device.slotId} 设备名称`}
                        defaultValue={device.name}
                        key={`${device.slotId}-${device.name}`}
                        onBlur={(event) => engine.renameDevice(device.slotId, event.target.value)}
                    />
                    <span>{device.slotId} · {device.type}</span>
                </div>
                <button
                    aria-label={`移除 ${device.name}`}
                    className="icon-button icon-button--danger"
                    onClick={() => engine.removeDevice(device.slotId)}
                    title="移除虚拟设备"
                    type="button"
                >
                    <Trash aria-hidden="true" />
                </button>
            </header>

            <div className="device-meta">
                <label className="switch-row">
                    <input
                        checked={device.hasDevice}
                        onChange={(event) => engine.setPresence(device.slotId, event.target.checked)}
                        type="checkbox"
                    />
                    <span aria-hidden="true" className="switch-track"><span /></span>
                    <span>{device.hasDevice ? "模拟蓝牙在线" : "模拟蓝牙离线"}</span>
                </label>
                <label className="battery-field">
                    <span>电量</span>
                    <input
                        max="100"
                        min="0"
                        onChange={(event) => engine.setPower(device.slotId, Number(event.target.value))}
                        type="range"
                        value={device.power}
                    />
                    <output>{device.power}%</output>
                </label>
            </div>

            <div className="channels-grid">
                <ChannelPanel channel={0} device={device} engine={engine} />
                <ChannelPanel channel={1} device={device} engine={engine} />
            </div>
        </article>
    );
}

export default function App() {
    const initialDevices = useRef([createVirtualDevice(0), createVirtualDevice(1)]);
    const [devices, setDevices] = useState(() => cloneDevices(initialDevices.current));
    const [logs, setLogs] = useState<DisplayLog[]>([]);
    const [relayUrl, setRelayUrl] = useState(DEFAULT_RELAY_URL);
    const [targetOrLink, setTargetOrLink] = useState("");
    const [connectionState, setConnectionState] = useState<ConnectionState>("idle");
    const [connectionError, setConnectionError] = useState<string | null>(null);
    const [appClientId, setAppClientId] = useState<string | null>(null);
    const [controllerId, setControllerId] = useState<string | null>(null);
    const [resolvedEndpoint, setResolvedEndpoint] = useState<string | null>(null);
    const socketRef = useRef<WebSocket | null>(null);
    const logSequence = useRef(0);
    const engineRef = useRef<ControlledAppEngine | null>(null);

    const appendLog = (level: LogLevel, message: string) => {
        const entry: DisplayLog = {
            id: ++logSequence.current,
            level,
            message,
            time: new Date().toLocaleTimeString("zh-CN", { hour12: false }),
        };
        setLogs((current) => [entry, ...current].slice(0, 300));
    };

    if (!engineRef.current) {
        engineRef.current = new ControlledAppEngine({
            onDevices: setDevices,
            onLog: ({ level, message }) => appendLog(level, message),
            send: (data) => {
                const socket = socketRef.current;
                if (!socket || socket.readyState !== WebSocket.OPEN) {
                    throw new Error("Relay 尚未连接，协议消息未发送");
                }
                socket.send(JSON.stringify({ data, type: "message" }));
            },
        }, initialDevices.current);
    }
    const engine = engineRef.current;

    useEffect(() => () => {
        engine.resetSession();
        socketRef.current?.close(1000, "simulator closed");
    }, [engine]);

    const disconnect = () => {
        const socket = socketRef.current;
        engine.resetSession();
        socketRef.current = null;
        socket?.close(1000, "manual disconnect");
        setConnectionState("idle");
        setAppClientId(null);
        setControllerId(null);
        appendLog("warning", "已主动断开 Relay");
    };

    const connect = () => {
        let endpoint: string;
        try {
            endpoint = resolveControlledEndpoint(relayUrl, targetOrLink);
        } catch (error) {
            const message = error instanceof Error ? error.message : "无法生成连接地址";
            setConnectionError(message);
            setConnectionState("error");
            appendLog("error", message);
            return;
        }

        socketRef.current?.close(1000, "replace connection");
        engine.resetSession();
        setConnectionError(null);
        setConnectionState("connecting");
        setAppClientId(null);
        setControllerId(null);
        setResolvedEndpoint(endpoint);
        appendLog("info", `正在连接 ${endpoint}`);

        const socket = new WebSocket(endpoint);
        socketRef.current = socket;
        socket.addEventListener("open", () => {
            if (socketRef.current !== socket) {
                return;
            }
            setConnectionState("connected");
            appendLog("success", "WebSocket 已连接，等待控制端确认");
        });
        socket.addEventListener("message", (event) => {
            if (socketRef.current !== socket || typeof event.data !== "string") {
                return;
            }
            let frame: Record<string, unknown>;
            try {
                frame = JSON.parse(event.data) as Record<string, unknown>;
            } catch {
                appendLog("warning", "Relay 返回了无法解析的文本帧");
                return;
            }
            if (frame.type === "hello" && typeof frame.clientId === "string") {
                setAppClientId(frame.clientId);
                appendLog("info", `获得被控端 clientId：${frame.clientId}`);
                return;
            }
            if (frame.type === "controller_attached") {
                const id = typeof frame.clientId === "string" ? frame.clientId : "未知控制端";
                setControllerId(id);
                setConnectionState("attached");
                engine.setControllerAttached(true);
                return;
            }
            if (frame.type === "controller_disconnected") {
                engine.resetSession();
                setControllerId(null);
                setConnectionState("connected");
                appendLog("warning", "控制端已断开，虚拟设备状态已保留");
                return;
            }
            if (frame.type === "message") {
                engine.handleControllerData(frame.data);
                return;
            }
            if (frame.type === "error") {
                appendLog("error", `Relay 错误：${String(frame.error ?? frame.message ?? "未知错误")}`);
            }
        });
        socket.addEventListener("error", () => {
            if (socketRef.current !== socket) {
                return;
            }
            const message = "WebSocket 连接异常，请确认控制端 ID 与 Relay 地址";
            setConnectionError(message);
            setConnectionState("error");
            appendLog("error", message);
        });
        socket.addEventListener("close", (event) => {
            if (socketRef.current !== socket) {
                return;
            }
            socketRef.current = null;
            engine.resetSession();
            setControllerId(null);
            setConnectionState(event.wasClean ? "idle" : "error");
            if (!event.wasClean) {
                const message = `Relay 已异常断开（${event.code || "无状态码"}）`;
                setConnectionError(message);
                appendLog("error", message);
            } else {
                appendLog("warning", "Relay 连接已关闭");
            }
        });
    };

    const connected = connectionState === "connected" || connectionState === "attached" || connectionState === "connecting";

    return (
        <div className="simulator-shell">
            <header className="app-header">
                <div className="brand-mark"><WaveSine aria-hidden="true" weight="light" /></div>
                <div>
                    <p>DG-LAB LINK · DEVELOPMENT TOOL</p>
                    <h1>被控端模拟器</h1>
                </div>
                <div className={`connection-pill connection-pill--${connectionState}`}>
                    <span />
                    {CONNECTION_LABEL[connectionState]}
                </div>
            </header>

            <main>
                <section className="connection-card">
                    <div className="section-heading">
                        <div>
                            <span className="section-kicker">SOCKET V4</span>
                            <h2>连接控制端</h2>
                            <p>粘贴 DG-LAB Link 中的配对链接，或输入控制端 ID。模拟器会作为一个包含多台设备的 APP 接入。</p>
                        </div>
                        {connectionState === "attached" ? <PlugsConnected aria-hidden="true" /> : <Plugs aria-hidden="true" />}
                    </div>

                    <div className="connection-form">
                        <label className="connection-form__target">
                            <span>控制端 ID 或完整配对链接</span>
                            <input
                                disabled={connected}
                                onChange={(event) => setTargetOrLink(event.target.value)}
                                placeholder="粘贴 https://dungeon-lab.cn/s/?... 或输入 targetId"
                                spellCheck="false"
                                value={targetOrLink}
                            />
                        </label>
                        <details className="relay-details">
                            <summary><CaretDown aria-hidden="true" /> Relay 地址</summary>
                            <label>
                                <span>使用控制端 ID 时采用此地址</span>
                                <input
                                    disabled={connected}
                                    onChange={(event) => setRelayUrl(event.target.value)}
                                    spellCheck="false"
                                    value={relayUrl}
                                />
                            </label>
                        </details>
                        <div className="connection-actions">
                            {connected ? (
                                <button className="button button--secondary" onClick={disconnect} type="button">
                                    <XCircle aria-hidden="true" />断开
                                </button>
                            ) : (
                                <button className="button button--primary" onClick={connect} type="button">
                                    <Link aria-hidden="true" />连接控制端
                                </button>
                            )}
                        </div>
                    </div>

                    {connectionError && <div className="connection-error"><Warning aria-hidden="true" />{connectionError}</div>}

                    <div className="session-grid">
                        <div><span>虚拟 APP clientId</span><strong>{appClientId ?? "等待 Relay"}</strong></div>
                        <div><span>控制端 clientId</span><strong>{controllerId ?? "尚未接入"}</strong></div>
                        <div><span>暴露设备数</span><strong>{devices.length} 台</strong></div>
                        <div><span>实际连接地址</span><strong title={resolvedEndpoint ?? ""}>{resolvedEndpoint ?? "尚未生成"}</strong></div>
                    </div>
                </section>

                <section className="devices-section">
                    <div className="section-heading section-heading--compact">
                        <div>
                            <span className="section-kicker">VIRTUAL DEVICES</span>
                            <h2>虚拟设备</h2>
                            <p>每台设备拥有独立 slotId、A/B 强度、上限、状态和波形统计。</p>
                        </div>
                        <button
                            className="button button--secondary"
                            disabled={devices.length >= MAX_VIRTUAL_DEVICES}
                            onClick={() => engine.addDevice()}
                            type="button"
                        >
                            <Plus aria-hidden="true" />添加设备
                        </button>
                    </div>

                    <div className="device-list">
                        {devices.map((device) => <DeviceCard device={device} engine={engine} key={device.slotId} />)}
                        {devices.length === 0 && (
                            <div className="empty-state">
                                <DeviceMobile aria-hidden="true" />
                                <strong>当前没有虚拟设备</strong>
                                <span>添加设备后会通过 devices.patch 立即通知控制端。</span>
                            </div>
                        )}
                    </div>
                </section>

                <section className="action-log-grid">
                    <div className="custom-actions">
                        <div className="section-heading section-heading--compact">
                            <div>
                                <span className="section-kicker">APP EVENT</span>
                                <h2>自定义动作</h2>
                                <p>模拟 APP 发送不带设备归属的 custom.action。</p>
                            </div>
                        </div>
                        <div className="action-buttons">
                            {Array.from({ length: 10 }, (_, action) => (
                                <button key={action} onClick={() => engine.sendCustomAction(action)} type="button">{action}</button>
                            ))}
                        </div>
                    </div>

                    <div className="event-log">
                        <div className="event-log__header">
                            <div>
                                <span className="section-kicker">EVENT LOG</span>
                                <h2>协议记录</h2>
                            </div>
                            <button className="text-button" onClick={() => setLogs([])} type="button">清空</button>
                        </div>
                        <div aria-live="polite" className="event-log__list">
                            {logs.map((entry) => (
                                <div className={`log-entry log-entry--${entry.level}`} key={entry.id}>
                                    {logIcon(entry.level)}
                                    <time>{entry.time}</time>
                                    <span>{entry.message}</span>
                                </div>
                            ))}
                            {logs.length === 0 && <p className="log-empty">连接后将在这里显示设备操作与状态变化。</p>}
                        </div>
                    </div>
                </section>

                <aside className="safety-note">
                    <Warning aria-hidden="true" />
                    <p><strong>这是协议模拟器，不会产生真实输出。</strong>它可以验证多设备寻址、波形路由、掉线和反向强度控制，但不能替代真实蓝牙设备的最终安全测试。</p>
                </aside>
            </main>
        </div>
    );
}
