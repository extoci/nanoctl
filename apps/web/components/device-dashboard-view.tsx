"use client";

import { useState } from "react";
import type { DeviceSummary } from "../lib/convex";

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

  async function addDevice() {
    setError(null);
    try {
      setPairing(await operations.createPairingCode());
    } catch {
      setError("Could not create a setup code. Try again.");
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

  return (
    <main className="shell">
      <header className="topbar">
        <div>
          <p className="eyebrow">nanoctl</p>
          <h1>Devices</h1>
        </div>
        <div className="actions">
          <button className="secondary" type="button" onClick={operations.signOut}>
            Sign out
          </button>
          <button className="primary" type="button" onClick={() => void addDevice()}>
            Add device
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
            <strong className="pairing-code">{pairing.code}</strong>
          </div>
          <p>
            Run <code>nanoctl enroll {pairing.code}</code> on the computer. This code expires in ten
            minutes.
          </p>
          <button className="secondary" type="button" onClick={() => setPairing(null)}>
            Done
          </button>
        </section>
      ) : null}

      <section className="device-grid">
        {devices === undefined ? <p>Loading devices…</p> : null}
        {devices?.length === 0 ? (
          <article className="empty-state">
            <h2>No devices yet</h2>
            <p>Install the headless service on a computer, then pair it with a one-time code.</p>
          </article>
        ) : null}
        {devices?.map((device) => (
          <article className="device-card" key={device._id}>
            <div className="device-heading">
              <span
                className={`presence ${
                  device.status === "online" && !device.ready ? "unready" : device.status
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
                  {device.status === "online" && !device.ready ? "needs attention" : device.status}
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
                disabled={device.status !== "online" || !device.ready || busyDevice === device._id}
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
                disabled={busyDevice === device._id || device.status === "disabled"}
                onClick={() => void remove(device._id, device.name)}
              >
                Remove
              </button>
            </div>
          </article>
        ))}
      </section>

      <section className="activity">
        <div>
          <p className="eyebrow">Security activity</p>
          <h2>Recent access</h2>
        </div>
        {auditEvents === undefined ? <p>Loading activity…</p> : null}
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

function formatAuditAction(action: string): string {
  return action
    .split(".")
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(" ");
}
