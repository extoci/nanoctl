/// <reference types="vite/client" />

import { convexTest } from "convex-test";
import { afterEach, describe, expect, test, vi } from "vitest";

import { internal } from "./_generated/api";
import schema from "./schema";

const modules = import.meta.glob([
  "./**/*.ts",
  "./**/*.js",
  "!./**/*.d.ts",
  "!./**/*.test.ts",
  "!./**/*.convex.test.ts",
]);

describe("retention maintenance", () => {
  afterEach(() => vi.useRealTimers());

  test("purges expired private data while retaining records inside policy windows", async () => {
    const t = convexTest(schema, modules);
    const now = Date.now();
    await t.run(async (ctx) => {
      const deviceId = await ctx.db.insert("devices", {
        ownerId: "owner",
        name: "Desktop",
        platform: "linux",
        architecture: "x64",
        agentVersion: "1.0.0",
        status: "online",
        tokenHash: "token",
        capabilitiesJson: "{}",
        lastSeenAt: now,
        createdAt: now,
      });
      const expiredSessionId = await ctx.db.insert("sessions", {
        ownerId: "owner",
        deviceId,
        state: "ended",
        expiresAt: now - 31 * 24 * 60 * 60 * 1000,
        createdAt: now - 32 * 24 * 60 * 60 * 1000,
        updatedAt: now - 31 * 24 * 60 * 60 * 1000,
        endedAt: now - 31 * 24 * 60 * 60 * 1000,
      });
      const retainedSessionId = await ctx.db.insert("sessions", {
        ownerId: "owner",
        deviceId,
        state: "ended",
        expiresAt: now - 1,
        createdAt: now - 1_000,
        updatedAt: now - 1_000,
        endedAt: now - 1_000,
      });
      await ctx.db.insert("signals", {
        sessionId: expiredSessionId,
        sender: "controller",
        kind: "offer",
        sequence: 0,
        envelope: "{}",
        expiresAt: now - 1,
        createdAt: now - 1_000,
      });
      await ctx.db.insert("signals", {
        sessionId: retainedSessionId,
        sender: "controller",
        kind: "offer",
        sequence: 0,
        envelope: "{}",
        expiresAt: now + 60_000,
        createdAt: now,
      });
      await ctx.db.insert("pairingCodes", {
        ownerId: "owner",
        codeHash: "expired",
        expiresAt: now - 25 * 60 * 60 * 1000,
        createdAt: now - 26 * 60 * 60 * 1000,
      });
      await ctx.db.insert("pairingCodes", {
        ownerId: "owner",
        codeHash: "retained",
        expiresAt: now - 23 * 60 * 60 * 1000,
        createdAt: now - 24 * 60 * 60 * 1000,
      });
      await ctx.db.insert("auditEvents", {
        ownerId: "owner",
        action: "expired",
        createdAt: now - 91 * 24 * 60 * 60 * 1000,
      });
      await ctx.db.insert("auditEvents", {
        ownerId: "owner",
        action: "retained",
        createdAt: now - 89 * 24 * 60 * 60 * 1000,
      });
      await ctx.db.insert("rateLimits", {
        key: "expired",
        windowStartedAt: now - 2_000,
        count: 1,
        expiresAt: now - 1,
      });
      await ctx.db.insert("rateLimits", {
        key: "retained",
        windowStartedAt: now,
        count: 1,
        expiresAt: now + 60_000,
      });
    });

    await t.mutation(internal.maintenance.purgeExpired);
    const remaining = await t.run(async (ctx) => ({
      sessions: (await ctx.db.query("sessions").collect()).map((row) => row.state),
      signals: (await ctx.db.query("signals").collect()).map((row) => row.expiresAt > now),
      pairings: (await ctx.db.query("pairingCodes").collect()).map((row) => row.codeHash),
      audits: (await ctx.db.query("auditEvents").collect()).map((row) => row.action),
      rateLimits: (await ctx.db.query("rateLimits").collect()).map((row) => row.key),
    }));
    expect(remaining).toEqual({
      sessions: ["ended"],
      signals: [true],
      pairings: ["retained"],
      audits: ["retained"],
      rateLimits: ["retained"],
    });
  });

  test("schedules bounded continuation batches until an expired backlog is empty", async () => {
    vi.useFakeTimers();
    const t = convexTest(schema, modules);
    const now = Date.now();
    await t.run(async (ctx) => {
      const deviceId = await ctx.db.insert("devices", {
        ownerId: "owner",
        name: "Desktop",
        platform: "linux",
        architecture: "x64",
        agentVersion: "1.0.0",
        status: "online",
        tokenHash: "token",
        capabilitiesJson: "{}",
        lastSeenAt: now,
        createdAt: now,
      });
      const sessionId = await ctx.db.insert("sessions", {
        ownerId: "owner",
        deviceId,
        state: "connected",
        expiresAt: now + 60_000,
        createdAt: now,
        updatedAt: now,
      });
      for (let sequence = 0; sequence < 257; sequence += 1) {
        await ctx.db.insert("signals", {
          sessionId,
          sender: "controller",
          kind: "ice-complete",
          sequence,
          envelope: "{}",
          expiresAt: now - 1,
          createdAt: now - 1,
        });
      }
    });

    await t.mutation(internal.maintenance.purgeExpired);
    expect(await t.run(async (ctx) => ctx.db.query("signals").collect())).toHaveLength(1);
    await t.finishAllScheduledFunctions(vi.runAllTimers);
    expect(await t.run(async (ctx) => ctx.db.query("signals").collect())).toHaveLength(0);
  });
});
