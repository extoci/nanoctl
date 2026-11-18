/// <reference types="vite/client" />

import { convexTest } from "convex-test";
import { expect, test } from "vitest";
import { api } from "./_generated/api";
import schema from "./schema";

const modules = import.meta.glob(["./**/*.ts", "./**/*.js", "!./**/*.d.ts", "!./**/*.test.ts"]);

const activeStates = ["requested", "ringing", "negotiating", "connected"] as const;

async function setup() {
  const t = convexTest(schema, modules);
  const now = Date.now();
  const deviceId = await t.run((ctx) =>
    ctx.db.insert("devices", {
      ownerId: "owner",
      name: "Test desktop",
      platform: "linux",
      architecture: "x64",
      agentVersion: "1.0.0",
      status: "online",
      tokenHash: "test-token",
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
      lastSeenAt: now,
      createdAt: now,
    }),
  );
  const seed = (state: (typeof activeStates)[number] | "ended" | "failed", expiresAt: number) =>
    t.run((ctx) =>
      ctx.db.insert("sessions", {
        ownerId: "owner",
        deviceId,
        state,
        createdAt: now,
        updatedAt: now,
        expiresAt,
      }),
    );
  const create = () =>
    t
      .withIdentity({ issuer: "https://shoo.dev", subject: "owner" })
      .mutation(api.sessions.create, { deviceId });
  return { t, now, seed, create };
}

test.each(activeStates)("rejects an expired-first bypass for %s sessions", async (state) => {
  const { t, now, seed, create } = await setup();
  await seed(state, now - 60_000);
  await seed(state, now + 60_000);
  const outcome = await create().then(
    () => "ADMITTED",
    (error: Error) => `REJECTED: ${error.message}`,
  );
  console.log(`sessions.create expired-first ${state}: ${outcome}`);
  expect(outcome).toBe("REJECTED: Device already has an active session");
  expect(await t.run((ctx) => ctx.db.query("sessions").collect())).toHaveLength(2);
});

test("creates a session when there are no previous sessions", async () => {
  const { create } = await setup();
  await expect(create()).resolves.toHaveProperty("sessionId");
});

test("allows expired non-terminal and live terminal sessions", async () => {
  const { now, seed, create } = await setup();
  for (const state of activeStates) await seed(state, now - 60_000);
  await seed("ended", now + 60_000);
  await seed("failed", now + 60_000);
  await expect(create()).resolves.toHaveProperty("sessionId");
});
