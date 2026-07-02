import type { RTCPeerConnection } from "werift";

export type VideoCaptureConfig = {
  ffmpegInputArgs: string[];
  source: string;
};

export type AudioCaptureConfig = {
  ffmpegInputArgs: string[];
  source: string;
};

export type SessionStore = Map<string, number>;
export type ViewerStore = Map<string, RTCPeerConnection>;

export type OfferPayload = {
  type: "offer";
  sdp: string;
};

export type SessionDescriptionPayload = {
  type: "answer" | "offer";
  sdp: string;
};

export type SourceMode = "screen" | "testsrc";

export type CliOptions = {
  port?: number;
  pin?: string;
  fps?: number;
  videoBitrate?: string;
  useHwaccel?: boolean;
  source?: SourceMode;
  rtpPort?: number;
  audio?: boolean;
  audioDevice?: string;
  audioRtpPort?: number;
  control?: boolean;
  controlBridgePath?: string;
};

export type RuntimeConfig = {
  port: number;
  pin: string;
  fps: number;
  videoBitrate: string;
  useHwaccel: boolean;
  source: SourceMode;
  rtpPort: number;
  audioEnabled: boolean;
  audioDevice?: string;
  audioRtpPort: number;
  controlEnabled: boolean;
  controlBridgePath?: string;
};

export type PointerEventPayload = {
  kind: "pointer";
  action: "move" | "down" | "up" | "click";
  x: number;
  y: number;
  button?: 0 | 1 | 2;
};

export type KeyEventPayload = {
  kind: "key";
  action: "down" | "up" | "press";
  key: string;
};

export type ControlEventPayload = PointerEventPayload | KeyEventPayload;
