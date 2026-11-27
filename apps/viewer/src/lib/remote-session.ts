import {
  MAX_CONTROL_MESSAGE_BYTES,
  PROTOCOL_VERSION,
  assertSignalPayload,
  normalizeSignalEnvelope,
  type ControlMessage,
  type DisplayInfo,
  type NormalizedSignal,
  type SignalPayload,
  type VideoCodec,
} from "@nanoctl/protocol";
import { recordDebugEvent } from "./debug-report";
import { codecLabel, orderVideoCodecs, type CodecChoice } from "./video-codec";
import { encodeBinaryInput, encodeGamepadHeartbeat, type GamepadStateInput } from "./binary-input";
import { reliableControlBufferIsSaturated } from "./control-backpressure";
import { ClipboardSync } from "./clipboard-sync";
import { GamepadForwarder, type GamepadStatus } from "./gamepad";
import { videoRates, type VideoSample } from "./video-rates";
import {
  attachDataChannelVideo,
  attachWebCodecsVideo,
  createVideoEncodedStreams,
  supportsWebCodecsVideo,
  type DirectVideoCodec,
  type WebCodecsVideoHandle,
} from "./webcodecs-video";
import { normalizedPosition, videoGeometry } from "./touch-input";

export type RemoteSessionPhase =
  | "negotiating"
  | "connected"
  | "reconnecting"
  | "ending"
  | "ended"
  | "failed";

export type RemoteSessionMetrics = {
  resolution: string;
  fps: number | null;
  bitrateKbps: number | null;
  rttMs: number | null;
  packetsLost: number;
  framesDropped: number;
  /** Share of packets lost since the previous stats sample. */
  lossPercent: number | null;
  jitterMs: number | null;
  route: "direct" | "relay" | "unknown";
  codec: string | null;
  decoder: string | null;
  decodeMs: number | null;
};

export type RemoteSessionSnapshot = {
  phase: RemoteSessionPhase;
  reason?: string;
  reconnectAttempt: number;
  iceGeneration: number;
  inputEnabled: boolean;
  mouseLocked: boolean;
  gamepadEnabled: boolean;
  gamepadStatus: GamepadStatus;
  firstFrameReceived: boolean;
  metrics: RemoteSessionMetrics | null;
  /** Increments each time the host reports that its OS refused injected input. */
  inputBlocked: number;
};

export type RemoteSessionOperations = {
  sendSignal: (args: { sessionId: string; envelope: string }) => Promise<unknown>;
  endSession: (args: {
    sessionId: string;
    reason: string;
    controllerEpoch?: string;
  }) => Promise<unknown>;
};

export type HostSignalRow = {
  sequence: number;
  envelope: string;
};

export type QualityLimits = {
  maxFps?: number;
  maxBitrateKbps?: number;
  maxWidth?: number;
  maxHeight?: number;
};

const controlTextEncoder = new TextEncoder();

export type RemoteCursorShape = {
  width: number;
  height: number;
  hotspotX: number;
  hotspotY: number;
  /** Base64-encoded RGBA pixels. */
  pixels: string;
};

export type RemoteCursorPosition = { x: number; y: number; visible: boolean };

export type RemoteSessionOptions = {
  sessionId: string;
  controllerEpoch: string;
  displays: readonly DisplayInfo[];
  iceServers: readonly RTCIceServer[];
  video: HTMLVideoElement;
  audio?: HTMLAudioElement;
  onAudioTrackChange?: () => void;
  systemAudioEnabled?: boolean;
  initialAudioEnabled?: boolean;
  remoteInputEnabled?: boolean;
  clipboardEnabled?: boolean;
  gamepadAvailable?: boolean;
  /** Host understands compact binary motion reports. */
  binaryInput?: boolean;
  /** An immutable browser-side policy can further restrict host capabilities. */
  remoteInputPolicy?: boolean;
  preferences?: { inputEnabled: boolean; displayId?: string; gamepadEnabled?: boolean };
  /** Codecs the host can encode; the viewer orders its offer so the host picks the best one. */
  videoCodecs?: readonly VideoCodec[];
  codecChoice?: CodecChoice;
  qualityLimits?: QualityLimits;
  operations: RemoteSessionOperations;
  getIceServers?: () => Promise<readonly RTCIceServer[]>;
  revalidateController?: () => Promise<{
    owned: boolean;
    controllerEpoch?: string;
  }>;
  onPageRestore?: (controllerEpoch?: string) => void;
  onSnapshot: (snapshot: RemoteSessionSnapshot) => void;
  onCursorShape?: (shape: RemoteCursorShape) => void;
  onCursorPosition?: (position: RemoteCursorPosition) => void;
  peerFactory?: (configuration: RTCConfiguration) => RTCPeerConnection;
};

const NEGOTIATION_TIMEOUT_MS = 20_000;
const FIRST_FRAME_TIMEOUT_MS = 15_000;
const DISCONNECTED_GRACE_MS = 1_500;
const MAX_RECONNECT_ATTEMPTS = 3;
const STATS_INTERVAL_MS = 1_000;
const MAX_PENDING_CANDIDATES = 128;

/**
 * Owns one browser remote-desktop session.
 *
 * The interface is intentionally small: start, ingest signals, send control, and stop. All
 * ordering and resource ownership lives behind this seam. A replacement session gets a new
 * controller instance, while an ICE restart stays inside the same controller and increments the
 * explicit ICE generation.
 */
export class RemoteSessionController {
  private readonly options: RemoteSessionOptions;
  private readonly peerFactory: (configuration: RTCConfiguration) => RTCPeerConnection;
  private readonly processedSignals = new Set<string>();
  private readonly processedSignalOrder: string[] = [];
  private readonly pendingCandidates = new Map<
    number,
    { key: string; candidate: RTCIceCandidateInit }[]
  >();
  private readonly pendingEndOfCandidates = new Map<number, string>();

  private peer: RTCPeerConnection | null = null;
  private webCodecsVideo: WebCodecsVideoHandle | null = null;
  private remoteVideoStream: MediaStream | null = null;
  private pendingEncodedStreams: {
    readable: ReadableStream;
    writable: WritableStream;
  } | null = null;
  private pointerChannel: RTCDataChannel | null = null;
  private gamepadChannel: RTCDataChannel | null = null;
  private controlChannel: RTCDataChannel | null = null;
  private clipboardChannel: RTCDataChannel | null = null;
  private clipboard: ClipboardSync | null = null;
  private gamepadFrame: number | null = null;
  private gamepadStatus: GamepadStatus = "waiting";
  private readonly gamepad = new GamepadForwarder((message) =>
    message.type === "gamepad" || message.type === "gamepad-sparse"
      ? this.sendControl(message)
      : this.sendGamepadHeartbeat(),
  );
  private keyQueue: Promise<void> = Promise.resolve();
  private lastKeyRepeatAt = 0;
  private readonly onClipboardFocus = () => {
    void this.clipboard?.focus();
  };
  private readonly onPaste = (event: ClipboardEvent) => {
    if (event.clipboardData && this.inputEnabled)
      this.clipboard?.local(event.clipboardData.getData("text/plain"));
  };
  private phase: RemoteSessionPhase = "negotiating";
  private reason: string | undefined;
  private reconnectAttempt = 0;
  private iceGeneration = 0;
  private readonly controllerEpoch: string;
  private readonly remoteInputPolicy: boolean;
  private displays: readonly DisplayInfo[];
  private remoteInputAllowed: boolean;
  private nextSequence: number;
  private remoteDescriptionGeneration: number | null = null;
  private hostEpoch: string | null = null;
  private pendingHostRestartGeneration: number | null = null;
  private lifecycleGeneration = 0;
  private inputEnabled = false;
  private inputBlocked = 0;
  private audioEnabled: boolean;
  private readonly pressedKeys = new Set<string>();
  private readonly preferences: {
    inputEnabled: boolean;
    displayId?: string;
    gamepadEnabled?: boolean;
  };
  private firstFrameReceived = false;
  private stopPromise: Promise<void> | null = null;
  private signalQueue: Promise<void> = Promise.resolve();
  private signalPublicationQueue: Promise<void> = Promise.resolve();
  private negotiationQueue: Promise<void> = Promise.resolve();
  private outstandingOfferGeneration: number | null = null;
  private restartTimer: number | null = null;
  private negotiationTimer: number | null = null;
  private firstFrameTimer: number | null = null;
  private statsTimer: number | null = null;
  private recoveryPromise: Promise<void> | null = null;
  private recoveryRetryRequested = false;
  private notifiedEnd = false;
  private statsInFlight = false;
  private previousVideoSample: VideoSample | null = null;
  private previousDirectVideoSample:
    | (VideoSample & {
        decodeTimeMs?: number;
        decodeTimedFrames?: number;
      })
    | null = null;
  private readonly localUfragGenerations = new Map<string, number>();
  private readonly offerPublicationGates = new Map<number, Promise<boolean>>();
  private readonly offerCompletionWaiters = new Map<number, () => void>();
  private metrics: RemoteSessionMetrics | null = null;
  private videoTransceiver: RTCRtpTransceiver | null = null;
  private qualityLimits: QualityLimits = {};
  private viewportObserver: ResizeObserver | null = null;
  private lastViewportQualityKey = "";
  private codecPreferencesApplied = false;
  private previousDecode: { id: string; time: number; frames: number } | null = null;
  private previousLoss: { id: string; lost: number; received: number } | null = null;

