"use client";

import { PROTOCOL_VERSION, assertSignalEnvelope, type SignalEnvelope } from "@nanoctl/protocol";
import { useAction, useMutation, useQuery } from "convex/react";
import { useEffect, useRef, useState } from "react";
import { functions } from "../lib/convex";

export function RemoteViewer({ sessionId }: { sessionId: string }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<RTCPeerConnection | null>(null);
  const sendSequenceRef = useRef(0);
  const processedSignalsRef = useRef(new Set<number>());
  const sendSignal = useMutation(functions.signals.send);
  const endSession = useMutation(functions.sessions.end);
  const getTurnCredentials = useAction(functions.sessions.turnCredentials);
  const incoming = useQuery(functions.signals.list, {
    sessionId,
    afterSequence: -1,
  });
  const [status, setStatus] = useState("Negotiating");
  const [iceServers, setIceServers] = useState<RTCIceServer[] | null>(null);
  const [ending, setEnding] = useState(false);

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
    peer.addTransceiver("audio", { direction: "recvonly" });

    peer.ontrack = ({ streams }) => {
      const stream = streams[0];
      if (stream && videoRef.current) videoRef.current.srcObject = stream;
    };
    peer.onconnectionstatechange = () => setStatus(peer.connectionState);
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

    function sendControl(payload: object) {
      const isMotion =
        "type" in payload &&
        payload.type === "pointer" &&
        "action" in payload &&
        payload.action === "move";
      const channel = isMotion ? pointerChannel : controlChannel;
      if (isMotion && channel.bufferedAmount > 64 * 1024) return;
      if (channel.readyState === "open") channel.send(JSON.stringify(payload));
    }
    const video = videoRef.current;
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
      if (!video) return;
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
    const contextMenu = (event: MouseEvent) => event.preventDefault();
    const key = (event: KeyboardEvent) => {
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
    video?.addEventListener("pointermove", pointer);
    video?.addEventListener("pointerdown", pointer);
    video?.addEventListener("pointerup", pointer);
    video?.addEventListener("wheel", wheel, { passive: false });
    video?.addEventListener("contextmenu", contextMenu);
    video?.addEventListener("keydown", key);
    video?.addEventListener("keyup", key);
    const keepalive = window.setInterval(() => sendControl({ type: "ping" }), 1_000);

    void (async () => {
      const offer = await peer.createOffer();
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
    })();

    return () => {
      video?.removeEventListener("pointermove", pointer);
      video?.removeEventListener("pointerdown", pointer);
      video?.removeEventListener("pointerup", pointer);
      video?.removeEventListener("wheel", wheel);
      video?.removeEventListener("contextmenu", contextMenu);
      video?.removeEventListener("keydown", key);
      video?.removeEventListener("keyup", key);
      window.clearInterval(keepalive);
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
        <span>{status}</span>
        <div className="viewer-actions">
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
