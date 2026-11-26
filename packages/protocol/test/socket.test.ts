import { expect, test } from "bun:test";
import { assertHostInfo, parseAgentMessage } from "../src";

const host = {
  hostName: "desktop",
  platform: "linux",
  version: "3.0.0-alpha.15",
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
test("audio capability is independent of viewer control permission", () => {
  const audioHost = { ...host, capabilities: { ...host.capabilities, systemAudio: true } };
  const message = { type: "hello", sessionId: "audio", host: audioHost };
  expect(parseAgentMessage(message)).toEqual(message);
  expect(audioHost.viewer.mayControl).toBe(false);
});
test("accepts Windows audio output announcements and rejects invalid selections", () => {
  const outputs = [{ id: "1", name: "Speakers", isDefault: true }];
  const message = { type: "audio-outputs", outputs, selectedId: "1" };
  expect(parseAgentMessage(message)).toEqual(message);
  const systemDefault = { type: "audio-outputs", outputs: [], selectedId: null };
  expect(parseAgentMessage(systemDefault)).toEqual(systemDefault);
  expect(() => parseAgentMessage({ ...message, selectedId: "missing" })).toThrow();
  expect(() => parseAgentMessage({ ...message, outputs: [{ id: "1" }] })).toThrow();
  expect(() =>
    assertHostInfo({ ...host, capabilities: { ...host.capabilities, audioOutputs: outputs } }),
  ).not.toThrow();
  expect(() =>
    assertHostInfo({ ...host, capabilities: { ...host.capabilities, audioOutputs: "invalid" } }),
  ).toThrow();
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
