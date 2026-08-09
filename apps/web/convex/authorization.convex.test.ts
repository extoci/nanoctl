/// <reference types="vite/client" />

import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";
import type { Id } from "./_generated/dataModel";

import { api, internal } from "./_generated/api";
import schema from "./schema";

const modules = import.meta.glob([
  "./**/*.ts",
  "./**/*.js",
  "!./**/*.d.ts",
  "!./**/*.test.ts",
  "!./**/*.convex.test.ts",
]);

async function seedDevice(t: ReturnType<typeof convexTest>, ownerId: string) {
  return t.run(async (ctx) =>
    ctx.db.insert("devices", {
      ownerId,
      name: "Test desktop",
      platform: "linux",
      architecture: "x64",
      agentVersion: "1.0.0",
      status: "online",
      tokenHash: `token-${ownerId}`,
      capabilitiesJson: JSON.stringify({
        protocolVersion: 2,
        codecs: ["h264"],
        ready: true,
        displays: [
          {
            id: "display-1",
            name: "Display 1",
            width: 1920,
            height: 1080,
            scaleFactor: 1,
            primary: true,
          },
        ],
      }),
      lastSeenAt: Date.now(),
      createdAt: Date.now(),
    }),
  );
}

function asOwner(t: ReturnType<typeof convexTest>, subject: string) {
  return t.withIdentity({ issuer: "https://shoo.dev", subject });
}

async function claimHostForTest(
  t: ReturnType<typeof convexTest>,
  args: {
    deviceId: Id<"devices">;
    sessionId: Id<"sessions">;
    controllerEpoch: string;
    hostEpoch: string;
    expectedHostEpoch?: string;
    generation: number;
  },
) {
  expect(await t.mutation(internal.agent.claimHost, args)).toBe(true);
}

