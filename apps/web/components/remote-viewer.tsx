"use client";

import {
  PROTOCOL_VERSION,
  assertSignalEnvelope,
  type ControlMessage,
  type SignalEnvelope,
} from "@nanoctl/protocol";
import { useAction, useMutation, useQuery } from "convex/react";
import { useEffect, useRef, useState } from "react";
import { functions } from "../lib/convex";

type ViewerMetrics = {
  resolution: string;
  fps: number;
  bitrateKbps: number;
  rttMs: number;
  packetsLost: number;
  framesDropped: number;
  route: "direct" | "relay" | "unknown";
};

export function RemoteViewer({ sessionId }: { sessionId: string }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<RTCPeerConnection | null>(null);
  const sendSequenceRef = useRef(0);
  const processedSignalsRef = useRef(new Set<number>());
  const restartAttemptsRef = useRef(0);
  const inputEnabledRef = useRef(true);
  const releaseInputRef = useRef<() => void>(() => {});
  const selectDisplayRef = useRef<(displayId: string) => boolean>(() => false);
  const sendSignal = useMutation(functions.signals.send);
  const endSession = useMutation(functions.sessions.end);
  const getTurnCredentials = useAction(functions.sessions.turnCredentials);
  const session = useQuery(functions.sessions.getState, { sessionId });
  const incoming = useQuery(functions.signals.list, {
    sessionId,
    afterSequence: -1,
  });
  const [status, setStatus] = useState("Negotiating");
  const [iceServers, setIceServers] = useState<RTCIceServer[] | null>(null);
  const [ending, setEnding] = useState(false);
  const [inputEnabled, setInputEnabled] = useState(true);
  const [selectedDisplay, setSelectedDisplay] = useState("");
  const [metrics, setMetrics] = useState<ViewerMetrics | null>(null);

  useEffect(() => {
    if (!session) return;
    if (
      session.state === "ended" ||
      session.state === "failed" ||
      session.expiresAt <= Date.now()
    ) {
      setStatus(session.endReason ? `ended: ${session.endReason}` : session.state);
      peerRef.current?.close();
    }
  }, [session]);

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
    let cancelled = false;
    void getTurnCredentials({ sessionId })
      .then((turn) => {
        if (cancelled) return;
        setIceServers([
          { urls: "stun:stun.cloudflare.com:3478" },
          ...(turn
            ? [{ urls: turn.urls, username: turn.username, credential: turn.credential }]
            : []),
        ]);
      })
      .catch(() => {
        if (!cancelled) setIceServers([{ urls: "stun:stun.cloudflare.com:3478" }]);
      });
    return () => {
      cancelled = true;
    };
  }, [getTurnCredentials, sessionId]);

  useEffect(() => {
    if (!iceServers) return;
    const peer = new RTCPeerConnection({
      iceServers,
      bundlePolicy: "max-bundle",
    });
    peerRef.current = peer;
    const endOnPageHide = () => {
      void endSession({ sessionId, reason: "controller disconnected" });
    };
    window.addEventListener("pagehide", endOnPageHide, { once: true });
    const pointerChannel = peer.createDataChannel("nanoctl.pointer.v1", {
      ordered: false,
      maxRetransmits: 0,
    });
    const controlChannel = peer.createDataChannel("nanoctl.control.v1");
    peer.addTransceiver("video", { direction: "recvonly" });

    peer.ontrack = ({ streams }) => {
      const stream = streams[0];
      if (stream && videoRef.current) videoRef.current.srcObject = stream;
    };
    let restartTimer: number | null = null;
    let offerInFlight = false;
    const sendOffer = async (iceRestart: boolean) => {
      if (offerInFlight || peer.signalingState === "closed") return;
      offerInFlight = true;
      try {
        const offer = await peer.createOffer({ iceRestart });
        await peer.setLocalDescription(offer);
        await sendSignal({
          sessionId,
          envelope: JSON.stringify({
            version: PROTOCOL_VERSION,
            sessionId,
            sequence: sendSequenceRef.current++,
            sender: "controller",
            sentAt: Date.now(),
            payload: { type: "offer", sdp: offer.sdp ?? "" },
          }),
        });
      } finally {
        offerInFlight = false;
      }
    };
    const restart = () => {
      if (restartAttemptsRef.current >= 3 || peer.signalingState === "closed") {
        setStatus("failed");
        return;
      }
      restartAttemptsRef.current += 1;
      setStatus(`reconnecting (${restartAttemptsRef.current}/3)`);
      void sendOffer(true);
    };
    peer.onconnectionstatechange = () => {
      const state = peer.connectionState;
      if (state === "connected") {
        restartAttemptsRef.current = 0;
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
      }
      setStatus(state);
    };
    peer.onicecandidate = ({ candidate }) => {
      const payload = candidate
        ? {
            type: "ice-candidate" as const,
            candidate: candidate.candidate,
            sdpMid: candidate.sdpMid,
            sdpMLineIndex: candidate.sdpMLineIndex,
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
      });
    };

    function sendControl(payload: ControlMessage) {
      if (
        !inputEnabledRef.current &&
        (payload.type === "pointer" || payload.type === "key" || payload.type === "display")
      ) {
        return;
      }
      const isMotion = payload.type === "pointer" && payload.action === "move";
      const channel = isMotion ? pointerChannel : controlChannel;
      if (isMotion && channel.bufferedAmount > 64 * 1024) return;
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
          if (statsStopped) return;
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

    void sendOffer(false);

    return () => {
      video?.removeEventListener("pointermove", pointer);
      video?.removeEventListener("pointerdown", pointer);
      video?.removeEventListener("pointerup", pointer);
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
      pointerChannel.close();
      controlChannel.close();
      peer.close();
      peerRef.current = null;
      window.removeEventListener("pagehide", endOnPageHide);
    };
  }, [endSession, iceServers, sendSignal, sessionId]);

  useEffect(() => {
    const peer = peerRef.current;
    if (!peer || !incoming) return;
    for (const row of incoming) {
      if (!row || typeof row !== "object" || !("envelope" in row)) continue;
      let envelope: unknown;
      try {
        envelope = JSON.parse(String(row.envelope));
        assertSignalEnvelope(envelope);
      } catch {
        continue;
      }
      const signal = envelope as SignalEnvelope;
      if (signal.sender !== "host") continue;
      if (processedSignalsRef.current.has(signal.sequence)) continue;
      processedSignalsRef.current.add(signal.sequence);
      if (signal.payload.type === "answer") {
        void peer.setRemoteDescription({ type: "answer", sdp: signal.payload.sdp });
      } else if (signal.payload.type === "ice-candidate") {
        void peer.addIceCandidate(signal.payload);
      } else if (signal.payload.type === "end") {
        setStatus(`ended: ${signal.payload.reason}`);
        peer.close();
      }
    }
  }, [incoming]);

  return (
    <main className="viewer">
      <div className="viewer-bar">
        <a href="/dashboard">← Devices</a>
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
