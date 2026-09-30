export const DEFAULT_RELAY_URL = "wss://trex.dungeon-lab.cn/v4";
export const MAX_VIRTUAL_DEVICES = 8;

export type Channel = 0 | 1;
export type ChannelStatus = 0 | 1 | 2 | 3 | 4;
export type LogLevel = "info" | "success" | "warning" | "error";

export interface VirtualChannel {
    intensity: number;
    intensityMax: number;
    muted: boolean;
    status: ChannelStatus;
    pulsePackets: number;
    pulseFrames: number;
    lastFrame: string;
    lastOperation: string;
}

export interface VirtualDevice {
    id: number;
    slotId: string;
    name: string;
    type: "COYOTE_030";
    power: number;
    hasDevice: boolean;
    channels: [VirtualChannel, VirtualChannel];
}

export interface SimulatorLog {
    level: LogLevel;
    message: string;
}

interface EngineCallbacks {
    onDevices: (devices: VirtualDevice[]) => void;
    onLog: (entry: SimulatorLog) => void;
    send: (data: Record<string, unknown>) => void;
}

interface PendingTask {
    channel: Channel;
    reqId: string;
    slotId: string;
    timer: ReturnType<typeof setTimeout>;
    type: number;
}

type JsonObject = Record<string, unknown>;

function asObject(value: unknown): JsonObject | null {
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
        return null;
    }
    return value as JsonObject;
}

function clampInteger(value: number, minimum: number, maximum: number): number {
    if (!Number.isFinite(value)) {
        return minimum;
    }
    return Math.min(maximum, Math.max(minimum, Math.round(value)));
}

function cloneChannel(channel: VirtualChannel): VirtualChannel {
    return { ...channel };
}

export function cloneDevices(devices: VirtualDevice[]): VirtualDevice[] {
    return devices.map((device) => ({
        ...device,
        channels: [cloneChannel(device.channels[0]), cloneChannel(device.channels[1])],
    }));
}

export function createVirtualDevice(index: number): VirtualDevice {
    const sequence = index + 1;
    const channel = (): VirtualChannel => ({
        intensity: 0,
        intensityMax: 100,
        muted: false,
        status: 2,
        pulsePackets: 0,
        pulseFrames: 0,
        lastFrame: "—",
        lastOperation: "等待控制指令",
    });

    return {
        id: index,
        slotId: `sim-slot-${sequence}`,
        name: `虚拟郊狼 ${sequence}`,
        type: "COYOTE_030",
        power: Math.max(20, 96 - index * 7),
        hasDevice: true,
        channels: [channel(), channel()],
    };
}

function ensureWebSocketUrl(value: string): URL {
    let url: URL;
    try {
        url = new URL(value);
    } catch {
        throw new Error("Relay 地址或配对链接不是有效 URL");
    }
    if (url.protocol !== "ws:" && url.protocol !== "wss:") {
        throw new Error("被控端连接地址必须使用 ws:// 或 wss://");
    }
    return url;
}

export function resolveControlledEndpoint(relayUrl: string, targetOrLink: string): string {
    const target = targetOrLink.trim();
    if (!target) {
        throw new Error("请输入控制端 ID 或粘贴配对链接");
    }

    if (/^https?:\/\//i.test(target)) {
        let shareUrl: URL;
        try {
            shareUrl = new URL(target);
        } catch {
            throw new Error("配对链接不是有效 URL");
        }
        const embedded = shareUrl.searchParams.get("url");
        if (!embedded) {
            throw new Error("配对链接中缺少被控端 WebSocket 地址");
        }
        const endpoint = ensureWebSocketUrl(embedded);
        if (!endpoint.searchParams.get("tid")) {
            throw new Error("配对链接中缺少控制端 ID（tid）");
        }
        return endpoint.toString();
    }

    if (/^wss?:\/\//i.test(target)) {
        const endpoint = ensureWebSocketUrl(target);
        if (!endpoint.searchParams.get("tid")) {
            throw new Error("被控端 WebSocket 地址中缺少控制端 ID（tid）");
        }
        return endpoint.toString();
    }

    const endpoint = ensureWebSocketUrl(relayUrl.trim());
    endpoint.searchParams.set("tid", target);
    return endpoint.toString();
}

