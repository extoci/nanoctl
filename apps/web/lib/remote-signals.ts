import { assertSignalEnvelope, type SignalEnvelope } from "@nanoctl/protocol";

export type HostSignalRow = {
  sequence: number;
  envelope: string;
};

type SignalPeer = Pick<
  RTCPeerConnection,
  "addIceCandidate" | "close" | "remoteDescription" | "setRemoteDescription" | "signalingState"
>;

export async function processHostSignals(
  peer: SignalPeer,
  sessionId: string,
  rows: readonly HostSignalRow[],
  processed: Set<number>,
  onEnd: (reason: string) => void,
  isCurrent: () => boolean = () => true,
): Promise<void> {
  const ordered = [...rows].sort((left, right) => left.sequence - right.sequence);
  for (const row of ordered) {
    if (!isCurrent()) return;
    if (!Number.isSafeInteger(row.sequence) || row.sequence < 0 || processed.has(row.sequence)) {
      continue;
    }
    let unknownEnvelope: unknown;
    try {
      unknownEnvelope = JSON.parse(row.envelope);
      assertSignalEnvelope(unknownEnvelope);
    } catch {
      continue;
    }
    const signal = unknownEnvelope as SignalEnvelope;
    if (
      signal.sender !== "host" ||
      signal.sessionId !== sessionId ||
      signal.sequence !== row.sequence
    ) {
      continue;
    }
    switch (signal.payload.type) {
      case "answer":
        await peer.setRemoteDescription({
          type: "answer",
          sdp: signal.payload.sdp,
        });
        if (!isCurrent()) return;
        break;
      case "ice-candidate":
        // Safari rejects ICE candidates until the answer has been installed. The host can publish
        // a candidate before its answer reaches Convex because both writes are asynchronous. Keep
        // the row unprocessed so the next reactive query retries it after the answer arrives;
        // Chromium's more permissive queuing must not hide this ordering bug.
        if (!peer.remoteDescription || peer.signalingState === "have-local-offer") continue;
        await peer.addIceCandidate({
          candidate: signal.payload.candidate,
          sdpMid: signal.payload.sdpMid,
          sdpMLineIndex: signal.payload.sdpMLineIndex,
          usernameFragment: signal.payload.usernameFragment,
        });
        if (!isCurrent()) return;
        break;
      case "ice-complete":
        if (!peer.remoteDescription || peer.signalingState === "have-local-offer") continue;
        await peer.addIceCandidate(null);
        if (!isCurrent()) return;
        break;
      case "end":
        if (!isCurrent()) return;
        onEnd(signal.payload.reason);
        peer.close();
        break;
      case "offer":
        continue;
    }
    processed.add(row.sequence);
  }
}
