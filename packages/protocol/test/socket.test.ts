import { expect, test } from "bun:test";
import { assertHostInfo, parseAgentMessage } from "../src";

const host = {
  hostName: "desktop",
  platform: "linux",
  version: "3.0.0-alpha.1",
  displays: [],
  remoteInput: false,
  capabilities: { displays: [] },
  viewer: { identity: { login: "dev@example.com", displayName: "", tags: [] }, mayControl: false },
};

test("accepts hello with an empty display list and identity", () => {
  const message = { type: "hello", sessionId: "one", host };
  expect(parseAgentMessage(message)).toEqual(message);
  expect(() => assertHostInfo(host)).not.toThrow();
});
test("accepts signal objects and end reasons", () => {
  const message = {
    type: "signal",
    envelope: {
      version: 2,
      sessionId: "one",
      controllerEpoch: "controller",
      hostEpoch: "host",
      generation: 0,
      sequence: 1,
      sender: "host",
      sentAt: Date.now(),
      payload: { type: "answer", sdp: "answer" },
    },
  };
  expect(parseAgentMessage(message)).toEqual(message);
  expect(parseAgentMessage({ type: "end", reason: "replaced" })).toEqual({
    type: "end",
    reason: "replaced",
  });
});
test("rejects empty or malformed messages, identities, displays and legacy signal wrappers", () => {
  for (const message of [
    null,
    {},
    { type: "end", reason: "" },
    { type: "hello", sessionId: "", host },
    { type: "hello", sessionId: "one", host: { ...host, viewer: {} } },
    { type: "hello", sessionId: "one", host: { ...host, displays: [{}] } },
    { type: "signal", envelope: "{}" },
    { type: "unknown" },
  ]) {
    expect(() => parseAgentMessage(message)).toThrow();
  }
});
