import { afterEach, describe, expect, test } from "bun:test";

import { RemoteSessionController, type HostSignalRow } from "./remote-session";

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
const originalMediaStream = globalThis.MediaStream;
const originalNavigator = globalThis.navigator;

afterEach(() => {
  defineGlobal("window", originalWindow);
  defineGlobal("document", originalDocument);
  defineGlobal("MediaStream", originalMediaStream);
  defineGlobal("navigator", originalNavigator);
});

describe("remote session lifecycle", () => {
  test("audio is recvonly when advertised, even for view-only sessions", async () => {
    installBrowserGlobals();
    for (const enabled of [false, true]) {
      const peer = new FakePeer();
      const video = new FakeVideo();
      const audio = {
        srcObject: null as unknown,
        muted: true,
        play: async () => {},
        pause: () => {},
      };
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        video: video as unknown as HTMLVideoElement,
        audio: audio as unknown as HTMLAudioElement,
        systemAudioEnabled: enabled,
        remoteInputPolicy: false,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      expect(peer.transceivers).toEqual(
        enabled
          ? [
              ["video", "recvonly"],
              ["audio", "recvonly"],
            ]
          : [["video", "recvonly"]],
      );
      peer.ontrack?.({
        receiver: { playoutDelayHint: 1, jitterBufferTarget: 100 },
        streams: [],
        track: { kind: "audio" },
      } as unknown as RTCTrackEvent);
      expect(audio.srcObject instanceof FakeMediaStream).toBe(enabled);
      expect(video.srcObject).toBeNull();
      expect(audio.muted).toBe(true);
      await controller.stop();
      expect(audio.srcObject).toBeNull();
    }
  });

  test("clipboard is control-only and sends text before a remote paste key", async () => {
    installBrowserGlobals();
    Object.assign(document, { hasFocus: () => true });
    defineGlobal("navigator", {
      clipboard: { readText: async () => "local paste", writeText: async () => {} },
    });
    for (const mayControl of [false, true]) {
      const peer = new FakePeer();
      const video = new FakeVideo();
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        video: video as unknown as HTMLVideoElement,
        clipboardEnabled: true,
        remoteInputPolicy: mayControl,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      expect(peer.channels.has("nanoctl.clipboard.v1")).toBe(mayControl);
      if (mayControl) {
        video.listeners.get("keydown")?.({
          type: "keydown",
          code: "KeyV",
          key: "v",
          ctrlKey: true,
          preventDefault: () => {},
        } as unknown as Event);
        await flushSignals();
        expect(peer.sent.map(([label]) => label)).toEqual([
          "nanoctl.clipboard.v1",
          "nanoctl.control.v1",
        ]);
        expect(JSON.parse(peer.sent[0]![1]).text).toBe("local paste");
      }
      await controller.stop();
    }
  });

  test("restores a valid display while input is off, but not for view-only or missing displays", async () => {
    installBrowserGlobals();
    const preferences = { inputEnabled: true, displayId: "secondary" };
    const display = {
      id: "secondary",
      name: "Secondary",
      width: 1920,
      height: 1080,
      scaleFactor: 1,
      primary: false,
    };
    for (const [mayControl, displays] of [
      [true, [display]],
      [false, [display]],
      [true, []],
    ] as const) {
      const peer = new FakePeer();
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays,
        iceServers: [],
        preferences,
        remoteInputPolicy: mayControl,
        video: new FakeVideo() as unknown as HTMLVideoElement,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      if (mayControl && displays.length) expect(controller.selectDisplay("secondary")).toBe(true);
      controller.setInputEnabled(false);
      peer.channels.get("nanoctl.control.v1")!.sent = [];
      peer.channels.get("nanoctl.control.v1")!.onopen?.();
      expect(controller.snapshot.inputEnabled).toBe(false);
      expect(peer.channels.get("nanoctl.control.v1")!.sent).toEqual(
        mayControl && displays.length
          ? [JSON.stringify({ type: "display", displayId: "secondary" })]
          : [],
      );
      await controller.stop();
    }
  });
  test("input preference survives new peers and never overrides view-only policy", async () => {
    installBrowserGlobals();
    const preferences = { inputEnabled: true };
    for (const mayControl of [true, true, false]) {
      const peer = new FakePeer();
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        preferences,
        remoteInputPolicy: mayControl,
        video: new FakeVideo() as unknown as HTMLVideoElement,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      expect(controller.snapshot.inputEnabled).toBe(preferences.inputEnabled && mayControl);
      peer.channels.get("nanoctl.control.v1")!.onopen?.();
      expect(controller.snapshot.inputEnabled).toBe(preferences.inputEnabled && mayControl);
      controller.setInputEnabled(false);
      expect(preferences.inputEnabled).toBe(false);
      await controller.stop();
      if (!mayControl) {
        preferences.inputEnabled = true;
        const viewOnly = new RemoteSessionController({
          sessionId: "view",
          controllerEpoch: "controller",
          displays: [],
          iceServers: [],
          preferences,
          remoteInputPolicy: false,
          video: new FakeVideo() as unknown as HTMLVideoElement,
          operations: { sendSignal: async () => null, endSession: async () => null },
          peerFactory: () => new FakePeer() as unknown as RTCPeerConnection,
          onSnapshot: () => {},
        });
        await viewOnly.start();
        expect(viewOnly.snapshot.inputEnabled).toBe(false);
        await viewOnly.stop();
      }
    }
  });
  test("refreshes mutable input capabilities without tearing down the peer", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      remoteInputEnabled: false,
      video: new FakeVideo() as unknown as HTMLVideoElement,
      operations: {
        sendSignal: async () => null,
        endSession: async () => null,
      },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    await controller.start();
    controller.setInputEnabled(true);
    expect(controller.snapshot.inputEnabled).toBe(false);
    controller.updateCapabilities([], true);
    controller.setInputEnabled(true);
    expect(controller.snapshot.inputEnabled).toBe(true);
    await controller.stop();
  });

  test("restarts ICE when a native host epoch changes after an answer", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    const offers: string[] = [];
    const remoteDescriptions: string[] = [];
    const video = new FakeVideo();
    let controller: RemoteSessionController;
    controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: video as unknown as HTMLVideoElement,
      operations: {
        sendSignal: async ({ envelope }) => {
          const parsed = JSON.parse(envelope) as {
            payload?: { type?: string };
          };
          if (parsed.payload?.type === "offer") {
            offers.push(envelope);
            if (offers.length === 2) {
              // The replacement host arrives after the restart offer is already published. The
              // matching offer must be shared by B and C until its answer is applied.
              controller.ingestHostSignals([
                hostSignal(3, "host-c", 0, { type: "answer", sdp: "bootstrap-answer-c" }),
              ]);
            }
          }
        },
        endSession: async () => null,
      },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    peer.onRemoteDescription = (sdp) => remoteDescriptions.push(sdp);
    await controller.start();
    expect(offers).toHaveLength(1);

    controller.ingestHostSignals([hostSignal(1, "host-a", 0, { type: "answer", sdp: "answer-a" })]);
    await flushSignals();
    expect(remoteDescriptions).toEqual(["answer-a"]);

    controller.ingestHostSignals([
      hostSignal(2, "host-b", 0, { type: "answer", sdp: "bootstrap-answer-b" }),
    ]);
    await flushSignals();
    expect(offers).toHaveLength(2);
    expect(remoteDescriptions).toEqual(["answer-a"]);
    expect(controller.snapshot.iceGeneration).toBe(1);

    controller.ingestHostSignals([hostSignal(4, "host-c", 1, { type: "answer", sdp: "answer-c" })]);
    await flushSignals();
    await flushSignals();
    expect(remoteDescriptions).toEqual(["answer-a", "answer-c"]);

    await controller.stop();
  });

  test("shows a host terminal signal as a failed session", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: new FakeVideo() as unknown as HTMLVideoElement,
      operations: {
        sendSignal: async () => null,
        endSession: async () => null,
      },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    await controller.start();
    controller.ingestHostSignals([
      hostSignal(1, "host", 0, { type: "end", reason: "encoder unavailable" }),
    ]);
    await flushSignals();

    expect(controller.snapshot.phase).toBe("failed");
    expect(controller.snapshot.reason).toBe("failed: encoder unavailable");
  });

  test("revalidates ownership before restoring a BFCache page", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    let restoredEpoch: string | undefined;
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller-old",
      displays: [],
      iceServers: [],
      video: new FakeVideo() as unknown as HTMLVideoElement,
      operations: {
        sendSignal: async () => null,
        endSession: async () => null,
      },
      revalidateController: async () => ({
        owned: false,
        controllerEpoch: "controller-new",
      }),
      onPageRestore: (controllerEpoch) => {
        restoredEpoch = controllerEpoch;
      },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    await controller.start();
    const onPageShow = (
      controller as unknown as { onPageShow: (event: PageTransitionEvent) => void }
    ).onPageShow;
    onPageShow({ persisted: true } as PageTransitionEvent);
    await flushSignals();

    expect(controller.snapshot.phase).toBe("ended");
    expect(restoredEpoch).toBe("controller-new");
  });

  test("recreates after BFCache revalidation fails on a closed peer", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    let restored = false;
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: new FakeVideo() as unknown as HTMLVideoElement,
      operations: {
        sendSignal: async () => null,
        endSession: async () => null,
      },
      revalidateController: async () => {
        throw new Error("control plane unavailable");
      },
      onPageRestore: () => {
        restored = true;
      },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    await controller.start();
    peer.close();
    const onPageShow = (
      controller as unknown as { onPageShow: (event: PageTransitionEvent) => void }
    ).onPageShow;
    onPageShow({ persisted: true } as PageTransitionEvent);
    await flushSignals();

    expect(controller.snapshot.phase).toBe("ended");
    expect(restored).toBe(true);
  });
});