  private readonly onWindowBlur = () => {
    this.gamepad.release();
    this.releaseInput();
  };
  private readonly onVisibilityChange = () => {
    if (document.visibilityState === "hidden") this.onWindowBlur();
    else this.onClipboardFocus();
  };
  private readonly onPageHide = (event: PageTransitionEvent) => {
    // A persisted page may return from the back-forward cache with its React tree intact. Release
    // input while hidden, but leave the peer available for pageshow recovery; a real unload still
    // closes the peer without sending a terminal signal.
    this.inputEnabled = false;
    this.releaseInput();
    this.publishSnapshot();
    if (!event.persisted) void this.stop("controller disconnected", false);
  };
  private readonly onPageShow = (event: PageTransitionEvent) => {
    if (
      !event.persisted ||
      this.phase === "ending" ||
      this.phase === "ended" ||
      this.phase === "failed"
    ) {
      return;
    }
    void this.restorePage(this.lifecycleGeneration, this.peer);
  };
  private readonly onEmergencyShortcut = (event: KeyboardEvent) => {
    if (
      event.type !== "keydown" ||
      event.code !== "Escape" ||
      !event.ctrlKey ||
      !event.altKey ||
      !event.shiftKey
    ) {
      return;
    }
    event.preventDefault();
    event.stopImmediatePropagation();
    this.inputEnabled = false;
    this.releaseInput();
    this.publishSnapshot();
    if (document.fullscreenElement) void document.exitFullscreen();
  };
  private readonly onVideoLoadedData = () => {
    if (this.phase === "ending" || this.phase === "ended" || this.phase === "failed") return;
    this.firstFrameReceived = true;
    this.clearFirstFrameTimer();
    this.sendQualityLimits();
    this.publishSnapshot();
  };

  // The WebCodecs path renders through a track generator or a plain canvas;
  // where neither touches the <video> element (WebKit has no
  // MediaStreamTrackGenerator), loadeddata never fires and the session would
  // otherwise hit the first-frame timeout despite video being on screen.
  private readonly onDirectVideoFrame = () => {
    if (this.firstFrameReceived) return;
    this.onVideoLoadedData();
  };

  constructor(options: RemoteSessionOptions) {
    this.qualityLimits = { ...options.qualityLimits };
    this.options = options;
    this.preferences = options.preferences ?? { inputEnabled: true };
    this.controllerEpoch = options.controllerEpoch;
    this.remoteInputPolicy = options.remoteInputPolicy !== false;
    this.displays = options.displays;
    this.remoteInputAllowed = this.remoteInputPolicy && options.remoteInputEnabled !== false;
    this.audioEnabled = options.initialAudioEnabled !== false;
    // Keep sequence numbers fresh and exactly representable by JavaScript clients.
    this.nextSequence = safeSequenceSeed();
    this.peerFactory =
      options.peerFactory ?? ((configuration) => new RTCPeerConnection(configuration));
    if (typeof ResizeObserver !== "undefined") {
      this.viewportObserver = new ResizeObserver(() => this.sendQualityLimits());
      this.viewportObserver.observe(options.video);
    }
  }

  get snapshot(): RemoteSessionSnapshot {
    return {
      phase: this.phase,
      ...(this.reason ? { reason: this.reason } : {}),
      reconnectAttempt: this.reconnectAttempt,
      iceGeneration: this.iceGeneration,
      inputEnabled: this.inputEnabled,
      mouseLocked: document.pointerLockElement === this.options.video,
      gamepadEnabled: this.gamepadFrame !== null,
      gamepadStatus: this.gamepadStatus,
      firstFrameReceived: this.firstFrameReceived,
      metrics: this.metrics,
      inputBlocked: this.inputBlocked,
    };
  }

  /** Update capability metadata without tearing down a healthy peer. */
  updateCapabilities(displays: readonly DisplayInfo[], remoteInputEnabled: boolean): void {
    this.displays = displays;
    this.remoteInputAllowed = this.remoteInputPolicy && remoteInputEnabled;
    if (!this.remoteInputAllowed) {
      this.inputEnabled = false;
      this.releaseInput();
    }
    this.publishSnapshot();
  }

  start(): Promise<void> {
    if (this.peer || this.phase !== "negotiating") return Promise.resolve();
    const lifecycle = ++this.lifecycleGeneration;
    const peer = this.peerFactory({
      iceServers: [...this.options.iceServers],
      bundlePolicy: "max-bundle",
      iceTransportPolicy: "all",
    });
    this.peer = peer;
    this.firstFrameReceived = false;
    this.attachPeer(peer, lifecycle);
    this.attachInput();
    this.statsTimer = window.setInterval(() => this.collectStats(lifecycle), STATS_INTERVAL_MS);
    this.publishSnapshot();
    return this.sendOffer(false, lifecycle).catch(() => {
      if (!this.isCurrent(lifecycle, peer)) return;
      void this.fail("signaling failed");
    });
  }

  ingestHostSignals(rows: readonly HostSignalRow[]): void {
    const lifecycle = this.lifecycleGeneration;
    const peer = this.peer;
    if (!peer || this.phase === "ending" || this.phase === "ended" || this.phase === "failed") {
      return;
    }
    this.signalQueue = this.signalQueue
      .then(async () => {
        if (!this.isCurrent(lifecycle, peer)) return;
        for (const row of [...rows].sort((left, right) => left.sequence - right.sequence)) {
          if (!this.isCurrent(lifecycle, peer)) return;
          await this.applyHostSignal(row, lifecycle, peer);
        }
      })
      .catch(() => {
        if (this.isCurrent(lifecycle, peer)) void this.fail("signaling failed");
      });
  }

  setInputEnabled(enabled: boolean): void {
    if (this.phase === "ending" || this.phase === "ended" || this.phase === "failed") return;
    this.preferences.inputEnabled = enabled;
    this.inputEnabled = enabled && this.remoteInputAllowed && this.isControlOpen();
    if (!enabled) this.releaseInput();
    else {
      this.options.video.focus();
      this.startGamepadPolling();
    }
    this.publishSnapshot();
  }

  togglePointerLock(): void {
    if (!this.inputEnabled || typeof this.options.video.requestPointerLock !== "function") return;
    if (document.pointerLockElement === this.options.video) {
      document.exitPointerLock();
      return;
    }
    const request = this.options.video.requestPointerLock as (options?: {
      unadjustedMovement?: boolean;
    }) => Promise<void> | void;
    try {
      const pending = request.call(this.options.video, { unadjustedMovement: true });
      if (pending instanceof Promise) {
        void pending.catch(() => this.options.video.requestPointerLock());
      }
    } catch {
      void this.options.video.requestPointerLock();
    }
  }

  setGamepadEnabled(enabled: boolean): void {
    this.preferences.gamepadEnabled = enabled;
    if (enabled) {
      this.startGamepadPolling();
    } else {
      this.stopGamepads();
      this.gamepad.detach();
    }
    this.publishSnapshot();
  }

  private startGamepadPolling(): void {
    if (
      this.gamepadFrame !== null ||
      !this.preferences.gamepadEnabled ||
      !this.options.gamepadAvailable ||
      !this.inputEnabled ||
      this.phase !== "connected" ||
      typeof navigator.getGamepads !== "function"
    )
      return;
    this.gamepad.resume();
    this.gamepadFrame = window.requestAnimationFrame(this.pollGamepads);
  }

  private readonly pollGamepads = (now: number): void => {
    this.gamepadFrame = window.requestAnimationFrame(this.pollGamepads);
    if (
      this.inputEnabled &&
      this.phase === "connected" &&
      document.visibilityState === "visible" &&
      document.hasFocus()
    ) {
      try {
        const status = this.gamepad.sample(navigator.getGamepads(), now);
        if (status !== this.gamepadStatus) {
          this.gamepadStatus = status;
          this.publishSnapshot();
        }
      } catch {
        this.setGamepadEnabled(false);
      }
    } else {
      this.gamepad.release();
    }
  };

  private stopGamepads(): void {
    if (this.gamepadFrame === null) return;
    window.cancelAnimationFrame(this.gamepadFrame);
    this.gamepadFrame = null;
    this.gamepadStatus = "waiting";
    this.gamepad.release();
  }

  private sendGamepadHeartbeat(): boolean {
    const channel = this.gamepadChannel;
    if (
      !this.options.binaryInput ||
      !this.inputEnabled ||
      !this.remoteInputAllowed ||
      !channel ||
      channel.readyState !== "open" ||
      channel.bufferedAmount > 4 * 1024
    )
      return false;
    try {
      channel.send(encodeGamepadHeartbeat());
      return true;
    } catch {
      return false;
    }
  }

  /** Applies live without renegotiation; muted viewers do not receive Opus packets. */
  setAudioEnabled(enabled: boolean): boolean {
    this.audioEnabled = enabled;
    return this.sendControl({ type: "audio", enabled });
  }

  /** Applies live without renegotiation; the host clamps to its configured maximums. */
  setQualityLimits(limits: QualityLimits): boolean {
    this.qualityLimits = { ...limits };
    this.lastViewportQualityKey = "";
    return this.sendQualityLimits() !== false;
  }

