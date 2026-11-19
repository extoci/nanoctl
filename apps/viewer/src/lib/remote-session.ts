import {
  MAX_CONTROL_MESSAGE_BYTES,
  PROTOCOL_VERSION,
  assertSignalPayload,
  normalizeSignalEnvelope,
  type ControlMessage,
  type DisplayInfo,
  type NormalizedSignal,
  type SignalPayload,
} from "@nanoctl/protocol";
import { reliableControlBufferIsSaturated } from "./control-backpressure";
import { ClipboardSync } from "./clipboard-sync";
import { videoRates, type VideoSample } from "./video-rates";

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
  rttMs: number;
  packetsLost: number;
  framesDropped: number;
  route: "direct" | "relay" | "unknown";
};

export type RemoteSessionSnapshot = {
  phase: RemoteSessionPhase;
  reason?: string;
  reconnectAttempt: number;
  iceGeneration: number;
  inputEnabled: boolean;
  firstFrameReceived: boolean;
  metrics: RemoteSessionMetrics | null;
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

export type RemoteSessionOptions = {
  sessionId: string;
  controllerEpoch: string;
  displays: readonly DisplayInfo[];
  iceServers: readonly RTCIceServer[];
  video: HTMLVideoElement;
  remoteInputEnabled?: boolean;
  clipboardEnabled?: boolean;
  /** An immutable browser-side policy can further restrict host capabilities. */
  remoteInputPolicy?: boolean;
  operations: RemoteSessionOperations;
  getIceServers?: () => Promise<readonly RTCIceServer[]>;
  revalidateController?: () => Promise<{
    owned: boolean;
    controllerEpoch?: string;
  }>;
  onPageRestore?: (controllerEpoch?: string) => void;
  onSnapshot: (snapshot: RemoteSessionSnapshot) => void;
  peerFactory?: (configuration: RTCConfiguration) => RTCPeerConnection;
};

const NEGOTIATION_TIMEOUT_MS = 20_000;
const FIRST_FRAME_TIMEOUT_MS = 15_000;
const DISCONNECTED_GRACE_MS = 1_500;
const MAX_RECONNECT_ATTEMPTS = 3;
const STATS_INTERVAL_MS = 1_000;
const KEEPALIVE_INTERVAL_MS = 1_000;
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
  private pointerChannel: RTCDataChannel | null = null;
  private controlChannel: RTCDataChannel | null = null;
  private clipboardChannel: RTCDataChannel | null = null;
  private clipboard: ClipboardSync | null = null;
  private keyQueue: Promise<void> = Promise.resolve();
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
  private firstFrameReceived = false;
  private stopPromise: Promise<void> | null = null;
  private signalQueue: Promise<void> = Promise.resolve();
  private signalPublicationQueue: Promise<void> = Promise.resolve();
  private negotiationQueue: Promise<void> = Promise.resolve();
  private outstandingOfferGeneration: number | null = null;
  private restartTimer: number | null = null;
  private negotiationTimer: number | null = null;
  private firstFrameTimer: number | null = null;
  private keepaliveTimer: number | null = null;
  private statsTimer: number | null = null;
  private recoveryPromise: Promise<void> | null = null;
  private recoveryRetryRequested = false;
  private notifiedEnd = false;
  private statsInFlight = false;
  private previousVideoSample: VideoSample | null = null;
  private readonly localUfragGenerations = new Map<string, number>();
  private readonly offerPublicationGates = new Map<number, Promise<boolean>>();
  private readonly offerCompletionWaiters = new Map<number, () => void>();
  private metrics: RemoteSessionMetrics | null = null;

  private readonly onWindowBlur = () => this.releaseInput();
  private readonly onVisibilityChange = () => {
    if (document.visibilityState === "hidden") this.releaseInput();
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
    this.publishSnapshot();
  };

  constructor(options: RemoteSessionOptions) {
    this.options = options;
    this.controllerEpoch = options.controllerEpoch;
    this.remoteInputPolicy = options.remoteInputPolicy !== false;
    this.displays = options.displays;
    this.remoteInputAllowed = this.remoteInputPolicy && options.remoteInputEnabled !== false;
    // Keep sequence numbers fresh and exactly representable by JavaScript clients.
    this.nextSequence = safeSequenceSeed();
    this.peerFactory =
      options.peerFactory ?? ((configuration) => new RTCPeerConnection(configuration));
  }

  get snapshot(): RemoteSessionSnapshot {
    return {
      phase: this.phase,
      ...(this.reason ? { reason: this.reason } : {}),
      reconnectAttempt: this.reconnectAttempt,
      iceGeneration: this.iceGeneration,
      inputEnabled: this.inputEnabled,
      firstFrameReceived: this.firstFrameReceived,
      metrics: this.metrics,
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
    this.keepaliveTimer = window.setInterval(() => {
      this.sendControl({ type: "ping", nonce: Date.now(), sentAt: Date.now() });
    }, KEEPALIVE_INTERVAL_MS);
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
    this.inputEnabled = enabled && this.remoteInputAllowed && this.isControlOpen();
    if (!enabled) this.releaseInput();
    else this.options.video.focus();
    this.publishSnapshot();
  }

  selectDisplay(displayId: string): boolean {
    if (!this.inputEnabled || !this.isControlOpen()) return false;
    if (!this.displays.some((display) => display.id === displayId)) return false;
    return this.sendControl({ type: "display", displayId });
  }

  sendControl(payload: ControlMessage): boolean {
    const channel =
      payload.type === "pointer" && payload.action === "move"
        ? this.pointerChannel
        : this.controlChannel;
    if (!channel || channel.readyState !== "open") return false;
    if (
      !this.inputEnabled &&
      (payload.type === "pointer" || payload.type === "key" || payload.type === "display")
    ) {
      return false;
    }
    const isMotion = payload.type === "pointer" && payload.action === "move";
    const serialized = JSON.stringify(payload);
    if (new TextEncoder().encode(serialized).byteLength > MAX_CONTROL_MESSAGE_BYTES) return false;
    if (isMotion && channel.bufferedAmount > 64 * 1024) return false;
    if (!isMotion && reliableControlBufferIsSaturated(channel.bufferedAmount)) {
      this.inputEnabled = false;
      this.releaseInput();
      this.publishSnapshot();
      void this.fail("control channel stalled");
      return false;
    }
    try {
      channel.send(serialized);
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
      this.releaseInput();
      this.detachInput();
      if (peer) {
        peer.onconnectionstatechange = null;
        peer.onicecandidate = null;
        peer.ontrack = null;
        this.options.video.removeEventListener("loadeddata", this.onVideoLoadedData);
        this.pointerChannel?.close();
        this.controlChannel?.close();
        this.clipboardChannel?.close();
        this.clipboardChannel = null;
        this.clipboard = null;
        this.pointerChannel = null;
        this.controlChannel = null;
        peer.close();
      }
      this.peer = null;
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
    if (this.options.clipboardEnabled && this.remoteInputPolicy) {
      const channel = peer.createDataChannel("nanoctl.clipboard.v1", { ordered: true });
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
    this.controlChannel = peer.createDataChannel("nanoctl.control.v1", {
      ordered: true,
    });
    this.inputEnabled = this.remoteInputAllowed && this.controlChannel.readyState === "open";
    this.controlChannel.onopen = () => {
      if (!this.isCurrent(lifecycle, peer)) return;
      this.inputEnabled = this.remoteInputAllowed;
      this.publishSnapshot();
    };
    this.options.video.addEventListener("loadeddata", this.onVideoLoadedData);
    peer.addTransceiver("video", { direction: "recvonly" });
    peer.ontrack = ({ receiver, streams, track }) => {
      if (!this.isCurrent(lifecycle, peer)) return;
      if ("playoutDelayHint" in receiver) {
        (receiver as RTCRtpReceiver & { playoutDelayHint?: number }).playoutDelayHint = 0;
      }
      const stream = streams[0] ?? new MediaStream([track]);
      this.options.video.srcObject = stream;
      void this.options.video.play().catch(() => {});
    };
    peer.onconnectionstatechange = () => {
      if (!this.isCurrent(lifecycle, peer)) return;
      switch (peer.connectionState) {
        case "connected":
          this.reconnectAttempt = 0;
          this.clearRecoveryTimer();
          this.clearNegotiationTimer();
          this.phase = "connected";
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
    video.addEventListener("pointermove", this.onPointer);
    video.addEventListener("pointerdown", this.onPointer);
    video.addEventListener("pointerup", this.onPointer);
    video.addEventListener("pointercancel", this.onPointerCancel);
    video.addEventListener("lostpointercapture", this.onPointerCancel);
    video.addEventListener("wheel", this.onWheel, { passive: false });
    video.addEventListener("contextmenu", this.onContextMenu);
    video.addEventListener("keydown", this.onKey);
    video.addEventListener("keyup", this.onKey);
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
    video.removeEventListener("pointermove", this.onPointer);
    video.removeEventListener("pointerdown", this.onPointer);
    video.removeEventListener("pointerup", this.onPointer);
    video.removeEventListener("pointercancel", this.onPointerCancel);
    video.removeEventListener("lostpointercapture", this.onPointerCancel);
    video.removeEventListener("wheel", this.onWheel);
    video.removeEventListener("contextmenu", this.onContextMenu);
    video.removeEventListener("keydown", this.onKey);
    video.removeEventListener("keyup", this.onKey);
    window.removeEventListener("keydown", this.onEmergencyShortcut, true);
    window.removeEventListener("blur", this.onWindowBlur);
    window.removeEventListener("focus", this.onClipboardFocus);
    window.removeEventListener("paste", this.onPaste);
    document.removeEventListener("visibilitychange", this.onVisibilityChange);
    window.removeEventListener("pagehide", this.onPageHide);
    window.removeEventListener("pageshow", this.onPageShow);
  }

  private readonly onPointer = (event: PointerEvent): void => {
    if (!this.inputEnabled) return;
    const position = this.normalizedPosition(event);
    if (!position) return;
    if (event.type === "pointerdown") {
      event.preventDefault();
      this.options.video.focus();
      this.options.video.setPointerCapture(event.pointerId);
    }
    this.sendControl({
      type: "pointer",
      action: event.type === "pointermove" ? "move" : event.type === "pointerdown" ? "down" : "up",
      ...position,
      button: event.button === 1 || event.button === 2 ? event.button : 0,
    });
  };

  private readonly onPointerCancel = (): void => this.releaseInput();

  private readonly onWheel = (event: WheelEvent): void => {
    if (!this.inputEnabled) return;
    const position = this.normalizedPosition(event);
    if (!position) return;
    event.preventDefault();
    this.sendControl({
      type: "pointer",
      action: "wheel",
      ...position,
      deltaX: clampWheel(event.deltaX),
      deltaY: clampWheel(event.deltaY),
    });
  };

  private readonly onContextMenu = (event: MouseEvent): void => {
    if (this.inputEnabled) event.preventDefault();
  };

  private readonly onKey = (event: KeyboardEvent): void => {
    if (!this.inputEnabled) return;
    event.preventDefault();
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
    this.keyQueue = this.keyQueue.then(async () => {
      if (event.type === "keydown" && event.code === "KeyV" && (event.ctrlKey || event.metaKey))
        await this.clipboard?.focus();
      if (this.inputEnabled) this.sendControl(message);
    });
  };

  private normalizedPosition(event: MouseEvent): { x: number; y: number } | null {
    const video = this.options.video;
    if (video.videoWidth === 0 || video.videoHeight === 0) return null;
    const rect = video.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return null;
    const sourceRatio = video.videoWidth / video.videoHeight;
    const elementRatio = rect.width / rect.height;
    const renderedWidth = sourceRatio > elementRatio ? rect.width : rect.height * sourceRatio;
    const renderedHeight = sourceRatio > elementRatio ? rect.width / sourceRatio : rect.height;
    const left = rect.left + (rect.width - renderedWidth) / 2;
    const top = rect.top + (rect.height - renderedHeight) / 2;
    return {
      x: clamp01((event.clientX - left) / renderedWidth),
      y: clamp01((event.clientY - top) / renderedHeight),
    };
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
    const channel = this.controlChannel;
    if (channel?.readyState === "open") {
      try {
        channel.send(JSON.stringify({ type: "release" } satisfies ControlMessage));
      } catch {
        // Closing a channel is already a release boundary.
      }
    }
    this.options.video.blur();
  }

  private isControlOpen(): boolean {
    return this.controlChannel?.readyState === "open";
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
    if (this.keepaliveTimer !== null) window.clearInterval(this.keepaliveTimer);
    if (this.statsTimer !== null) window.clearInterval(this.statsTimer);
    this.keepaliveTimer = null;
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

  private async collectStats(lifecycle: number): Promise<void> {
    const peer = this.peer;
    if (!peer || !this.isCurrent(lifecycle, peer) || this.statsInFlight) return;
    this.statsInFlight = true;
    try {
      const report = await peer.getStats();
      if (!this.isCurrent(lifecycle, peer)) return;
      let inbound: RTCStats | undefined;
      let candidatePair: RTCStats | undefined;
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
          candidatePair = stats;
        }
      });
      if (!inbound) return;
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
      const remoteCandidateId = candidatePair
        ? statString(candidatePair, "remoteCandidateId")
        : undefined;
      const remoteCandidate = remoteCandidateId ? report.get(remoteCandidateId) : undefined;
      const route =
        statString(remoteCandidate, "candidateType") === "relay"
          ? "relay"
          : remoteCandidate
            ? "direct"
            : "unknown";
      this.metrics = {
        resolution:
          (statNumber(inbound, "frameWidth") ?? 0) > 0 &&
          (statNumber(inbound, "frameHeight") ?? 0) > 0
            ? `${statNumber(inbound, "frameWidth")}×${statNumber(inbound, "frameHeight")}`
            : "—",
        ...rates,
        rttMs: Math.round((statNumber(candidatePair, "currentRoundTripTime") ?? 0) * 1_000),
        packetsLost: Math.max(0, Math.round(statNumber(inbound, "packetsLost") ?? 0)),
        framesDropped: Math.max(0, Math.round(statNumber(inbound, "framesDropped") ?? 0)),
        route,
      };
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

function clamp01(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(1, value));
}

function clampWheel(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(-20, Math.min(20, value));
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
