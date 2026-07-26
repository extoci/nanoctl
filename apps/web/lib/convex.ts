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
    getState: makeFunctionReference<
      "query",
      { sessionId: string },
      {
        state: "requested" | "ringing" | "negotiating" | "connected" | "ended" | "failed";
        expiresAt: number;
        endReason?: string;
        displays: {
          id: string;
          name: string;
          width: number;
          height: number;
          scaleFactor: number;
          primary: boolean;
        }[];
      }
    >("sessions:getState"),
    create: makeFunctionReference<
      "mutation",
      { deviceId: string },
      { sessionId: string; expiresAt: number }
    >("sessions:create"),
    end: makeFunctionReference<"mutation", { sessionId: string; reason: string }, null>(
      "sessions:end",
    ),
    turnCredentials: makeFunctionReference<
      "action",
      { sessionId: string },
      {
        urls: string[];
        username: string;
        credential: string;
        expiresAt: number;
      } | null
    >("sessions:turnCredentials"),
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
