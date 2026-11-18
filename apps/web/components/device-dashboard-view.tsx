"use client";

import { useEffect, useState } from "react";
import type { DeviceSummary } from "../lib/convex";
import { BrandMark } from "./brand-mark";

export type AuditSummary = {
  _id: string;
  action: string;
  detail?: string;
  createdAt: number;
  deviceName?: string;
};

type DashboardOperations = {
  createPairingCode: () => Promise<{ code: string; expiresAt: number }>;
  createSession: (deviceId: string) => Promise<{ sessionId: string; expiresAt: number }>;
  renameDevice: (deviceId: string, name: string) => Promise<void>;
  removeDevice: (deviceId: string) => Promise<void>;
  signOut: () => void;
};

export function DeviceDashboardView({
  devices,
  auditEvents,
  operations,
}: {
  devices: DeviceSummary[] | undefined;
  auditEvents: AuditSummary[] | undefined;
  operations: DashboardOperations;
}) {
  const [pairing, setPairing] = useState<{ code: string; expiresAt: number } | null>(null);
  const [busyDevice, setBusyDevice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copiedCode, setCopiedCode] = useState(false);
  const [pairingBusy, setPairingBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 5_000);
    return () => window.clearInterval(timer);
  }, []);

  async function addDevice() {
    if (pairingBusy) return;
    setError(null);
    setPairingBusy(true);
    try {
      setPairing(await operations.createPairingCode());
      setCopiedCode(false);
    } catch {
      setError("Could not create a setup code. Try again.");
    } finally {
      setPairingBusy(false);
    }
  }

  async function connect(deviceId: string) {
    setError(null);
    setBusyDevice(deviceId);
    try {
      const session = await operations.createSession(deviceId);
      window.location.assign(`/connect/${encodeURIComponent(session.sessionId)}`);
    } catch {
      setError("Could not start the remote session.");
    } finally {
      setBusyDevice(null);
    }
  }

  async function rename(deviceId: string, currentName: string) {
    const name = window.prompt("Device name", currentName)?.trim();
    if (!name || name === currentName) return;
    setError(null);
    setBusyDevice(deviceId);
    try {
      await operations.renameDevice(deviceId, name);
    } catch {
      setError("Could not rename the device.");
    } finally {
      setBusyDevice(null);
    }
  }

  async function remove(deviceId: string, name: string) {
    if (!window.confirm(`Remove ${name}? It must be paired again before it can reconnect.`)) return;
    setError(null);
    setBusyDevice(deviceId);
    try {
      await operations.removeDevice(deviceId);
    } catch {
      setError("Could not remove the device.");
    } finally {
      setBusyDevice(null);
    }
  }

  async function copyPairingCode(code: string) {
    setError(null);
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(code);
      } else {
        const input = document.createElement("textarea");
        input.value = code;
        input.setAttribute("readonly", "true");
        input.style.position = "fixed";
        input.style.opacity = "0";
        document.body.append(input);
        try {
          input.select();
          if (!document.execCommand("copy")) throw new Error("copy unavailable");
        } finally {
          input.remove();
        }
      }
      setCopiedCode(true);
      window.setTimeout(() => setCopiedCode(false), 1_500);
    } catch {
      setCopiedCode(false);
      setError("Could not copy the setup code. Select and copy it manually.");
    }
  }

  return (
    <main className="shell">
      <header className="topbar">
        <div className="topbar-brand">
          <BrandMark />
          <div>
            <p className="eyebrow">workspace</p>
            <h1>Devices</h1>
          </div>
        </div>
        <div className="actions">
          <button className="secondary" type="button" onClick={operations.signOut}>
            Sign out
          </button>
          <button
            className="primary"
            type="button"
            disabled={pairingBusy}
            onClick={() => void addDevice()}
          >
            {pairingBusy ? "Creating…" : "Add device"}
          </button>
        </div>
      </header>

      {error ? (
        <p className="inline-error" role="alert">
          {error}
        </p>
      ) : null}

      {pairing ? (
        <section className="pairing-card" aria-live="polite">
          <div>
            <p className="eyebrow">One-time setup code</p>
            <strong className="pairing-code" aria-label="One-time setup code">
              {pairing.code}
            </strong>
            <p>
              {now < pairing.expiresAt
                ? `Run nanoctl enroll ${pairing.code} on the computer. ${formatRemaining(pairing.expiresAt - now)} remaining.`
                : "This setup code has expired. Create a new code to continue."}
            </p>
          </div>
          <div className="pairing-actions">
            <button
              className="secondary"
              type="button"
              disabled={now >= pairing.expiresAt}
              onClick={() => void copyPairingCode(pairing.code)}
            >
              {copiedCode ? "Copied" : "Copy code"}
            </button>
            {now >= pairing.expiresAt ? (
              <button
                className="secondary"
                type="button"
                disabled={pairingBusy}
                onClick={() => void addDevice()}
              >
                New code
              </button>
            ) : null}
            <button className="secondary" type="button" onClick={() => setPairing(null)}>
              Done
            </button>
          </div>
        </section>
      ) : null}

      <section className="device-grid">
        {devices === undefined ? <p className="loading-line">Loading devices…</p> : null}
        {devices?.length === 0 ? (
          <article className="empty-state">
            <p className="eyebrow">get started</p>
            <h2>No devices yet</h2>
            <p>Install the headless service on a computer, then pair it with a one-time code.</p>
          </article>
        ) : null}
        {devices?.map((device) => {
          const status = effectiveDeviceStatus(device, now);
          const statusKey = deviceStatusKey(device, status);
          const statusLabel = deviceStatusLabel(device, status);
          return (
            <article className="device-card" key={device._id}>
              <div className="device-heading">
                <span
                  className={`presence ${
                    status === "online" && !device.ready ? "unready" : status
                  }`}
                  aria-hidden="true"
                />
                <div>
                  <h2>{device.name}</h2>
                  <p>
                    {device.platform} / {device.architecture}
                  </p>
                </div>
              </div>
              <dl>
                <div>
                  <dt>Status</dt>
                  <dd>
                    <span className={`status-pill ${statusKey}`}>{statusLabel}</span>
                  </dd>
                </div>
                <div>
                  <dt>Agent</dt>
                  <dd>{device.agentVersion}</dd>
                </div>
                <div>
                  <dt>Last seen</dt>
                  <dd>{new Date(device.lastSeenAt).toLocaleString()}</dd>
                </div>
              </dl>
              <div className="device-actions">
                <button
                  className="primary connect"
                  type="button"
                  disabled={status !== "online" || !device.ready || busyDevice === device._id}
                  onClick={() => void connect(device._id)}
                >
                  {busyDevice === device._id ? "Working…" : "Connect"}
                </button>
                <button
                  className="secondary"
                  type="button"
                  disabled={busyDevice === device._id}
                  onClick={() => void rename(device._id, device.name)}
                >
                  Rename
                </button>
                <button
                  className="danger"
                  type="button"
                  disabled={busyDevice === device._id || status === "disabled"}
                  onClick={() => void remove(device._id, device.name)}
                >
                  Remove
                </button>
              </div>
            </article>
          );
        })}
      </section>

      <section className="activity">
        <div className="activity-header">
          <p className="eyebrow">Security activity</p>
          <h2>Recent access</h2>
        </div>
        {auditEvents === undefined ? <p className="loading-line">Loading activity…</p> : null}
        {auditEvents?.length === 0 ? <p>No access events yet.</p> : null}
        {auditEvents?.length ? (
          <ol className="activity-list">
            {auditEvents.map((event) => (
              <li key={event._id}>
                <div>
                  <strong>{formatAuditAction(event.action)}</strong>
                  <span>{event.deviceName ?? "Unknown device"}</span>
                </div>
                <time dateTime={new Date(event.createdAt).toISOString()}>
                  {new Date(event.createdAt).toLocaleString()}
                </time>
              </li>
            ))}
          </ol>
        ) : null}
      </section>
    </main>
  );
}

