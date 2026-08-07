"use client";

import { PROTOCOL_VERSION, type ControlMessage } from "@nanoctl/protocol";
import { useAction, useMutation, useQuery } from "convex/react";
import { useEffect, useRef, useState } from "react";
import { functions } from "../lib/convex";
import { reliableControlBufferIsSaturated } from "../lib/control-backpressure";
import { processHostSignals } from "../lib/remote-signals";

type ViewerMetrics = {
  resolution: string;
  fps: number;
  bitrateKbps: number;
  rttMs: number;
  packetsLost: number;
  framesDropped: number;
  route: "direct" | "relay" | "unknown";
};

export type ViewerSession = {
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
};

type ViewerOperations = {
  sendSignal: (args: { sessionId: string; envelope: string }) => Promise<unknown>;
  endSession: (args: { sessionId: string; reason: string }) => Promise<unknown>;
  getTurnCredentials: (args: { sessionId: string }) => Promise<{
    urls: string[];
    username: string;
    credential: string;
    expiresAt: number;
  } | null>;
};

const NEGOTIATION_TIMEOUT_MS = 30_000;

function isTerminalSession(session: ViewerSession | undefined): boolean {
  return Boolean(
    !session ||
    session.state === "ended" ||
    session.state === "failed" ||
    session.expiresAt <= Date.now(),
  );
}

export function RemoteViewer({ sessionId }: { sessionId: string }) {
  const sendSignal = useMutation(functions.signals.send);
  const endSession = useMutation(functions.sessions.end);
  const getTurnCredentials = useAction(functions.sessions.turnCredentials);
  const session = useQuery(functions.sessions.getState, { sessionId });
  const incoming = useQuery(functions.signals.list, {
    sessionId,
    afterSequence: -1,
  });

  return (
    <RemoteViewerCore
      key={sessionId}
      sessionId={sessionId}
      session={session}
      incoming={incoming}
      operations={{ sendSignal, endSession, getTurnCredentials }}
    />
  );
}