function remoteChannel(channel: VirtualChannel): JsonObject {
    return {
        comfortLimit: {
            comfortMax: channel.intensityMax,
            mode: "simple",
        },
        intensityMax: channel.intensityMax,
        isMuted: channel.muted,
        warmUpScale: 1,
    };
}

export function toRemoteDevice(device: VirtualDevice): JsonObject {
    return {
        id: device.id,
        name: device.name,
        props: {
            channelAStatus: device.channels[0].status,
            channelBStatus: device.channels[1].status,
            connectState: device.hasDevice ? "connected" : "disconnected",
            intensityA: device.channels[0].intensity,
            intensityB: device.channels[1].intensity,
            power: device.power,
            version: 3,
        },
        slotId: device.slotId,
        slotState: {
            channelA: remoteChannel(device.channels[0]),
            channelB: remoteChannel(device.channels[1]),
            hasDevice: device.hasDevice,
            markLight: device.hasDevice ? "green" : "yellow",
        },
        type: device.type,
    };
}

function framePreview(value: unknown): string {
    if (typeof value === "string") {
        return value.slice(0, 32);
    }
    if (Array.isArray(value)) {
        return JSON.stringify(value).slice(0, 32);
    }
    return "无法解析";
}

export class ControlledAppEngine {
    private readonly callbacks: EngineCallbacks;
    private controllerAttached = false;
    private devices: VirtualDevice[];
    private readonly pendingTasks = new Map<string, PendingTask>();

    public constructor(callbacks: EngineCallbacks, initialDevices?: VirtualDevice[]) {
        this.callbacks = callbacks;
        this.devices = cloneDevices(
            initialDevices ?? [createVirtualDevice(0), createVirtualDevice(1)],
        );
    }

    public getDevices(): VirtualDevice[] {
        return cloneDevices(this.devices);
    }

    public setControllerAttached(attached: boolean): void {
        this.controllerAttached = attached;
        if (attached) {
            this.log("success", `控制端已接入，已上报 ${this.devices.length} 台虚拟设备`);
            this.announceSnapshot();
        }
    }

    public resetSession(): void {
        this.controllerAttached = false;
        for (const task of this.pendingTasks.values()) {
            clearTimeout(task.timer);
        }
        this.pendingTasks.clear();
    }

    public announceSnapshot(): void {
        if (!this.controllerAttached) {
            return;
        }
        this.send({
            devices: this.devices.map(toRemoteDevice),
            ev: "devices.snapshot",
            t: "ev",
        });
    }

    public addDevice(): void {
        if (this.devices.length >= MAX_VIRTUAL_DEVICES) {
            this.log("warning", `最多只能模拟 ${MAX_VIRTUAL_DEVICES} 台设备`);
            return;
        }
        let index = 0;
        const existing = new Set(this.devices.map((device) => device.slotId));
        while (existing.has(`sim-slot-${index + 1}`)) {
            index += 1;
        }
        const device = createVirtualDevice(index);
        this.devices.push(device);
        this.emitDevices();
        if (this.controllerAttached) {
            this.send({
                added: [toRemoteDevice(device)],
                ev: "devices.patch",
                removed: [],
                t: "ev",
            });
        }
        this.log("info", `已添加 ${device.name}（${device.slotId}）`);
    }

