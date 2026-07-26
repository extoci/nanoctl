export const PROTOCOL_VERSION = 1 as const;

export const SESSION_STATES = [
  "requested",
  "ringing",
  "negotiating",
  "connected",
  "ended",
  "failed",
] as const;

export type SessionState = (typeof SESSION_STATES)[number];
export type SessionRole = "controller" | "host";

export type SignalEnvelope = {
  readonly version: typeof PROTOCOL_VERSION;
  readonly sessionId: string;
  readonly sequence: number;
  readonly sender: SessionRole;
  readonly sentAt: number;
  readonly payload: SignalPayload;
};

export type SignalPayload =
  | { readonly type: "offer"; readonly sdp: string }
  | { readonly type: "answer"; readonly sdp: string }
  | {
      readonly type: "ice-candidate";
      readonly candidate: string;
      readonly sdpMid: string | null;
      readonly sdpMLineIndex: number | null;
    }
  | { readonly type: "ice-complete" }
  | { readonly type: "renegotiate"; readonly reason: string }
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
  | { readonly type: "ping"; readonly nonce: number; readonly sentAt: number }
  | { readonly type: "pong"; readonly nonce: number; readonly sentAt: number };

export type DeviceCapabilities = {
  readonly protocolVersion: typeof PROTOCOL_VERSION;
  readonly platform: "windows" | "macos" | "linux";
  readonly architecture: "x64" | "arm64";
  readonly displays: readonly DisplayInfo[];
  readonly codecs: readonly VideoCodec[];
  readonly input: boolean;
  readonly clipboard: boolean;
  readonly systemAudio: boolean;
};

export type DisplayInfo = {
  readonly id: string;
  readonly name: string;
  readonly width: number;
  readonly height: number;
  readonly scaleFactor: number;
  readonly primary: boolean;
};

export type VideoCodec = "h264" | "vp9" | "av1";

export type SessionPreferences = {
  readonly codec: VideoCodec | "auto";
  readonly maxFps: 30 | 60 | 90 | 120;
  readonly maxBitrateKbps: number;
  readonly maxWidth: number;
  readonly maxHeight: number;
  readonly enableAudio: boolean;
  readonly enableClipboard: boolean;
  readonly quality: "responsiveness" | "balanced" | "quality";
};

const MAX_SDP_BYTES = 1_000_000;
const MAX_CANDIDATE_BYTES = 8_192;

export function assertSignalEnvelope(value: unknown): asserts value is SignalEnvelope {
  if (!isRecord(value)) throw new Error("signal must be an object");
  if (value.version !== PROTOCOL_VERSION) throw new Error("unsupported protocol version");
  if (!isBoundedString(value.sessionId, 1, 128)) throw new Error("invalid session id");
  if (!Number.isSafeInteger(value.sequence) || Number(value.sequence) < 0) {
    throw new Error("invalid signal sequence");
  }
  if (value.sender !== "controller" && value.sender !== "host") {
    throw new Error("invalid signal sender");
  }
  if (!Number.isSafeInteger(value.sentAt) || Number(value.sentAt) <= 0) {
    throw new Error("invalid signal timestamp");
  }
  assertSignalPayload(value.payload);
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
      if (value.sdpMid !== null && typeof value.sdpMid !== "string") {
        throw new Error("invalid sdpMid");
      }
      if (
        value.sdpMLineIndex !== null &&
        (!Number.isInteger(value.sdpMLineIndex) || Number(value.sdpMLineIndex) < 0)
      ) {
        throw new Error("invalid sdpMLineIndex");
      }
      return;
    case "ice-complete":
      return;
    case "renegotiate":
    case "end":
      if (!isBoundedString(value.reason, 1, 512)) throw new Error("invalid reason");
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
