import { describe, expect, test } from "bun:test";
import { parseSignalEnvelope } from "./lib";

function envelope(sender: "controller" | "host", type: string) {
  return JSON.stringify({
    version: 1,
    sessionId: "session",
    sequence: 7,
    sender,
    sentAt: 1,
    payload: { type },
  });
}

describe("signal envelope indexing", () => {
  test("extracts an authenticated role-compatible kind", () => {
    expect(parseSignalEnvelope(envelope("controller", "offer"), "controller")).toEqual({
      sessionId: "session",
      sender: "controller",
      sequence: 7,
      kind: "offer",
    });
    expect(parseSignalEnvelope(envelope("host", "answer"), "host").kind).toBe("answer");
  });

  test("rejects role confusion and unknown payload kinds", () => {
    expect(() => parseSignalEnvelope(envelope("host", "offer"), "host")).toThrow();
    expect(() => parseSignalEnvelope(envelope("controller", "answer"), "controller")).toThrow();
    expect(() =>
      parseSignalEnvelope(envelope("controller", "renegotiate"), "controller"),
    ).toThrow();
  });
});
