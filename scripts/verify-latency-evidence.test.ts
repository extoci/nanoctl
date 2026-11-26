import { expect, test } from "bun:test";
import { validateLatencyEvidence } from "./verify-latency-evidence";

function summary(samples: number[]) {
  const sorted = [...samples].sort((a, b) => a - b);
  const quantile = (p: number) => sorted[Math.ceil(sorted.length * p) - 1];
  return {
    count: sorted.length,
    mean: sorted.reduce((sum, sample) => sum + sample, 0) / sorted.length,
    p50: quantile(0.5),
    p95: quantile(0.95),
    p99: quantile(0.99),
    max: sorted[sorted.length - 1],
    samplesMs: samples,
  };
}

function evidence() {
  return {
    schemaVersion: 1,
    platform: "linux",
    arch: "x64",
    chromium: "test",
    encoder: "software",
    quality: { maxWidth: 1920, maxHeight: 1080, maxFps: 60 },
    videoSize: { width: 1920, height: 1080 },
    durationMs: 9_000,
    clockUncertaintyMs: 1,
    fps: 60,
    video: summary(Array.from({ length: 100 }, () => 40)),
    input: summary(Array.from({ length: 12 }, () => 60)),
  };
}

test("accepts raw regression evidence inside the interim ceilings", () => {
  expect(validateLatencyEvidence(evidence())).toEqual([]);
});

test("rejects missing raw samples and summary tampering", () => {
  const record = evidence();
  record.video.samplesMs = [];
  expect(validateLatencyEvidence(record).join("\n")).toContain("video: samplesMs length");
});

test("rejects catastrophic video and input latency regressions", () => {
  const record = evidence();
  record.video = summary(Array.from({ length: 100 }, () => 121));
  record.input = summary(Array.from({ length: 12 }, () => 151));
  const errors = validateLatencyEvidence(record).join("\n");
  expect(errors).toContain("video p95");
  expect(errors).toContain("input p95");
});

test("rejects output above the reference resolution cap", () => {
  const record = evidence();
  record.videoSize.width = 1922;
  expect(validateLatencyEvidence(record).join("\n")).toContain("exceeds reference cap");
});
