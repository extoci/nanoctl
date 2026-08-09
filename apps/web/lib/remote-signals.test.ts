import { describe, expect, test } from "bun:test";

import { createSignalProcessingState, processHostSignals } from "./remote-signals";

function envelope(
  sequence: number,
  payload: Record<string, unknown>,
  sessionId = "session",
  generation = 0,
  controllerEpoch = "controller-epoch",
  hostEpoch = "host-epoch",
): string {
  return JSON.stringify({
    version: 2,
    sessionId,
    controllerEpoch,
    hostEpoch,
    generation,
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
    const processed = new Set<string>();
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
    expect(processed).toEqual(new Set(["host-epoch:0:1", "host-epoch:0:2", "host-epoch:0:3"]));
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
    const processed = new Set<string>();
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
      [{ sequence: 1, envelope: envelope(1, { type: "answer", sdp: "v=0" }) }, candidate, complete],
      processed,
      () => {},
    );

    expect(calls).toEqual(["answer", "candidate", "complete"]);
    expect(processed).toEqual(new Set(["host-epoch:0:1", "host-epoch:0:2", "host-epoch:0:3"]));
  });

  test("does not apply a duplicate answer after the peer is stable", async () => {
    let answerCount = 0;
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => {
        answerCount += 1;
      },
      addIceCandidate: async () => {},
      close: () => {},
    };
    const processed = new Set<string>();
    const state = createSignalProcessingState();

    await processHostSignals(
      peer,
      "session",
      [{ sequence: 1, envelope: envelope(1, { type: "answer", sdp: "answer" }) }],
      processed,
      () => {},
      () => true,
      state,
    );
    await processHostSignals(
      peer,
      "session",
      [{ sequence: 2, envelope: envelope(2, { type: "answer", sdp: "duplicate" }) }],
      processed,
      () => {},
      () => true,
      state,
    );

    expect(answerCount).toBe(1);
    expect(processed).toEqual(new Set(["host-epoch:0:1", "host-epoch:0:2"]));
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
    const processed = new Set<string>();
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
    expect(processed).toEqual(new Set(["host-epoch:0:1", "host-epoch:0:2"]));
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
    const processed = new Set<string>();
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
    const processed = new Set<string>();
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

  test("does not apply an old generation after an ICE restart", async () => {
    const calls: string[] = [];
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => {
        calls.push("answer");
      },
      addIceCandidate: async (candidate: RTCIceCandidateInit | null) => {
        calls.push(candidate ? "candidate" : "complete");
      },
      close: () => {},
    };
    const processed = new Set<string>();
    const state = createSignalProcessingState();
    await processHostSignals(
      peer,
      "session",
      [
        { sequence: 10, envelope: envelope(10, { type: "answer", sdp: "new" }, "session", 1) },
        {
          sequence: 11,
          envelope: envelope(
            11,
            {
              type: "ice-candidate",
              candidate: "candidate:new",
              sdpMid: "0",
              sdpMLineIndex: 0,
            },
            "session",
            1,
          ),
        },
      ],
      processed,
      () => {},
      () => true,
      state,
    );
    await processHostSignals(
      peer,
      "session",
      [
        {
          sequence: 12,
          envelope: envelope(
            12,
            {
              type: "ice-candidate",
              candidate: "candidate:old",
              sdpMid: "0",
              sdpMLineIndex: 0,
            },
            "session",
            0,
          ),
        },
        { sequence: 13, envelope: envelope(13, { type: "ice-complete" }, "session", 0) },
      ],
      processed,
      () => {},
      () => true,
      state,
    );

    expect(calls).toEqual(["answer", "candidate"]);
    expect(processed).toEqual(
      new Set(["host-epoch:1:10", "host-epoch:1:11", "host-epoch:0:12", "host-epoch:0:13"]),
    );
  });

  test("keeps a generation's candidate pending until its matching answer arrives", async () => {
    const calls: string[] = [];
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => {
        calls.push("answer");
      },
      addIceCandidate: async (candidate: RTCIceCandidateInit | null) => {
        calls.push(candidate ? "candidate" : "complete");
      },
      close: () => {},
    };
    const processed = new Set<string>();
    const state = createSignalProcessingState();
    const candidate = {
      sequence: 21,
      envelope: envelope(
        21,
        {
          type: "ice-candidate",
          candidate: "candidate:restart",
          sdpMid: "0",
          sdpMLineIndex: 0,
        },
        "session",
        2,
      ),
    };
    const complete = {
      sequence: 22,
      envelope: envelope(22, { type: "ice-complete" }, "session", 2),
    };
    await processHostSignals(
      peer,
      "session",
      [candidate, complete],
      processed,
      () => {},
      () => true,
      state,
    );
    expect(calls).toEqual([]);
    expect(processed).toEqual(new Set<string>());

    await processHostSignals(
      peer,
      "session",
      [{ sequence: 20, envelope: envelope(20, { type: "answer", sdp: "restart" }, "session", 2) }],
      processed,
      () => {},
      () => true,
      state,
    );
    expect(calls).toEqual(["answer", "candidate", "complete"]);
    expect(processed).toEqual(new Set(["host-epoch:2:20", "host-epoch:2:21", "host-epoch:2:22"]));
  });

  test("ignores host rows from a replaced controller epoch", async () => {
    const calls: string[] = [];
    const peer = {
      remoteDescription: null,
      signalingState: "stable" as RTCSignalingState,
      setRemoteDescription: async () => {
        calls.push("answer");
      },
      addIceCandidate: async () => {},
      close: () => {},
    };
    const processed = new Set<string>();
    await processHostSignals(
      peer,
      "session",
      [
        {
          sequence: 1,
          envelope: envelope(1, { type: "answer", sdp: "old" }, "session", 0, "old-controller"),
        },
        {
          sequence: 2,
          envelope: envelope(2, { type: "answer", sdp: "new" }, "session", 0, "new-controller"),
        },
      ],
      processed,
      () => {},
      () => true,
      createSignalProcessingState(),
      "new-controller",
    );
    expect(calls).toEqual(["answer"]);
    expect(processed).toEqual(new Set(["host-epoch:0:2"]));
  });
});
