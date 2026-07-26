import { assertSignalEnvelope, type SignalEnvelope } from "@nanoctl/protocol";

export type HostSignalRow = {
  sequence: number;
  envelope: string;
};

type SignalPeer = Pick<RTCPeerConnection, "addIceCandidate" | "close" | "setRemoteDescription">;

export async function processHostSignals(
  peer: SignalPeer,
  sessionId: string,
  rows: readonly HostSignalRow[],
  processed: Set<number>,
  onEnd: (reason: string) => void,
): Promise<void> {
  const ordered = [...rows].sort((left, right) => left.sequence - right.sequence);
  for (const row of ordered) {
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
        break;
      case "ice-candidate":
        await peer.addIceCandidate({
          candidate: signal.payload.candidate,
          sdpMid: signal.payload.sdpMid,
          sdpMLineIndex: signal.payload.sdpMLineIndex,
        });
        break;
      case "ice-complete":
        await peer.addIceCandidate(null);
        break;
      case "end":
        onEnd(signal.payload.reason);
        peer.close();
        break;
      case "offer":
      case "renegotiate":
        continue;
    }
    processed.add(row.sequence);
  }
}
