"use client";

import { useAction, useMutation, useQuery } from "convex/react";
import { useState } from "react";
import { functions } from "../lib/convex";
import { signOut } from "../lib/shoo";

export function DeviceDashboard() {
  const devices = useQuery(functions.devices.list, {});
  const createPairingCode = useAction(functions.devices.createPairingCode);
  const createSession = useMutation(functions.sessions.create);
  const [pairing, setPairing] = useState<{ code: string; expiresAt: number } | null>(null);
  const [busyDevice, setBusyDevice] = useState<string | null>(null);

  async function addDevice() {
    setPairing(await createPairingCode({}));
  }

  async function connect(deviceId: string) {
    setBusyDevice(deviceId);
    try {
      const session = await createSession({ deviceId });
      window.location.assign(`/connect/${encodeURIComponent(session.sessionId)}`);
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
          <button className="secondary" type="button" onClick={() => void signOut()}>
            Sign out
          </button>
          <button className="primary" type="button" onClick={() => void addDevice()}>
            Add device
          </button>
        </div>
      </header>

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
              <span className={`presence ${device.status}`} aria-hidden="true" />
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
                <dd>{device.status}</dd>
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
            <button
              className="primary connect"
              type="button"
              disabled={device.status !== "online" || busyDevice === device._id}
              onClick={() => void connect(device._id)}
            >
              {busyDevice === device._id ? "Connecting…" : "Connect"}
            </button>
          </article>
        ))}
      </section>
    </main>
  );
}
