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
