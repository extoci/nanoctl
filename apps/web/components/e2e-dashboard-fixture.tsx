"use client";

import { useState } from "react";
import type { DeviceSummary } from "../lib/convex";
import { DeviceDashboardView, type AuditSummary } from "./device-dashboard-view";

const NOW = Date.UTC(2026, 6, 26, 12, 0, 0);

const INITIAL_DEVICES: DeviceSummary[] = [
  {
    _id: "ready-device",
    name: "Studio workstation",
    platform: "windows",
    architecture: "x64",
    agentVersion: "1.0.0",
    status: "online",
    ready: true,
    lastSeenAt: NOW,
    createdAt: NOW,
  },
  {
    _id: "unready-device",
    name: "Linux laptop",
    platform: "linux",
    architecture: "arm64",
    agentVersion: "1.0.0",
    status: "online",
    ready: false,
    lastSeenAt: NOW,
    createdAt: NOW,
  },
  {
    _id: "offline-device",
    name: "Travel Mac",
    platform: "macos",
    architecture: "arm64",
    agentVersion: "1.0.0",
    status: "offline",
    ready: true,
    lastSeenAt: NOW - 86_400_000,
    createdAt: NOW,
  },
];

const AUDIT_EVENTS: AuditSummary[] = [
  {
    _id: "audit-1",
    action: "session.connected",
    deviceName: "Studio workstation",
    createdAt: NOW,
  },
];

export function E2eDashboardFixture({
  state,
}: {
  state: "loading" | "empty" | "mixed" | "errors";
}) {
  const [devices, setDevices] = useState(state === "empty" ? [] : INITIAL_DEVICES);
  const [signedOut, setSignedOut] = useState(false);

  if (signedOut) {
    return (
      <main className="centered">
        <h1>Fixture signed out</h1>
      </main>
    );
  }

  return (
    <DeviceDashboardView
      devices={state === "loading" ? undefined : devices}
      auditEvents={state === "loading" ? undefined : state === "empty" ? [] : AUDIT_EVENTS}
      operations={{
        createPairingCode: async () => {
          if (state === "errors") throw new Error("fixture pairing failure");
          return { code: "ABCDE-FGHJK-MNPQR-STVWX", expiresAt: NOW + 600_000 };
        },
        createSession: async () => {
          throw new Error("fixture session failure");
        },
        renameDevice: async (deviceId, name) => {
          if (state === "errors") throw new Error("fixture rename failure");
          setDevices((current) =>
            current.map((device) => (device._id === deviceId ? { ...device, name } : device)),
          );
        },
        removeDevice: async (deviceId) => {
          if (state === "errors") throw new Error("fixture removal failure");
          setDevices((current) =>
            current.map((device) =>
              device._id === deviceId ? { ...device, status: "disabled" } : device,
            ),
          );
        },
        signOut: () => setSignedOut(true),
      }}
    />
  );
}