  selectDisplay(displayId: string): boolean {
    if (!this.inputEnabled || !this.isControlOpen()) return false;
    if (!this.displays.some((display) => display.id === displayId)) return false;
    if (!this.sendControl({ type: "display", displayId })) return false;
    this.preferences.displayId = displayId;
    return true;
  }

  sendControl(payload: ControlMessage | GamepadStateInput): boolean {
    const gamepadState = payload.type === "gamepad" || payload.type === "gamepad-sparse";
    // Sparse snapshots self-repair individual slots and use the bounded-loss
    // gamepad lane. Full snapshots are the sparse baseline, so they must not be
    // dropped; they ride the reliable control lane even when they carry pads.
    const lossyGamepadSnapshot = payload.type === "gamepad-sparse" && payload.pads.length > 0;
    const channel = lossyGamepadSnapshot
      ? this.gamepadChannel
      : payload.type === "pointer" && payload.action === "move"
        ? this.pointerChannel
        : this.controlChannel;
    if (!channel || channel.readyState !== "open") return false;
    if (gamepadState) {
      if (
        !this.options.gamepadAvailable ||
        (payload.pads.length > 0 && (!this.inputEnabled || !this.remoteInputAllowed)) ||
        channel.bufferedAmount > 4 * 1024
      )
        return false;
      try {
        const binary = this.options.binaryInput ? encodeBinaryInput(payload) : null;
        if (binary) channel.send(binary);
        else channel.send(JSON.stringify(payload));
        return true;
      } catch {
        return false;
      }
    }
    if (
      !this.inputEnabled &&
      (payload.type === "pointer" || payload.type === "key" || payload.type === "text")
    ) {
      return false;
    }
    if (payload.type === "display" && !this.remoteInputAllowed) return false;
    const isMotion = payload.type === "pointer" && payload.action === "move";
    const binary = this.options.binaryInput ? encodeBinaryInput(payload) : null;
    let serialized: string | null = null;
    if (!binary) {
      serialized = JSON.stringify(payload);
      if (controlTextEncoder.encode(serialized).byteLength > MAX_CONTROL_MESSAGE_BYTES)
        return false;
    }
    if (isMotion && channel.bufferedAmount > 64 * 1024) return false;
    if (!isMotion && reliableControlBufferIsSaturated(channel.bufferedAmount)) {
      this.inputEnabled = false;
      this.releaseInput();
      this.publishSnapshot();
      void this.fail("control channel stalled");
      return false;
    }
    try {
      if (binary) channel.send(binary);
      else channel.send(serialized!);
      return true;
    } catch {
      if (!isMotion) {
        this.inputEnabled = false;
        this.releaseInput();
        void this.fail("control channel unavailable");
      }
      return false;
    }
  }

  async stop(reason = "ended by controller", notifyControlPlane = false): Promise<void> {
    if (this.stopPromise) return this.stopPromise;
    this.stopPromise = (async () => {
      const terminalPhase: RemoteSessionPhase =
        reason.includes("failed") ||
        reason.includes("stalled") ||
        reason.includes("unavailable") ||
        reason.includes("timeout")
          ? "failed"
          : "ended";
      this.phase = "ending";
      this.reason = reason;
      this.inputEnabled = false;
      this.firstFrameReceived = false;
      this.outstandingOfferGeneration = null;
      for (const generation of this.offerCompletionWaiters.keys()) {
        this.completeOffer(generation);
      }
      const peer = this.peer;
      this.lifecycleGeneration += 1;
      this.clearTimers();
      this.stopGamepads();
      this.releaseInput();
      this.detachInput();
      if (peer) {
        peer.onconnectionstatechange = null;
        peer.onicecandidate = null;
        peer.ontrack = null;
        this.options.video.removeEventListener("loadeddata", this.onVideoLoadedData);
        this.viewportObserver?.disconnect();
        this.viewportObserver = null;
        this.pointerChannel?.close();
        this.gamepadChannel?.close();
        this.controlChannel?.close();
        this.clipboardChannel?.close();
        this.clipboardChannel = null;
        this.clipboard = null;
        if (this.options.audio) {
          this.options.audio.pause();
          this.options.audio.srcObject = null;
          this.options.onAudioTrackChange?.();
        }
        this.pointerChannel = null;
        this.gamepadChannel = null;
        this.controlChannel = null;
        peer.close();
      }
      this.peer = null;
      this.webCodecsVideo?.drop();
      this.webCodecsVideo = null;
      this.remoteVideoStream = null;
      this.pendingEncodedStreams = null;
      const stream = this.options.video.srcObject;
      if (stream instanceof MediaStream) {
        for (const track of stream.getTracks()) track.stop();
      }
      this.options.video.srcObject = null;
      this.phase = terminalPhase;
      this.publishSnapshot();
      if (notifyControlPlane && !this.notifiedEnd) {
        this.notifiedEnd = true;
        try {
          await this.options.operations.endSession({
            sessionId: this.options.sessionId,
            reason: reason.slice(0, 256),
            controllerEpoch: this.controllerEpoch,
          });
        } catch {
          // The peer is already closed and input is released. A lost terminal mutation must not
          // keep native/browser resources alive or make a second stop unsafe.
        }
      }
    })();
    return this.stopPromise;
  }

