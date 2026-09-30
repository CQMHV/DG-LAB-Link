import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
    ControlledAppEngine,
    createVirtualDevice,
    DEFAULT_RELAY_URL,
    resolveControlledEndpoint,
    type SimulatorLog,
    type VirtualDevice,
} from "./protocol";

describe("resolveControlledEndpoint", () => {
    it("uses the same official relay path as the controller", () => {
        expect(resolveControlledEndpoint(DEFAULT_RELAY_URL, "controller-1"))
            .toBe("wss://trex.dungeon-lab.cn/v4?tid=controller-1");
    });

    it("combines the relay endpoint and controller id", () => {
        expect(resolveControlledEndpoint("wss://trex.dungeon-lab.cn/v4/", "controller-1"))
            .toBe("wss://trex.dungeon-lab.cn/v4/?tid=controller-1");
    });

    it("extracts the controlled websocket endpoint from an official pairing link", () => {
        const websocket = "wss://trex.dungeon-lab.cn/v4/?tid=controller-2";
        const link = `https://dungeon-lab.cn/s/?v=1&action=socket&url=${encodeURIComponent(websocket)}`;
        expect(resolveControlledEndpoint("wss://ignored.example/v4", link)).toBe(websocket);
    });

    it("rejects a websocket endpoint without tid", () => {
        expect(() => resolveControlledEndpoint("wss://ignored.example/v4", "wss://relay.example/v4"))
            .toThrow("tid");
    });
});

describe("ControlledAppEngine", () => {
    let devices: VirtualDevice[];
    let logs: SimulatorLog[];
    let sent: Array<Record<string, unknown>>;
    let engine: ControlledAppEngine;

    beforeEach(() => {
        vi.useFakeTimers();
        devices = [];
        logs = [];
        sent = [];
        engine = new ControlledAppEngine({
            onDevices: (next) => { devices = next; },
            onLog: (entry) => { logs.push(entry); },
            send: (data) => { sent.push(data); },
        }, [createVirtualDevice(0), createVirtualDevice(1)]);
    });

    afterEach(() => {
        engine.resetSession();
        vi.useRealTimers();
    });

    it("announces every virtual device when the controller attaches", () => {
        engine.setControllerAttached(true);

        expect(sent).toHaveLength(1);
        expect(sent[0]).toMatchObject({ ev: "devices.snapshot", t: "ev" });
        expect((sent[0].devices as unknown[])).toHaveLength(2);
    });

    it("returns the complete device list for devices.get", () => {
        engine.setControllerAttached(true);
        sent = [];
        engine.handleControllerData({ m: "devices.get", reqId: "get-1", t: "req" });

        expect(sent).toHaveLength(1);
        expect(sent[0]).toMatchObject({ reqId: "get-1", t: "resp" });
        expect(((sent[0].result as { devices: unknown[] }).devices)).toHaveLength(2);
    });

    it("applies relative intensity to only the addressed device and channel", () => {
        engine.setControllerAttached(true);
        sent = [];
        engine.handleControllerData({
            data: { c: 1, s: "sim-slot-2", t: 3, v: 12 },
            m: "device.op",
            reqId: "intensity-1",
            t: "req",
        });

        expect(devices[0].channels[1].intensity).toBe(0);
        expect(devices[1].channels[0].intensity).toBe(0);
        expect(devices[1].channels[1].intensity).toBe(12);
        expect(sent.some((message) => message.ev === "slots.patch")).toBe(true);
        expect(sent.some((message) => message.reqId === "intensity-1" && message.t === "resp")).toBe(true);
    });

    it("records pulse packets independently and completes them after their frames are consumed", () => {
        engine.setControllerAttached(true);
        sent = [];
        engine.handleControllerData({
            data: {
                c: 0,
                d: 200,
                s: "sim-slot-1",
                t: 0,
                v: ["0102030405060708", "1112131415161718"],
            },
            m: "device.op",
            reqId: "pulse-1",
            t: "req",
        });

        expect(devices[0].channels[0].pulsePackets).toBe(1);
        expect(devices[0].channels[0].pulseFrames).toBe(2);
        expect(devices[1].channels[0].pulsePackets).toBe(0);
        expect(sent.some((message) => message.reqId === "pulse-1")).toBe(false);

        vi.advanceTimersByTime(200);
        expect(sent).toContainEqual({
            reqId: "pulse-1",
            result: {
                channel: 0,
                reason: "completed",
                slotId: "sim-slot-1",
                type: 0,
            },
            t: "resp",
        });
    });

    it("clears only matching pending tasks", () => {
        engine.setControllerAttached(true);
        sent = [];
        for (const [reqId, slotId] of [["pulse-a", "sim-slot-1"], ["pulse-b", "sim-slot-2"]]) {
            engine.handleControllerData({
                data: { c: 0, d: 1000, s: slotId, t: 0, v: ["0102030405060708"] },
                m: "device.op",
                reqId,
                t: "req",
            });
        }
        engine.handleControllerData({
            data: { s: "sim-slot-1" },
            m: "device.op.clear",
            reqId: "clear-1",
            t: "req",
        });

        expect(sent).toContainEqual({
            reqId: "pulse-a",
            result: { channel: 0, reason: "cleared", slotId: "sim-slot-1", type: 0 },
            t: "resp",
        });
        expect(sent.some((message) => message.reqId === "pulse-b")).toBe(false);
        expect(sent).toContainEqual({ reqId: "clear-1", result: {}, t: "resp" });
    });

    it("reports manual phone-side intensity changes with a slot patch", () => {
        engine.setControllerAttached(true);
        sent = [];
        engine.setChannelIntensity("sim-slot-2", 0, 33);

        expect(devices[1].channels[0].intensity).toBe(33);
        expect(sent).toHaveLength(1);
        expect(sent[0]).toMatchObject({ ev: "slots.patch", t: "ev" });
        expect(logs.at(-1)?.message).toContain("手机端上报强度 33");
    });
});
