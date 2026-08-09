import { normalizeSignalEnvelope, type NormalizedSignal } from "@nanoctl/protocol";

export type HostSignalRow = {
  sequence: number;
  envelope: string;
};

type SignalPeer = Pick<
  RTCPeerConnection,
  "addIceCandidate" | "close" | "remoteDescription" | "setRemoteDescription" | "signalingState"
>;

export type SignalProcessingState = {
  remoteGeneration: number | null;
  remoteHostEpoch: string | null;
  pendingCandidates: Map<number, { key: string; candidate: RTCIceCandidateInit }[]>;
  pendingEndOfCandidates: Map<number, string>;
};

export function createSignalProcessingState(): SignalProcessingState {
  return {
    remoteGeneration: null,
    remoteHostEpoch: null,
    pendingCandidates: new Map(),
    pendingEndOfCandidates: new Map(),
  };
}

/**
 * Apply host signaling through one ordered seam. A candidate is not marked processed until its
 * matching answer/ICE generation is installed, so a reactive query can safely retry it.
 */
export async function processHostSignals(
  peer: SignalPeer,
  sessionId: string,
  rows: readonly HostSignalRow[],
  processed: Set<unknown>,
  onEnd: (reason: string) => void,
  isCurrent: () => boolean = () => true,
  state: SignalProcessingState = createSignalProcessingState(),
  controllerEpoch?: string,
): Promise<void> {
  const ordered = [...rows].sort((left, right) => left.sequence - right.sequence);
  for (const row of ordered) {
    if (!isCurrent()) return;
    if (!Number.isSafeInteger(row.sequence) || row.sequence < 0) continue;
    let signal: NormalizedSignal;
    try {
      signal = normalizeSignalEnvelope(JSON.parse(row.envelope));
    } catch {
      continue;
    }
    if (
      signal.sender !== "host" ||
      signal.sessionId !== sessionId ||
      (controllerEpoch !== undefined && signal.controllerEpoch !== controllerEpoch) ||
      signal.sequence !== row.sequence
    ) {
      continue;
    }
    const key = `${signal.hostEpoch}:${signal.generation}:${signal.sequence}`;
    if (processed.has(key)) continue;
    switch (signal.payload.type) {
      case "answer": {
        if (
          state.remoteGeneration !== null &&
          (signal.generation < state.remoteGeneration ||
            (signal.generation === state.remoteGeneration &&
              (state.remoteHostEpoch === signal.hostEpoch ||
                peer.signalingState !== "have-local-offer")))
        ) {
          processed.add(key);
          continue;
        }
        await peer.setRemoteDescription({ type: "answer", sdp: signal.payload.sdp });
        if (!isCurrent()) return;
        state.remoteGeneration = signal.generation;
        state.remoteHostEpoch = signal.hostEpoch;
        for (const pending of state.pendingCandidates.get(signal.generation) ?? []) {
          await peer.addIceCandidate(pending.candidate);
          if (!isCurrent()) return;
          processed.add(pending.key);
        }
        state.pendingCandidates.delete(signal.generation);
        const endKey = state.pendingEndOfCandidates.get(signal.generation);
        if (endKey !== undefined) {
          await peer.addIceCandidate(null);
          processed.add(endKey);
          state.pendingEndOfCandidates.delete(signal.generation);
        }
        processed.add(key);
        break;
      }
      case "ice-candidate": {
        if (state.remoteGeneration !== null && signal.generation < state.remoteGeneration) {
          processed.add(key);
          continue;
        }
        const candidate = {
          candidate: signal.payload.candidate,
          sdpMid: signal.payload.sdpMid,
          sdpMLineIndex: signal.payload.sdpMLineIndex,
          usernameFragment: signal.payload.usernameFragment ?? null,
        } satisfies RTCIceCandidateInit;
        if (state.remoteGeneration !== signal.generation) {
          const pending = state.pendingCandidates.get(signal.generation) ?? [];
          if (pending.length < 128 && !pending.some((entry) => entry.key === key)) {
            pending.push({ key, candidate });
          }
          state.pendingCandidates.set(signal.generation, pending);
          continue;
        }
        await peer.addIceCandidate(candidate);
        if (!isCurrent()) return;
        processed.add(key);
        break;
      }
      case "ice-complete": {
        if (state.remoteGeneration !== null && signal.generation < state.remoteGeneration) {
          processed.add(key);
          continue;
        }
        if (state.remoteGeneration !== signal.generation) {
          state.pendingEndOfCandidates.set(signal.generation, key);
          continue;
        }
        await peer.addIceCandidate(null);
        if (!isCurrent()) return;
        processed.add(key);
        break;
      }
      case "end":
        processed.add(key);
        if (!isCurrent()) return;
        onEnd(signal.payload.reason);
        peer.close();
        break;
      case "offer":
        // The controller is the offerer. A host offer is valid JSON but invalid for this role.
        break;
    }
  }
}
