import { makeFunctionReference } from "convex/server";

export type DeviceSummary = {
  _id: string;
  name: string;
  platform: "windows" | "macos" | "linux";
  architecture: "x64" | "arm64";
  agentVersion: string;
  status: "online" | "offline" | "disabled";
  lastSeenAt: number;
  createdAt: number;
};

export const functions = {
  devices: {
    list: makeFunctionReference<"query", Record<string, never>, DeviceSummary[]>("devices:list"),
    createPairingCode: makeFunctionReference<
      "action",
      Record<string, never>,
      { code: string; expiresAt: number }
    >("devices:createPairingCode"),
    remove: makeFunctionReference<"mutation", { deviceId: string }, null>("devices:remove"),
    rename: makeFunctionReference<"mutation", { deviceId: string; name: string }, null>(
      "devices:rename",
    ),
  },
  sessions: {
    create: makeFunctionReference<
      "mutation",
      { deviceId: string },
      { sessionId: string; expiresAt: number }
    >("sessions:create"),
    end: makeFunctionReference<"mutation", { sessionId: string; reason: string }, null>(
      "sessions:end",
    ),
  },
  signals: {
    list: makeFunctionReference<"query", { sessionId: string; afterSequence: number }, unknown[]>(
      "signals:list",
    ),
    send: makeFunctionReference<"mutation", { sessionId: string; envelope: string }, null>(
      "signals:send",
    ),
  },
} as const;
