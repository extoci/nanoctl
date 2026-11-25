import { afterEach, expect, test } from "bun:test";
import type { HostInfo } from "@nanoctl/protocol";
import type { RemoteSessionController, RemoteSessionOptions } from "./remote-session";
import { SocketSession, reconnectDelay } from "./socket-session";

const originals = {
  window: globalThis.window,
  fetch: globalThis.fetch,
  WebSocket: globalThis.WebSocket,
};
const sessions: SocketSession[] = [];
afterEach(() => {
  sessions.splice(0).forEach((session) => session.dispose());
  Object.entries(originals).forEach(([key, value]) =>
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value }),
  );
});

const info: HostInfo = {
  hostName: "desktop",
  platform: "linux",
  version: "3.0.0-alpha.11",
  displays: [],
  remoteInput: false,
  viewer: {
    identity: { login: "dev@example.com", displayName: "Dev", tags: [] },
    mayControl: false,
  },
  capabilities: {
    protocolVersion: 2,
    platform: "linux",
    architecture: "x64",
    displays: [],
    codecs: ["h264"],
    input: false,
    clipboard: false,
    systemAudio: false,
  },
};
class FakeSocket {
  static OPEN = 1;
  static instances: FakeSocket[] = [];
  readyState = 1;
  bufferedAmount = 0;
  sent: string[] = [];
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;
  constructor(readonly url: URL) {
    FakeSocket.instances.push(this);
  }
  send(text: string) {
    this.sent.push(text);
  }
  close() {
    this.readyState = 3;
  }
  receive(value: unknown) {
    this.onmessage?.({ data: JSON.stringify(value) });
  }
}
function fixture(protocol = "http:", response = Response.json(info)) {
  FakeSocket.instances = [];
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      location: { href: protocol + "//desktop:7337/", protocol },
      addEventListener() {},
      removeEventListener() {},
    },
  });
  Object.defineProperty(globalThis, "WebSocket", { configurable: true, value: FakeSocket });
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async () => response.clone(),
  });
  const statuses: string[] = [];
  const details: string[] = [];
  const controllers: {
    options: RemoteSessionOptions;
    started: boolean;
    stopped: boolean;
    signals: unknown[];
  }[] = [];
  const hosts: HostInfo[] = [];
  const session = new SocketSession({
    video: {} as HTMLVideoElement,
    onInfo: (host) => hosts.push(host),
    onSnapshot() {},
    onStatus: (status, detail) => {
      statuses.push(status);
      if (detail) details.push(detail);
    },
    createController: (options) => {
      const controller = { options, started: false, stopped: false, signals: [] as unknown[] };
      controllers.push(controller);
      return {
        start: async () => {
          controller.started = true;
        },
        stop: async () => {
          controller.stopped = true;
        },
        ingestHostSignals: (signals: unknown) => controller.signals.push(signals),
      } as unknown as RemoteSessionController;
    },
  });
  sessions.push(session);
  session.connect();
  return { session, controllers, hosts, statuses, details };
}
async function socket() {
  for (let i = 0; i < 20 && !FakeSocket.instances.length; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
  const value = FakeSocket.instances.at(-1);
  if (!value) throw new Error("Socket did not open");
  return value;
}

test("clipboard requires both available remote input and a control grant", async () => {
  for (const [remoteInput, mayControl] of [
    [false, true],
    [true, false],
    [false, false],
    [true, true],
  ]) {
    const state = fixture();
    const ws = await socket();
    ws.receive({
      type: "hello",
      sessionId: "one",
      host: {
        ...info,
        remoteInput,
        viewer: { ...info.viewer, mayControl },
        capabilities: { ...info.capabilities, clipboard: true },
      },
    });
    expect(state.controllers[0]!.options.clipboardEnabled).toBe(!!remoteInput && !!mayControl);
    state.session.dispose();
  }
});

test("hello creates a view-only controller with no ICE servers and streams host signals directly", async () => {
  const state = fixture("https:");
  const ws = await socket();
  expect(ws.url.protocol).toBe("wss:");
  ws.receive({ type: "hello", sessionId: "one", host: info });
  const controller = state.controllers[0]!;
  expect(controller.started).toBe(true);
  expect(controller.options.iceServers).toEqual([]);
  expect(controller.options.remoteInputPolicy).toBe(false);
  expect(state.hosts.at(-1)).toEqual(info);
  const envelope = {
    version: 2,
    sessionId: "one",
    controllerEpoch: controller.options.controllerEpoch,
    hostEpoch: "host",
    generation: 0,
    sequence: 1,
    sender: "host",
    sentAt: Date.now(),
    payload: { type: "answer", sdp: "answer" },
  };
  ws.receive({ type: "signal", envelope });
  expect(controller.signals).toEqual([[{ sequence: 1, envelope: JSON.stringify(envelope) }]]);
  await controller.options.operations.sendSignal({
    sessionId: "one",
    envelope: JSON.stringify(envelope),
  });
  expect(JSON.parse(ws.sent[0]!)).toEqual({ type: "signal", envelope });
});

test("a dropped socket stops the old peer and reconnects with a fresh session and epoch", async () => {
  const state = fixture();
  const first = await socket();
  expect(first.url.protocol).toBe("ws:");
  first.receive({ type: "hello", sessionId: "one", host: info });
  const stale = first.onmessage!;
  first.onclose!();
  expect(state.controllers[0]!.stopped).toBe(true);
  expect(state.statuses.at(-1)).toBe("reconnecting");
  await new Promise((resolve) => setTimeout(resolve, 550));
  const next = await socket();
  expect(next).not.toBe(first);
  stale({ data: JSON.stringify({ type: "end", reason: "replaced" }) });
  next.receive({ type: "hello", sessionId: "two", host: info });
  expect(state.controllers[1]!.options.sessionId).toBe("two");
  expect(state.controllers[1]!.options.controllerEpoch).not.toBe(
    state.controllers[0]!.options.controllerEpoch,
  );
  expect(state.statuses.at(-1)).toBe("online");
});

test("replacement releases the peer and requires manual reconnect", async () => {
  const state = fixture();
  const ws = await socket();
  ws.receive({ type: "hello", sessionId: "one", host: info });
  state.controllers[0]!.options.preferences!.inputEnabled = false;
  state.controllers[0]!.options.preferences!.displayId = "secondary";
  ws.receive({ type: "end", reason: "replaced" });
  expect(state.details.at(-1)).toBe("Another viewer took over");
  expect(state.controllers[0]!.stopped).toBe(true);
  expect(ws.onclose).toBeNull();
  await new Promise((resolve) => setTimeout(resolve, 550));
  expect(FakeSocket.instances.length).toBe(1);
  expect(state.statuses.at(-1)).toBe("replaced");
  state.session.connect();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(FakeSocket.instances.length).toBe(2);
  FakeSocket.instances[1]!.receive({ type: "hello", sessionId: "two", host: info });
  expect(state.controllers[1]!.options.preferences).toEqual({
    inputEnabled: false,
    displayId: "secondary",
  });
});

test("the signaling socket opens without waiting for host info", () => {
  const state = fixture();
  expect(FakeSocket.instances).toHaveLength(1);
  FakeSocket.instances[0]!.receive({ type: "hello", sessionId: "one", host: info });
  expect(state.controllers[0]!.started).toBe(true);
});

test("forbidden responses preserve the agent explanation and close the socket", async () => {
  const state = fixture("http:", new Response("identity=dev: forbidden", { status: 403 }));
  const ws = await socket();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(state.statuses.at(-1)).toBe("error");
  expect(state.details.at(-1)).toBe("identity=dev: forbidden");
  expect(ws.readyState).toBe(3);
  await new Promise((resolve) => setTimeout(resolve, 550));
  expect(FakeSocket.instances).toHaveLength(1);
});

test("a socket refused before hello reports the forbidden explanation, not a retry", async () => {
  let respond: (response: Response) => void = () => {};
  const state = fixture("http:");
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: () => new Promise<Response>((resolve) => (respond = resolve)),
  });
  state.session.connect();
  const ws = await socket();
  ws.onerror!();
  ws.onclose!();
  expect(state.statuses.at(-1)).toBe("connecting");
  respond(new Response("identity=dev: forbidden", { status: 403 }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(state.statuses.at(-1)).toBe("error");
  expect(state.details.at(-1)).toBe("identity=dev: forbidden");
  await new Promise((resolve) => setTimeout(resolve, 550));
  expect(FakeSocket.instances.filter((value) => value.readyState === 1)).toEqual([]);
});

test("host info that arrives after hello does not overwrite it", async () => {
  let respond: (response: Response) => void = () => {};
  const state = fixture("http:");
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: () => new Promise<Response>((resolve) => (respond = resolve)),
  });
  state.session.connect();
  const ws = await socket();
  ws.receive({ type: "hello", sessionId: "one", host: info });
  respond(Response.json({ ...info, hostName: "stale" }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(state.hosts.at(-1)).toEqual(info);
});

test("replacement survives a data-channel close racing the terminal socket message", async () => {
  const state = fixture();
  const ws = await socket();
  ws.receive({ type: "hello", sessionId: "one", host: info });
  await state.controllers[0]!.options.operations.endSession({
    sessionId: "one",
    reason: "control channel unavailable",
  });
  expect(ws.readyState).toBe(FakeSocket.OPEN);
  ws.receive({ type: "end", reason: "replaced" });
  expect(state.statuses.at(-1)).toBe("replaced");
  expect(state.details.at(-1)).toBe("Another viewer took over");
});

test("malformed messages fail closed, and disposal ignores late callbacks", async () => {
  const state = fixture();
  const ws = await socket();
  const stale = ws.onmessage!;
  ws.receive({ type: "signal", envelope: {} });
  expect(state.statuses.at(-1)).toBe("error");
  state.session.dispose();
  stale({ data: JSON.stringify({ type: "hello", sessionId: "late", host: info }) });
  expect(state.controllers).toEqual([]);
});

test.each(["host shutdown", "control channel unavailable"])("%s is transient", async (reason) => {
  const state = fixture();
  const first = await socket();
  first.receive({ type: "hello", sessionId: "one", host: info });
  first.receive({ type: "end", reason });
  expect(state.statuses.at(-1)).toBe("reconnecting");
  await new Promise((resolve) => setTimeout(resolve, 550));
  expect(FakeSocket.instances).toHaveLength(2);
});

test("reconnect backoff is exponential, bounded and exhausts", () => {
  expect(Array.from({ length: 7 }, (_, attempt) => reconnectDelay(attempt))).toEqual([
    500,
    1000,
    2000,
    4000,
    8000,
    null,
    null,
  ]);
});
