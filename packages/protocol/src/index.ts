export type HostLocation = { hostName: string; dnsName: string; url: string };
export type HostsResponse = {
  self: HostLocation;
  hosts: (HostLocation & { os: string; online: boolean })[];
};

export function assertHostsResponse(value: unknown): asserts value is HostsResponse {
  const location = (v: unknown): v is HostLocation => {
    if (!v || typeof v !== "object") return false;
    const h = v as Record<string, unknown>;
    if (
      typeof h.hostName !== "string" ||
      typeof h.dnsName !== "string" ||
      typeof h.url !== "string"
    )
      return false;
    try {
      const url = new URL(h.url);
      return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password;
    } catch {
      return false;
    }
  };
  if (!value || typeof value !== "object") throw new Error("invalid hosts response");
  const response = value as Record<string, unknown>;
  if (
    !location(response.self) ||
    !Array.isArray(response.hosts) ||
    !response.hosts.every(
      (h: unknown) =>
        location(h) &&
        typeof (h as Record<string, unknown>).os === "string" &&
        typeof (h as Record<string, unknown>).online === "boolean",
    )
  )
    throw new Error("invalid hosts response");
}

/** The wire contract shared by the tailnet viewer and native agent. */
export const PROTOCOL_VERSION = 2 as const;
export const MIN_SUPPORTED_PROTOCOL_VERSION = 1 as const;

export type SupportedProtocolVersion = 1 | typeof PROTOCOL_VERSION;

export type SessionRole = "controller" | "host";

/** A monotonic ICE generation, independent of the signal sequence. */
export type IceGeneration = number;

/** Identifies one controller peer, independently of its ICE restart generation. */
export type ControllerEpoch = string;

/** Identifies one native host peer incarnation. */
export type HostEpoch = string;

export type SignalEnvelope = {
  readonly version: typeof PROTOCOL_VERSION;
  readonly sessionId: string;
  readonly controllerEpoch: ControllerEpoch;
  readonly hostEpoch: HostEpoch;
  readonly generation: IceGeneration;
  readonly sequence: number;
  readonly sender: SessionRole;
  readonly sentAt: number;
  readonly payload: SignalPayload;
};

/** The decoded shape accepted from a v1 peer or a v2 peer. */
export type WireSignalEnvelope = Omit<
  SignalEnvelope,
  "version" | "controllerEpoch" | "hostEpoch" | "generation"
> & {
  readonly version: SupportedProtocolVersion;
  /** Protocol 1 peers do not know about controller epochs. */
  readonly controllerEpoch?: ControllerEpoch;
  /** Protocol 1 peers do not know about host peer incarnations. */
  readonly hostEpoch?: HostEpoch;
  readonly generation?: IceGeneration;
};

export type SignalPayload =
  | { readonly type: "offer"; readonly sdp: string }
  | { readonly type: "answer"; readonly sdp: string }
  | {
      readonly type: "ice-candidate";
      readonly candidate: string;
      readonly sdpMid: string | null;
      readonly sdpMLineIndex: number | null;
      /** ICE ufrag lets a peer reject a candidate from an older restart. */
      readonly usernameFragment?: string | null;
    }
  | {
      readonly type: "ice-complete";
      readonly sdpMid?: string | null;
    }
  | { readonly type: "end"; readonly reason: string };

export type ControlMessage =
  | {
      readonly type: "pointer";
      readonly action: "move" | "down" | "up" | "wheel";
      readonly x: number;
      readonly y: number;
      readonly button?: 0 | 1 | 2;
      readonly deltaX?: number;
      readonly deltaY?: number;
    }
  | {
      readonly type: "key";
      readonly action: "down" | "up";
      readonly code: string;
      readonly key: string;
      readonly modifiers: number;
      readonly repeat: boolean;
    }
  | { readonly type: "display"; readonly displayId: string }
  | { readonly type: "release" }
  | { readonly type: "text"; readonly text: string }
  | { readonly type: "ping"; readonly nonce: number; readonly sentAt: number }
  | { readonly type: "pong"; readonly nonce: number; readonly sentAt: number };

export type DeviceCapabilities = {
  readonly protocolVersion: SupportedProtocolVersion;
  readonly platform: "windows" | "macos" | "linux";
  readonly architecture: "x64" | "arm64";
  readonly displays: readonly DisplayInfo[];
  readonly codecs: readonly VideoCodec[];
  readonly input: boolean;
  readonly clipboard: boolean;
  readonly systemAudio: boolean;
  /** Whether the agent can accept remote input in this capability state. */
  readonly remoteInput?: boolean;
  readonly ready?: boolean;
  readonly readiness?: ReadinessState;
};