  private attachPeer(peer: RTCPeerConnection, lifecycle: number): void {
    if (this.options.clipboardEnabled && this.remoteInputAllowed) {
      const channel = peer.createDataChannel("nanoctl.clipboard.v1", {
        ordered: true,
      });
      this.clipboardChannel = channel;
      this.clipboard = new ClipboardSync({
        focused: () => this.clipboardChannel === channel && document.hasFocus(),
        read: () => navigator.clipboard.readText(),
        write: (text) => navigator.clipboard.writeText(text),
        send: (message) => {
          if (this.clipboardChannel !== channel || channel.readyState !== "open") return false;
          try {
            channel.send(message);
            return true;
          } catch {
            return false;
          }
        },
      });
      channel.onmessage = (event) => this.clipboard?.receive(event.data);
      channel.onopen = this.onClipboardFocus;
    }
    this.pointerChannel = peer.createDataChannel("nanoctl.pointer.v1", {
      ordered: false,
      maxRetransmits: 0,
    });
    this.gamepadChannel = peer.createDataChannel("nanoctl.gamepad.v1", {
      ordered: false,
      // Sparse gamepad reports are sequenced absolute slot state, so delayed
      // retransmits cannot regress newer input. Bounded repair replaces the
      // expensive always-on application duplicate window; expired copies are
      // too stale to matter for game input.
      maxPacketLifeTime: 100,
    });
    this.controlChannel = peer.createDataChannel("nanoctl.control.v1", {
      ordered: true,
    });
    this.inputEnabled =
      this.preferences.inputEnabled &&
      this.remoteInputAllowed &&
      this.controlChannel.readyState === "open";
    this.controlChannel.onmessage = (event) => {
      if (!this.isCurrent(lifecycle, peer) || typeof event.data !== "string") return;
      let message: { type?: unknown };
      try {
        message = JSON.parse(event.data) as { type?: unknown };
      } catch {
        return;
      }
      if (message.type === "inputBlocked") {
        this.inputBlocked += 1;
        this.publishSnapshot();
        return;
      }
      if (message.type === "cursorShape") {
        const shape = message as unknown as RemoteCursorShape;
        if (
          shape.width > 0 &&
          shape.height > 0 &&
          typeof shape.pixels === "string" &&
          shape.hotspotX >= 0 &&
          shape.hotspotY >= 0
        ) {
          this.options.onCursorShape?.(shape);
        }
        return;
      }
      if (message.type === "cursor") {
        const position = message as unknown as RemoteCursorPosition;
        if (
          Number.isFinite(position.x) &&
          Number.isFinite(position.y) &&
          typeof position.visible === "boolean"
        ) {
          this.options.onCursorPosition?.(position);
        }
      }
    };
    this.controlChannel.onopen = () => {
      if (!this.isCurrent(lifecycle, peer)) return;
      this.onControlReady();
    };
    this.options.video.addEventListener("loadeddata", this.onVideoLoadedData);
    this.videoTransceiver = peer.addTransceiver("video", {
      direction: "recvonly",
    });
    this.codecPreferencesApplied = false;
    // Chrome only allows encoded streams before any RTP arrives, so the
    // transform must be created here — before negotiation — not in ontrack.
    // The data-channel WebCodecs path handles H.264/HEVC Annex-B and AV1
    // low-overhead OBUs. The RTP encoded-transform tap remains H.264-only;
    // in "auto" it must be created before the answer reveals whether H.264
    // was selected.
    const h264TapEligible =
      this.options.codecChoice === undefined ||
      this.options.codecChoice === "auto" ||
      this.options.codecChoice === "h264";
    const directVideoEligible =
      h264TapEligible || this.options.codecChoice === "hevc" || this.options.codecChoice === "av1";
    const receiver = this.videoTransceiver?.receiver as RTCRtpReceiver | undefined;
    this.pendingEncodedStreams =
      h264TapEligible && receiver && supportsWebCodecsVideo(receiver)
        ? (createVideoEncodedStreams(receiver) as {
            readable: ReadableStream;
            writable: WritableStream;
          } | null)
        : null;
    const restoreRemoteVideo = () => {
      if (!this.isCurrent(lifecycle, peer) || !this.remoteVideoStream) return;
      if (this.options.video.srcObject !== this.remoteVideoStream) {
        this.options.video.srcObject = this.remoteVideoStream;
        void this.options.video.play().catch(() => {});
      }
    };
    // True low-latency path: raw AUs over an unreliable data channel feed
    // WebCodecs directly — no jitter buffer anywhere. The channels are
    // created whenever the choice allows a direct-decode codec, then kept only
    // when the negotiated codec is one WebCodecs can decode that way —
    // otherwise they close so the agent falls back to RTP.
    if (typeof VideoDecoder === "function" && directVideoEligible) {
      const videoChannel = peer.createDataChannel("nanoctl.video.v1", {
        ordered: false,
        // Time-bounded retransmission repairs isolated SCTP loss for a fraction
        // of an IDR's cost. Unordered delivery means it cannot head-of-line
        // block newer frames. The lifetime covers one retransmission on real
        // WAN/Tailscale RTTs while staying well below the viewer's 400ms
        // reorder expiry, after which copies would be too stale to matter.
        maxPacketLifeTime: 160,
      });
      const idrChannel = peer.createDataChannel("nanoctl.video-idr.v1");
      // Compressed access units must arrive as ArrayBuffer. The spec default is
      // Blob in some implementations, which would make the zero-copy feed path
      // ignore every frame.
      videoChannel.binaryType = "arraybuffer";
      idrChannel.binaryType = "arraybuffer";
      videoChannel.onopen = () => {
        if (!this.isCurrent(lifecycle, peer)) return;
        const directVideoCodec = negotiatedDirectVideoCodec(peer);
        if (!directVideoCodec) {
          videoChannel.close();
          idrChannel.close();
          return;
        }
        // The data-channel path beats the jitter-buffered transform tap.
        this.webCodecsVideo?.drop();
        this.webCodecsVideo = attachDataChannelVideo(
          videoChannel,
          idrChannel,
          this.options.video,
          restoreRemoteVideo,
          directVideoCodec,
          this.onDirectVideoFrame,
        );
      };
    }
    if (this.options.systemAudioEnabled) peer.addTransceiver("audio", { direction: "recvonly" });
    peer.ontrack = ({ receiver, streams, track }) => {
      if (!this.isCurrent(lifecycle, peer)) return;
      if ("jitterBufferTarget" in receiver) {
        (receiver as RTCRtpReceiver & { jitterBufferTarget?: number }).jitterBufferTarget = 0;
      }
      if ("playoutDelayHint" in receiver) {
        (receiver as RTCRtpReceiver & { playoutDelayHint?: number }).playoutDelayHint = 0;
      }
      if ("jitterBufferTarget" in receiver) {
        (receiver as RTCRtpReceiver & { jitterBufferTarget?: number }).jitterBufferTarget = 0;
      }
      if (track.kind === "audio") {
        if (this.options.audio && this.options.systemAudioEnabled) {
          this.options.audio.srcObject = new MediaStream([track]);
          this.options.onAudioTrackChange?.();
        }
        return;
      }
      const stream = streams[0] ?? new MediaStream([track]);
      this.remoteVideoStream = stream;
      // The transform tap sits behind Chrome's jitter buffer anyway, so
      // only use it when the data-channel path did not attach — and only
      // when the negotiated codec is H.264, which is all the tap decodes.
      if (
        this.pendingEncodedStreams &&
        !this.webCodecsVideo &&
        negotiatedDirectVideoCodec(peer) === "h264"
      ) {
        const encoded = this.pendingEncodedStreams as Parameters<typeof attachWebCodecsVideo>[0];
        this.webCodecsVideo = attachWebCodecsVideo(
          encoded,
          this.options.video,
          restoreRemoteVideo,
          this.onDirectVideoFrame,
        );
      }
      // The data-channel path owns the element through its track generator:
      // writing the RTP stream over srcObject starves the generator, so the
      // decoder's output frames stay unclosed and the decoder wedges.
      if (!this.webCodecsVideo) {
        this.options.video.srcObject = stream;
        void this.options.video.play().catch(() => {});
      }
    };
    peer.onconnectionstatechange = () => {
      if (!this.isCurrent(lifecycle, peer)) return;
      recordDebugEvent(
        `peer ${peer.connectionState}`,
        `ice ${peer.iceConnectionState}; generation ${this.iceGeneration}`,
      );
      switch (peer.connectionState) {
        case "connected":
          this.reconnectAttempt = 0;
          this.clearRecoveryTimer();
          this.clearNegotiationTimer();
          this.phase = "connected";
          // Data channels survive an ICE restart, so a recovered connection re-opens input
          // and gamepad polling without waiting for another channel.onopen.
          this.onControlReady();
          this.armFirstFrameTimer(lifecycle, peer);
          this.publishSnapshot();
          break;
        case "disconnected":
          this.inputEnabled = false;
          this.releaseInput();
          this.publishSnapshot();
          this.scheduleRecovery(lifecycle, peer);
          break;
        case "failed":
          this.inputEnabled = false;
          this.releaseInput();
          this.requestRecovery(lifecycle, peer);
          break;
        case "closed":
          break;
        default:
          this.phase = "negotiating";
          this.publishSnapshot();
      }
    };
    peer.onicecandidate = ({ candidate }) => {
      if (!this.isCurrent(lifecycle, peer)) return;
      const usernameFragment = candidate?.usernameFragment ?? null;
      const generation = usernameFragment
        ? (this.localUfragGenerations.get(usernameFragment) ?? this.iceGeneration)
        : this.iceGeneration;
      const hasCandidate = candidate !== null && candidate.candidate.length > 0;
      const payload: SignalPayload = hasCandidate
        ? {
            type: "ice-candidate",
            candidate: candidate.candidate,
            sdpMid: candidate.sdpMid,
            sdpMLineIndex: candidate.sdpMLineIndex,
            usernameFragment,
          }
        : { type: "ice-complete" };
      const gate = this.offerPublicationGates.get(generation) ?? Promise.resolve(true);
      void gate
        .then((offerPublished) => {
          if (!offerPublished || !this.isCurrent(lifecycle, peer)) return;
          return this.publish(payload, generation, lifecycle);
        })
        .catch(() => {
          if (this.isCurrent(lifecycle, peer)) void this.fail("signaling failed");
        });
    };
    this.pointerChannel.onclose = () => {
      if (this.isCurrent(lifecycle, peer)) this.releaseInput();
    };
    this.controlChannel.onclose = () => {
      if (this.isCurrent(lifecycle, peer)) {
        this.inputEnabled = false;
        this.releaseInput();
        void this.fail("control channel unavailable");
      }
    };
  }

  private attachInput(): void {
    const video = this.options.video;
    document.addEventListener("pointerlockchange", this.onPointerLockChange);
    video.addEventListener("pointermove", this.onPointer);
    video.addEventListener("pointerdown", this.onPointer);
    video.addEventListener("pointerup", this.onPointer);
    video.addEventListener("pointercancel", this.onPointerCancel);
    video.addEventListener("lostpointercapture", this.onLostPointerCapture);
    video.addEventListener("wheel", this.onWheel, { passive: false });
    video.addEventListener("contextmenu", this.onContextMenu);
    window.addEventListener("keydown", this.onKey);
    window.addEventListener("keyup", this.onKey);
    window.addEventListener("keydown", this.onEmergencyShortcut, true);
    window.addEventListener("blur", this.onWindowBlur);
    window.addEventListener("focus", this.onClipboardFocus);
    window.addEventListener("paste", this.onPaste);
    document.addEventListener("visibilitychange", this.onVisibilityChange);
    window.addEventListener("pagehide", this.onPageHide);
    window.addEventListener("pageshow", this.onPageShow);
  }

  private detachInput(): void {
    const video = this.options.video;
    document.removeEventListener("pointerlockchange", this.onPointerLockChange);
    video.removeEventListener("pointermove", this.onPointer);
    video.removeEventListener("pointerdown", this.onPointer);
    video.removeEventListener("pointerup", this.onPointer);
    video.removeEventListener("pointercancel", this.onPointerCancel);
    video.removeEventListener("lostpointercapture", this.onLostPointerCapture);
    video.removeEventListener("wheel", this.onWheel);
    video.removeEventListener("contextmenu", this.onContextMenu);
    window.removeEventListener("keydown", this.onKey);
    window.removeEventListener("keyup", this.onKey);
    window.removeEventListener("keydown", this.onEmergencyShortcut, true);
    window.removeEventListener("blur", this.onWindowBlur);
    window.removeEventListener("focus", this.onClipboardFocus);
    window.removeEventListener("paste", this.onPaste);
    document.removeEventListener("visibilitychange", this.onVisibilityChange);
    window.removeEventListener("pagehide", this.onPageHide);
    window.removeEventListener("pageshow", this.onPageShow);
  }

  private readonly onPointerLockChange = (): void => this.publishSnapshot();