describe("control-plane authorization", () => {
  test("enforces authentication on every agent HTTP route except one-time enrollment", async () => {
    const t = convexTest(schema, modules);
    const protectedRequests: Array<[string, RequestInit | undefined]> = [
      ["/v1/agent/heartbeat", { method: "POST", body: "{}" }],
      ["/v1/agent/sessions", undefined],
      ["/v1/agent/signal", { method: "POST", body: "{}" }],
      ["/v1/agent/claim-host", { method: "POST", body: "{}" }],
      ["/v1/agent/turn?sessionId=unknown", undefined],
    ];
    for (const [path, init] of protectedRequests) {
      const response = await t.fetch(path, init);
      expect(response.status, path).toBe(401);
      expect(response.headers.get("cache-control"), path).toBe("no-store");
      await expect(response.json(), path).resolves.toEqual({ error: "unauthorized" });
    }
  });

  test("requires Shoo identity on every public control-plane operation", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    const offer = JSON.stringify({
      version: 1,
      sessionId,
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "v=0" },
    });

    const operations = [
      t.query(api.devices.list),
      t.action(api.devices.createPairingCode),
      t.mutation(api.devices.rename, { deviceId, name: "No identity" }),
      t.mutation(api.devices.remove, { deviceId }),
      t.query(api.audit.listRecent),
      t.query(api.sessions.getState, { sessionId }),
      t.mutation(api.sessions.create, { deviceId }),
      t.mutation(api.sessions.end, { sessionId, reason: "No identity" }),
      t.action(api.sessions.turnCredentials, { sessionId }),
      t.query(api.signals.list, { sessionId, afterSequence: -1 }),
      t.mutation(api.signals.send, { sessionId, envelope: offer }),
    ];
    const results = await Promise.allSettled(operations);
    expect(results).toHaveLength(11);
    expect(results.every((result) => result.status === "rejected")).toBe(true);
  });

  test("consumes one pairing code at most once under concurrent enrollment", async () => {
    const t = convexTest(schema, modules);
    await t.run(async (ctx) => {
      await ctx.db.insert("pairingCodes", {
        ownerId: "owner-a",
        codeHash: "pairing-hash",
        expiresAt: Date.now() + 60_000,
        createdAt: Date.now(),
      });
    });
    const enroll = (tokenHash: string) =>
      t.mutation(internal.agent.enroll, {
        codeHash: "pairing-hash",
        tokenHash,
        name: "Desktop",
        platform: "linux",
        architecture: "x64",
        agentVersion: "1.0.0",
        capabilitiesJson: "{}",
      });
    const results = await Promise.all([enroll("token-a"), enroll("token-b")]);
    expect(results.filter(Boolean)).toHaveLength(1);
    const devices = await t.run(async (ctx) => ctx.db.query("devices").collect());
    expect(devices).toHaveLength(1);
  });

  test("isolates device reads and mutations by Shoo subject", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const ownerB = asOwner(t, "owner-b");

    expect(await ownerA.query(api.devices.list)).toHaveLength(1);
    expect(await ownerB.query(api.devices.list)).toEqual([]);
    await expect(
      ownerB.mutation(api.devices.rename, {
        deviceId,
        name: "Stolen desktop",
      }),
    ).rejects.toThrow();
    await ownerA.mutation(api.devices.rename, {
      deviceId,
      name: "Owner desktop",
    });
    expect((await ownerA.query(api.devices.list))[0]?.name).toBe("Owner desktop");
  });

  test("isolates recent audit activity by Shoo subject", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    await t.run(async (ctx) => {
      await ctx.db.insert("auditEvents", {
        ownerId: "owner-a",
        deviceId,
        action: "session.connected",
        createdAt: 2,
      });
      await ctx.db.insert("auditEvents", {
        ownerId: "owner-b",
        action: "device.enrolled",
        createdAt: 1,
      });
    });
    const ownerA = asOwner(t, "owner-a");
    const ownerB = asOwner(t, "owner-b");
    expect(await ownerA.query(api.audit.listRecent)).toEqual([
      expect.objectContaining({
        action: "session.connected",
        deviceName: "Test desktop",
      }),
    ]);
    expect(await ownerB.query(api.audit.listRecent)).toEqual([
      expect.objectContaining({
        action: "device.enrolled",
      }),
    ]);
  });

  test("binds sessions and signaling to one owner and one active session", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const ownerB = asOwner(t, "owner-b");

    await expect(ownerB.mutation(api.sessions.create, { deviceId })).rejects.toThrow();
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    await expect(ownerA.mutation(api.sessions.create, { deviceId })).rejects.toThrow();

    const envelope = JSON.stringify({
      version: 2,
      sessionId,
      controllerEpoch: "controller-owner-a",
      hostEpoch: "",
      generation: 0,
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "v=0" },
    });
    await expect(ownerB.mutation(api.signals.send, { sessionId, envelope })).rejects.toThrow();
    await ownerA.mutation(api.signals.send, { sessionId, envelope });
    await ownerA.mutation(api.signals.send, { sessionId, envelope });

    const evidence = await t.run(async (ctx) => {
      const session = await ctx.db.get(sessionId);
      const signals = await ctx.db
        .query("signals")
        .withIndex("by_session_sender_sequence", (q) =>
          q.eq("sessionId", sessionId).eq("sender", "controller"),
        )
        .collect();
      return { state: session?.state, signalCount: signals.length };
    });
    expect(evidence).toEqual({ state: "negotiating", signalCount: 1 });
  });

  test("does not admit legacy v1 signaling into an active session", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const legacyOffer = JSON.stringify({
      version: 1,
      sessionId,
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "v=0" },
    });

    await expect(
      owner.mutation(api.signals.send, { sessionId, envelope: legacyOffer }),
    ).rejects.toThrow("Unsupported signal protocol");
  });

  test("handles replay, sequence gaps, wrong roles, expiry, and cross-device host signals", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const otherDeviceId = await seedDevice(t, "owner-b");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    const envelope = (sequence: number, payload: Record<string, unknown>, sender = "controller") =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-owner-a",
        hostEpoch: sender === "host" ? "host-owner-a" : "",
        generation: 0,
        sequence,
        sender,
        sentAt: Date.now(),
        payload,
      });

    const offer = envelope(0, { type: "offer", sdp: "v=0" });
    await ownerA.mutation(api.signals.send, { sessionId, envelope: offer });
    await ownerA.mutation(api.signals.send, { sessionId, envelope: offer });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-owner-a",
      hostEpoch: "host-owner-a",
      generation: 0,
    });
    await ownerA.mutation(api.signals.send, {
      sessionId,
      envelope: envelope(7, {
        type: "ice-candidate",
        candidate: "candidate:gap",
        sdpMid: "0",
        sdpMLineIndex: 0,
      }),
    });
    await expect(
      ownerA.mutation(api.signals.send, {
        sessionId,
        envelope: envelope(8, { type: "answer", sdp: "v=0" }),
      }),
    ).rejects.toThrow();
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId: otherDeviceId,
        sessionId,
        sequence: 0,
        envelope: envelope(0, { type: "answer", sdp: "v=0" }, "host"),
      }),
    ).toBe(false);

    const pending = await t.query(internal.agent.pendingSessions, { deviceId });
    expect(pending[0]?.signals.map((signal) => signal.sequence)).toEqual([0, 7]);
    await t.run(async (ctx) => {
      await ctx.db.patch(sessionId, { expiresAt: Date.now() - 1 });
    });
    await expect(
      ownerA.mutation(api.signals.send, {
        sessionId,
        envelope: envelope(8, { type: "ice-complete" }),
      }),
    ).rejects.toThrow("Session unavailable");
    expect(
      await t.query(internal.agent.authorizeSession, {
        deviceId,
        sessionId,
      }),
    ).toBe(false);
  });

  test("replays a fresh host epoch even when its sequence is below the old cursor", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const hostEnvelope = (
      sequence: number,
      payload: Record<string, unknown>,
      controllerEpoch: string,
      hostEpoch: string,
    ) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch,
        hostEpoch,
        generation: 0,
        sequence,
        sender: "host",
        sentAt: Date.now(),
        payload,
      });
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-old",
        hostEpoch: "",
        generation: 0,
        sequence: 0,
        sender: "controller",
        sentAt: Date.now(),
        payload: { type: "offer", sdp: "v=0" },
      }),
    });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-old",
      hostEpoch: "host-old",
      generation: 0,
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 900,
      envelope: hostEnvelope(900, { type: "answer", sdp: "v=0" }, "controller-old", "host-old"),
    });
    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-new",
      expectedControllerEpoch: "controller-old",
    });
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-new",
        hostEpoch: "",
        generation: 0,
        sequence: 1,
        sender: "controller",
        sentAt: Date.now(),
        payload: { type: "offer", sdp: "v=0 new-controller-epoch" },
      }),
    });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-new",
      hostEpoch: "host-new",
      generation: 0,
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 100,
      envelope: hostEnvelope(
        100,
        { type: "answer", sdp: "v=0 new-host-epoch" },
        "controller-new",
        "host-new",
      ),
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 101,
      envelope: hostEnvelope(
        101,
        {
          type: "ice-candidate",
          candidate: "candidate:fresh-epoch",
          sdpMid: "0",
          sdpMLineIndex: 0,
        },
        "controller-new",
        "host-new",
      ),
    });

    const signals = await owner.query(api.signals.list, {
      sessionId,
      afterSequence: 900,
    });
    expect(signals.map((signal) => signal.sequence)).toEqual([100, 101]);
  });

  test("keeps candidates that were published before the replayed answer", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const envelope = (sequence: number, payload: Record<string, unknown>) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-a",
        hostEpoch: "host-a",
        generation: 0,
        sequence,
        sender: "host",
        sentAt: Date.now(),
        payload,
      });
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-a",
        hostEpoch: "",
        generation: 0,
        sequence: 1,
        sender: "controller",
        sentAt: Date.now(),
        payload: { type: "offer", sdp: "v=0" },
      }),
    });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-a",
      hostEpoch: "host-a",
      generation: 0,
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 2,
      envelope: envelope(2, {
        type: "ice-candidate",
        candidate: "candidate:before-answer",
        sdpMid: "0",
        sdpMLineIndex: 0,
      }),
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 3,
      envelope: envelope(3, { type: "answer", sdp: "v=0 answer" }),
    });
    await t.mutation(internal.agent.sendSignal, {
      deviceId,
      sessionId,
      sequence: 4,
      envelope: envelope(4, { type: "answer", sdp: "v=0 retry" }),
    });

    const signals = await owner.query(api.signals.list, { sessionId, afterSequence: -1 });
    expect(signals.map((signal) => signal.sequence)).toEqual([2, 4]);
    expect(signals[0]?.envelope).toContain("before-answer");
    expect(signals[1]?.envelope).toContain("retry");
  });

  test("rejects delayed host rows from a replaced controller epoch", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const envelope = (
      sender: "controller" | "host",
      epoch: string,
      generation: number,
      sequence: number,
      payload: Record<string, unknown>,
    ) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: epoch,
        hostEpoch: sender === "host" ? "host-test" : "",
        generation,
        sequence,
        sender,
        sentAt: Date.now(),
        payload,
      });
    const oldOffer = envelope("controller", "controller-old", 0, 10, {
      type: "offer",
      sdp: "v=0 old",
    });
    const newOffer = envelope("controller", "controller-new", 0, 20, {
      type: "offer",
      sdp: "v=0 new",
    });
    await owner.mutation(api.signals.send, { sessionId, envelope: oldOffer });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-old",
      hostEpoch: "host-old",
      generation: 0,
    });
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 11,
        envelope: JSON.stringify({
          ...JSON.parse(envelope("host", "controller-old", 0, 11, { type: "answer", sdp: "old" })),
          hostEpoch: "host-old",
        }),
      }),
    ).toBe(true);
    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-new",
      expectedControllerEpoch: "controller-old",
    });
    await owner.mutation(api.signals.send, { sessionId, envelope: newOffer });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-new",
      hostEpoch: "host-new",
      generation: 0,
    });

    // Simulate a delayed old worker that bypassed the current request path before the epoch check
    // was deployed. The replay query must still keep it out of the new browser peer.
    await t.run(async (ctx) => {
      const session = await ctx.db.get(sessionId);
      await ctx.db.insert("signals", {
        sessionId,
        sender: "host",
        kind: "answer",
        sequence: 12,
        envelope: envelope("host", "controller-new", 0, 12, { type: "answer", sdp: "late-old" }),
        createdAt: Date.now(),
        expiresAt: session!.expiresAt,
      });
    });
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 13,
        envelope: envelope("host", "controller-old", 0, 13, { type: "answer", sdp: "old" }),
      }),
    ).toBe(false);
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 21,
        envelope: JSON.stringify({
          ...JSON.parse(envelope("host", "controller-new", 0, 21, { type: "answer", sdp: "new" })),
          hostEpoch: "host-new",
        }),
      }),
    ).toBe(true);

    const signals = await owner.query(api.signals.list, { sessionId, afterSequence: 0 });
    expect(signals.map((signal) => signal.sequence)).toEqual([21]);
    expect(JSON.parse(signals[0]!.envelope).payload.sdp).toBe("new");
  });

  test("rejects delayed controller signals after a newer peer claim", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const signal = (controllerEpoch: string, sequence: number, payload: Record<string, unknown>) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch,
        hostEpoch: "",
        generation: 0,
        sequence,
        sender: "controller",
        sentAt: Date.now(),
        payload,
      });

    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: signal("controller-old", 1, { type: "offer", sdp: "v=0 old" }),
    });
    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-new",
      expectedControllerEpoch: "controller-old",
    });
    await expect(
      owner.mutation(api.signals.send, {
        sessionId,
        envelope: signal("controller-old", 2, {
          type: "ice-candidate",
          candidate: "candidate:old",
          sdpMid: "0",
          sdpMLineIndex: 0,
        }),
      }),
    ).rejects.toThrow("Controller peer is no longer current");
    await expect(
      owner.mutation(api.signals.send, {
        sessionId,
        envelope: signal("controller-old", 3, { type: "end", reason: "late" }),
      }),
    ).rejects.toThrow("Controller peer is no longer current");
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: signal("controller-new", 4, { type: "offer", sdp: "v=0 new" }),
    });
    await expect(owner.query(api.sessions.getState, { sessionId })).resolves.toMatchObject({
      state: "negotiating",
    });
  });

  test("ignores a late scoped end from a replaced browser controller", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-old",
        hostEpoch: "",
        generation: 0,
        sequence: 1,
        sender: "controller",
        sentAt: 1,
        payload: { type: "offer", sdp: "v=0 old" },
      }),
    });
    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-new",
      expectedControllerEpoch: "controller-old",
    });

    await owner.mutation(api.sessions.end, {
      sessionId,
      reason: "late old page cleanup",
      controllerEpoch: "controller-old",
    });
    await expect(owner.query(api.sessions.getState, { sessionId })).resolves.toMatchObject({
      state: "negotiating",
    });

    await owner.mutation(api.sessions.end, {
      sessionId,
      reason: "explicit current controller end",
      controllerEpoch: "controller-new",
    });
    await expect(owner.query(api.sessions.getState, { sessionId })).resolves.toMatchObject({
      state: "ended",
    });
  });

  test("uses compare-and-swap epochs instead of client clocks for controller ownership", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });

    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-a",
    });
    await owner.mutation(api.sessions.claimController, {
      sessionId,
      controllerEpoch: "controller-b",
      expectedControllerEpoch: "controller-a",
    });
    await expect(
      owner.mutation(api.sessions.claimController, {
        sessionId,
        controllerEpoch: "delayed-controller",
        expectedControllerEpoch: "controller-a",
      }),
    ).rejects.toThrow("Controller claim is stale");
    await expect(owner.query(api.sessions.getState, { sessionId })).resolves.toMatchObject({
      controllerEpoch: "controller-b",
    });
  });

  test("switches host epochs so a native restart can recover after an old answer", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const offer = JSON.stringify({
      version: 2,
      sessionId,
      controllerEpoch: "controller-a",
      hostEpoch: "",
      generation: 0,
      sequence: 1,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "v=0" },
    });
    await owner.mutation(api.signals.send, { sessionId, envelope: offer });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-a",
      hostEpoch: "host-a",
      generation: 0,
    });
    const answer = (hostEpoch: string, sequence: number) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-a",
        hostEpoch,
        generation: 0,
        sequence,
        sender: "host",
        sentAt: Date.now(),
        payload: { type: "answer", sdp: "v=0" },
      });
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 2,
        envelope: answer("host-a", 2),
      }),
    ).toBe(true);
    expect(
      await t.mutation(internal.agent.claimHost, {
        deviceId,
        sessionId,
        controllerEpoch: "controller-a",
        hostEpoch: "host-b",
        expectedHostEpoch: "host-a",
        generation: 0,
      }),
    ).toBe(true);
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 3,
        envelope: answer("host-b", 3),
      }),
    ).toBe(true);
    await expect(owner.query(api.signals.list, { sessionId, afterSequence: -1 })).resolves.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ sequence: 3, envelope: expect.stringContaining('"host-b"') }),
      ]),
    );
  });

  test("surfaces the agent's terminal negotiation reason", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const owner = asOwner(t, "owner-a");
    const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
    const envelope = (sender: "controller" | "host", payload: Record<string, unknown>) =>
      JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: sender === "controller" ? "controller-test" : "controller-test",
        hostEpoch: sender === "host" ? "host-test" : "",
        generation: 0,
        sequence: 0,
        sender,
        sentAt: Date.now(),
        payload,
      });
    await owner.mutation(api.signals.send, {
      sessionId,
      envelope: envelope("controller", { type: "offer", sdp: "v=0" }),
    });
    await claimHostForTest(t, {
      deviceId,
      sessionId,
      controllerEpoch: "controller-test",
      hostEpoch: "host-test",
      generation: 0,
    });
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 0,
        envelope: envelope("host", {
          type: "end",
          reason: "agent could not initialize the remote desktop session",
        }),
      }),
    ).toBe(true);
    await expect(owner.query(api.sessions.getState, { sessionId })).resolves.toMatchObject({
      state: "failed",
      endReason: "agent could not initialize the remote desktop session",
    });
  });

  test("denies cross-owner session reads, ending, host signals, and TURN authorization", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const ownerB = asOwner(t, "owner-b");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });

    await expect(ownerB.query(api.sessions.getState, { sessionId })).rejects.toThrow();
    await expect(
      ownerB.query(api.signals.list, { sessionId, afterSequence: -1 }),
    ).rejects.toThrow();
    await expect(
      ownerB.mutation(api.sessions.end, { sessionId, reason: "cross-owner" }),
    ).rejects.toThrow();
    expect(
      await t.query(internal.sessions.authorizeTurn, {
        sessionId,
        ownerId: "owner-b",
      }),
    ).toBe(false);

    const session = await t.run(async (ctx) => ctx.db.get(sessionId));
    expect(session?.state).toBe("requested");
  });

  test("does not offer sessions to a reachable but unready device", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    await t.run(async (ctx) => {
      const device = await ctx.db.get(deviceId);
      if (!device) throw new Error("seeded device disappeared");
      const capabilities = JSON.parse(device.capabilitiesJson) as Record<string, unknown>;
      await ctx.db.patch(deviceId, {
        capabilitiesJson: JSON.stringify({ ...capabilities, ready: false }),
      });
    });
    const ownerA = asOwner(t, "owner-a");
    expect((await ownerA.query(api.devices.list))[0]?.ready).toBe(false);
    await expect(ownerA.mutation(api.sessions.create, { deviceId })).rejects.toThrow(
      "Device is unavailable",
    );
  });

  test("makes a controller end signal atomically terminal", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    await ownerA.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-test",
        hostEpoch: "",
        generation: 0,
        sequence: 0,
        sender: "controller",
        sentAt: Date.now(),
        payload: { type: "offer", sdp: "v=0" },
      }),
    });
    await ownerA.mutation(api.signals.send, {
      sessionId,
      envelope: JSON.stringify({
        version: 2,
        sessionId,
        controllerEpoch: "controller-test",
        hostEpoch: "",
        generation: 0,
        sequence: 1,
        sender: "controller",
        sentAt: Date.now(),
        payload: { type: "end", reason: "leaving" },
      }),
    });
    const evidence = await t.run(async (ctx) => {
      const session = await ctx.db.get(sessionId);
      const events = await ctx.db
        .query("auditEvents")
        .withIndex("by_owner_time", (q) => q.eq("ownerId", "owner-a"))
        .collect();
      return {
        state: session?.state,
        reason: session?.endReason,
        endedEvents: events.filter((event) => event.action === "session.ended").length,
      };
    });
    expect(evidence).toEqual({
      state: "ended",
      reason: "ended by controller signal",
      endedEvents: 1,
    });
  });

  test("revocation terminates sessions and invalidates the agent token", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });

    await ownerA.mutation(api.devices.remove, { deviceId });
    const evidence = await t.run(async (ctx) => ({
      device: await ctx.db.get(deviceId),
      session: await ctx.db.get(sessionId),
    }));
    expect(evidence.device?.status).toBe("disabled");
    expect(evidence.device?.tokenHash).toMatch(/^revoked:/);
    expect(evidence.session?.state).toBe("ended");
    expect(evidence.session?.endReason).toBe("device revoked");
    expect(
      await t.query(internal.agent.authenticate, {
        tokenHash: "token-owner-a",
      }),
    ).toBeNull();
  });

  test("rejects controller signaling after a terminal failure", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    await t.run(async (ctx) => {
      await ctx.db.patch(sessionId, {
        state: "failed",
        endedAt: Date.now(),
        updatedAt: Date.now(),
        endReason: "media failed",
      });
    });
    const envelope = JSON.stringify({
      version: 1,
      sessionId,
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "v=0" },
    });
    await expect(ownerA.mutation(api.signals.send, { sessionId, envelope })).rejects.toThrow();
  });

  test("makes terminal device and session mutations idempotent", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });

    await ownerA.mutation(api.sessions.end, { sessionId, reason: "done" });
    await ownerA.mutation(api.sessions.end, { sessionId, reason: "duplicate" });
    await ownerA.mutation(api.devices.remove, { deviceId });
    await ownerA.mutation(api.devices.remove, { deviceId });

    const evidence = await t.run(async (ctx) => {
      const device = await ctx.db.get(deviceId);
      const events = await ctx.db.query("auditEvents").collect();
      return {
        tokenHash: device?.tokenHash,
        endedEvents: events.filter((event) => event.action === "session.ended").length,
        revokedEvents: events.filter((event) => event.action === "device.revoked").length,
      };
    });
    expect(evidence.tokenHash).toBe("revoked:token-owner-a");
    expect(evidence.endedEvents).toBe(1);
    expect(evidence.revokedEvents).toBe(1);
  });

  test("matches the terminal session model across generated signaling transitions", async () => {
    type State = "requested" | "negotiating" | "connected" | "ended" | "failed";
    type Command = "controller-offer" | "controller-end" | "host-answer" | "host-end";
    const commands: Command[] = ["controller-offer", "controller-end", "host-answer", "host-end"];

    for (let seed = 1; seed <= 8; seed += 1) {
      const t = convexTest(schema, modules);
      const deviceId = await seedDevice(t, `owner-${seed}`);
      const owner = asOwner(t, `owner-${seed}`);
      const { sessionId } = await owner.mutation(api.sessions.create, { deviceId });
      await owner.mutation(api.sessions.claimController, {
        sessionId,
        controllerEpoch: `controller-${seed}`,
      });
      let model: State = "requested";
      let random = seed;
      let controllerSequence = 0;
      let hostSequence = 0;
      let hasOffer = false;
      let activeGeneration = -1;
      const controllerEpoch = `controller-${seed}`;
      const hostEpoch = `host-${seed}`;
      for (let step = 0; step < 12; step += 1) {
        random = (random * 1_664_525 + 1_013_904_223) >>> 0;
        const command = commands[random % commands.length]!;
        const terminal = model === "ended" || model === "failed";
        if (command.startsWith("controller")) {
          const kind = command === "controller-offer" ? "offer" : "end";
          const sequence = controllerSequence++;
          const generation =
            kind === "offer" ? activeGeneration + 1 : Math.max(0, activeGeneration);
          const operation = owner.mutation(api.signals.send, {
            sessionId,
            envelope: JSON.stringify({
              version: 2,
              sessionId,
              controllerEpoch,
              hostEpoch: "",
              generation,
              sequence,
              sender: "controller",
              sentAt: Date.now(),
              payload:
                kind === "offer"
                  ? { type: kind, sdp: `v=0\r\na=x-seed:${seed}-${step}` }
                  : { type: kind, reason: "model terminal" },
            }),
          });
          if (terminal) {
            await expect(operation).rejects.toThrow();
          } else {
            await operation;
            if (kind === "end") model = "ended";
            else {
              activeGeneration = generation;
              await claimHostForTest(t, {
                deviceId,
                sessionId,
                controllerEpoch,
                hostEpoch,
                ...(hasOffer ? { expectedHostEpoch: hostEpoch } : {}),
                generation,
              });
              hasOffer = true;
              if (model === "requested") model = "negotiating";
            }
          }
        } else {
          const kind = command === "host-answer" ? "answer" : "end";
          const sequence = hostSequence++;
          const accepted = await t.mutation(internal.agent.sendSignal, {
            deviceId,
            sessionId,
            sequence,
            envelope: JSON.stringify({
              version: 2,
              sessionId,
              controllerEpoch,
              hostEpoch,
              generation: activeGeneration,
              sequence,
              sender: "host",
              sentAt: Date.now(),
              payload:
                kind === "answer"
                  ? { type: kind, sdp: `v=0\r\na=x-seed:${seed}-${step}` }
                  : { type: kind, reason: "model media failure" },
            }),
          });
          expect(accepted).toBe(!terminal && hasOffer);
          if (!terminal && hasOffer) {
            if (kind === "end") model = "failed";
            else if (model === "negotiating") model = "connected";
          }
        }
        const actual = await t.run(async (ctx) => (await ctx.db.get(sessionId))?.state);
        expect(actual).toBe(model);
      }
    }
  });

  test("treats malformed and terminal host session IDs as unavailable", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    expect(
      await t.query(internal.agent.authorizeSession, {
        deviceId,
        sessionId: "not-a-convex-id",
      }),
    ).toBe(false);
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId: "not-a-convex-id",
        sequence: 0,
        envelope: "{}",
      }),
    ).toBe(false);

    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    await ownerA.mutation(api.sessions.end, { sessionId, reason: "done" });
    expect(
      await t.mutation(internal.agent.sendSignal, {
        deviceId,
        sessionId,
        sequence: 0,
        envelope: JSON.stringify({
          version: 1,
          sessionId,
          sequence: 0,
          sender: "host",
          sentAt: Date.now(),
          payload: { type: "answer", sdp: "v=0" },
        }),
      }),
    ).toBe(false);
  });
});
