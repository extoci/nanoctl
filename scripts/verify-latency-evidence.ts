import { readFile } from "node:fs/promises";

// Interim regression ceilings for the reference loopback scene. The ADR release
// targets are tighter (1080p60 mean <= 30 ms, p95 <= 50 ms); these ceilings are
// deliberately loose enough for shared CI desktops while still rejecting the
// multi-hundred-millisecond regressions this rewrite is meant to prevent.
const minimumPresentedFps = 30;
const maximumVideoP95Ms = 120;
const maximumInputP95Ms = 150;

type Summary = {
  count?: unknown;
  mean?: unknown;
  p50?: unknown;
  p95?: unknown;
  p99?: unknown;
  max?: unknown;
  samplesMs?: unknown;
};

type Evidence = {
  schemaVersion?: unknown;
  platform?: unknown;
  arch?: unknown;
  chromium?: unknown;
  encoder?: unknown;
  quality?: unknown;
  videoSize?: unknown;
  durationMs?: unknown;
  clockUncertaintyMs?: unknown;
  fps?: unknown;
  video?: unknown;
  input?: unknown;
};

function finite(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function validateSummary(label: string, value: unknown, minimumCount: number): string[] {
  const errors: string[] = [];
  if (typeof value !== "object" || value === null) return [`${label}: missing summary`];
  const summary = value as Summary;
  if (!Number.isSafeInteger(summary.count) || Number(summary.count) < minimumCount) {
    errors.push(`${label}: fewer than ${minimumCount} samples`);
  }
  for (const field of ["mean", "p50", "p95", "p99", "max"] as const) {
    if (!finite(summary[field]) || Number(summary[field]) < 0)
      errors.push(`${label}: invalid ${field}`);
  }
  if (!Array.isArray(summary.samplesMs)) {
    errors.push(`${label}: raw samplesMs missing`);
    return errors;
  }
  const samples = summary.samplesMs;
  if (samples.length !== summary.count)
    errors.push(`${label}: samplesMs length does not match count`);
  if (!samples.every(finite) || samples.some((sample) => sample < 0)) {
    errors.push(`${label}: raw samples contain invalid values`);
    return errors;
  }
  const sorted = [...samples].sort((a, b) => a - b);
  const quantile = (p: number) => sorted[Math.ceil(sorted.length * p) - 1];
  const mean = sorted.reduce((sum, sample) => sum + sample, 0) / sorted.length;
  const close = (actual: unknown, expected: number) =>
    finite(actual) && Math.abs(actual - expected) < 0.001;
  if (!close(summary.mean, mean)) errors.push(`${label}: mean does not match raw samples`);
  if (!close(summary.p50, quantile(0.5))) errors.push(`${label}: p50 does not match raw samples`);
  if (!close(summary.p95, quantile(0.95))) errors.push(`${label}: p95 does not match raw samples`);
  if (!close(summary.p99, quantile(0.99))) errors.push(`${label}: p99 does not match raw samples`);
  if (!close(summary.max, sorted[sorted.length - 1]))
    errors.push(`${label}: max does not match raw samples`);
  return errors;
}

export function validateLatencyEvidence(record: unknown): string[] {
  const errors: string[] = [];
  if (typeof record !== "object" || record === null)
    return ["latency evidence: expected a JSON object"];
  const evidence = record as Evidence;
  if (evidence.schemaVersion !== 1) errors.push("unsupported schemaVersion");
  for (const field of ["platform", "arch", "chromium", "encoder"] as const) {
    if (typeof evidence[field] !== "string" || evidence[field] === "")
      errors.push(`missing ${field}`);
  }
  const quality = evidence.quality as
    | { maxWidth?: unknown; maxHeight?: unknown; maxFps?: unknown }
    | undefined;
  if (quality?.maxWidth !== 1920 || quality.maxHeight !== 1080 || quality.maxFps !== 60) {
    errors.push("latency evidence is not the 1920x1080p60 reference profile");
  }
  const size = evidence.videoSize as { width?: unknown; height?: unknown } | undefined;
  if (!finite(size?.width) || !finite(size.height) || size.width! < 2 || size.height! < 2) {
    errors.push("invalid decoded video size");
  } else if (size.width > 1920 || size.height > 1080) {
    errors.push(`decoded video exceeds reference cap: ${size.width}x${size.height}`);
  }
  if (!finite(evidence.durationMs) || evidence.durationMs < 8_000)
    errors.push("sample duration below 8 seconds");
  if (!finite(evidence.clockUncertaintyMs) || evidence.clockUncertaintyMs >= 10) {
    errors.push("clock uncertainty is missing or too large");
  }
  if (!finite(evidence.fps) || evidence.fps < minimumPresentedFps) {
    errors.push(`presented fps below regression floor ${minimumPresentedFps}`);
  }
  errors.push(...validateSummary("video", evidence.video, 100));
  errors.push(...validateSummary("input", evidence.input, 10));
  const video = evidence.video as Summary | undefined;
  const input = evidence.input as Summary | undefined;
  if (finite(video?.p95) && video.p95 > maximumVideoP95Ms) {
    errors.push(
      `video p95 ${video.p95.toFixed(1)} ms exceeds regression ceiling ${maximumVideoP95Ms} ms`,
    );
  }
  if (finite(input?.p95) && input.p95 > maximumInputP95Ms) {
    errors.push(
      `input p95 ${input.p95.toFixed(1)} ms exceeds regression ceiling ${maximumInputP95Ms} ms`,
    );
  }
  return errors;
}

async function main(): Promise<void> {
  const paths = Bun.argv.slice(2);
  if (paths.length === 0) throw new Error("usage: bun run evidence:latency -- <latency.json>...");
  let failed = false;
  for (const path of paths) {
    const errors = validateLatencyEvidence(JSON.parse(await readFile(path, "utf8")));
    if (errors.length > 0) {
      failed = true;
      for (const error of errors) console.error(`${path}: ${error}`);
    } else {
      console.log(`${path}: latency evidence pass`);
    }
  }
  if (failed) process.exitCode = 2;
}

if (import.meta.main) await main();