  private readonly onPointer = (event: PointerEvent): void => {
    if (event.pointerType === "touch") return;
    if (!this.inputEnabled) return;
    const position = this.normalizedPosition(event);
    if (!position) return;
    if (event.type === "pointerdown") {
      event.preventDefault();
      this.options.video.focus();
      try {
        this.options.video.setPointerCapture(event.pointerId);
      } catch {
        // Synthetic or already-ended pointers can reject capture; still deliver the click.
      }
    }
    const locked = document.pointerLockElement === this.options.video;
    if (locked && event.type === "pointermove") {
      this.sendControl({
        type: "pointer",
        action: "move",
        x: 0,
        y: 0,
        deltaX: event.movementX,
        deltaY: event.movementY,
      });
      return;
    }
    this.sendControl({
      type: "pointer",
      action: event.type === "pointermove" ? "move" : event.type === "pointerdown" ? "down" : "up",
      ...position,
      ...(locked && { deltaX: 0, deltaY: 0 }),
      button: event.button === 1 || event.button === 2 ? event.button : 0,
    });
  };

  private readonly onPointerCancel = (event: PointerEvent): void => {
    if (event.pointerType !== "touch") this.releaseInput();
  };

  private readonly onLostPointerCapture = (event: PointerEvent): void => {
    if (event.buttons !== 0) this.releaseInput();
  };

  private readonly onWheel = (event: WheelEvent): void => {
    if (!this.inputEnabled) return;
    const position = this.normalizedPosition(event);
    if (!position) return;
    event.preventDefault();
    this.sendControl({
      type: "pointer",
      action: "wheel",
      ...position,
      deltaX: wheelPixels(event.deltaX, event.deltaMode),
      deltaY: wheelPixels(event.deltaY, event.deltaMode),
    });
  };

  private readonly onContextMenu = (event: MouseEvent): void => {
    if (this.inputEnabled) event.preventDefault();
  };

  private readonly onKey = (event: KeyboardEvent): void => {
    if (!this.inputEnabled) return;
    if (event.type === "keydown" && event.ctrlKey && event.altKey && event.code === "KeyM") {
      event.preventDefault();
      this.togglePointerLock();
      return;
    }
    // Keys go to the host while the video or nothing in particular has focus; panel controls keep
    // theirs, and Tab from the page still moves browser focus so the controls stay reachable.
    // A key the host saw pressed is always released there, wherever focus has moved since.
    const target = event.target;
    const releasing = event.type === "keyup" && this.pressedKeys.delete(event.code);
    if (
      !releasing &&
      target !== this.options.video &&
      (event.code === "Tab" || (target !== document.body && target !== document.documentElement))
    )
      return;
    if (event.type === "keydown") this.pressedKeys.add(event.code);
    event.preventDefault();
    // The host synthesizes typematic repeats from the held key state. Browser
    // repeats are throttled to liveness ticks so a stuck key still times out
    // when the viewer or network dies, without paying a datagram at typematic rate.
    if (event.type === "keydown" && event.repeat) {
      if (event.timeStamp - this.lastKeyRepeatAt < 250) return;
      this.lastKeyRepeatAt = event.timeStamp;
    }
    const message: ControlMessage = {
      type: "key",
      action: event.type === "keydown" ? "down" : "up",
      code: event.code,
      key: event.key,
      modifiers:
        Number(event.shiftKey) |
        (Number(event.ctrlKey) << 1) |
        (Number(event.altKey) << 2) |
        (Number(event.metaKey) << 3),
      repeat: event.repeat,
    };
    if (!this.clipboard) {
      this.sendControl(message);
      return;
    }
    this.keyQueue = this.keyQueue
      .then(async () => {
        if (event.type === "keydown" && event.code === "KeyV" && (event.ctrlKey || event.metaKey)) {
          await Promise.race([
            this.clipboard?.focus(),
            new Promise<void>((resolve) => window.setTimeout(resolve, 250)),
          ]);
        }
        if (this.inputEnabled) this.sendControl(message);
      })
      .catch(() => {});
  };

  private normalizedPosition(event: MouseEvent): { x: number; y: number } | null {
    const video = this.options.video;
    const geometry = videoGeometry(
      video.getBoundingClientRect(),
      video.videoWidth,
      video.videoHeight,
    );
    return geometry ? normalizedPosition({ x: event.clientX, y: event.clientY }, geometry) : null;
  }

  private async applyHostSignal(
    row: HostSignalRow,
    lifecycle: number,
    peer: RTCPeerConnection,
  ): Promise<void> {
    if (!Number.isSafeInteger(row.sequence) || row.sequence < 0) return;
    let signal: NormalizedSignal;
    try {
      signal = normalizeSignalEnvelope(JSON.parse(row.envelope));
    } catch {
      return;
    }
    if (
      signal.sender !== "host" ||
      signal.sessionId !== this.options.sessionId ||
      signal.controllerEpoch !== this.controllerEpoch ||
      signal.sequence !== row.sequence ||
      signal.hostEpoch.length === 0
    ) {
      return;
    }
    const key = `${signal.hostEpoch}:${signal.generation}:${signal.sequence}`;
    if (this.processedSignals.has(key)) return;

    if (this.hostEpoch === null) {
      this.hostEpoch = signal.hostEpoch;
    } else if (this.hostEpoch !== signal.hostEpoch) {
      // A native restart gets a fresh host epoch. Its answer for the old browser offer is not
      // usable once that offer has already reached stable, so make the epoch transition an ICE
      // recovery boundary. If the old offer is still outstanding, the replacement can complete
      // it normally; this covers a host crash before its first answer was published.
      this.hostEpoch = signal.hostEpoch;
      this.pendingCandidates.delete(signal.generation);
      this.pendingEndOfCandidates.delete(signal.generation);
      const canCompleteOutstandingOffer =
        signal.payload.type !== "end" && this.outstandingOfferGeneration === signal.generation;
      if (!canCompleteOutstandingOffer) {
        this.markSignalProcessed(key);
        if (signal.payload.type === "end") {
          await this.stop(`failed: ${signal.payload.reason}`, false);
          return;
        }
        // If a restart offer is already outstanding, the replacement host can answer that offer
        // directly. Do not advance the pending boundary to generation+1 and strand its matching
        // answer behind a second recovery offer.
        const restartGeneration = this.outstandingOfferGeneration ?? this.iceGeneration + 1;
        this.pendingHostRestartGeneration = Math.max(
          this.pendingHostRestartGeneration ?? restartGeneration,
          restartGeneration,
        );
        this.requestRecovery(lifecycle, peer);
        return;
      }
    }
    if (
      this.pendingHostRestartGeneration !== null &&
      signal.generation < this.pendingHostRestartGeneration
    ) {
      this.markSignalProcessed(key);
      return;
    }
    if (
      this.pendingHostRestartGeneration !== null &&
      signal.generation >= this.pendingHostRestartGeneration
    ) {
      this.pendingHostRestartGeneration = null;
    }
    switch (signal.payload.type) {
      case "answer":
        if ((await this.applyAnswer(signal, lifecycle, peer)) === "retry") return;
        if (!this.isCurrent(lifecycle, peer)) return;
        this.markSignalProcessed(key);
        return;
      case "ice-candidate":
        if (signal.generation < this.iceGeneration) {
          this.markSignalProcessed(key);
          return;
        }
        if (this.remoteDescriptionGeneration !== signal.generation) {
          this.bufferCandidate(signal.generation, key, {
            candidate: signal.payload.candidate,
            sdpMid: signal.payload.sdpMid,
            sdpMLineIndex: signal.payload.sdpMLineIndex,
            usernameFragment: signal.payload.usernameFragment ?? null,
          });
          return;
        }
        await peer.addIceCandidate({
          candidate: signal.payload.candidate,
          sdpMid: signal.payload.sdpMid,
          sdpMLineIndex: signal.payload.sdpMLineIndex,
          usernameFragment: signal.payload.usernameFragment ?? null,
        });
        if (!this.isCurrent(lifecycle, peer)) return;
        this.markSignalProcessed(key);
        return;
      case "ice-complete":
        if (signal.generation < this.iceGeneration) {
          this.markSignalProcessed(key);
          return;
        }
        if (this.remoteDescriptionGeneration !== signal.generation) {
          this.pendingEndOfCandidates.set(signal.generation, key);
          return;
        }
        await peer.addIceCandidate(null);
        if (!this.isCurrent(lifecycle, peer)) return;
        this.markSignalProcessed(key);
        return;
      case "end":
        this.markSignalProcessed(key);
        await this.stop(`failed: ${signal.payload.reason}`, false);
        return;
      case "offer":
        return;
    }
  }

