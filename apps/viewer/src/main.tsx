import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { HostInfo } from "@nanoctl/protocol";
import type { RemoteSessionSnapshot } from "./lib/remote-session";
import { SocketSession, type SocketStatus } from "./lib/socket-session";
import { setSoundEnabled } from "./lib/sound";
import "./style.css";
import { HostPicker } from "./lib/host-picker";

const initial: RemoteSessionSnapshot = {
  phase: "negotiating",
  reconnectAttempt: 0,
  iceGeneration: 0,
  inputEnabled: false,
  firstFrameReceived: false,
  metrics: null,
};
type KeyboardLock = { lock: (keys?: string[]) => Promise<void>; unlock: () => void };

function Viewer() {
  const videoRef = useRef<HTMLVideoElement>(null);
  const audioRef = useRef<HTMLAudioElement>(null);
  const [soundEnabled, setSound] = useState(false);
  const transport = useRef<SocketSession | null>(null);
  const [info, setInfo] = useState<HostInfo | null>(null);
  const [snapshot, setSnapshot] = useState(initial);
  const [status, setStatus] = useState<SocketStatus>("connecting");
  const [detail, setDetail] = useState("");
  const [selectedDisplay, setSelectedDisplay] = useState("");
  const [showStats, setShowStats] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const [keyboardLocked, setKeyboardLocked] = useState(false);
  const [notice, setNotice] = useState("");

  useEffect(() => {
    if (info?.hostName) document.title = `${info.hostName} · nanoctl`;
  }, [info?.hostName]);

  useEffect(() => {
    if (!videoRef.current) return;
    if (typeof window.RTCPeerConnection !== "function" || typeof window.WebSocket !== "function") {
      setStatus("error");
      setDetail("Browser unsupported. Use a current browser with WebRTC enabled.");
      return;
    }
    const session = new SocketSession({
      video: videoRef.current,
      audio: audioRef.current ?? undefined,
      onInfo: (host) => {
        setInfo(host);
        setSelectedDisplay((previous) =>
          host.displays.some((display) => display.id === previous)
            ? previous
            : (host.displays.find((display) => display.primary)?.id ?? host.displays[0]?.id ?? ""),
        );
      },
      onSnapshot: setSnapshot,
      onStatus: (next, message) => {
        setStatus(next);
        setDetail(message ?? "");
        if (next !== "online") setSnapshot(initial);
      },
    });
    transport.current = session;
    session.connect();
    return () => {
      session.dispose();
      transport.current = null;
    };
  }, []);

  useEffect(() => {
    const keyboard = (navigator as Navigator & { keyboard?: KeyboardLock }).keyboard;
    let cancelled = false;
    const release = () => {
      keyboard?.unlock();
      setKeyboardLocked(false);
    };
    const change = () => {
      const active = Boolean(document.fullscreenElement);
      setFullscreen(active);
      if (!active || !snapshot.inputEnabled || !window.isSecureContext || !keyboard) {
        release();
        return;
      }
      void keyboard
        .lock()
        .then(() => {
          if (cancelled || !document.fullscreenElement) {
            keyboard.unlock();
            return;
          }
          setKeyboardLocked(true);
        })
        .catch(() => {
          setKeyboardLocked(false);
        });
    };
    document.addEventListener("fullscreenchange", change);
    change();
    return () => {
      cancelled = true;
      document.removeEventListener("fullscreenchange", change);
      release();
    };
  }, [snapshot.inputEnabled]);

  const terminal = status === "error" || status === "replaced";
  const live = status === "online" && snapshot.phase === "connected" && snapshot.firstFrameReceived;
  const label = terminal
    ? detail || "Connection failed"
    : status === "connecting"
      ? "Connecting to your desktop"
      : status === "reconnecting"
        ? "Reconnecting"
        : snapshot.phase === "connected"
          ? live
            ? "Live"
            : "Connected · waiting for video"
          : snapshot.phase === "reconnecting"
            ? "Recovering connection"
            : snapshot.phase === "failed" || snapshot.phase === "ended"
              ? (snapshot.reason ?? "Session ended")
              : "Negotiating";
  const noDisplays = info !== null && info.displays.length === 0;
  const readiness = info?.capabilities.readiness;
  const permissionIssue = readiness === "capture-permission" || readiness === "input-permission";
  const metrics = snapshot.metrics;

  async function toggleFullscreen() {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch {
      setNotice("Fullscreen is not available in this browser.");
    }
  }

  return (
    <main className="viewer">
      <header className="bar">
        <div className="brand" aria-label="nanoctl">
          n<span>nanoctl</span>
        </div>
        <div className="host">
          <HostPicker name={info?.hostName || "Remote desktop"} />
          <span>
            {info?.platform ?? "Tailnet connection"} <i>·</i>{" "}
            {info?.remoteInput ? "Control" : "View only"}
            {info?.capabilities.clipboard && info.remoteInput && info.viewer.mayControl
              ? " · Clipboard sync on"
              : ""}
          </span>
        </div>
        <div className="identity">
          <span className="secure-dot" />
          {info?.viewer.identity.login ?? "Authenticating…"}
        </div>
      </header>
      <section className="stage" aria-label="Remote desktop">
        <audio ref={audioRef} autoPlay muted={!soundEnabled} />
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted
          tabIndex={0}
          aria-label="Remote desktop video"
        />
        {!live && (
          <div className="overlay" role={terminal ? "alert" : "status"}>
            <div className="status-card">
              {!terminal && <span className="spinner" aria-hidden="true" />}
              <span className="eyebrow">YOUR TAILNET · YOUR DESKTOP</span>
              <h1>{label}</h1>
              <p>
                {noDisplays
                  ? "The host has no displays. Attach a display and grant screen-capture permission, then reconnect."
                  : permissionIssue
                    ? "The host needs screen-recording or input permissions. Grant access on the host and reconnect."
                    : terminal
                      ? "The desktop is disconnected. Your keyboard and pointer have been released."
                      : status === "reconnecting"
                        ? "The connection was interrupted. Retrying with a new session…"
                        : "Establishing a private, direct connection to this host."}
              </p>
              {(terminal || snapshot.phase === "failed" || snapshot.phase === "ended") && (
                <button className="primary" onClick={() => transport.current?.connect()}>
                  Reconnect
                </button>
              )}
            </div>
          </div>
        )}
        {showStats && (
          <aside className="stats" aria-label="Connection statistics">
            <span>
              Resolution <b>{metrics?.resolution ?? "—"}</b>
            </span>
            <span>
              Frame rate <b>{metrics?.fps == null ? "—" : metrics.fps.toFixed(1)} fps</b>
            </span>
            <span>
              Bitrate{" "}
              <b>{metrics?.bitrateKbps == null ? "—" : `${metrics.bitrateKbps.toFixed(2)} kbps`}</b>
            </span>
            <span>
              RTT <b>{metrics ? `${metrics.rttMs} ms` : "—"}</b>
            </span>
            <span>
              Route <b>{metrics?.route ?? "unknown"}</b>
            </span>
          </aside>
        )}
        {notice && (
          <button className="notice" onClick={() => setNotice("")}>
            {notice} ×
          </button>
        )}
      </section>
      <footer className="toolbar">
        <div className="health">
          <span className={live ? "secure-dot" : "status-dot"} />
          <span
            data-testid="connection-state"
            data-phase={status === "online" ? snapshot.phase : status}
          >
            {live ? "Live" : label}
          </span>
        </div>
        <div className="actions">
          {info?.capabilities.systemAudio && (
            <button
              aria-pressed={soundEnabled}
              onClick={async () => {
                if (audioRef.current)
                  setSound(await setSoundEnabled(audioRef.current, !soundEnabled));
              }}
            >
              {soundEnabled ? "Mute sound" : "Enable sound"}
            </button>
          )}
          <label className="display-picker">
            <span>Display</span>
            <select
              aria-label="Display"
              value={selectedDisplay}
              disabled={!snapshot.inputEnabled || noDisplays || terminal || status !== "online"}
              onChange={(event) => {
                if (transport.current?.controller?.selectDisplay(event.target.value))
                  setSelectedDisplay(event.target.value);
              }}
            >
              {noDisplays || !info ? (
                <option value="">No displays</option>
              ) : (
                info.displays.map((display) => (
                  <option key={display.id} value={display.id}>
                    {display.name} ({display.width}×{display.height})
                  </option>
                ))
              )}
            </select>
          </label>
          <button
            aria-pressed={snapshot.inputEnabled}
            disabled={!info?.remoteInput || terminal || status !== "online"}
            title="Release input: Ctrl+Alt+Shift+Esc"
            onClick={() => transport.current?.controller?.setInputEnabled(!snapshot.inputEnabled)}
          >
            {info?.remoteInput ? `Input ${snapshot.inputEnabled ? "on" : "off"}` : "View only"}
          </button>
          <button aria-pressed={showStats} onClick={() => setShowStats(!showStats)}>
            Stats
          </button>
          <button onClick={() => void toggleFullscreen()}>
            {fullscreen ? "Exit fullscreen" : "Fullscreen"}
          </button>
        </div>
        <span className="escape-hint">
          {keyboardLocked ? "Keyboard locked · " : ""}Release: <kbd>Ctrl Alt Shift Esc</kbd>
        </span>
      </footer>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<Viewer />);