export function RemoteViewerCore({
  sessionId,
  session,
  incoming,
  operations: { sendSignal, endSession, getTurnCredentials },
}: {
  sessionId: string;
  session: ViewerSession | undefined;
  incoming: { sequence: number; envelope: string }[] | undefined;
  operations: ViewerOperations;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<RTCPeerConnection | null>(null);
  const peerSessionRef = useRef<string | null>(null);
  const sessionGenerationRef = useRef(0);
  const sendSequenceRef = useRef(0);
  const processedSignalsRef = useRef(new Set<number>());
  const signalTaskRef = useRef<Promise<void>>(Promise.resolve());
  const restartAttemptsRef = useRef(0);
  const inputEnabledRef = useRef(true);
  const releaseInputRef = useRef<() => void>(() => {});
  const selectDisplayRef = useRef<(displayId: string) => boolean>(() => false);
  const [status, setStatus] = useState("Negotiating");
  const [iceServers, setIceServers] = useState<RTCIceServer[] | null>(null);
  const [ending, setEnding] = useState(false);
  const [inputEnabled, setInputEnabled] = useState(true);
  const [selectedDisplay, setSelectedDisplay] = useState("");
  const [metrics, setMetrics] = useState<ViewerMetrics | null>(null);
  const iceServersSessionRef = useRef<string | null>(null);
  const sessionActive = Boolean(session && !isTerminalSession(session));

  // A viewer can be retained while the route changes between sessions. Reset every piece of
  // signaling state before the new TURN/peer effects run; sequence zero is reserved for the
  // initial offer that the Windows agent uses to discover a session.
  useEffect(() => {
    sessionGenerationRef.current += 1;
    sendSequenceRef.current = 0;
    processedSignalsRef.current = new Set<number>();
    signalTaskRef.current = Promise.resolve();
    restartAttemptsRef.current = 0;
    peerSessionRef.current = null;
    iceServersSessionRef.current = null;
    setIceServers(null);
    setSelectedDisplay("");
    setMetrics(null);
    setStatus("Negotiating");
    setEnding(false);
    inputEnabledRef.current = true;
    setInputEnabled(true);
  }, [sessionId]);

  useEffect(() => {
    if (!session) return;
    if (
      session.state === "ended" ||
      session.state === "failed" ||
      session.expiresAt <= Date.now()
    ) {
      setStatus(session.endReason ? `ended: ${session.endReason}` : session.state);
      peerRef.current?.close();
      if (videoRef.current) videoRef.current.srcObject = null;
    }
  }, [session]);

  useEffect(() => {
    if (!session || !isTerminalSession(session)) return;
    const endOnPageHide = () => {
      void endSession({ sessionId, reason: "controller disconnected" }).catch(() => {});
    };
    window.addEventListener("pagehide", endOnPageHide, { once: true });
    return () => window.removeEventListener("pagehide", endOnPageHide);
  }, [endSession, session, sessionId]);

  useEffect(() => {
    if (selectedDisplay || !session?.displays.length) return;
    setSelectedDisplay(
      session.displays.find((display) => display.primary)?.id ?? session.displays[0]?.id ?? "",
    );
  }, [selectedDisplay, session]);

  async function leaveSession() {
    if (ending) return;
    setEnding(true);
    peerRef.current?.close();
    try {
      await endSession({ sessionId, reason: "ended by controller" });
    } finally {
      window.location.assign("/dashboard");
    }
  }

  function setRemoteControl(enabled: boolean) {
    inputEnabledRef.current = enabled;
    setInputEnabled(enabled);
    if (!enabled) releaseInputRef.current();
    else videoRef.current?.focus();
  }

  useEffect(() => {
    if (!sessionActive) return;
    let cancelled = false;
    void getTurnCredentials({ sessionId })
      .then((turn) => {
        if (cancelled) return;
        iceServersSessionRef.current = sessionId;
        setIceServers([
          { urls: "stun:stun.cloudflare.com:3478" },
          ...(turn
            ? [{ urls: turn.urls, username: turn.username, credential: turn.credential }]
            : []),
        ]);
      })
      .catch(() => {
        if (!cancelled) {
          iceServersSessionRef.current = sessionId;
          setIceServers([{ urls: "stun:stun.cloudflare.com:3478" }]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [getTurnCredentials, sessionActive, sessionId]);

  useEffect(() => {
    if (!sessionActive || !iceServers || iceServersSessionRef.current !== sessionId) return;
    const generation = sessionGenerationRef.current;
    let disposed = false;
    const isCurrent = () =>
      !disposed &&
      sessionGenerationRef.current === generation &&
      peerSessionRef.current === sessionId;
    const peer = new RTCPeerConnection({
      iceServers,
      bundlePolicy: "max-bundle",
    });
    peerRef.current = peer;
    peerSessionRef.current = sessionId;
    const endOnPageHide = () => {
      if (!isCurrent()) return;
      void endSession({ sessionId, reason: "controller disconnected" });
    };
    window.addEventListener("pagehide", endOnPageHide, { once: true });
    const pointerChannel = peer.createDataChannel("nanoctl.pointer.v1", {
      ordered: false,
      maxRetransmits: 0,
    });
    const controlChannel = peer.createDataChannel("nanoctl.control.v1");
    peer.addTransceiver("video", { direction: "recvonly" });

    peer.ontrack = ({ receiver, streams, track }) => {
      if (!isCurrent()) return;
      if ("playoutDelayHint" in receiver) {
        (receiver as RTCRtpReceiver & { playoutDelayHint?: number }).playoutDelayHint = 0;
      }
      const stream = streams[0] ?? new MediaStream([track]);
      if (stream && videoRef.current) {
        videoRef.current.srcObject = stream;
        void videoRef.current.play().catch(() => {});
      }
    };
    let restartTimer: number | null = null;
    let negotiationTimer: number | null = null;
    let offerInFlight = false;
    const sendOffer = async (iceRestart: boolean) => {
      if (!isCurrent() || offerInFlight || peer.signalingState === "closed") return;
      offerInFlight = true;
      try {
        const offer = await peer.createOffer({ iceRestart });
        if (!isCurrent()) return;
        await peer.setLocalDescription(offer);
        if (!isCurrent()) return;
        const localDescription = peer.localDescription;
        if (!localDescription || localDescription.type !== "offer") {
          throw new Error("local offer was not installed");
        }
        await sendSignal({
          sessionId,
          envelope: JSON.stringify({
            version: PROTOCOL_VERSION,
            sessionId,
            sequence: sendSequenceRef.current++,
            sender: "controller",
            sentAt: Date.now(),
            payload: { type: "offer", sdp: localDescription.sdp ?? "" },
          }),
        });
        if (!iceRestart && negotiationTimer === null) {
          negotiationTimer = window.setTimeout(() => {
            negotiationTimer = null;
            if (
              !isCurrent() ||
              peer.connectionState === "connected" ||
              peer.signalingState === "closed"
            )
              return;
            setStatus("failed: host did not answer");
            peer.close();
            void endSession({
              sessionId,
              reason: "host did not answer within 30 seconds",
            }).catch(() => {});
          }, NEGOTIATION_TIMEOUT_MS);
        }
      } finally {
        offerInFlight = false;
      }
    };
    const restart = () => {
      if (!isCurrent()) return;
      if (restartAttemptsRef.current >= 3 || peer.signalingState === "closed") {
        setStatus("failed");
        peer.close();
        void endSession({ sessionId, reason: "controller connection failed" }).catch(() => {});
        return;
      }
      restartAttemptsRef.current += 1;
      setStatus(`reconnecting (${restartAttemptsRef.current}/3)`);
      void sendOffer(true).catch(() => {
        if (isCurrent()) setStatus("signaling failed");
      });
    };
    peer.onconnectionstatechange = () => {
      if (!isCurrent()) return;
      const state = peer.connectionState;
      if (state === "connected") {
        restartAttemptsRef.current = 0;
        if (negotiationTimer !== null) window.clearTimeout(negotiationTimer);
        negotiationTimer = null;
        if (restartTimer !== null) window.clearTimeout(restartTimer);
        restartTimer = null;
      } else if (state === "disconnected" && restartTimer === null) {
        restartTimer = window.setTimeout(() => {
          restartTimer = null;
          restart();
        }, 3_000);
      } else if (state === "failed") {
        if (restartTimer !== null) window.clearTimeout(restartTimer);
        restartTimer = null;
        restart();
        return;
      } else if (state === "closed") {
        return;
      }
      setStatus(state);
    };
    peer.onicecandidate = ({ candidate }) => {
      if (!isCurrent()) return;
      const payload = candidate
        ? {
            type: "ice-candidate" as const,
            candidate: candidate.candidate,
            sdpMid: candidate.sdpMid,
            sdpMLineIndex: candidate.sdpMLineIndex,
            usernameFragment: candidate.usernameFragment ?? null,
          }
        : { type: "ice-complete" as const };
      void sendSignal({
        sessionId,
        envelope: JSON.stringify({
          version: PROTOCOL_VERSION,
          sessionId,
          sequence: sendSequenceRef.current++,
          sender: "controller",
          sentAt: Date.now(),
          payload,
        }),
      }).catch(() => {
        if (isCurrent()) setStatus("signaling failed");
      });
    };

    function sendControl(payload: ControlMessage) {
      if (!isCurrent()) return;
      if (
        !inputEnabledRef.current &&
        (payload.type === "pointer" || payload.type === "key" || payload.type === "display")
      ) {
        return;
      }
      const isMotion = payload.type === "pointer" && payload.action === "move";
      const channel = isMotion ? pointerChannel : controlChannel;
      if (isMotion && channel.bufferedAmount > 64 * 1024) return;
      if (!isMotion && reliableControlBufferIsSaturated(channel.bufferedAmount)) {
        if (peer.connectionState !== "closed") {
          inputEnabledRef.current = false;
          setInputEnabled(false);
          setStatus("control channel stalled");
          peer.close();
          void endSession({
            sessionId,
            reason: "reliable control channel stalled",
          }).catch(() => {});
        }
        return;
      }
      if (channel.readyState === "open") channel.send(JSON.stringify(payload));
    }
    const video = videoRef.current;
    const releaseInput = () => {
      if (controlChannel.readyState === "open") {
        controlChannel.send(JSON.stringify({ type: "release" } satisfies ControlMessage));
      }
      video?.blur();
    };
    releaseInputRef.current = releaseInput;
    selectDisplayRef.current = (displayId) => {
      if (!inputEnabledRef.current || controlChannel.readyState !== "open") return false;
      controlChannel.send(JSON.stringify({ type: "display", displayId } satisfies ControlMessage));
      return true;
    };
    const normalizedPosition = (event: MouseEvent) => {
      if (!video || video.videoWidth === 0 || video.videoHeight === 0) return null;
      const rect = video.getBoundingClientRect();
      const sourceRatio = video.videoWidth / video.videoHeight;
      const elementRatio = rect.width / rect.height;
      const renderedWidth = sourceRatio > elementRatio ? rect.width : rect.height * sourceRatio;
      const renderedHeight = sourceRatio > elementRatio ? rect.width / sourceRatio : rect.height;
      const left = rect.left + (rect.width - renderedWidth) / 2;
      const top = rect.top + (rect.height - renderedHeight) / 2;
      return {
        x: Math.max(0, Math.min(1, (event.clientX - left) / renderedWidth)),
        y: Math.max(0, Math.min(1, (event.clientY - top) / renderedHeight)),
      };
    };
    const pointer = (event: PointerEvent) => {
      if (!video || !inputEnabledRef.current) return;
      const position = normalizedPosition(event);
      if (!position) return;
      if (event.type === "pointerdown") {
        event.preventDefault();
        video.focus();
        video.setPointerCapture(event.pointerId);
      }
      sendControl({
        type: "pointer",
        action:
          event.type === "pointermove" ? "move" : event.type === "pointerdown" ? "down" : "up",
        ...position,
        button: event.button === 1 || event.button === 2 ? event.button : 0,
      });
    };
    const wheel = (event: WheelEvent) => {
      if (!inputEnabledRef.current) return;
      const position = normalizedPosition(event);
      if (!position) return;
      event.preventDefault();
      sendControl({
        type: "pointer",
        action: "wheel",
        ...position,
        deltaX: event.deltaX,
        deltaY: event.deltaY,
      });
    };
    const contextMenu = (event: MouseEvent) => {
      if (inputEnabledRef.current) event.preventDefault();
    };
    const key = (event: KeyboardEvent) => {
      if (!inputEnabledRef.current) return;
      event.preventDefault();
      sendControl({
        type: "key",
        action: event.type === "keydown" ? "down" : "up",
        code: event.code,
        key: event.key,
        modifiers:
          Number(event.shiftKey) |
          (Number(event.ctrlKey) << 1) |
          (Number(event.altKey) << 2) |
          (Number(event.metaKey) << 3),
        repeat: event.repeat,
      });
    };
    const emergencyEscape = (event: KeyboardEvent) => {
      if (
        event.type === "keydown" &&
        event.code === "Escape" &&
        event.ctrlKey &&
        event.altKey &&
        event.shiftKey
      ) {
        event.preventDefault();
        event.stopImmediatePropagation();
        inputEnabledRef.current = false;
        setInputEnabled(false);
        releaseInput();
        if (document.fullscreenElement) void document.exitFullscreen();
      }
    };
    const releaseOnBlur = () => releaseInput();
    const releaseWhenHidden = () => {
      if (document.visibilityState === "hidden") releaseInput();
    };
    video?.addEventListener("pointermove", pointer);
    video?.addEventListener("pointerdown", pointer);
    video?.addEventListener("pointerup", pointer);
    video?.addEventListener("pointercancel", releaseInput);
    video?.addEventListener("lostpointercapture", releaseInput);
    video?.addEventListener("wheel", wheel, { passive: false });
    video?.addEventListener("contextmenu", contextMenu);
    video?.addEventListener("keydown", key);
    video?.addEventListener("keyup", key);
    window.addEventListener("keydown", emergencyEscape, true);
    window.addEventListener("blur", releaseOnBlur);
    document.addEventListener("visibilitychange", releaseWhenHidden);
    let pingNonce = 0;
    const keepalive = window.setInterval(
      () => sendControl({ type: "ping", nonce: pingNonce++, sentAt: Date.now() }),
      1_000,
    );
    let previousBytes: { value: number; timestamp: number } | null = null;
    let statsInFlight = false;
    let statsStopped = false;
    const statsTimer = window.setInterval(() => {
      if (statsInFlight || peer.connectionState === "closed") return;
      statsInFlight = true;
      void peer
        .getStats()
        .then((report) => {
          if (statsStopped || !isCurrent()) return;
          let inbound: RTCStats | undefined;
          let candidatePair: RTCStats | undefined;
          report.forEach((stats) => {
            if (
              stats.type === "inbound-rtp" &&
              (stringStat(stats, "kind") === "video" || stringStat(stats, "mediaType") === "video")
            ) {
              inbound = stats;
            }
            if (
              stats.type === "candidate-pair" &&
              stringStat(stats, "state") === "succeeded" &&
              booleanStat(stats, "nominated")
            ) {
              candidatePair = stats;
            }
          });
          if (!inbound) return;
          const bytes = numberStat(inbound, "bytesReceived") ?? 0;
          const now = inbound.timestamp;
          const elapsed = previousBytes ? now - previousBytes.timestamp : 0;
          const bitrateKbps =
            previousBytes && elapsed > 0
              ? Math.max(0, Math.round(((bytes - previousBytes.value) * 8) / elapsed))
              : 0;
          previousBytes = { value: bytes, timestamp: now };
          const remoteCandidateId = candidatePair
            ? stringStat(candidatePair, "remoteCandidateId")
            : undefined;
          const remoteCandidate = remoteCandidateId ? report.get(remoteCandidateId) : undefined;
          const route =
            stringStat(remoteCandidate, "candidateType") === "relay"
              ? "relay"
              : remoteCandidate
                ? "direct"
                : "unknown";
          const width = numberStat(inbound, "frameWidth") ?? 0;
          const height = numberStat(inbound, "frameHeight") ?? 0;
          setMetrics({
            resolution: width > 0 && height > 0 ? `${width}×${height}` : "—",
            fps: Math.round(numberStat(inbound, "framesPerSecond") ?? 0),
            bitrateKbps,
            rttMs: Math.round((numberStat(candidatePair, "currentRoundTripTime") ?? 0) * 1_000),
            packetsLost: Math.max(0, Math.round(numberStat(inbound, "packetsLost") ?? 0)),
            framesDropped: Math.max(0, Math.round(numberStat(inbound, "framesDropped") ?? 0)),
            route,
          });
        })
        .catch(() => {})
        .finally(() => {
          statsInFlight = false;
        });
    }, 1_000);

    void sendOffer(false).catch(() => {
      if (isCurrent()) setStatus("signaling failed");
    });

    return () => {
      disposed = true;
      video?.removeEventListener("pointermove", pointer);
      video?.removeEventListener("pointerdown", pointer);
      video?.removeEventListener("pointerup", pointer);
      video?.removeEventListener("pointercancel", releaseInput);
      video?.removeEventListener("lostpointercapture", releaseInput);
      video?.removeEventListener("wheel", wheel);
      video?.removeEventListener("contextmenu", contextMenu);
      video?.removeEventListener("keydown", key);
      video?.removeEventListener("keyup", key);
      window.removeEventListener("keydown", emergencyEscape, true);
      window.removeEventListener("blur", releaseOnBlur);
      document.removeEventListener("visibilitychange", releaseWhenHidden);
      releaseInput();
      releaseInputRef.current = () => {};
      selectDisplayRef.current = () => false;
      statsStopped = true;
      window.clearInterval(keepalive);
      window.clearInterval(statsTimer);
      if (restartTimer !== null) window.clearTimeout(restartTimer);
      if (negotiationTimer !== null) window.clearTimeout(negotiationTimer);
      pointerChannel.close();
      controlChannel.close();
      peer.close();
      if (videoRef.current) videoRef.current.srcObject = null;
      if (peerRef.current === peer) peerRef.current = null;
      if (peerSessionRef.current === sessionId) peerSessionRef.current = null;
      window.removeEventListener("pagehide", endOnPageHide);
    };
  }, [endSession, iceServers, sendSignal, sessionActive, sessionId]);

  useEffect(() => {
    const peer = peerRef.current;
    const generation = sessionGenerationRef.current;
    if (!peer || peerSessionRef.current !== sessionId || !incoming) return;
    signalTaskRef.current = signalTaskRef.current
      .then(() => {
        if (
          sessionGenerationRef.current !== generation ||
          peerSessionRef.current !== sessionId ||
          peerRef.current !== peer ||
          peer.signalingState === "closed"
        ) {
          return;
        }
        return processHostSignals(
          peer,
          sessionId,
          incoming,
          processedSignalsRef.current,
          (reason) => {
            if (sessionGenerationRef.current === generation) setStatus(`ended: ${reason}`);
          },
          () =>
            sessionGenerationRef.current === generation &&
            peerSessionRef.current === sessionId &&
            peerRef.current === peer &&
            peer.signalingState !== "closed",
        );
      })
      .catch(() => {
        if (sessionGenerationRef.current === generation) setStatus("signaling failed");
      });
  }, [iceServers, incoming, sessionId]);

  return (
    <main className="viewer">
      <div className="viewer-bar">
        <a
          className="viewer-back"
          href="/dashboard"
          onClick={(event) => {
            event.preventDefault();
            void leaveSession();
          }}
        >
          ← Devices
        </a>
        <div className="viewer-health">
          <span>{status}</span>
          {metrics ? (
            <small>
              {metrics.resolution} · {metrics.fps} fps · {formatBitrate(metrics.bitrateKbps)} ·{" "}
              {metrics.rttMs} ms · {metrics.route} · {metrics.packetsLost} lost ·{" "}
              {metrics.framesDropped} dropped
            </small>
          ) : null}
        </div>
        <div className="viewer-actions">
          {session && session.displays.length > 1 ? (
            <label className="display-picker">
              <span>Display</span>
              <select
                value={selectedDisplay}
                disabled={!inputEnabled}
                onChange={(event) => {
                  if (selectDisplayRef.current(event.target.value)) {
                    setSelectedDisplay(event.target.value);
                  }
                }}
              >
                {session.displays.map((display) => (
                  <option key={display.id} value={display.id}>
                    {display.name} ({display.width}×{display.height})
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          <button
            type="button"
            title="Emergency release: Ctrl+Alt+Shift+Escape"
            onClick={() => setRemoteControl(!inputEnabled)}
          >
            Control: {inputEnabled ? "on" : "off"}
          </button>
          <button type="button" onClick={() => void videoRef.current?.requestFullscreen()}>
            Fullscreen
          </button>
          <button
            className="danger"
            type="button"
            disabled={ending}
            onClick={() => void leaveSession()}
          >
            {ending ? "Ending…" : "End session"}
          </button>
        </div>
      </div>
      <video ref={videoRef} autoPlay playsInline tabIndex={0} />
    </main>
  );
}

function statValue(stats: RTCStats | undefined, key: string): unknown {
  return stats ? (stats as unknown as Record<string, unknown>)[key] : undefined;
}

function numberStat(stats: RTCStats | undefined, key: string): number | undefined {
  const value = statValue(stats, key);
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function stringStat(stats: RTCStats | undefined, key: string): string | undefined {
  const value = statValue(stats, key);
  return typeof value === "string" ? value : undefined;
}

function booleanStat(stats: RTCStats | undefined, key: string): boolean {
  return statValue(stats, key) === true;
}

function formatBitrate(kbps: number): string {
  return kbps >= 1_000 ? `${(kbps / 1_000).toFixed(1)} Mbps` : `${kbps} kbps`;
}
