import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { ControlMessage, HostInfo } from "@nanoctl/protocol";
import type { QualityLimits, RemoteSessionSnapshot } from "./lib/remote-session";
import { SocketSession, type SocketStatus } from "./lib/socket-session";
import { TouchGestures, attachTouchInput, videoGeometry, type TouchMode } from "./lib/touch-input";
import { SoftKeyboard } from "./soft-keyboard";
import { SoundPlayback, type SoundState } from "./lib/sound";
import {
  CODEC_CHOICES,
  loadCodecChoice,
  saveCodecChoice,
  type CodecChoice,
} from "./lib/video-codec";
import "./style.css";
import { HostPicker } from "./lib/host-picker";
import { comboMessages, connectSteps, connectionQuality, keyCombos } from "./lib/session-ui";

const initial: RemoteSessionSnapshot = {
  phase: "negotiating",
  reconnectAttempt: 0,
  iceGeneration: 0,
  inputEnabled: false,
  gamepadEnabled: false,
  gamepadStatus: "waiting",
  firstFrameReceived: false,
  metrics: null,
};
type KeyboardLock = {
  lock: (keys?: string[]) => Promise<void>;
  unlock: () => void;
};

function Viewer() {
  const videoRef = useRef<HTMLVideoElement>(null);
  const audioRef = useRef<HTMLAudioElement>(null);
  const soundPlayback = useRef<SoundPlayback | null>(null);
  const [sound, setSound] = useState<SoundState>("muted");
  const [soundPromptDismissed, setSoundPromptDismissed] = useState(false);
  const transport = useRef<SocketSession | null>(null);
  const [info, setInfo] = useState<HostInfo | null>(null);
  const [snapshot, setSnapshot] = useState(initial);
  const [status, setStatus] = useState<SocketStatus>("connecting");
  const [detail, setDetail] = useState("");
  const [selectedDisplay, setSelectedDisplay] = useState("");
  const [showStats, setShowStats] = useState(false);
  const [codecChoice, setCodecChoiceState] = useState<CodecChoice>(() => {
    try {
      return loadCodecChoice();
    } catch {
      return "auto";
    }
  });
  const codecChoiceRef = useRef(codecChoice);
  const [qualityLimits, setQualityLimitsState] = useState<QualityLimits>(() => {
    try {
      return JSON.parse(localStorage.getItem("quality-limits") ?? "{}") as QualityLimits;
    } catch {
      return {};
    }
  });
  const qualityLimitsRef = useRef(qualityLimits);
  const setQualityLimits = (next: QualityLimits) => {
    qualityLimitsRef.current = next;
    setQualityLimitsState(next);
    try {
      localStorage.setItem("quality-limits", JSON.stringify(next));
    } catch {}
    transport.current?.controller?.setQualityLimits(next);
  };
  const setCodecChoice = (next: CodecChoice) => {
    codecChoiceRef.current = next;
    setCodecChoiceState(next);
    try {
      saveCodecChoice(next);
    } catch {}
    transport.current?.connect();
  };
  const [fullscreen, setFullscreen] = useState(false);
  const [keyboardLocked, setKeyboardLocked] = useState(false);
  const [notice, setNotice] = useState("");
  const [panelOpen, setPanelOpen] = useState(false);
  const [scale, setScaleState] = useState<"fit" | "original">(() => {
    try {
      return localStorage.getItem("scale") === "original" ? "original" : "fit";
    } catch {
      return "fit";
    }
  });
  const setScale = (next: "fit" | "original") => {
    setScaleState(next);
    try {
      localStorage.setItem("scale", next);
    } catch {}
  };
  const [natural, setNatural] = useState<{
    width: number;
    height: number;
  } | null>(null);
  const [coarse] = useState(() => matchMedia("(pointer: coarse)").matches);
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const resize = () =>
      setNatural(video.videoWidth ? { width: video.videoWidth, height: video.videoHeight } : null);
    video.addEventListener("resize", resize);
    return () => video.removeEventListener("resize", resize);
  }, []);
  const [touchMode, setTouchMode] = useState<TouchMode>(() => {
    try {
      return localStorage.getItem("touch-mode") === "direct" ? "direct" : "trackpad";
    } catch {
      return "trackpad";
    }
  });
  const cursorRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    try {
      localStorage.setItem("touch-mode", touchMode);
    } catch {
      /* Storage may be unavailable. */
    }
    let cursor = { x: 0.5, y: 0.5 };
    const paintCursor = () => {
      const geometry = videoGeometry(
        video.getBoundingClientRect(),
        video.videoWidth,
        video.videoHeight,
      );
      const stage = video.parentElement?.getBoundingClientRect();
      if (!geometry || !stage || !cursorRef.current) return;
      cursorRef.current.style.left = `${geometry.left - stage.left + cursor.x * geometry.width}px`;
      cursorRef.current.style.top = `${geometry.top - stage.top + cursor.y * geometry.height}px`;
    };
    const gestures = new TouchGestures({
      mode: touchMode,
      geometry: () =>
        videoGeometry(video.getBoundingClientRect(), video.videoWidth, video.videoHeight),
      send: (message) => transport.current?.controller?.sendControl(message),
      onCursor: (position) => {
        cursor = position;
        paintCursor();
      },
      onZoom: (zoom) => {
        video.style.transform = `translate(${zoom.offsetX}px, ${zoom.offsetY}px) scale(${zoom.scale})`;
        paintCursor();
      },
    });
    const detach = attachTouchInput(video, gestures);
    const resize = new ResizeObserver(paintCursor);
    resize.observe(video);
    video.addEventListener("loadeddata", paintCursor);
    window.addEventListener("resize", paintCursor);
    paintCursor();
    return () => {
      detach();
      resize.disconnect();
      video.removeEventListener("loadeddata", paintCursor);
      window.removeEventListener("resize", paintCursor);
      video.style.transform = "";
    };
  }, [touchMode, snapshot.inputEnabled]);

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
    const audio = audioRef.current;
    if (audio) soundPlayback.current = new SoundPlayback(audio, setSound);
    const session = new SocketSession({
      video: videoRef.current,
      audio: audio ?? undefined,
      onAudioTrackChange: () => soundPlayback.current?.trackChanged(),
      onInfo: (host) => {
        setInfo(host);
        setSelectedDisplay((previous) =>
          host.displays.some((display) => display.id === previous)
            ? previous
            : (host.displays.find((display) => display.primary)?.id ?? host.displays[0]?.id ?? ""),
        );
      },
      onSnapshot: setSnapshot,
      codecChoice: () => codecChoiceRef.current,
      qualityLimits: () => qualityLimitsRef.current,
      onStatus: (next, message) => {
        setStatus(next);
        setDetail(message ?? "");
        if (next !== "online") setSnapshot(initial);
      },
    });
    transport.current = session;
    session.connect();
    return () => {
      soundPlayback.current = null;
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

  const terminal = status === "error" || status === "replaced" || status === "disconnected";
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

  const quality = live ? connectionQuality(metrics) : null;
  const steps = connectSteps(status === "online", snapshot.phase, snapshot.firstFrameReceived);
  const hostName = info?.hostName || "Remote desktop";
  const send = (message: ControlMessage) => transport.current?.controller?.sendControl(message);
  const inputReady = snapshot.inputEnabled && status === "online";
  const isIOS =
    /iPhone|iPad|iPod/.test(navigator.userAgent) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  const standalone =
    matchMedia("(display-mode: standalone)").matches ||
    (navigator as Navigator & { standalone?: boolean }).standalone;
  const gamepadAvailable = typeof navigator.getGamepads === "function";
  const showGamepad =
    info?.remoteInput && (info.platform === "windows" || info.capabilities.gamepad);
  const soundOff = sound === "muted" || sound === "blocked";
  const soundPrompt =
    status === "online" &&
    info?.capabilities.systemAudio &&
    (sound === "blocked" || (sound === "muted" && !soundPromptDismissed));
  const toggleSound = () => soundPlayback.current?.toggle();

  return (
    <main className={`viewer scale-${scale} ${panelOpen ? "panel-open" : ""}`}>
      <section className={`stage scale-${scale}`} aria-label="Remote desktop">
        <audio ref={audioRef} muted />
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted
          tabIndex={0}
          aria-label="Remote desktop video"
          style={
            scale === "original" && natural
              ? { width: natural.width, height: natural.height }
              : undefined
          }
        />
        {showStats && (
          <aside className="stats" aria-label="Connection statistics">
            <span>
              Resolution <b>{metrics?.resolution ?? "—"}</b>
            </span>
            <span>
              Frame rate <b>{metrics?.fps == null ? "—" : `${metrics.fps.toFixed(1)} fps`}</b>
            </span>
            <span>
              Bitrate{" "}
              <b>{metrics?.bitrateKbps == null ? "—" : `${metrics.bitrateKbps.toFixed(2)} kbps`}</b>
            </span>
            <span>
              RTT <b>{metrics?.rttMs == null ? "—" : `${metrics.rttMs} ms`}</b>
            </span>
            <span>
              Route <b>{metrics?.route ?? "unknown"}</b>
            </span>
            <span>
              Codec <b>{metrics?.codec ?? "—"}</b>
            </span>
            <span>
              Decoder <b>{metrics?.decoder ?? "—"}</b>
            </span>
            <span>
              Decode <b>{metrics?.decodeMs == null ? "—" : `${metrics.decodeMs.toFixed(1)} ms`}</b>
            </span>
          </aside>
        )}
        {touchMode === "trackpad" && live && (
          <span ref={cursorRef} className="touch-cursor" aria-hidden="true" />
        )}
        {!live && (
          <div className="overlay" role={terminal ? "alert" : "status"}>
            <div className="status-card">
              <span className="eyebrow">{hostName}</span>
              <h1>{label}</h1>
              <p>
                {noDisplays
                  ? "The host has no displays. Attach a display and grant screen-capture permission, then reconnect."
                  : permissionIssue
                    ? "The host needs screen-recording or input permissions. Grant access on the host and reconnect."
                    : status === "disconnected"
                      ? "Your keyboard and pointer have been released."
                      : terminal
                        ? "The desktop is disconnected. Your keyboard and pointer have been released."
                        : status === "reconnecting"
                          ? "The connection was interrupted. Retrying with a new session…"
                          : "Private, direct connection over your tailnet."}
              </p>
              {!terminal && (
                <ol className="steps" aria-label="Connection progress">
                  {steps.map((step) => (
                    <li key={step.label} data-state={step.state}>
                      <span
                        className={step.state === "active" ? "spinner" : "step-mark"}
                        aria-hidden="true"
                      />
                      {step.label}
                    </li>
                  ))}
                </ol>
              )}
              {(terminal || snapshot.phase === "failed" || snapshot.phase === "ended") && (
                <button className="primary" onClick={() => transport.current?.connect()}>
                  Reconnect
                </button>
              )}
            </div>
          </div>
        )}
        {notice && (
          <button className="notice" onClick={() => setNotice("")}>
            {notice} ×
          </button>
        )}
      </section>
      {soundPrompt && (
        <div className="sound-prompt">
          <button onClick={toggleSound}>
            {sound === "blocked" ? "Tap to resume sound" : "Tap for sound"}
          </button>
          {sound !== "blocked" && (
            <button aria-label="Dismiss sound prompt" onClick={() => setSoundPromptDismissed(true)}>
              ×
            </button>
          )}
        </div>
      )}
      <button
        className="panel-tab"
        aria-label="Session options"
        aria-expanded={panelOpen}
        aria-controls="session-panel"
        title="Session options"
        onClick={() => setPanelOpen(!panelOpen)}
      >
        <span className={quality ? `quality-dot ${quality}` : "status-dot"} aria-hidden="true" />
        <span aria-hidden="true">{panelOpen ? "›" : "‹"}</span>
      </button>
      <aside
        id="session-panel"
        className="session-panel"
        aria-label="Session panel"
        inert={!panelOpen}
      >
        <header className="panel-head">
          <HostPicker name={hostName} />
          <div className="health">
            <span className={quality ? `quality-dot ${quality}` : "status-dot"} />
            <span
              data-testid="connection-state"
              data-phase={status === "online" ? snapshot.phase : status}
            >
              {live ? "Live" : label}
            </span>
            {quality && <span className="quality-label">· {quality} connection</span>}
          </div>
          <small>
            {info?.platform ?? "Tailnet"} · {info?.remoteInput ? "Control" : "View only"} ·{" "}
            <span>{info?.viewer.identity.login ?? "Authenticating…"}</span>
            {info?.capabilities.clipboard && info.remoteInput && info.viewer.mayControl
              ? " · Clipboard sync on"
              : ""}
          </small>
        </header>
        <div className="toolbar">
          <h2>Session</h2>
          <div className="row">
            {document.fullscreenEnabled && !standalone && (
              <button onClick={() => void toggleFullscreen()}>
                {fullscreen ? "Exit fullscreen" : "Fullscreen"}
              </button>
            )}
            <button
              aria-pressed={scale === "original"}
              onClick={() => setScale(scale === "fit" ? "original" : "fit")}
            >
              {scale === "fit" ? "Shrink to fit" : "Original size"}
            </button>
          </div>
          {isIOS && !standalone && (
            <p className="panel-hint">
              For a full-screen view, tap Safari’s Share button, then Add to Home Screen.
            </p>
          )}
          {info && info.displays.length > 1 && (
            <>
              <h2>Displays</h2>
              <div className="row" role="group" aria-label="Displays">
                {info.displays.map((display) => (
                  <button
                    key={display.id}
                    aria-pressed={display.id === selectedDisplay}
                    disabled={terminal || status !== "online"}
                    title={`${display.width}×${display.height}`}
                    onClick={() => {
                      if (transport.current?.controller?.selectDisplay(display.id))
                        setSelectedDisplay(display.id);
                    }}
                  >
                    {display.name}
                  </button>
                ))}
              </div>
            </>
          )}
          {info && info.capabilities.codecs.length > 1 && (
            <>
              <h2>Video codec</h2>
              <div className="row" role="group" aria-label="Video codec">
                {CODEC_CHOICES.filter(
                  (choice) =>
                    choice.value === "auto" || info.capabilities.codecs.includes(choice.value),
                ).map((choice) => (
                  <button
                    key={choice.value}
                    aria-pressed={choice.value === codecChoice}
                    disabled={terminal}
                    onClick={() => {
                      if (choice.value !== codecChoice) setCodecChoice(choice.value);
                    }}
                  >
                    {choice.label}
                  </button>
                ))}
              </div>
            </>
          )}
          <h2>Frame rate</h2>
          <div className="row" role="group" aria-label="Maximum frame rate">
            {[undefined, 60, 30].map((fps) => (
              <button
                key={fps ?? "max"}
                aria-pressed={qualityLimits.maxFps === fps}
                disabled={terminal}
                onClick={() => setQualityLimits({ ...qualityLimits, maxFps: fps })}
              >
                {fps ? `${fps} fps` : "Max"}
              </button>
            ))}
          </div>
          <h2>Bitrate</h2>
          <div className="row" role="group" aria-label="Maximum bitrate">
            {[undefined, 20_000, 10_000, 5_000].map((kbps) => (
              <button
                key={kbps ?? "max"}
                aria-pressed={qualityLimits.maxBitrateKbps === kbps}
                disabled={terminal}
                onClick={() => setQualityLimits({ ...qualityLimits, maxBitrateKbps: kbps })}
              >
                {kbps ? `${kbps / 1_000} Mbps` : "Max"}
              </button>
            ))}
          </div>
          <h2>Input</h2>
          <div className="row">
            <button
              aria-pressed={snapshot.inputEnabled}
              disabled={!info?.remoteInput || terminal || status !== "online"}
              title="Release input: Ctrl+Alt+Shift+Esc"
              onClick={() => transport.current?.controller?.setInputEnabled(!snapshot.inputEnabled)}
            >
              {info?.remoteInput ? `Input ${snapshot.inputEnabled ? "on" : "off"}` : "View only"}
            </button>
            {showGamepad && (
              <button
                aria-pressed={snapshot.gamepadEnabled}
                disabled={
                  !info.capabilities.gamepad || !gamepadAvailable || !snapshot.inputEnabled || !live
                }
                aria-describedby="gamepad-hint"
                onClick={() =>
                  transport.current?.controller?.setGamepadEnabled(!snapshot.gamepadEnabled)
                }
              >
                {!info.capabilities.gamepad
                  ? "Gamepad unavailable"
                  : `Gamepad ${snapshot.gamepadEnabled ? "on" : "off"}`}
              </button>
            )}
            {coarse && (
              <button
                onClick={() => setTouchMode(touchMode === "trackpad" ? "direct" : "trackpad")}
              >
                {touchMode === "trackpad" ? "Trackpad" : "Direct"}
              </button>
            )}
            <span
              className="soft-keys"
              onFocusCapture={(event) => {
                if (event.target instanceof HTMLTextAreaElement) setPanelOpen(false);
              }}
            >
              <SoftKeyboard enabled={inputReady} platform={info?.platform} send={send} />
            </span>
          </div>
          {showGamepad && (
            <p className="panel-hint" id="gamepad-hint" role="status">
              {!info.capabilities.gamepad
                ? "Install ViGEmBus on the Windows PC and restart nanoctl."
                : !gamepadAvailable
                  ? "Open the HTTPS viewer link in a browser with gamepad support."
                  : snapshot.gamepadStatus === "connected"
                    ? "Controller detected. Windows sees recognized PlayStation controllers as DualShock 4 and other controllers as Xbox 360."
                    : snapshot.gamepadStatus === "unsupported"
                      ? "Controller detected, but this browser doesn’t support its standard mapping."
                      : snapshot.gamepadEnabled
                        ? "Waiting for a controller. Pair it in Bluetooth settings, then press a controller button here."
                        : "Pair your controller in Bluetooth settings, turn on Gamepad, then press a controller button."}
            </p>
          )}
          {coarse && (
            <p className="panel-hint">
              {touchMode === "trackpad"
                ? "Slide to move the pointer. Tap to click, or tap twice and hold to drag."
                : "Tap the desktop where you want to click. Touch and hold to right-click."}{" "}
              Scroll with two fingers or pinch to zoom.
            </p>
          )}
          <h2>Send keys</h2>
          <div className="row" role="group" aria-label="Send keys">
            {keyCombos(info?.platform).map((combo) => (
              <button
                key={combo.label}
                disabled={!inputReady}
                onClick={() => comboMessages(combo).forEach(send)}
              >
                {combo.label}
              </button>
            ))}
          </div>
          {info && (
            <>
              <h2>Sound</h2>
              <div className="row">
                {info.capabilities.systemAudio ? (
                  <button aria-pressed={!soundOff} onClick={toggleSound}>
                    {sound === "blocked"
                      ? "Tap to resume sound"
                      : soundOff
                        ? "Enable sound"
                        : sound === "playing"
                          ? "Mute sound"
                          : "Cancel sound"}
                  </button>
                ) : (
                  <span>Host audio unavailable</span>
                )}
              </div>
              {(sound === "waiting" || sound === "starting") && info.capabilities.systemAudio && (
                <div role="status">Waiting for host audio…</div>
              )}
            </>
          )}
          <h2>Details</h2>
          <div className="row">
            <button aria-pressed={showStats} onClick={() => setShowStats(!showStats)}>
              Stats
            </button>
          </div>
          <button
            className="disconnect"
            disabled={terminal}
            onClick={() => {
              transport.current?.disconnect();
              setPanelOpen(false);
            }}
          >
            Disconnect
          </button>
          <p className="escape-hint">
            {keyboardLocked ? "Keyboard locked · " : ""}Release: <kbd>Ctrl Alt Shift Esc</kbd>
          </p>
        </div>
      </aside>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<Viewer />);