  private async applyAnswer(
    signal: NormalizedSignal,
    lifecycle: number,
    peer: RTCPeerConnection,
  ): Promise<"applied" | "ignored" | "retry"> {
    // A host peer restart must not replay a second answer into a stable browser peer. Only the
    // answer for the local offer currently waiting on the wire can change the remote description.
    if (this.outstandingOfferGeneration !== signal.generation) return "ignored";
    if (peer.signalingState !== "have-local-offer") return "retry";
    if (signal.generation < this.iceGeneration) return "ignored";
    if (!this.isCurrent(lifecycle, peer)) return "ignored";
    if (signal.generation > this.iceGeneration) this.iceGeneration = signal.generation;
    await peer.setRemoteDescription({
      type: "answer",
      sdp: signal.payload.type === "answer" ? signal.payload.sdp : "",
    });
    if (!this.isCurrent(lifecycle, peer)) return "ignored";
    this.remoteDescriptionGeneration = signal.generation;
    this.outstandingOfferGeneration = null;
    for (const generation of this.pendingCandidates.keys()) {
      if (generation < signal.generation) this.pendingCandidates.delete(generation);
    }
    for (const generation of this.pendingEndOfCandidates.keys()) {
      if (generation < signal.generation) this.pendingEndOfCandidates.delete(generation);
    }
    const candidates = this.pendingCandidates.get(signal.generation) ?? [];
    this.pendingCandidates.delete(signal.generation);
    for (const pending of candidates) {
      if (!this.isCurrent(lifecycle, peer)) return "ignored";
      await peer.addIceCandidate(pending.candidate);
      if (!this.isCurrent(lifecycle, peer)) return "ignored";
      this.markSignalProcessed(pending.key);
    }
    const endKey = this.pendingEndOfCandidates.get(signal.generation);
    if (endKey !== undefined) {
      await peer.addIceCandidate(null);
      if (!this.isCurrent(lifecycle, peer)) return "ignored";
      this.markSignalProcessed(endKey);
      this.pendingEndOfCandidates.delete(signal.generation);
    }
    this.clearNegotiationTimer();
    this.publishSnapshot();
    this.completeOffer(signal.generation);
    return "applied";
  }

  private bufferCandidate(generation: number, key: string, candidate: RTCIceCandidateInit): void {
    const candidates = this.pendingCandidates.get(generation) ?? [];
    if (candidates.length >= MAX_PENDING_CANDIDATES) return;
    if (!candidates.some((pending) => pending.key === key)) candidates.push({ key, candidate });
    this.pendingCandidates.set(generation, candidates);
  }

  private sendOffer(iceRestart: boolean, lifecycle: number): Promise<void> {
    this.negotiationQueue = this.negotiationQueue.then(async () => {
      const peer = this.peer;
      if (!peer || !this.isCurrent(lifecycle, peer) || peer.signalingState === "closed") return;
      if (iceRestart) {
        this.iceGeneration += 1;
        if ("restartIce" in peer && typeof peer.restartIce === "function") peer.restartIce();
      }
      const generation = this.iceGeneration;
      this.outstandingOfferGeneration = generation;
      let resolveOfferPublication: (published: boolean) => void = () => {};
      let offerPublicationSettled = false;
      const offerPublication = new Promise<boolean>((resolve) => {
        resolveOfferPublication = resolve;
      });
      this.offerPublicationGates.set(generation, offerPublication);
      try {
        await this.applyCodecPreferences();
        const offer = await peer.createOffer({ iceRestart });
        if (!this.isCurrent(lifecycle, peer)) {
          if (this.outstandingOfferGeneration === generation) {
            this.outstandingOfferGeneration = null;
            this.completeOffer(generation);
          }
          resolveOfferPublication(false);
          offerPublicationSettled = true;
          return;
        }
        // Install the gate before setLocalDescription: browsers may emit the first ICE candidate
        // immediately after that call, and those candidates must be inserted after the offer.
        await peer.setLocalDescription(offer);
        const local = peer.localDescription;
        if (!local || local.type !== "offer") throw new Error("local offer was not installed");
        this.rememberLocalUfrag(local.sdp ?? "", generation);
        await this.publish({ type: "offer", sdp: local.sdp ?? "" }, generation, lifecycle);
        resolveOfferPublication(true);
        offerPublicationSettled = true;
        if (!this.isCurrent(lifecycle, peer)) return;
        this.clearNegotiationTimer();
        this.negotiationTimer = window.setTimeout(() => {
          if (
            (this.phase === "negotiating" || this.phase === "reconnecting") &&
            this.peer === peer &&
            this.lifecycleGeneration === lifecycle &&
            this.iceGeneration === generation
          ) {
            if (iceRestart) {
              this.completeOffer(generation);
              this.requestRecovery(lifecycle, peer, true);
            } else void this.fail("host answer timeout");
          }
        }, NEGOTIATION_TIMEOUT_MS);
      } catch (error) {
        if (this.outstandingOfferGeneration === generation) {
          this.outstandingOfferGeneration = null;
          this.completeOffer(generation);
        }
        if (!offerPublicationSettled) {
          resolveOfferPublication(false);
          offerPublicationSettled = true;
        }
        throw error;
      }
    });
    return this.negotiationQueue;
  }

  private scheduleRecovery(lifecycle: number, peer: RTCPeerConnection): void {
    if (this.restartTimer !== null) return;
    this.restartTimer = window.setTimeout(() => {
      this.restartTimer = null;
      this.requestRecovery(lifecycle, peer);
    }, DISCONNECTED_GRACE_MS);
  }

  private requestRecovery(
    lifecycle: number,
    peer: RTCPeerConnection,
    retryAfterCurrent = false,
  ): void {
    if (this.recoveryPromise !== null) {
      if (retryAfterCurrent) this.recoveryRetryRequested = true;
      return;
    }
    const recovery = this.recover(lifecycle, peer).catch(() => {});
    this.recoveryPromise = recovery;
    void recovery.finally(() => {
      if (this.recoveryPromise !== recovery) return;
      this.recoveryPromise = null;
      if (this.recoveryRetryRequested && this.isCurrent(lifecycle, peer)) {
        this.recoveryRetryRequested = false;
        this.requestRecovery(lifecycle, peer);
      } else {
        this.recoveryRetryRequested = false;
      }
    });
  }

  private waitForOfferCompletion(generation: number): Promise<void> {
    if (this.outstandingOfferGeneration !== generation) return Promise.resolve();
    return new Promise((resolve) => {
      this.offerCompletionWaiters.set(generation, resolve);
    });
  }

  private completeOffer(generation: number): void {
    const resolve = this.offerCompletionWaiters.get(generation);
    if (!resolve) return;
    this.offerCompletionWaiters.delete(generation);
    resolve();
  }

  private async recover(lifecycle: number, peer: RTCPeerConnection): Promise<void> {
    if (!this.isCurrent(lifecycle, peer) || this.phase === "ending") return;
    if (this.reconnectAttempt >= MAX_RECONNECT_ATTEMPTS) {
      await this.fail("controller connection failed");
      return;
    }
    this.reconnectAttempt += 1;
    this.phase = "reconnecting";
    this.inputEnabled = false;
    this.releaseInput();
    this.publishSnapshot();
    try {
      const iceServers = await this.options.getIceServers?.();
      if (iceServers && this.isCurrent(lifecycle, peer) && "setConfiguration" in peer) {
        peer.setConfiguration({
          iceServers: [...iceServers],
          bundlePolicy: "max-bundle",
          iceTransportPolicy: "all",
        });
      }
      await this.sendOffer(true, lifecycle);
      const generation = this.iceGeneration;
      if (this.outstandingOfferGeneration === generation) {
        await this.waitForOfferCompletion(generation);
      }
    } catch {
      if (this.isCurrent(lifecycle, peer)) await this.fail("reconnect failed");
    }
  }

  private async restorePage(lifecycle: number, peer: RTCPeerConnection | null): Promise<void> {
    let ownership: {
      owned: boolean;
      controllerEpoch?: string;
    } = { owned: true };
    if (this.options.revalidateController) {
      try {
        ownership = await this.options.revalidateController();
      } catch {
        // A restored page must not send input while ownership cannot be checked. Keep the peer
        // alive for a later reactive update instead of stealing another controller on a transient
        // control-plane outage.
        if (!peer || peer.connectionState === "closed" || peer.signalingState === "closed") {
          await this.stop("controller disconnected", false);
          this.options.onPageRestore?.();
          return;
        }
        if (peer && this.isCurrent(lifecycle, peer)) {
          this.inputEnabled = false;
          this.releaseInput();
          this.publishSnapshot();
        }
        return;
      }
    }
    if (this.phase === "ending" || this.phase === "ended" || this.phase === "failed") return;
    if (
      !peer ||
      this.peer !== peer ||
      peer.connectionState === "closed" ||
      peer.signalingState === "closed"
    ) {
      await this.stop("controller disconnected", false);
      this.options.onPageRestore?.(ownership.controllerEpoch);
      return;
    }
    if (!ownership.owned) {
      if (!this.isCurrent(lifecycle, peer)) return;
      await this.stop("controller disconnected", false);
      this.options.onPageRestore?.(ownership.controllerEpoch);
      return;
    }
    if (peer.connectionState === "failed" || peer.connectionState === "disconnected") {
      this.requestRecovery(lifecycle, peer);
    } else if (peer.connectionState === "connected") {
      this.inputEnabled =
        this.preferences.inputEnabled && this.remoteInputAllowed && this.isControlOpen();
      this.publishSnapshot();
    }
  }

  private async fail(reason: string): Promise<void> {
    if (this.phase === "ending" || this.phase === "ended" || this.phase === "failed") return;
    await this.stop(reason, true);
  }

  get controllerEpochId(): string {
    return this.controllerEpoch;
  }