function effectiveDeviceStatus(device: DeviceSummary, now: number): DeviceSummary["status"] {
  if (device.status === "disabled") return "disabled";
  return device.status === "online" && now - device.lastSeenAt <= device.presenceWindowMs
    ? "online"
    : "offline";
}

function deviceStatusKey(device: DeviceSummary, status: DeviceSummary["status"]): string {
  if (status === "online" && !device.ready) return "unready";
  return status;
}

function deviceStatusLabel(device: DeviceSummary, status: DeviceSummary["status"]): string {
  if (status !== "online") return status;
  if (device.ready && !device.remoteInput) return "view only";
  if (device.ready) return "online";
  return readinessLabel(device.readiness);
}

function readinessLabel(readiness: DeviceSummary["readiness"]): string {
  switch (readiness) {
    case "capture-permission":
      return "needs capture permission";
    case "upgrade-required":
      return "upgrade nanoctl to 2.0";
    case "input-permission":
      return "needs input permission";
    case "no-display":
      return "no display";
    case "encoder-unavailable":
      return "encoder unavailable";
    case "starting":
      return "starting";
    default:
      return "needs attention";
  }
}

function formatRemaining(milliseconds: number): string {
  const seconds = Math.max(0, Math.ceil(milliseconds / 1_000));
  const minutes = Math.floor(seconds / 60);
  return minutes > 0 ? `Expires in ${minutes}m ${seconds % 60}s.` : `Expires in ${seconds}s.`;
}

function formatAuditAction(action: string): string {
  return action
    .split(".")
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(" ");
}
