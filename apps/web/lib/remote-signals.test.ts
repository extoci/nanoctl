import { describe, expect, test } from "bun:test";

import { processHostSignals } from "./remote-signals";

function envelope(
  sequence: number,
  payload: Record<string, unknown>,
  sessionId = "session",
): string {
  return JSON.stringify({
    version: 1,
    sessionId,
    sequence,
    sender: "host",
    sentAt: 1,
    payload,
  });
}

describe("remote signal application", () => {
  test("installs the answer before candidates even when rows arrive out of order", async () => {
    const calls: string[] = [];
    let remoteDescription: RTCSessionDescription | null = null;
    let signalingState: RTCSignalingState = "stable";
    const peer = {
      get remoteDescription() {
        return remoteDescription;
      },
      setRemoteDescription: async () => {
        calls.push("answer");
        remoteDescription = { type: "answer", sdp: "v=0" } as RTCSessionDescription;
        signalingState = "stable";
      },
      get signalingState() {
        return signalingState;
      },
      addIceCandidate: async (candidate: RTCIceCandidateInit | null) => {
        calls.push(candidate ? "candidate" : "complete");
      },
      close: () => {
        calls.push("close");
      },
    };
    const processed = new Set<number>();
    await processHostSignals(
      peer,
      "session",
      [
        {
          sequence: 2,
          envelope: envelope(2, {
            type: "ice-candidate",
            candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
            sdpMid: "0",
            sdpMLineIndex: 0,
          }),
        },
        {
          sequence: 1,
          envelope: envelope(1, { type: "answer", sdp: "v=0" }),
        },
        {
          sequence: 3,
          envelope: envelope(3, { type: "ice-complete" }),
        },
      ],
      processed,
      () => {},
    );
    expect(calls).toEqual(["answer", "candidate", "complete"]);
    expect(processed).toEqual(new Set([1, 2, 3]));
  });

  test("defers candidates and completion until an answer is installed", async () => {
    const calls: string[] = [];
    let remoteDescription: RTCSessionDescription | null = null;
    let signalingState: RTCSignalingState = "stable";
    const peer = {
      get remoteDescription() {
        return remoteDescription;
      },
      setRemoteDescription: async () => {
        calls.push("answer");
        remoteDescription = { type: "answer", sdp: "v=0" } as RTCSessionDescription;
        signalingState = "stable";
      },
      get signalingState() {
        return signalingState;
      },
      addIceCandidate: async (candidate: RTCIceCandidateInit | null) => {
        calls.push(candidate ? "candidate" : "complete");
      },
      close: () => {},
    };
    const processed = new Set<number>();
    const candidate = {
      sequence: 2,
      envelope: envelope(2, {
        type: "ice-candidate",
        candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
        sdpMid: "0",
        sdpMLineIndex: 0,
      }),
    };
    const complete = { sequence: 3, envelope: envelope(3, { type: "ice-complete" }) };

    await processHostSignals(peer, "session", [candidate, complete], processed, () => {});

    expect(calls).toEqual([]);
    expect(processed).toEqual(new Set());

    await processHostSignals(
      peer,
      "session",
      [
        { sequence: 1, envelope: envelope(1, { type: "answer", sdp: "v=0" }) },
        candidate,
        complete,
      ],
      processed,
      () => {},
    );

    expect(calls).toEqual(["answer", "candidate", "complete"]);
    expect(processed).toEqual(new Set([1, 2, 3]));
  });

  test("defers restart candidates while the new local offer is outstanding", async () => {
    const calls: string[] = [];
    let signalingState: RTCSignalingState = "have-local-offer";
    const peer = {
      remoteDescription: { type: "answer", sdp: "old-answer" } as RTCSessionDescription,
      get signalingState() {
        return signalingState;
      },
      setRemoteDescription: async () => {
        calls.push("answer");
        signalingState = "stable";
      },
      addIceCandidate: async () => {
        calls.push("candidate");
      },
      close: () => {},
    };
    const processed = new Set<number>();
    const candidate = {
      sequence: 2,
      envelope: envelope(2, {
        type: "ice-candidate",
        candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
        sdpMid: "0",
        sdpMLineIndex: 0,
      }),
    };

    await processHostSignals(peer, "session", [candidate], processed, () => {});

    expect(calls).toEqual([]);
    expect(processed).toEqual(new Set());

    await processHostSignals(
      peer,
      "session",
      [{ sequence: 1, envelope: envelope(1, { type: "answer", sdp: "new-answer" }) }, candidate],
      processed,
      () => {},
    );

    expect(calls).toEqual(["answer", "candidate"]);
    expect(processed).toEqual(new Set([1, 2]));
  });

  test("does not mark rejected or cross-session signals as processed", async () => {
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => {
        throw new Error("description rejected");
      },
      addIceCandidate: async () => {},
      close: () => {},
    };
    const processed = new Set<number>();
    await expect(
      processHostSignals(
        peer,
        "session",
        [
          {
            sequence: 1,
            envelope: envelope(1, { type: "answer", sdp: "v=0" }, "other"),
          },
          {
            sequence: 2,
            envelope: envelope(2, { type: "answer", sdp: "v=0" }),
          },
        ],
        processed,
        () => {},
      ),
    ).rejects.toThrow("description rejected");
    expect(processed.size).toBe(0);
  });

  test("stops applying a signal when its peer generation is replaced", async () => {
    let releaseDescription!: () => void;
    const descriptionReady = new Promise<void>((resolve) => {
      releaseDescription = resolve;
    });
    let current = true;
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => descriptionReady,
      addIceCandidate: async () => {
        throw new Error("old peer must not receive later signals");
      },
      close: () => {},
    };
    const processed = new Set<number>();
    const processing = processHostSignals(
      peer,
      "session",
      [
        { sequence: 1, envelope: envelope(1, { type: "answer", sdp: "v=0" }) },
        {
          sequence: 2,
          envelope: envelope(2, {
            type: "ice-candidate",
            candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
            sdpMid: "0",
            sdpMLineIndex: 0,
          }),
        },
      ],
      processed,
      () => {},
      () => current,
    );
    await Promise.resolve();
    current = false;
    releaseDescription();
    await processing;
    expect(processed.size).toBe(0);
  });
});
