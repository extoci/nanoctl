/// <reference types="vite/client" />

import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";

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
        protocolVersion: 1,
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

describe("control-plane authorization", () => {
  test("enforces authentication on every agent HTTP route except one-time enrollment", async () => {
    const t = convexTest(schema, modules);
    const protectedRequests: Array<[string, RequestInit | undefined]> = [
      ["/v1/agent/heartbeat", { method: "POST", body: "{}" }],
      ["/v1/agent/sessions", undefined],
      ["/v1/agent/signal", { method: "POST", body: "{}" }],
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
      version: 1,
      sessionId,
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

  test("handles replay, sequence gaps, wrong roles, expiry, and cross-device host signals", async () => {
    const t = convexTest(schema, modules);
    const deviceId = await seedDevice(t, "owner-a");
    const otherDeviceId = await seedDevice(t, "owner-b");
    const ownerA = asOwner(t, "owner-a");
    const { sessionId } = await ownerA.mutation(api.sessions.create, { deviceId });
    const envelope = (sequence: number, payload: Record<string, unknown>, sender = "controller") =>
      JSON.stringify({
        version: 1,
        sessionId,
        sequence,
        sender,
        sentAt: Date.now(),
        payload,
      });

    const offer = envelope(0, { type: "offer", sdp: "v=0" });
    await ownerA.mutation(api.signals.send, { sessionId, envelope: offer });
    await ownerA.mutation(api.signals.send, { sessionId, envelope: offer });
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
        version: 1,
        sessionId,
        sequence: 0,
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
