import { afterEach, describe, expect, test } from "bun:test";

import { RemoteSessionController, type HostSignalRow } from "./remote-session";

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
const originalMediaStream = globalThis.MediaStream;

afterEach(() => {
  defineGlobal("window", originalWindow);
  defineGlobal("document", originalDocument);
  defineGlobal("MediaStream", originalMediaStream);
});

describe("remote session lifecycle", () => {
  test("requests zero receiver buffering only through supported browser APIs", async () => {
    installBrowserGlobals();
    for (const receiver of [
      { jitterBufferTarget: 100, playoutDelayHint: 0.1 },
      { playoutDelayHint: 0.1 },
      {},
    ]) {
      const peer = new FakePeer();
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        video: new FakeVideo() as unknown as HTMLVideoElement,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      const supported = Object.keys(receiver);
      await controller.start();
      peer.ontrack?.({ receiver, streams: [], track: {} } as unknown as RTCTrackEvent);
      for (const property of supported) expect(Reflect.get(receiver, property)).toBe(0);
      expect(Object.keys(receiver)).toEqual(supported);
      await controller.stop();
    }
  });
  test("normal pointer capture loss preserves keyboard focus; cancellation still releases input", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    const video = new FakeVideo();
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: video as unknown as HTMLVideoElement,
      operations: { sendSignal: async () => null, endSession: async () => null },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });
    await controller.start();
    const channel = peer.channels[1]!;
    channel.sent = [];
    video.focus();
    video.dispatchEvent(Object.assign(new Event("lostpointercapture"), { buttons: 0 }));
    expect(video.focused).toBe(true);
    expect(channel.sent).toEqual([]);
    video.dispatchEvent(
      Object.assign(new Event("keydown"), { code: "Space", key: " ", repeat: false }),
    );
    expect(JSON.parse(channel.sent.at(-1)!)).toMatchObject({
      type: "key",
      action: "down",
      code: "Space",
    });
    for (const [type, buttons] of [
      ["lostpointercapture", 1],
      ["pointercancel", 0],
    ] as const) {
      video.focus();
      video.dispatchEvent(Object.assign(new Event(type), { buttons }));
      expect(video.focused).toBe(false);
      expect(JSON.parse(channel.sent.at(-1)!)).toEqual({ type: "release" });
    }
    controller.setInputEnabled(false);
    channel.sent = [];
    video.dispatchEvent(Object.assign(new Event("lostpointercapture"), { buttons: 0 }));
    video.dispatchEvent(Object.assign(new Event("keydown"), { code: "Space", key: " " }));
    expect(channel.sent).toEqual([]);
    channel.readyState = "closed";
    video.dispatchEvent(Object.assign(new Event("lostpointercapture"), { buttons: 1 }));
    expect(channel.sent).toEqual([]);
    await controller.stop();
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
      peer.channels[1]!.sent = [];
      peer.channels[1]!.onopen?.();
      expect(controller.snapshot.inputEnabled).toBe(false);
      expect(peer.channels[1]!.sent).toEqual(
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
      peer.channels[1]!.onopen?.();
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

class FakeVideo extends EventTarget {
  srcObject: unknown = null;
  videoWidth = 1920;
  videoHeight = 1080;

  focused = false;

  async play(): Promise<void> {}

  focus(): void {
    this.focused = true;
  }

  blur(): void {
    this.focused = false;
  }
}

class FakeMediaStream {
  getTracks(): MediaStreamTrack[] {
    return [];
  }
}

class FakeDataChannel {
  sent: string[] = [];
  readyState: RTCDataChannelState = "open";
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = "closed";
    this.onclose?.();
  }
}

class FakePeer {
  channels: FakeDataChannel[] = [];
  connectionState: RTCPeerConnectionState = "new";
  signalingState: RTCSignalingState = "stable";
  localDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  onRemoteDescription: ((sdp: string) => void) | null = null;

  createDataChannel(): FakeDataChannel {
    const channel = new FakeDataChannel();
    this.channels.push(channel);
    return channel;
  }

  addTransceiver(): void {}

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