export type ReadinessState =
  | "ready"
  | "upgrade-required"
  | "capture-permission"
  | "input-permission"
  | "no-display"
  | "encoder-unavailable"
  | "starting";

export type DisplayInfo = {
  readonly id: string;
  readonly name: string;
  readonly width: number;
  readonly height: number;
  readonly scaleFactor: number;
  readonly primary: boolean;
};

export type VideoCodec = "h264" | "vp9" | "av1";

export type HostInfo = {
  readonly hostName: string;
  readonly platform: string;
  readonly version: string;
  readonly displays: readonly DisplayInfo[];
  readonly remoteInput: boolean;
  readonly capabilities: DeviceCapabilities & { readonly encoderBackends?: readonly string[] };
  readonly viewer: {
    readonly identity: {
      readonly login: string;
      readonly displayName: string;
      readonly tags: readonly string[];
    };
    readonly mayControl: boolean;
  };
};

export type AgentClientMessage =
  | { readonly type: "signal"; readonly envelope: SignalEnvelope }
  | { readonly type: "end"; readonly reason: string };

export type AgentServerMessage =
  | { readonly type: "hello"; readonly sessionId: string; readonly host: HostInfo }
  | AgentClientMessage;

export function assertHostInfo(value: unknown): asserts value is HostInfo {
  if (
    !isRecord(value) ||
    typeof value.hostName !== "string" ||
    typeof value.platform !== "string" ||
    typeof value.version !== "string" ||
    typeof value.remoteInput !== "boolean" ||
    !Array.isArray(value.displays) ||
    !value.displays.every(isDisplayInfo) ||
    !isRecord(value.capabilities) ||
    !Array.isArray(value.capabilities.displays) ||
    !value.capabilities.displays.every(isDisplayInfo) ||
    !isRecord(value.viewer) ||
    typeof value.viewer.mayControl !== "boolean" ||
    !isRecord(value.viewer.identity) ||
    typeof value.viewer.identity.login !== "string" ||
    typeof value.viewer.identity.displayName !== "string" ||
    !Array.isArray(value.viewer.identity.tags) ||
    !value.viewer.identity.tags.every((tag) => typeof tag === "string")
  ) {
    throw new Error("invalid host info");
  }
}

export function parseAgentMessage(value: unknown): AgentServerMessage {
  if (!isRecord(value)) throw new Error("invalid agent message");
  switch (value.type) {
    case "hello":
      if (!isBoundedString(value.sessionId, 1, 128)) throw new Error("invalid session ID");
      assertHostInfo(value.host);
      return { type: "hello", sessionId: value.sessionId, host: value.host };
    case "signal": {
      const envelope = normalizeSignalEnvelope(value.envelope);
      if (!isRecord(value.envelope) || value.envelope.version !== PROTOCOL_VERSION)
        throw new Error("expected protocol 2");
      return { type: "signal", envelope };
    }
    case "end":
      if (!isBoundedString(value.reason, 1, 512)) throw new Error("invalid end reason");
      return { type: "end", reason: value.reason };
    default:
      throw new Error("unknown agent message");
  }
}

function isDisplayInfo(value: unknown): value is DisplayInfo {
  return (
    isRecord(value) &&
    typeof value.id === "string" &&
    typeof value.name === "string" &&
    typeof value.width === "number" &&
    typeof value.height === "number" &&
    typeof value.scaleFactor === "number" &&
    typeof value.primary === "boolean"
  );
}

export type NormalizedSignal = Omit<
  SignalEnvelope,
  "version" | "controllerEpoch" | "hostEpoch" | "generation"
> & {
  readonly version: typeof PROTOCOL_VERSION;
  readonly controllerEpoch: ControllerEpoch;
  readonly hostEpoch: HostEpoch;
  readonly generation: IceGeneration;
};

const MAX_SESSION_ID_BYTES = 128;
const MAX_SDP_BYTES = 1_000_000;
const MAX_CANDIDATE_BYTES = 8_192;
const MAX_REASON_BYTES = 512;
const MAX_GENERATION = 1_000_000;
const MAX_CONTROLLER_EPOCH_BYTES = 128;
export const MAX_CONTROL_MESSAGE_BYTES = 64 * 1024;

/**
 * Validate and normalize a signal at a trust seam. The returned value is safe for routing and
 * has explicit epoch/generation values even when it came from a v1 peer.
 */