  private async publish(
    payload: SignalPayload,
    generation = this.iceGeneration,
    lifecycle = this.lifecycleGeneration,
  ): Promise<void> {
    const queued = this.signalPublicationQueue.then(async () => {
      if (
        lifecycle !== this.lifecycleGeneration ||
        this.phase === "ending" ||
        this.phase === "ended" ||
        this.phase === "failed"
      ) {
        return;
      }
      assertSignalPayload(payload);
      const envelope = {
        version: PROTOCOL_VERSION,
        sessionId: this.options.sessionId,
        controllerEpoch: this.controllerEpoch,
        hostEpoch: "",
        generation,
        sequence: this.nextSequence++,
        sender: "controller" as const,
        sentAt: Date.now(),
        payload,
      };
      await this.options.operations.sendSignal({
        sessionId: this.options.sessionId,
        envelope: JSON.stringify(envelope),
      });
    });
    this.signalPublicationQueue = queued.catch(() => {});
    return queued;
  }

  private releaseInput(): void {
    if (document.pointerLockElement === this.options.video) document.exitPointerLock();
    this.pressedKeys.clear();
    if (!this.inputEnabled) this.stopGamepads();
    const channel = this.controlChannel;
    if (channel?.readyState === "open") {
      try {
        // Wire v3 is binary-only on the input lanes: the JSON form was
        // rejected host-side, so stuck keys and buttons were never released.
        const binary = this.options.binaryInput ? encodeBinaryInput({ type: "release" }) : null;
        if (binary) channel.send(binary);
        else channel.send(JSON.stringify({ type: "release" } satisfies ControlMessage));
      } catch {
        // Closing a channel is already a release boundary.
      }
    }
    this.options.video.blur();
  }

  private isControlOpen(): boolean {
    return this.controlChannel?.readyState === "open";
  }

  private viewportEncodeLimits(): { maxWidth?: number; maxHeight?: number } {
    const video = this.options.video;
    if (typeof video.getBoundingClientRect !== "function") return {};
    const rect = video.getBoundingClientRect();
    const display =
      this.displays.find((candidate) => candidate.id === this.preferences.displayId) ??
      this.displays.find((candidate) => candidate.primary) ??
      this.displays[0];
    const sourceWidth = video.videoWidth || display?.width || 0;
    const sourceHeight = video.videoHeight || display?.height || 0;
    if (!rect.width || !rect.height || !sourceWidth || !sourceHeight) return {};
    const fit =
      typeof getComputedStyle === "function" ? getComputedStyle(video).objectFit : "contain";
    if (fit === "none") return {};
    let width = rect.width;
    let height = rect.height;
    if (fit === "contain" || fit === "cover" || fit === "scale-down") {
      const xScale = rect.width / sourceWidth;
      const yScale = rect.height / sourceHeight;
      const scale = fit === "cover" ? Math.max(xScale, yScale) : Math.min(xScale, yScale);
      width = sourceWidth * scale;
      height = sourceHeight * scale;
    }
    const density = window.devicePixelRatio || 1;
    const even = (value: number) => Math.max(2, Math.ceil((value * density) / 2) * 2);
    return { maxWidth: even(width), maxHeight: even(height) };
  }

  private sendQualityLimits(): boolean {
    if (!this.isControlOpen()) return false;
    const viewport = this.viewportEncodeLimits();
    const limits: QualityLimits = { ...this.qualityLimits, ...viewport };
    if (this.qualityLimits.maxWidth)
      limits.maxWidth = Math.min(this.qualityLimits.maxWidth, viewport.maxWidth ?? Infinity);
    if (this.qualityLimits.maxHeight)
      limits.maxHeight = Math.min(this.qualityLimits.maxHeight, viewport.maxHeight ?? Infinity);
    const key = `${limits.maxFps ?? ""}:${limits.maxBitrateKbps ?? ""}:${limits.maxWidth ?? ""}:${limits.maxHeight ?? ""}`;
    if (key === this.lastViewportQualityKey) return true;
    if (limits.maxFps || limits.maxBitrateKbps || (limits.maxWidth && limits.maxHeight)) {
      this.lastViewportQualityKey = key;
      return this.sendControl({ type: "quality", ...limits });
    }
    return true;
  }

  private onControlReady(): void {
    if (!this.isControlOpen()) return;
    this.lastViewportQualityKey = "";
    this.inputEnabled = this.preferences.inputEnabled && this.remoteInputAllowed;
    const displayId = this.preferences.displayId;
    if (displayId && this.displays.some((display) => display.id === displayId))
      this.sendControl({ type: "display", displayId });
    this.sendQualityLimits();
    this.sendControl({ type: "audio", enabled: this.audioEnabled });
    this.startGamepadPolling();
    this.publishSnapshot();
  }

  private isCurrent(lifecycle: number, peer: RTCPeerConnection): boolean {
    return (
      this.lifecycleGeneration === lifecycle &&
      this.peer === peer &&
      peer.signalingState !== "closed"
    );
  }

  private clearTimers(): void {
    this.clearRecoveryTimer();
    this.clearNegotiationTimer();
    this.clearFirstFrameTimer();
    if (this.statsTimer !== null) window.clearInterval(this.statsTimer);
    this.statsTimer = null;
  }

  private clearRecoveryTimer(): void {
    if (this.restartTimer !== null) window.clearTimeout(this.restartTimer);
    this.restartTimer = null;
  }

  private clearNegotiationTimer(): void {
    if (this.negotiationTimer !== null) window.clearTimeout(this.negotiationTimer);
    this.negotiationTimer = null;
  }

  private armFirstFrameTimer(lifecycle: number, peer: RTCPeerConnection): void {
    if (this.firstFrameReceived || this.firstFrameTimer !== null) return;
    this.firstFrameTimer = window.setTimeout(() => {
      this.firstFrameTimer = null;
      if (
        this.isCurrent(lifecycle, peer) &&
        this.phase === "connected" &&
        !this.firstFrameReceived
      ) {
        void this.fail("first frame timeout");
      }
    }, FIRST_FRAME_TIMEOUT_MS);
  }

  private clearFirstFrameTimer(): void {
    if (this.firstFrameTimer !== null) window.clearTimeout(this.firstFrameTimer);
    this.firstFrameTimer = null;
  }

  private async applyCodecPreferences(): Promise<void> {
    const transceiver = this.videoTransceiver;
    if (this.codecPreferencesApplied || !transceiver) return;
    this.codecPreferencesApplied = true;
    if (typeof transceiver.setCodecPreferences !== "function") return;
    const receive = RTCRtpReceiver.getCapabilities?.("video")?.codecs;
    if (!receive || !this.options.videoCodecs) return;
    try {
      const order = await orderVideoCodecs(
        this.options.codecChoice ?? "auto",
        this.options.videoCodecs,
        receive,
      );
      if (order) transceiver.setCodecPreferences(order);
    } catch {
      // Codec ordering is an optimization; the browser's default order still negotiates H.264.
    }
  }

  private applyDirectVideoStats(dcLoss: number | undefined): void {
    const directStats = this.webCodecsVideo?.directDataChannel ? this.webCodecsVideo.stats() : null;
    if (directStats?.bytes === undefined) return;
    const base: RemoteSessionMetrics = this.metrics ?? {
      resolution: "—",
      fps: null,
      bitrateKbps: null,
      rttMs: null,
      packetsLost: 0,
      framesDropped: 0,
      lossPercent: null,
      jitterMs: null,
      route: "unknown",
      codec: null,
      decoder: null,
      decodeMs: null,
    };
    const directSample = {
      id: "direct-video",
      bytes: directStats.bytes,
      frames: directStats.decoded,
      timestamp: performance.now(),
      decodeTimeMs: directStats.decodeTimeMs,
      decodeTimedFrames: directStats.decodeTimedFrames,
    };
    const directRates = videoRates(directSample, this.previousDirectVideoSample);
    const previousDirect = this.previousDirectVideoSample;
    const decodeTimeDelta =
      directSample.decodeTimeMs !== undefined && previousDirect?.decodeTimeMs !== undefined
        ? directSample.decodeTimeMs - previousDirect.decodeTimeMs
        : 0;
    const decodeFrameDelta =
      directSample.decodeTimedFrames !== undefined &&
      previousDirect?.decodeTimedFrames !== undefined
        ? directSample.decodeTimedFrames - previousDirect.decodeTimedFrames
        : 0;
    this.previousDirectVideoSample = directSample;
    this.metrics = {
      ...base,
      resolution:
        directStats.width && directStats.height
          ? `${directStats.width}×${directStats.height}`
          : base.resolution,
      fps: directRates.fps,
      bitrateKbps: directRates.bitrateKbps,
      packetsLost: directStats.packetsLost ?? base.packetsLost,
      framesDropped: directStats.dropped,
      lossPercent: dcLoss !== undefined ? Math.round(dcLoss * 1_000) / 10 : base.lossPercent,
      codec:
        directStats.codec === "h264"
          ? "H.264"
          : directStats.codec === "hevc"
            ? "HEVC"
            : directStats.codec === "av1"
              ? "AV1"
              : base.codec,
      decoder: "WebCodecs",
      decodeMs:
        decodeFrameDelta > 0 && decodeTimeDelta >= 0
          ? Math.round((decodeTimeDelta / decodeFrameDelta) * 10) / 10
          : null,
    };
  }

