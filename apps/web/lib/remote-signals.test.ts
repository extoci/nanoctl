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
    const peer = {
      setRemoteDescription: async () => {
        calls.push("answer");
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

  test("does not mark rejected or cross-session signals as processed", async () => {
    const peer = {
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
});
