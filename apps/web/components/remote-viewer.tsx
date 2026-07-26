"use client";

import { PROTOCOL_VERSION, assertSignalEnvelope, type SignalEnvelope } from "@nanoctl/protocol";
import { useMutation, useQuery } from "convex/react";
import { useEffect, useRef, useState } from "react";
import { functions } from "../lib/convex";

export function RemoteViewer({ sessionId }: { sessionId: string }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const peerRef = useRef<RTCPeerConnection | null>(null);
  const sendSequenceRef = useRef(0);
  const processedSignalsRef = useRef(new Set<number>());
  const sendSignal = useMutation(functions.signals.send);
  const incoming = useQuery(functions.signals.list, {
    sessionId,
    afterSequence: -1,
  });
  const [status, setStatus] = useState("Negotiating");

  useEffect(() => {
    const turnUrls = (process.env.NEXT_PUBLIC_TURN_URLS ?? "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    const peer = new RTCPeerConnection({
      iceServers: [
        { urls: "stun:stun.cloudflare.com:3478" },
        ...(turnUrls.length > 0 ? [{ urls: turnUrls }] : []),
      ],
      bundlePolicy: "max-bundle",
    });
    peerRef.current = peer;
    const control = peer.createDataChannel("control", {
      ordered: false,
      maxRetransmits: 0,
    });
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
      if (control.readyState === "open") control.send(JSON.stringify(payload));
    }
    const video = videoRef.current;
    const pointer = (event: PointerEvent) => {
      if (!video) return;
      const rect = video.getBoundingClientRect();
      sendControl({
        type: "pointer",
        action:
          event.type === "pointermove" ? "move" : event.type === "pointerdown" ? "down" : "up",
        x: (event.clientX - rect.left) / rect.width,
        y: (event.clientY - rect.top) / rect.height,
        button: event.button === 1 || event.button === 2 ? event.button : 0,
      });
    };
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
    video?.addEventListener("keydown", key);
    video?.addEventListener("keyup", key);

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
      video?.removeEventListener("keydown", key);
      video?.removeEventListener("keyup", key);
      control.close();
      peer.close();
      peerRef.current = null;
    };
  }, [sendSignal, sessionId]);

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
        <button type="button" onClick={() => void videoRef.current?.requestFullscreen()}>
          Fullscreen
        </button>
      </div>
      <video ref={videoRef} autoPlay playsInline tabIndex={0} />
    </main>
  );
}