  private async collectStats(lifecycle: number): Promise<void> {
    // Receiver-measured data-channel loss is the host's only congestion
    // signal while video rides the data channel — RTP feedback describes the
    // thin keepalive leg — so report it every second before any early return.
    const dcLoss = this.webCodecsVideo?.takeLinkLoss?.();
    if (dcLoss !== undefined) this.sendControl({ type: "link-stats", loss: dcLoss });
    const peer = this.peer;
    if (!peer || !this.isCurrent(lifecycle, peer) || this.statsInFlight) return;
    this.statsInFlight = true;
    try {
      const report = await peer.getStats();
      if (!this.isCurrent(lifecycle, peer)) return;
      let inbound: RTCStats | undefined;
      const nominatedPairs: RTCStats[] = [];
      report.forEach((stats) => {
        if (
          stats.type === "inbound-rtp" &&
          (statString(stats, "kind") === "video" || statString(stats, "mediaType") === "video")
        ) {
          inbound = stats;
        }
        if (
          stats.type === "candidate-pair" &&
          statString(stats, "state") === "succeeded" &&
          statBoolean(stats, "nominated")
        ) {
          nominatedPairs.push(stats);
        }
      });
      if (!inbound) {
        // Safari can omit inbound RTP stats while the thin RTP keepalive leg is
        // suppressed. Direct data-channel video has its own byte/frame counters
        // and must not depend on that RTP stats object existing.
        this.applyDirectVideoStats(dcLoss);
        this.publishSnapshot();
        return;
      }
      const sample: VideoSample = {
        id: inbound.id,
        bytes: statNumber(inbound, "bytesReceived") ?? 0,
        frames: statNumber(inbound, "framesDecoded"),
        timestamp: inbound.timestamp,
      };
      const rates = videoRates(
        sample,
        this.previousVideoSample,
        statNumber(inbound, "framesPerSecond"),
      );
      this.previousVideoSample = sample;
      const transportId = statString(inbound, "transportId");
      const transport = transportId ? report.get(transportId) : undefined;
      const selectedPairId = statString(transport, "selectedCandidatePairId");
      const selectedPair = selectedPairId ? report.get(selectedPairId) : undefined;
      const candidatePair =
        selectedPair?.type === "candidate-pair"
          ? selectedPair
          : nominatedPairs.length === 1
            ? nominatedPairs[0]
            : undefined;
      const localCandidateId = statString(candidatePair, "localCandidateId");
      const remoteCandidateId = statString(candidatePair, "remoteCandidateId");
      const localType = statString(
        localCandidateId ? report.get(localCandidateId) : undefined,
        "candidateType",
      );
      const remoteType = statString(
        remoteCandidateId ? report.get(remoteCandidateId) : undefined,
        "candidateType",
      );
      const route =
        localType === "relay" || remoteType === "relay"
          ? "relay"
          : localType && remoteType
            ? "direct"
            : "unknown";
      const rtt = statNumber(candidatePair, "currentRoundTripTime");
      const codecId = statString(inbound, "codecId");
      const decodeTime = statNumber(inbound, "totalDecodeTime");
      const decodedFrames = statNumber(inbound, "framesDecoded");
      const previousDecode = this.previousDecode;
      const decodeMs =
        decodeTime !== undefined &&
        decodedFrames !== undefined &&
        previousDecode?.id === inbound.id &&
        decodedFrames > previousDecode.frames
          ? ((decodeTime - previousDecode.time) * 1_000) / (decodedFrames - previousDecode.frames)
          : null;
      this.previousDecode =
        decodeTime !== undefined && decodedFrames !== undefined
          ? { id: inbound.id, time: decodeTime, frames: decodedFrames }
          : null;
      const lost = statNumber(inbound, "packetsLost");
      const received = statNumber(inbound, "packetsReceived");
      const previousLoss = this.previousLoss;
      const lostDelta =
        lost !== undefined && previousLoss?.id === inbound.id ? lost - previousLoss.lost : 0;
      const receivedDelta =
        received !== undefined && previousLoss?.id === inbound.id
          ? received - previousLoss.received
          : 0;
      const lossPercent =
        receivedDelta + lostDelta > 0
          ? Math.round((Math.max(0, lostDelta) / (receivedDelta + lostDelta)) * 1_000) / 10
          : null;
      this.previousLoss =
        lost !== undefined && received !== undefined ? { id: inbound.id, lost, received } : null;
      const jitter = statNumber(inbound, "jitter");
      this.metrics = {
        resolution:
          (statNumber(inbound, "frameWidth") ?? 0) > 0 &&
          (statNumber(inbound, "frameHeight") ?? 0) > 0
            ? `${statNumber(inbound, "frameWidth")}×${statNumber(inbound, "frameHeight")}`
            : "—",
        ...rates,
        rttMs: rtt !== undefined && rtt > 0 ? Math.round(rtt * 1_000) : null,
        packetsLost: Math.max(0, Math.round(statNumber(inbound, "packetsLost") ?? 0)),
        framesDropped: Math.max(0, Math.round(statNumber(inbound, "framesDropped") ?? 0)),
        lossPercent,
        jitterMs: jitter !== undefined && jitter >= 0 ? Math.round(jitter * 10_000) / 10 : null,
        route,
        codec: codecLabel(statString(codecId ? report.get(codecId) : undefined, "mimeType")),
        decoder: statString(inbound, "decoderImplementation") ?? null,
        decodeMs: decodeMs !== null && decodeMs >= 0 ? Math.round(decodeMs * 10) / 10 : null,
      };
      this.applyDirectVideoStats(dcLoss);
      this.publishSnapshot();
    } catch {
      // Stats are advisory. A browser can reject the query while the media path is healthy.
    } finally {
      this.statsInFlight = false;
    }
  }

  private rememberLocalUfrag(sdp: string, generation: number): void {
    const match = /(?:^|\r?\n)a=ice-ufrag:([^\r\n]+)/.exec(sdp);
    if (match?.[1]) this.localUfragGenerations.set(match[1], generation);
  }

  private publishSnapshot(): void {
    this.options.onSnapshot(this.snapshot);
  }

  private markSignalProcessed(key: string): void {
    if (this.processedSignals.has(key)) return;
    this.processedSignals.add(key);
    this.processedSignalOrder.push(key);
    while (this.processedSignalOrder.length > 4_096) {
      const oldest = this.processedSignalOrder.shift();
      if (oldest) this.processedSignals.delete(oldest);
    }
  }
}

/**
 * Normalizes a wheel delta to CSS pixels. Trackpads emit fine-grained
 * deltas that must not be rounded away, and line/page delta modes need
 * conversion before the host can map pixels to wheel units.
 */
export function wheelPixels(delta: number, deltaMode: number): number {
  if (!Number.isFinite(delta)) return 0;
  const pixels = deltaMode === 1 ? (delta * 100) / 3 : deltaMode === 2 ? delta * 800 : delta;
  return Math.max(-4_000, Math.min(4_000, pixels));
}

function safeSequenceSeed(): number {
  const values = new Uint32Array(2);
  if (typeof globalThis.crypto?.getRandomValues === "function") {
    globalThis.crypto.getRandomValues(values);
    const seed = ((values[0] ?? 0) & 0x001f_ffff) * 0x1_0000_0000 + (values[1] ?? 0);
    return Math.max(1, Math.min(Number.MAX_SAFE_INTEGER - 4_096, seed));
  }
  return Math.max(1, Math.floor(Math.random() * (Number.MAX_SAFE_INTEGER - 4_096)));
}

export function createControllerEpoch(): string {
  const runtimeCrypto = globalThis.crypto;
  if (runtimeCrypto && typeof runtimeCrypto.randomUUID === "function") {
    return runtimeCrypto.randomUUID();
  }
  return `epoch-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

/**
 * Returns the answer's video codec when the low-latency data-channel decode
 * path can carry it. Reads the first rtpmap in the video media section of the
 * remote description; null when it is missing, unreadable, or unsupported
 * (safe fallback: the RTP path keeps working untouched).
 */
function negotiatedDirectVideoCodec(peer: RTCPeerConnection): DirectVideoCodec | null {
  const sdp = peer.remoteDescription?.sdp;
  if (!sdp) return null;
  const start = sdp.indexOf("m=video");
  if (start === -1) return null;
  const videoSection = sdp.slice(start);
  const end = videoSection.indexOf("\nm=", "m=video".length);
  const section = end === -1 ? videoSection : videoSection.slice(0, end);
  const codec = /a=rtpmap:\d+ ([A-Za-z0-9-]+)\//.exec(section)?.[1]?.toUpperCase();
  if (codec === "H264") return "h264";
  if (codec === "H265" || codec === "HEVC") return "hevc";
  if (codec === "AV1") return "av1";
  return null;
}

function statValue(stats: RTCStats | undefined, key: string): unknown {
  return stats ? (stats as unknown as Record<string, unknown>)[key] : undefined;
}

function statNumber(stats: RTCStats | undefined, key: string): number | undefined {
  const value = statValue(stats, key);
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function statString(stats: RTCStats | undefined, key: string): string | undefined {
  const value = statValue(stats, key);
  return typeof value === "string" ? value : undefined;
}

function statBoolean(stats: RTCStats | undefined, key: string): boolean {
  return statValue(stats, key) === true;
}