function hostSignal(
  sequence: number,
  hostEpoch: string,
  generation: number,
  payload: Record<string, unknown>,
): HostSignalRow {
  return {
    sequence,
    envelope: JSON.stringify({
      version: 2,
      sessionId: "session",
      controllerEpoch: "controller",
      hostEpoch,
      generation,
      sequence,
      sender: "host",
      sentAt: 1,
      payload,
    }),
  };
}

async function flushSignals(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

function installBrowserGlobals(): void {
  const eventTarget = {
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  defineGlobal("window", {
    ...eventTarget,
    setInterval: globalThis.setInterval.bind(globalThis),
    clearInterval: globalThis.clearInterval.bind(globalThis),
    setTimeout: globalThis.setTimeout.bind(globalThis),
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
  });
  defineGlobal("document", {
    ...eventTarget,
    visibilityState: "visible",
    fullscreenElement: null,
  });
  defineGlobal("MediaStream", FakeMediaStream);
}

function defineGlobal(name: string, value: unknown): void {
  Object.defineProperty(globalThis, name, {
    configurable: true,
    value,
  });
}

class FakeVideo {
  listeners = new Map<string, (event: Event) => void>();
  srcObject: unknown = null;
  videoWidth = 1920;
  videoHeight = 1080;

  addEventListener(type: string, listener: (event: Event) => void): void {
    this.listeners.set(type, listener);
  }

  removeEventListener(): void {}

  focus(): void {}

  blur(): void {}
}

class FakeMediaStream {
  getTracks(): MediaStreamTrack[] {
    return [];
  }
}

class FakeDataChannel {
  constructor(private readonly record: (message: string) => void = () => {}) {}
  sent: string[] = [];
  readyState: RTCDataChannelState = "open";
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;

  send(message: string): void {
    this.sent.push(message);
    this.record(message);
  }

  close(): void {
    this.readyState = "closed";
    this.onclose?.();
  }
}

class FakePeer {
  transceivers: [string, RTCRtpTransceiverDirection | undefined][] = [];
  channels = new Map<string, FakeDataChannel>();
  sent: [string, string][] = [];
  connectionState: RTCPeerConnectionState = "new";
  signalingState: RTCSignalingState = "stable";
  localDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  onRemoteDescription: ((sdp: string) => void) | null = null;

  createDataChannel(label: string): FakeDataChannel {
    const channel = new FakeDataChannel((message) => this.sent.push([label, message]));
    this.channels.set(label, channel);
    return channel;
  }

  addTransceiver(kind: string, init: RTCRtpTransceiverInit): void {
    this.transceivers.push([kind, init.direction]);
  }

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    return { type: "offer", sdp: "v=0\r\na=ice-ufrag:fake" };
  }

  async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = description;
    this.signalingState = "have-local-offer";
  }

  async setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
    this.onRemoteDescription?.(description.sdp ?? "");
    this.signalingState = "stable";
  }

  async addIceCandidate(): Promise<void> {}

  setConfiguration(): void {}

  restartIce(): void {}

  async getStats(): Promise<Map<string, never>> {
    return new Map<string, never>();
  }

  close(): void {
    this.connectionState = "closed";
    this.signalingState = "closed";
  }
}
