import { describe, expect, test } from "bun:test";
import { validatePerformanceEvidence } from "./verify-performance-evidence";

const targets = [
  ["linux", "x86_64"],
  ["linux", "aarch64"],
  ["macos", "x86_64"],
  ["macos", "aarch64"],
  ["windows", "x86_64"],
  ["windows", "aarch64"],
] as const;
const profiles = ["interactive-1080p60", "soak-1080p30"] as const;

function record(target_os: string, target_arch: string, profile: (typeof profiles)[number]) {
  return {
    schema_version: 1,
    passed: true,
    agent_version: "1.0.0",
    web_version: "1.0.0",
    target_os,
    target_arch,
    profile,
    elapsed_seconds: profile === "interactive-1080p60" ? 1_800 : 28_800,
    width: 1_920,
    height: 1_080,
    average_fps: profile === "interactive-1080p60" ? 60 : 30,
    first_frame_milliseconds: 400,
    p95_frame_milliseconds: 20,
    p99_frame_milliseconds: 30,
    p95_input_to_photon_milliseconds: 90,
    rss_start_megabytes: 100,
    rss_end_megabytes: 105,
    rss_peak_megabytes: 120,
    capture_queue_peak: 1,
    encode_queue_peak: 1,
    monotonic_memory_growth: false,
  };
}

function completeRecords() {
  return targets.flatMap(([os, arch]) => profiles.map((profile) => record(os, arch, profile)));
}

describe("physical performance evidence", () => {
  test("accepts both performance profiles on every release target", () => {
    expect(validatePerformanceEvidence(completeRecords())).toEqual([]);
  });

  test("rejects short, slow, growing, duplicated, and missing runs", () => {
    const records = completeRecords().slice(0, -1);
    records[0] = {
      ...records[0],
      elapsed_seconds: 30,
      average_fps: 10,
      capture_queue_peak: 2,
      monotonic_memory_growth: true,
    };
    records.push(record("linux", "x86_64", "interactive-1080p60"));
    expect(validatePerformanceEvidence(records)).toEqual(
      expect.arrayContaining([
        "record 1: elapsed_seconds is below 1800",
        "record 1: average_fps is below 54",
        "record 1: capture_queue_peak exceeds the one-frame bound",
        "record 1: monotonic memory growth was not ruled out",
        "record 12: duplicate run linux-x64/interactive-1080p60",
        "missing run windows-arm64/soak-1080p30",
      ]),
    );
  });
});
