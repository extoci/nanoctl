import type { HostInfo } from "@nanoctl/protocol";
import type { RemoteSessionMetrics, RemoteSessionSnapshot } from "./remote-session";
import { browserDecodeSupport, type DecodeSupport } from "./video-codec";

export type DebugEvent = { at: string; event: string; detail?: string };
export type MetricsSample = { at: string } & RemoteSessionMetrics;

const MAX_EVENTS = 300;
const MAX_SAMPLES = 120;
const events: DebugEvent[] = [];
const samples: MetricsSample[] = [];

function push<T>(list: T[], item: T, limit: number): void {
  list.push(item);
  if (list.length > limit) list.splice(0, list.length - limit);
}

/** Records a viewer lifecycle event for "Copy debug info"; keeps the most recent 300. */
export function recordDebugEvent(event: string, detail?: string): void {
  push(events, { at: new Date().toISOString(), event, ...(detail ? { detail } : {}) }, MAX_EVENTS);
}

/** Keeps the last two minutes of one-second stats samples. */
export function recordMetrics(metrics: RemoteSessionMetrics): void {
  push(samples, { at: new Date().toISOString(), ...metrics }, MAX_SAMPLES);
}

export function resetDebugLog(): void {
  events.length = 0;
  samples.length = 0;
}

export type DebugReportInput = {
  info: HostInfo | null;
  snapshot: RemoteSessionSnapshot;
  socketStatus: string;
  socketDetail: string;
  codecChoice: string;
  /** Host report from /v1/debug, or why it is unavailable. */
  hostReport: string;
  decodeSupport?: DecodeSupport;
};

const METRIC_COLUMNS = [
  "resolution",
  "fps",
  "bitrateKbps",
  "rttMs",
  "jitterMs",
  "lossPercent",
  "packetsLost",
  "framesDropped",
  "decodeMs",
  "route",
  "codec",
  "decoder",
] as const;

function cell(value: unknown): string {
  return value == null ? "" : String(value).replaceAll("|", "/");
}

/** A Markdown bundle of viewer and host state, built to paste into a coding agent. */
export async function buildDebugReport(input: DebugReportInput): Promise<string> {
  const decode = input.decodeSupport ?? browserDecodeSupport;
  const codecs = await Promise.all(
    (["av1", "hevc", "h264"] as const).map(async (codec) => {
      const result = await decode(codec).catch(() => null);
      return `${codec}: ${result ? `supported=${result.supported} hardware=${result.powerEfficient}` : "unknown"}`;
    }),
  );
  const { snapshot, info } = input;
  const lines = [
    "# nanoctl viewer debug info",
    "",
    `Generated ${new Date().toISOString()}. Contains tailnet names and logins; review before sharing publicly.`,
    "",
    "## Viewer",
    "",
    `- url: ${location.origin}${location.pathname}`,
    `- user agent: ${navigator.userAgent}`,
    `- secure context: ${window.isSecureContext}`,
    `- screen: ${screen.width}×${screen.height} @${window.devicePixelRatio}x`,
    `- cores: ${navigator.hardwareConcurrency ?? "unknown"}`,
    `- codec choice: ${input.codecChoice}`,
    `- WebRTC decode: ${codecs.join("; ")}`,
    "",
    "## Session",
    "",
    `- socket: ${input.socketStatus}${input.socketDetail ? ` (${input.socketDetail})` : ""}`,
    `- phase: ${snapshot.phase}${snapshot.reason ? ` (${snapshot.reason})` : ""}`,
    `- first frame: ${snapshot.firstFrameReceived}`,
    `- reconnect attempt: ${snapshot.reconnectAttempt}; ICE generation: ${snapshot.iceGeneration}`,
    `- input: ${snapshot.inputEnabled}; input blocked by host OS: ${snapshot.inputBlocked}`,
    `- host: ${info ? `${info.hostName} ${info.platform} v${info.version}` : "unknown"}`,
    `- host codecs: ${info?.capabilities.codecs?.join(", ") ?? "unknown"}; encoders: ${info?.capabilities.encoderBackends?.join(", ") ?? "unknown"}`,
    "",
    "## Stats (1 s samples, newest last)",
    "",
  ];
  if (samples.length) {
    lines.push(
      `| time | ${METRIC_COLUMNS.join(" | ")} |`,
      `|${" --- |".repeat(METRIC_COLUMNS.length + 1)}`,
      ...samples.map(
        (sample) =>
          `| ${sample.at.slice(11, 19)} | ${METRIC_COLUMNS.map((key) => cell(sample[key])).join(" | ")} |`,
      ),
    );
  } else {
    lines.push("No stats samples yet.");
  }
  lines.push("", "## Viewer events", "", "```text");
  lines.push(
    ...(events.length
      ? events.map((entry) => `${entry.at} ${entry.event}${entry.detail ? ` ${entry.detail}` : ""}`)
      : ["(none)"]),
  );
  lines.push("```", "", "## Host report", "", input.hostReport.trim(), "");
  return lines.join("\n");
}

/** Fetches the host report; failures become text in the bundle rather than aborting it. */
export async function fetchHostReport(fetcher: typeof fetch = fetch): Promise<string> {
  try {
    const response = await fetcher("/v1/debug", { cache: "no-store" });
    const text = await response.text();
    return response.ok ? text : `Host report unavailable (${response.status}): ${text.trim()}`;
  } catch (error) {
    return `Host report unavailable: ${error instanceof Error ? error.message : String(error)}`;
  }
}