export function normalizeSignalEnvelope(value: unknown): NormalizedSignal {
  if (!isRecord(value)) throw new Error("signal must be an object");
  const isV2 = value.version === PROTOCOL_VERSION;
  if (value.version !== 1 && !isV2) {
    throw new Error("unsupported protocol version");
  }
  if (!isBoundedString(value.sessionId, 1, MAX_SESSION_ID_BYTES)) {
    throw new Error("invalid session id");
  }
  if (isV2 && !isBoundedString(value.controllerEpoch, 1, MAX_CONTROLLER_EPOCH_BYTES)) {
    throw new Error("invalid controller epoch");
  }
  if (
    !isV2 &&
    value.controllerEpoch !== undefined &&
    !isBoundedString(value.controllerEpoch, 0, MAX_CONTROLLER_EPOCH_BYTES)
  ) {
    throw new Error("invalid controller epoch");
  }
  if (isV2 && !isBoundedString(value.hostEpoch, 0, MAX_CONTROLLER_EPOCH_BYTES)) {
    throw new Error("invalid host epoch");
  }
  if (
    !isV2 &&
    value.hostEpoch !== undefined &&
    !isBoundedString(value.hostEpoch, 0, MAX_CONTROLLER_EPOCH_BYTES)
  ) {
    throw new Error("invalid host epoch");
  }
  if (
    !Number.isSafeInteger(value.generation ?? (isV2 ? Number.NaN : 0)) ||
    Number(value.generation ?? 0) < 0
  ) {
    throw new Error("invalid signal generation");
  }
  if (Number(value.generation ?? 0) > MAX_GENERATION) {
    throw new Error("signal generation is too large");
  }
  if (!Number.isSafeInteger(value.sequence) || Number(value.sequence) < 0) {
    throw new Error("invalid signal sequence");
  }
  if (value.sender !== "controller" && value.sender !== "host") {
    throw new Error("invalid signal sender");
  }
  if (
    isV2 &&
    ((value.sender === "host" && (value.hostEpoch as unknown) === "") ||
      (value.sender === "controller" && value.hostEpoch !== ""))
  ) {
    throw new Error("invalid host epoch for signal sender");
  }
  if (!Number.isSafeInteger(value.sentAt) || Number(value.sentAt) <= 0) {
    throw new Error("invalid signal timestamp");
  }
  assertSignalPayload(value.payload);
  return {
    version: PROTOCOL_VERSION,
    sessionId: value.sessionId,
    controllerEpoch: typeof value.controllerEpoch === "string" ? value.controllerEpoch : "",
    hostEpoch: typeof value.hostEpoch === "string" ? value.hostEpoch : "",
    generation: Number(value.generation ?? 0),
    sequence: Number(value.sequence),
    sender: value.sender,
    sentAt: Number(value.sentAt),
    payload: value.payload,
  };
}

export function assertSignalEnvelope(value: unknown): asserts value is WireSignalEnvelope {
  normalizeSignalEnvelope(value);
}

export function assertSignalPayload(value: unknown): asserts value is SignalPayload {
  if (!isRecord(value) || typeof value.type !== "string") {
    throw new Error("invalid signal payload");
  }
  switch (value.type) {
    case "offer":
    case "answer":
      if (!isBoundedString(value.sdp, 1, MAX_SDP_BYTES)) throw new Error("invalid SDP");
      return;
    case "ice-candidate":
      if (!isBoundedString(value.candidate, 1, MAX_CANDIDATE_BYTES)) {
        throw new Error("invalid ICE candidate");
      }
      if (value.sdpMid !== null && !isBoundedString(value.sdpMid, 0, 256)) {
        throw new Error("invalid sdpMid");
      }
      if (
        value.sdpMLineIndex !== null &&
        (!Number.isInteger(value.sdpMLineIndex) ||
          Number(value.sdpMLineIndex) < 0 ||
          Number(value.sdpMLineIndex) > 65_535)
      ) {
        throw new Error("invalid sdpMLineIndex");
      }
      if (
        value.usernameFragment !== undefined &&
        value.usernameFragment !== null &&
        !isBoundedString(value.usernameFragment, 1, 256)
      ) {
        throw new Error("invalid usernameFragment");
      }
      return;
    case "ice-complete":
      if (
        value.sdpMid !== undefined &&
        value.sdpMid !== null &&
        !isBoundedString(value.sdpMid, 0, 256)
      ) {
        throw new Error("invalid ICE media section");
      }
      return;
    case "end":
      if (!isBoundedString(value.reason, 1, MAX_REASON_BYTES)) throw new Error("invalid reason");
      return;
    default:
      throw new Error("unknown signal payload");
  }
}

export function clampNormalizedCoordinate(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(1, value));
}

function isBoundedString(value: unknown, min: number, max: number): value is string {
  return typeof value === "string" && value.length >= min && value.length <= max;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