    public removeDevice(slotId: string): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        this.cancelTasks((task) => task.slotId === slotId, "cancelled");
        this.devices = this.devices.filter((candidate) => candidate.slotId !== slotId);
        this.emitDevices();
        if (this.controllerAttached) {
            this.send({
                added: [],
                ev: "devices.patch",
                removed: [slotId],
                t: "ev",
            });
        }
        this.log("warning", `已移除 ${device.name}（${slotId}）`);
    }

    public renameDevice(slotId: string, name: string): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        device.name = name.trim() || device.name;
        this.emitDevices();
        this.announceSnapshot();
    }

    public setPower(slotId: string, value: number): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        device.power = clampInteger(value, 0, 100);
        this.deviceStateChanged(device);
    }

    public setPresence(slotId: string, hasDevice: boolean): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        device.hasDevice = hasDevice;
        this.deviceStateChanged(device);
        this.log(hasDevice ? "success" : "warning", `${device.name} 已${hasDevice ? "上线" : "离线"}`);
    }

    public setChannelIntensity(slotId: string, channel: Channel, value: number): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        const target = device.channels[channel];
        target.intensity = clampInteger(value, 0, target.intensityMax);
        target.lastOperation = `手机端上报强度 ${target.intensity}`;
        this.deviceStateChanged(device);
        this.log("info", `${device.name} · ${channel === 0 ? "A" : "B"} 手机端上报强度 ${target.intensity}`);
    }

    public setChannelLimit(slotId: string, channel: Channel, value: number): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        const target = device.channels[channel];
        target.intensityMax = clampInteger(value, 1, 200);
        target.intensity = Math.min(target.intensity, target.intensityMax);
        target.lastOperation = `设备上限调整为 ${target.intensityMax}`;
        this.deviceStateChanged(device);
    }

    public setChannelStatus(slotId: string, channel: Channel, status: ChannelStatus): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        device.channels[channel].status = status;
        device.channels[channel].lastOperation = `通道状态改为 ${status}`;
        this.deviceStateChanged(device);
    }

    public setMuted(slotId: string, channel: Channel, muted: boolean): void {
        const device = this.findDevice(slotId);
        if (!device) {
            return;
        }
        device.channels[channel].muted = muted;
        device.channels[channel].lastOperation = muted ? "手机端屏蔽通道" : "手机端解除屏蔽";
        this.deviceStateChanged(device);
    }

    public sendCustomAction(action: number): void {
        if (!this.controllerAttached) {
            this.log("warning", "控制端尚未接入，无法发送自定义动作");
            return;
        }
        const normalized = clampInteger(action, 0, 9);
        this.send({ action: normalized, ev: "custom.action", t: "ev" });
        this.log("info", `已发送 APP 自定义动作 ${normalized}`);
    }

    public handleControllerData(value: unknown): void {
        const request = asObject(value);
        if (!request || request.t !== "req" || typeof request.reqId !== "string") {
            this.log("warning", "忽略无法识别的控制端消息");
            return;
        }
        const reqId = request.reqId;
        if (this.pendingTasks.has(reqId)) {
            this.respondError(reqId, "duplicate_request_id");
            return;
        }
        if (request.m === "devices.get") {
            this.respond(reqId, { devices: this.devices.map(toRemoteDevice) });
            this.log("info", "控制端请求了设备全量快照");
            return;
        }
        if (request.m === "ping") {
            this.respond(reqId, Date.now());
            return;
        }
        if (request.m === "device.op.clear") {
            this.handleClear(reqId, request.data);
            return;
        }
        if (request.m === "device.op") {
            this.handleDeviceOperation(reqId, request.data);
            return;
        }
        this.respondError(reqId, "method_not_found");
        this.log("warning", `不支持的 RPC 方法：${String(request.m)}`);
    }

    private handleDeviceOperation(reqId: string, rawData: unknown): void {
        const data = asObject(rawData);
        const slotId = data?.s;
        const type = data?.t;
        const channel = data?.c;
        if (
            !data ||
            typeof slotId !== "string" ||
            typeof type !== "number" ||
            (channel !== 0 && channel !== 1)
        ) {
            this.respondError(reqId, "invalid_operate");
            return;
        }
        const device = this.findDevice(slotId);
        if (!device) {
            this.respondError(reqId, "slot_not_found");
            return;
        }
        const channelIndex = channel as Channel;
        const target = device.channels[channelIndex];

        switch (type) {
            case 0: {
                const frames = Array.isArray(data.v) ? data.v : null;
                if (!frames || frames.length === 0) {
                    this.respondError(reqId, "invalid_operate");
                    return;
                }
                if (data.im === true) {
                    this.cancelTasks(
                        (task) => task.slotId === slotId && task.channel === channelIndex && task.type === 0,
                        "replaced",
                    );
                }
                target.pulsePackets += 1;
                target.pulseFrames += frames.length;
                target.lastFrame = framePreview(frames.at(-1));
                target.lastOperation = `收到 ${frames.length} 帧波形`;
                this.emitDevices();
                if (target.pulsePackets === 1 || target.pulsePackets % 10 === 0) {
                    this.log(
                        "success",
                        `${device.name} · ${channelIndex === 0 ? "A" : "B"} 已收到 ${target.pulsePackets} 个波形包`,
                    );
                }
                const frameDuration = frames.length * 100;
                const requestedDuration = typeof data.d === "number" && data.d > 0
                    ? data.d
                    : frameDuration;
                this.scheduleCompletion(
                    reqId,
                    slotId,
                    channelIndex,
                    type,
                    Math.max(1, Math.min(frameDuration, requestedDuration)),
                );
                return;
            }
            case 3: {
                if (typeof data.v !== "number") {
                    this.respondError(reqId, "invalid_operate");
                    return;
                }
                target.intensity = clampInteger(target.intensity + data.v, 0, target.intensityMax);
                target.lastOperation = `控制端相对调整 ${data.v >= 0 ? "+" : ""}${data.v}`;
                this.deviceStateChanged(device);
                this.respondOperation(reqId, type, slotId, channelIndex, "completed");
                this.log("success", `${device.name} · ${channelIndex === 0 ? "A" : "B"} 强度变为 ${target.intensity}`);
                return;
            }
            case 4: {
                if (typeof data.v !== "number" || typeof data.d !== "number" || data.d < 0) {
                    this.respondError(reqId, "invalid_operate");
                    return;
                }
                target.intensity = clampInteger(data.v, 0, target.intensityMax);
                target.lastOperation = `临时强度 ${target.intensity}`;
                this.deviceStateChanged(device);
                this.scheduleCompletion(
                    reqId,
                    slotId,
                    channelIndex,
                    type,
                    Math.max(1, data.d),
                    () => {
                        const current = this.findDevice(slotId);
                        if (!current) {
                            return;
                        }
                        current.channels[channelIndex].intensity = 0;
                        current.channels[channelIndex].lastOperation = "临时强度已归零";
                        this.deviceStateChanged(current);
                    },
                );
                return;
            }
            case 5: {
                if (typeof data.v !== "boolean") {
                    this.respondError(reqId, "invalid_operate");
                    return;
                }
                target.muted = data.v;
                target.lastOperation = data.v ? "控制端设置静音" : "控制端取消静音";
                this.deviceStateChanged(device);
                this.respondOperation(reqId, type, slotId, channelIndex, "completed");
                return;
            }
            case 7: {
                if (data.v !== 0) {
                    this.respondError(reqId, "invalid_operate");
                    return;
                }
                target.intensity = 0;
                target.lastOperation = "控制端归零强度";
                this.deviceStateChanged(device);
                this.respondOperation(reqId, type, slotId, channelIndex, "completed");
                this.log("warning", `${device.name} · ${channelIndex === 0 ? "A" : "B"} 强度已归零`);
                return;
            }
            default:
                this.respondError(reqId, "invalid_operate");
        }
    }

    private handleClear(reqId: string, rawData: unknown): void {
        const data = rawData === undefined ? null : asObject(rawData);
        if (rawData !== undefined && !data) {
            this.respondError(reqId, "invalid_params");
            return;
        }
        const slotId = data?.s;
        const channel = data?.c;
        if (slotId !== undefined && typeof slotId !== "string") {
            this.respondError(reqId, "invalid_params");
            return;
        }
        if (channel !== undefined && (slotId === undefined || (channel !== 0 && channel !== 1))) {
            this.respondError(reqId, "invalid_params");
            return;
        }
        if (typeof slotId === "string" && !this.findDevice(slotId)) {
            this.respondError(reqId, "slot_not_found");
            return;
        }

        this.cancelTasks(
            (task) =>
                (slotId === undefined || task.slotId === slotId) &&
                (channel === undefined || task.channel === channel),
            "cleared",
        );
        for (const device of this.devices) {
            if (slotId !== undefined && device.slotId !== slotId) {
                continue;
            }
            for (const channelIndex of [0, 1] as const) {
                if (channel !== undefined && channelIndex !== channel) {
                    continue;
                }
                device.channels[channelIndex].lastOperation = "控制端清理任务";
            }
        }
        this.emitDevices();
        this.respond(reqId, {});
        this.log("warning", `已清理${slotId ? ` ${slotId}` : "全部设备"}${channel === undefined ? "" : ` · ${channel === 0 ? "A" : "B"}`}任务`);
    }

    private scheduleCompletion(
        reqId: string,
        slotId: string,
        channel: Channel,
        type: number,
        duration: number,
        onComplete?: () => void,
    ): void {
        const timer = setTimeout(() => {
            const task = this.pendingTasks.get(reqId);
            if (!task) {
                return;
            }
            this.pendingTasks.delete(reqId);
            onComplete?.();
            this.respondOperation(reqId, type, slotId, channel, "completed");
        }, duration);
        this.pendingTasks.set(reqId, { channel, reqId, slotId, timer, type });
    }

    private cancelTasks(
        predicate: (task: PendingTask) => boolean,
        reason: "cleared" | "replaced" | "cancelled",
    ): void {
        for (const [reqId, task] of [...this.pendingTasks]) {
            if (!predicate(task)) {
                continue;
            }
            clearTimeout(task.timer);
            this.pendingTasks.delete(reqId);
            if (this.controllerAttached) {
                this.respondOperation(reqId, task.type, task.slotId, task.channel, reason);
            }
        }
    }

    private deviceStateChanged(device: VirtualDevice): void {
        this.emitDevices();
        if (!this.controllerAttached) {
            return;
        }
        this.send({
            ev: "slots.patch",
            slots: [{
                props: {
                    channelAStatus: device.channels[0].status,
                    channelBStatus: device.channels[1].status,
                    connectState: device.hasDevice ? "connected" : "disconnected",
                    intensityA: device.channels[0].intensity,
                    intensityB: device.channels[1].intensity,
                    power: device.power,
                },
                slotId: device.slotId,
                slotState: {
                    channelA: remoteChannel(device.channels[0]),
                    channelB: remoteChannel(device.channels[1]),
                    hasDevice: device.hasDevice,
                    markLight: device.hasDevice ? "green" : "yellow",
                },
            }],
            t: "ev",
        });
    }

    private findDevice(slotId: string): VirtualDevice | undefined {
        return this.devices.find((device) => device.slotId === slotId);
    }

    private emitDevices(): void {
        this.callbacks.onDevices(this.getDevices());
    }

    private respond(reqId: string, result: unknown): void {
        this.send({ reqId, result, t: "resp" });
    }

    private respondError(reqId: string, error: string): void {
        this.send({ error, reqId, t: "resp" });
    }

    private respondOperation(
        reqId: string,
        type: number,
        slotId: string,
        channel: Channel,
        reason: "completed" | "cleared" | "replaced" | "cancelled",
    ): void {
        this.respond(reqId, { channel, reason, slotId, type });
    }

    private send(data: JsonObject): void {
        try {
            this.callbacks.send(data);
        } catch (error) {
            this.log("error", error instanceof Error ? error.message : "发送协议消息失败");
        }
    }

    private log(level: LogLevel, message: string): void {
        this.callbacks.onLog({ level, message });
    }
}
