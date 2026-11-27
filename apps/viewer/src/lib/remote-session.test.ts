import { afterEach, describe, expect, test } from "bun:test";

import { RemoteSessionController, wheelPixels, type HostSignalRow } from "./remote-session";
import { attachDataChannelVideo, attachWebCodecsVideo } from "./webcodecs-video";

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
const originalMediaStream = globalThis.MediaStream;
const originalNavigator = globalThis.navigator;
const originalVideoDecoder = (globalThis as { VideoDecoder?: unknown }).VideoDecoder;
const originalEncodedVideoChunk = (globalThis as { EncodedVideoChunk?: unknown }).EncodedVideoChunk;
const originalMediaStreamTrackGenerator = (globalThis as { MediaStreamTrackGenerator?: unknown })
  .MediaStreamTrackGenerator;

afterEach(() => {
  defineGlobal("window", originalWindow);
  defineGlobal("document", originalDocument);
  defineGlobal("MediaStream", originalMediaStream);
  defineGlobal("navigator", originalNavigator);
  defineGlobal("VideoDecoder", originalVideoDecoder);
  defineGlobal("EncodedVideoChunk", originalEncodedVideoChunk);
  defineGlobal("MediaStreamTrackGenerator", originalMediaStreamTrackGenerator);
});

describe("remote session lifecycle", () => {
  test("uses the selected video ICE pair and distinguishes missing RTT from zero latency", async () => {
    const intervals: (() => void)[] = [];
    installBrowserGlobals((callback) => {
      intervals.push(callback);
      return 0;
    });
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
    peer.stats.set("video", {
      id: "video",
      type: "inbound-rtp",
      timestamp: 1_000,
      kind: "video",
      transportId: "transport",
    });
    for (const callback of intervals) callback();
    await flushSignals();
    expect(controller.snapshot.metrics).toMatchObject({ route: "unknown", rttMs: null });

    peer.stats.set("transport", {
      type: "transport",
      selectedCandidatePairId: "selected",
    });
    peer.stats.set("other", {
      type: "candidate-pair",
      state: "succeeded",
      nominated: true,
      currentRoundTripTime: 0.5,
    });
    peer.stats.set("selected", {
      type: "candidate-pair",
      localCandidateId: "local",
      remoteCandidateId: "remote",
      currentRoundTripTime: 0.042,
    });
    peer.stats.set("local", { type: "local-candidate", candidateType: "relay" });
    peer.stats.set("remote", { type: "remote-candidate", candidateType: "host" });
    for (const callback of intervals) callback();
    await flushSignals();
    expect(controller.snapshot.metrics).toMatchObject({ route: "relay", rttMs: 42 });
    await controller.stop();
  });

  test("audio is recvonly when advertised, even for view-only sessions", async () => {
    installBrowserGlobals();
    for (const enabled of [false, true]) {
      const peer = new FakePeer();
      const video = new FakeVideo();
      const sources: unknown[] = [];
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
        onAudioTrackChange: () => sources.push(audio.srcObject),
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
      expect(sources).toEqual(enabled ? [audio.srcObject] : []);
      expect(video.srcObject).toBeNull();
      expect(audio.muted).toBe(true);
      await controller.stop();
      expect(audio.srcObject).toBeNull();
      if (enabled) expect(sources.at(-1)).toBeNull();
    }
  });

  test("publishes audio preference when control opens and when it changes", async () => {
    installBrowserGlobals();
    for (const initialAudioEnabled of [false, true]) {
      const peer = new FakePeer();
      const controller = new RemoteSessionController({
        sessionId: "session",
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        video: new FakeVideo() as unknown as HTMLVideoElement,
        initialAudioEnabled,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      const channel = peer.channels.get("nanoctl.control.v1")!;
      channel.sent = [];
      channel.onopen?.();
      expect(channel.sent.map((message) => JSON.parse(message))).toContainEqual({
        type: "audio",
        enabled: initialAudioEnabled,
      });
      channel.sent = [];
      expect(controller.setAudioEnabled(!initialAudioEnabled)).toBe(true);
      expect(channel.sent.map((message) => JSON.parse(message))).toContainEqual({
        type: "audio",
        enabled: !initialAudioEnabled,
      });
      await controller.stop();
    }
  });

  test("publishes physical viewport dimensions when control opens", async () => {
    installBrowserGlobals();
    Object.assign(window, { devicePixelRatio: 2 });
    const peer = new FakePeer();
    const video = Object.assign(new FakeVideo(), {
      getBoundingClientRect: () => ({ width: 640, height: 360 }),
    });
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [
        {
          id: "display",
          name: "Display",
          width: 1920,
          height: 1080,
          scaleFactor: 1,
          primary: true,
        },
      ],
      iceServers: [],
      video: video as unknown as HTMLVideoElement,
      operations: { sendSignal: async () => null, endSession: async () => null },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });
    await controller.start();
    peer.channels.get("nanoctl.control.v1")!.onopen?.();
    expect(
      peer.channels.get("nanoctl.control.v1")!.sent.map((message) => JSON.parse(message)),
    ).toContainEqual({ type: "quality", maxWidth: 1280, maxHeight: 720 });
    await controller.stop();
  });

  test("clipboard is control-only and sends text before a remote paste key", async () => {
    installBrowserGlobals();
    Object.assign(document, { hasFocus: () => true });
    defineGlobal("navigator", {
      clipboard: { readText: async () => "local paste", writeText: async () => {} },
    });
    for (const [mayControl, remoteInput] of [
      [false, true],
      [true, false],
      [false, false],
      [true, true],
    ]) {
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
        remoteInputEnabled: remoteInput,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      expect(peer.channels.has("nanoctl.clipboard.v1")).toBe(!!mayControl && !!remoteInput);
      if (mayControl && remoteInput) {
        pressKey("keydown", video, { code: "KeyV", key: "v", ctrlKey: true });
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

  test("gamepad polling resumes by itself when the connection recovers", async () => {
    installBrowserGlobals();
    const pad = {
      axes: [0, 0, 0, 0],
      buttons: Array.from({ length: 17 }, () => ({ pressed: false, value: 0 })),
      connected: true,
      id: "Xbox Wireless Controller",
      index: 0,
      mapping: "standard",
    };
    defineGlobal("navigator", { getGamepads: () => [pad] });
    const peer = new FakePeer();
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: new FakeVideo() as unknown as HTMLVideoElement,
      gamepadAvailable: true,
      preferences: { inputEnabled: true },
      operations: { sendSignal: async () => null, endSession: async () => null },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });
    await controller.start();
    peer.connectionState = "connected";
    peer.onconnectionstatechange?.();
    const controlChannel = peer.channels.get("nanoctl.control.v1")!;
    const channel = peer.channels.get("nanoctl.gamepad.v1")!;
    expect(peer.channelConfigs.get("nanoctl.gamepad.v1")).toMatchObject({
      ordered: false,
      maxPacketLifeTime: 100,
    });
    controller.setGamepadEnabled(true);
    expect(controller.snapshot.gamepadEnabled).toBe(true);
    controlChannel.sent = [];
    channel.sent = [];
    flushAnimationFrame(1_000);
    expect(JSON.parse(controlChannel.sent.at(-1)!)).toMatchObject({
      type: "gamepad",
      pads: [[0, 0, 0, 0, 0, 0, 0, 0]],
    });
    expect(channel.sent).toEqual([]);
    channel.sent = [];
    expect(controller.sendControl({ type: "gamepad", pads: [] })).toBe(true);
    expect(JSON.parse(controlChannel.sent.at(-1)!)).toEqual({ type: "gamepad", pads: [] });
    expect(channel.sent).toEqual([]);
    controlChannel.sent = [];

    peer.connectionState = "disconnected";
    peer.onconnectionstatechange?.();
    expect(controller.snapshot.inputEnabled).toBe(false);
    expect(controller.snapshot.gamepadEnabled).toBe(false);

    peer.connectionState = "connected";
    peer.onconnectionstatechange?.();
    expect(controller.snapshot.inputEnabled).toBe(true);
    expect(controller.snapshot.gamepadEnabled).toBe(true);
    channel.sent = [];
    flushAnimationFrame(2_000);
    expect(JSON.parse(controlChannel.sent.at(-1)!)).toMatchObject({
      type: "gamepad",
      pads: [[0, 0, 0, 0, 0, 0, 0, 0]],
    });
    expect(channel.sent).toEqual([]);

    controller.setInputEnabled(false);
    expect(controller.snapshot.gamepadEnabled).toBe(false);
    controller.setInputEnabled(true);
    expect(controller.snapshot.gamepadEnabled).toBe(true);
    channel.sent = [];
    flushAnimationFrame(3_000);
    expect(JSON.parse(controlChannel.sent.at(-1)!)).toMatchObject({
      type: "gamepad",
      pads: [[0, 0, 0, 0, 0, 0, 0, 0]],
    });
    expect(channel.sent).toEqual([]);
    await controller.stop();
  });

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
    const channel = peer.channels.get("nanoctl.control.v1")!;
    channel.sent = [];
    video.focus();
    video.dispatchEvent(Object.assign(new Event("lostpointercapture"), { buttons: 0 }));
    expect(video.focused).toBe(true);
    expect(channel.sent).toEqual([]);
    pressKey("keydown", video, { code: "Space", key: " ", repeat: false });
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
    pressKey("keydown", video, { code: "Space", key: " " });
    expect(channel.sent).toEqual([]);
    channel.readyState = "closed";
    video.dispatchEvent(Object.assign(new Event("lostpointercapture"), { buttons: 1 }));
    expect(channel.sent).toEqual([]);
    await controller.stop();
  });
  test("keys reach the host from the video or page but not from panel controls", async () => {
    installBrowserGlobals();
    const body = {};
    Object.assign(globalThis.document, { body });
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
    const channel = peer.channels.get("nanoctl.control.v1")!;
    channel.sent = [];
    pressKey("keydown", { tagName: "BUTTON" }, { code: "Space", key: " " });
    pressKey("keydown", body, { code: "Tab", key: "Tab" });
    expect(channel.sent).toEqual([]);
    for (const [target, code] of [
      [video, "KeyA"],
      [body, "KeyB"],
    ] as const) {
      pressKey("keydown", target, { code, key: code.at(-1)!.toLowerCase() });
      expect(JSON.parse(channel.sent.at(-1)!)).toMatchObject({ type: "key", code });
    }
    channel.sent = [];
    pressKey("keyup", { tagName: "BUTTON" }, { code: "KeyC", key: "c" });
    expect(channel.sent).toEqual([]);
    pressKey("keydown", body, { code: "ShiftLeft", key: "Shift", shiftKey: true });
    pressKey("keyup", { tagName: "BUTTON" }, { code: "ShiftLeft", key: "Shift" });
    expect(channel.sent.map((data) => JSON.parse(data).action)).toEqual(["down", "up"]);
    channel.sent = [];
    for (const timeStamp of [500, 600, 760])
      pressKey("keydown", body, { code: "KeyD", key: "d", repeat: true, timeStamp });
    expect(channel.sent.map((data) => JSON.parse(data).repeat)).toEqual([true, true]);
    await controller.stop();
  });

  test("counts host input-blocked notices on the control channel", async () => {
    installBrowserGlobals();
    const peer = new FakePeer();
    const snapshots: number[] = [];
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      video: new FakeVideo() as unknown as HTMLVideoElement,
      operations: { sendSignal: async () => null, endSession: async () => null },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: (snapshot) => snapshots.push(snapshot.inputBlocked),
    });
    await controller.start();
    const channel = peer.channels.get("nanoctl.control.v1")!;
    for (const data of ["not json", '{"type":"other"}', '{"type":"inputBlocked"}'])
      channel.onmessage?.({ data } as MessageEvent);
    expect(controller.snapshot.inputBlocked).toBe(1);
    expect(snapshots.at(-1)).toBe(1);
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
      peer.channels.get("nanoctl.control.v1")!.sent = [];
      peer.channels.get("nanoctl.control.v1")!.onopen?.();
      expect(controller.snapshot.inputEnabled).toBe(false);
      const audio = JSON.stringify({ type: "audio", enabled: true });
      expect(peer.channels.get("nanoctl.control.v1")!.sent).toEqual(
        mayControl && displays.length
          ? [JSON.stringify({ type: "display", displayId: "secondary" }), audio]
          : [audio],
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

  test("restores input after a transient ICE disconnected-to-connected flap", async () => {
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
    expect(controller.snapshot.inputEnabled).toBe(true);

    peer.connectionState = "disconnected";
    peer.onconnectionstatechange?.();
    expect(controller.snapshot.inputEnabled).toBe(false);

    peer.connectionState = "connected";
    peer.onconnectionstatechange?.();
    expect(controller.snapshot.inputEnabled).toBe(true);

    // Respect explicit user preference when input was intentionally turned off.
    controller.setInputEnabled(false);
    peer.connectionState = "disconnected";
    peer.onconnectionstatechange?.();
    peer.connectionState = "connected";
    peer.onconnectionstatechange?.();
    expect(controller.snapshot.inputEnabled).toBe(false);

    await controller.stop();
  });

  test("a hanging clipboard read on Ctrl+V does not stall subsequent keyboard input", async () => {
    installBrowserGlobals();
    defineGlobal("navigator", {
      clipboard: {
        readText: () => new Promise<string>(() => {}),
        writeText: async () => {},
      },
    });
    const peer = new FakePeer();
    const video = new FakeVideo();
    const controller = new RemoteSessionController({
      sessionId: "session",
      controllerEpoch: "controller",
      displays: [],
      iceServers: [],
      clipboardEnabled: true,
      video: video as unknown as HTMLVideoElement,
      operations: { sendSignal: async () => null, endSession: async () => null },
      peerFactory: () => peer as unknown as RTCPeerConnection,
      onSnapshot: () => {},
    });

    await controller.start();
    pressKey("keydown", video, {
      code: "KeyV",
      key: "v",
      ctrlKey: true,
      metaKey: false,
      shiftKey: false,
      altKey: false,
      repeat: false,
    });
    pressKey("keydown", video, {
      code: "KeyA",
      key: "a",
      ctrlKey: false,
      metaKey: false,
      shiftKey: false,
      altKey: false,
      repeat: false,
    });

    await new Promise((resolve) => setTimeout(resolve, 320));
    const keys = peer.sent
      .filter(([label]) => label === "nanoctl.control.v1")
      .map(([, message]) => JSON.parse(message) as { type?: string; code?: string })
      .filter((message) => message.type === "key")
      .map((message) => message.code);
    expect(keys).toEqual(["KeyV", "KeyA"]);

    await controller.stop();
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

const windowListeners = new Map<string, Set<(event: Event) => void>>();

/** Delivers a key event to window listeners as if it bubbled up from `target`. */
function pressKey(type: string, target: unknown, properties: object): void {
  const { timeStamp, ...assignable } = properties as { timeStamp?: number };
  const event = Object.assign(new Event(type, { cancelable: true }), assignable);
  if (timeStamp !== undefined) Object.defineProperty(event, "timeStamp", { value: timeStamp });
  Object.defineProperty(event, "target", { value: target });
  for (const listener of windowListeners.get(type) ?? []) listener(event);
}

const animationFrames = new Map<number, FrameRequestCallback>();
let animationFrameId = 0;

/** Runs one round of queued rAF callbacks (they re-queue themselves). */
function flushAnimationFrame(now: number): void {
  const callbacks = [...animationFrames.values()];
  animationFrames.clear();
  for (const callback of callbacks) callback(now);
}

function installBrowserGlobals(setInterval?: (callback: () => void) => number): void {
  windowListeners.clear();
  animationFrames.clear();
  const eventTarget = {
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  defineGlobal("window", {
    addEventListener: (type: string, listener: (event: Event) => void) => {
      if (!windowListeners.has(type)) windowListeners.set(type, new Set());
      windowListeners.get(type)!.add(listener);
    },
    removeEventListener: (type: string, listener: (event: Event) => void) =>
      windowListeners.get(type)?.delete(listener),
    setInterval: setInterval ?? globalThis.setInterval.bind(globalThis),
    clearInterval: globalThis.clearInterval.bind(globalThis),
    setTimeout: globalThis.setTimeout.bind(globalThis),
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    requestAnimationFrame: (callback: FrameRequestCallback) => {
      animationFrames.set(++animationFrameId, callback);
      return animationFrameId;
    },
    cancelAnimationFrame: (id: number) => {
      animationFrames.delete(id);
    },
  });
  defineGlobal("document", {
    ...eventTarget,
    visibilityState: "visible",
    fullscreenElement: null,
    hasFocus: () => true,
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
  constructor(private readonly record: (message: string) => void = () => {}) {}
  sent: string[] = [];
  readyState: RTCDataChannelState = "open";
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;

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
  stats = new Map<string, Record<string, unknown>>();
  transceivers: [string, RTCRtpTransceiverDirection | undefined][] = [];
  channels = new Map<string, FakeDataChannel>();
  channelConfigs = new Map<string, RTCDataChannelInit | undefined>();
  sent: [string, string][] = [];
  connectionState: RTCPeerConnectionState = "new";
  signalingState: RTCSignalingState = "stable";
  localDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  onRemoteDescription: ((sdp: string) => void) | null = null;

  createDataChannel(label: string, init?: RTCDataChannelInit): FakeDataChannel {
    const channel = new FakeDataChannel((message) => this.sent.push([label, message]));
    this.channels.set(label, channel);
    this.channelConfigs.set(label, init);
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

  async getStats(): Promise<Map<string, Record<string, unknown>>> {
    return this.stats;
  }

  close(): void {
    this.connectionState = "closed";
    this.signalingState = "closed";
  }
}

describe("webcodecs video gating and teardown", () => {
  test("creates WebCodecs encoded streams whenever the choice allows H.264", async () => {
    installBrowserGlobals(() => 0);
    defineGlobal("VideoDecoder", class FakeVideoDecoder {});
    for (const codecChoice of ["auto", "av1", "hevc", "h264"] as const) {
      let createEncodedStreamsCalls = 0;
      const peer = new FakePeer();
      peer.addTransceiver = (kind: string, init: RTCRtpTransceiverInit) => {
        peer.transceivers.push([kind, init.direction]);
        return {
          receiver: {
            createEncodedStreams: () => {
              createEncodedStreamsCalls += 1;
              return {
                readable: new ReadableStream(),
                writable: new WritableStream(),
              };
            },
          },
        } as unknown as RTCRtpTransceiver;
      };
      const video = new FakeVideo();
      const controller = new RemoteSessionController({
        sessionId: `session-${codecChoice}`,
        controllerEpoch: "controller",
        displays: [],
        iceServers: [],
        codecChoice,
        video: video as unknown as HTMLVideoElement,
        operations: { sendSignal: async () => null, endSession: async () => null },
        peerFactory: () => peer as unknown as RTCPeerConnection,
        onSnapshot: () => {},
      });
      await controller.start();
      expect(createEncodedStreamsCalls).toBe(
        codecChoice === "av1" || codecChoice === "hevc" ? 0 : 1,
      );
      expect(peer.channelConfigs.get("nanoctl.video.v1")).toMatchObject({
        ordered: false,
        maxPacketLifeTime: 160,
      });
      await controller.stop("done", false);
    }
  });

  test("data-channel decode drops resync at a keyframe instead of decoding corrupt deltas", () => {
    installBrowserGlobals(() => 0);
    const decoded: string[] = [];
    let resets = 0;
    const decoderInstances: FakeVideoDecoderForRecovery[] = [];
    class FakeVideoDecoderForRecovery {
      decodeQueueSize = 0;
      constructor() {
        decoderInstances.push(this);
      }
      configure() {}
      reset() {
        resets += 1;
        this.decodeQueueSize = 0;
      }
      decode(chunk: { init: { type: string } }) {
        decoded.push(chunk.init.type);
      }
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoderForRecovery);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(5 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes.set(au, 5);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false) =>
      channel.onmessage?.({ data: message(seq, key) } as MessageEvent);
    channel.sent = [];
    receive(0, true);
    receive(1);
    expect(decoded).toEqual(["key", "delta"]);

    decoderInstances[0]!.decodeQueueSize = 2;
    receive(2);
    expect(channel.sent).toContain("KF");
    expect(decoded).toEqual(["key", "delta"]);
    receive(3);
    expect(decoded).toEqual(["key", "delta"]);
    receive(4, true);
    expect(decoded).toEqual(["key", "delta", "key"]);
    expect(resets).toBeGreaterThan(0);
  });

  test("data-channel stats expose wire bytes, decode shape, and loss counters", () => {
    installBrowserGlobals(() => 0);
    let output: ((frame: unknown) => void) | null = null;
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      constructor(init: { output: (frame: unknown) => void }) {
        output = init.output;
      }
      configure() {}
      reset() {}
      decode(chunk: { init: { timestamp: number } }) {
        output?.({
          timestamp: chunk.init.timestamp,
          displayWidth: 1280,
          displayHeight: 720,
          close() {},
        });
      }
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    const handle = attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(5 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes.set(au, 5);
      return bytes.buffer;
    };
    channel.onmessage?.({ data: message(0, true) } as MessageEvent);
    channel.onmessage?.({ data: message(1, false) } as MessageEvent);
    expect(handle.stats()).toMatchObject({
      decoded: 2,
      width: 1280,
      height: 720,
      codec: "h264",
      bytes: 22,
      packetsLost: 0,
    });
    expect(handle.stats().decodeTimedFrames).toBe(2);
    handle.drop();
  });

  test("keyframe recovery skips do not amplify data-channel loss feedback", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    const handle = attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(5 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes.set(au, 5);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false) =>
      channel.onmessage?.({ data: message(seq, key) } as MessageEvent);

    receive(0, true);
    for (let seq = 1; seq <= 10; seq += 1) receive(seq);
    expect(handle.takeLinkLoss?.()).toBe(0);

    // Lose only sequence 11. Sequences 12-28 overflow the reorder window and
    // sequence 40 is the recovery IDR; the 29 dependent frames skipped by that
    // IDR must not be treated as independent network loss.
    for (let seq = 12; seq <= 28; seq += 1) receive(seq);
    expect(channel.sent).toContain("KF");
    receive(40, true);

    const loss = handle.takeLinkLoss?.();
    expect(loss).toBeGreaterThan(0);
    expect(loss).toBeLessThan(0.1);
    handle.drop();
  });

  test("keyframe requests use the reliable idr channel when present", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    const idrChannel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      idrChannel as unknown as RTCDataChannel,
      video as unknown as HTMLVideoElement,
    );
    expect(idrChannel.sent).toContain("KF");
    expect(channel.sent).not.toContain("KF");
  });

  test("data-channel loss requests recovery as soon as the reorder window overflows", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(17 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes[7] = 1;
      bytes.set(au, 17);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false) =>
      channel.onmessage?.({ data: message(seq, key) } as MessageEvent);
    receive(0, true);
    channel.sent = [];
    for (let seq = 2; seq <= 17; seq += 1) receive(seq);
    expect(channel.sent).not.toContain("KF");
    receive(18);
    expect(channel.sent).toContain("KF");
  });

  test("data-channel loss recovery does not wait for sixteen frames at low frame rates", () => {
    const intervals: (() => void)[] = [];
    installBrowserGlobals((callback) => {
      intervals.push(callback);
      return intervals.length;
    });
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    const handle = attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(5 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes.set(au, 5);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false) =>
      channel.onmessage?.({ data: message(seq, key) } as MessageEvent);
    const originalNow = Date.now;
    let now = 1_000;
    Date.now = () => now;
    try {
      receive(0, true);
      channel.sent = [];
      now = 1_100;
      receive(2);
      receive(3);
      expect(channel.sent).not.toContain("KF");
      now = 1_501;
      for (const callback of intervals) callback();
      expect(channel.sent).toContain("KF");
    } finally {
      Date.now = originalNow;
      handle.drop();
    }
  });

  test("data-channel reassembly tolerates fragmented frames interleaved with newer sequences", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean, part = 0, parts = 1) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const fragmented = parts > 1;
      const bytes = new Uint8Array((fragmented ? 9 : 5) + au.length);
      bytes[0] = seq;
      bytes[4] = (key ? 1 : 0) | (fragmented ? 2 : 0);
      let offset = 5;
      if (fragmented) {
        bytes[5] = part;
        bytes[7] = parts;
        offset = 9;
      }
      bytes.set(au, offset);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false, part = 0, parts = 1) =>
      channel.onmessage?.({ data: message(seq, key, part, parts) } as MessageEvent);
    receive(0, true);
    channel.sent = [];
    receive(1, false, 0, 2);
    for (let seq = 2; seq <= 17; seq += 1) receive(seq);
    expect(channel.sent).not.toContain("KF");
    receive(1, false, 1, 2);
    expect(channel.sent).not.toContain("KF");
  });

  test("idle heartbeat sequence watermarks expose a lost final delta", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const frame = (seq: number, key: boolean) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const bytes = new Uint8Array(5 + au.length);
      bytes[0] = seq;
      bytes[4] = key ? 1 : 0;
      bytes.set(au, 5);
      return bytes.buffer;
    };
    const heartbeat = (seq: number) => {
      const bytes = new Uint8Array(5);
      bytes[0] = seq;
      bytes[4] = 8;
      return bytes.buffer;
    };
    const receive = (data: ArrayBuffer) => channel.onmessage?.({ data } as MessageEvent);
    receive(frame(0, true));
    channel.sent = [];
    receive(heartbeat(1));
    expect(channel.sent).not.toContain("KF");
    receive(heartbeat(2));
    expect(channel.sent).toContain("KF");
  });

  test("incomplete access-unit loss requests keyframe recovery after the reorder window", () => {
    installBrowserGlobals(() => 0);
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const message = (seq: number, key: boolean, part = 0, parts = 1) => {
      const au = key ? [0, 0, 0, 1, 0x65, 1] : [0, 0, 0, 1, 0x41, 1];
      const fragmented = parts > 1;
      const bytes = new Uint8Array((fragmented ? 9 : 5) + au.length);
      bytes[0] = seq;
      bytes[4] = (key ? 1 : 0) | (fragmented ? 2 : 0);
      let offset = 5;
      if (fragmented) {
        bytes[5] = part;
        bytes[7] = parts;
        offset = 9;
      }
      bytes.set(au, offset);
      return bytes.buffer;
    };
    const receive = (seq: number, key = false, part = 0, parts = 1) =>
      channel.onmessage?.({ data: message(seq, key, part, parts) } as MessageEvent);
    receive(0, true);
    channel.sent = [];
    receive(1, false, 0, 2);
    for (let seq = 2; seq <= 17; seq += 1) receive(seq);
    expect(channel.sent).not.toContain("KF");
    receive(18);
    expect(channel.sent).toContain("KF");
  });

  test("waiting for a keyframe retries the recovery request", () => {
    const intervals: (() => void)[] = [];
    installBrowserGlobals((callback) => {
      intervals.push(callback);
      return intervals.length;
    });
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      configure() {}
      reset() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);
    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    const channel = new FakeDataChannel();
    (window as unknown as { __noEarlyKf?: boolean }).__noEarlyKf = true;
    const handle = attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    expect(channel.sent).not.toContain("KF");
    for (const callback of intervals) callback();
    expect(channel.sent).toContain("KF");
    channel.sent = [];
    handle.drop();
    for (const callback of intervals) callback();
    expect(channel.sent).not.toContain("KF");
  });

  test("webcodecs teardown does not clear an unowned video srcObject", async () => {
    installBrowserGlobals(() => 0);
    let decoderError: ((error: Error) => void) | null = null;
    class FakeVideoDecoder {
      decodeQueueSize = 0;
      constructor(init: { error: (error: Error) => void }) {
        decoderError = init.error;
      }
      configure() {}
      decode() {}
      close() {}
    }
    class FakeEncodedVideoChunk {
      constructor(public readonly init: unknown) {}
    }
    class FakeTrackGenerator {
      track = { stop() {} };
      writable = new WritableStream();
    }
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);

    const video = Object.assign(new FakeVideo(), {
      ownerDocument: { createElement: () => ({}) },
    });
    let readableController: ReadableStreamDefaultController<unknown> | null = null;
    const readable = new ReadableStream({
      start(controller) {
        readableController = controller;
      },
    });
    const writable = new WritableStream();
    let teardownNotified = 0;
    const handle = attachWebCodecsVideo(
      { readable, writable } as unknown as Parameters<typeof attachWebCodecsVideo>[0],
      video as unknown as HTMLVideoElement,
      () => {
        teardownNotified += 1;
      },
    );

    const liveStream = new FakeMediaStream();
    video.srcObject = liveStream;
    readableController!.enqueue({
      type: "key",
      timestamp: 0,
      data: new Uint8Array([0, 0, 0, 1, 0x65, 0x01]).buffer,
      getMetadata: () => ({ rtpTimestamp: 0 }),
    });
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(decoderError).not.toBeNull();
    // A single decoder error is not fatal: the decoder is rebuilt on the next
    // keyframe and the RTP leg stays untouched.
    decoderError!(new Error("Decoding error"));
    expect(video.srcObject).toBe(liveStream);
    expect(teardownNotified).toBe(0);
    // Repeated failure gives up on the WebCodecs path so the session falls
    // back to RTP video.
    decoderError!(new Error("Decoding error"));
    decoderError!(new Error("Decoding error"));
    expect(video.srcObject).toBe(liveStream);
    expect(teardownNotified).toBe(1);
    handle.drop();
  });
});

describe("wheel normalization", () => {
  test("keeps fine-grained trackpad deltas and normalizes delta modes", () => {
    expect(wheelPixels(1, 0)).toBe(1);
    expect(wheelPixels(0.5, 0)).toBe(0.5);
    expect(wheelPixels(100, 0)).toBe(100);
    expect(wheelPixels(3, 1)).toBeCloseTo(100, 5);
    expect(wheelPixels(0.5, 2)).toBe(400);
    expect(wheelPixels(10_000, 0)).toBe(4_000);
    expect(wheelPixels(Number.NaN, 0)).toBe(0);
  });
});
