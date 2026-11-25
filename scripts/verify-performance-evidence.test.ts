import { describe, expect, test } from "bun:test";
import { validatePerformanceEvidence } from "./verify-performance-evidence";

const targets = [
  ["linux", "x86_64"],
  ["linux", "aarch64"],
  ["macos", "aarch64"],
  ["windows", "x86_64"],
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
    capture_to_encode_p50_milliseconds: 4,
    capture_to_encode_p95_milliseconds: 12,
    encode_p50_milliseconds: 3,
    encode_p95_milliseconds: 8,
    p95_frame_milliseconds: 20,
    p99_frame_milliseconds: 30,
    p95_input_to_photon_milliseconds: 90,
    frame_drops: 2,
    packet_loss_percent: 0.1,
    rtt_p95_milliseconds: 35,
    decoded_fps: profile === "interactive-1080p60" ? 60 : 30,
    rendered_width: 1_920,
    rendered_height: 1_080,
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

  test("rejects unsupported release targets", () => {
    expect(validatePerformanceEvidence([record("macos", "x86_64", "interactive-1080p60")])).toEqual(
      expect.arrayContaining(["record 1: unexpected target macos-x64"]),
    );
    expect(
      validatePerformanceEvidence([record("windows", "aarch64", "interactive-1080p60")]),
    ).toEqual(expect.arrayContaining(["record 1: unexpected target windows-arm64"]));
  });

  test("rejects short, slow, growing, duplicated, and missing runs", () => {
    const records = completeRecords().slice(0, -1);
    records[0] = {
      ...records[0],
      elapsed_seconds: 30,
      average_fps: 10,
      decoded_fps: 10,
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
        "record 8: duplicate run linux-x64/interactive-1080p60",
        "missing run windows-x64/soak-1080p30",
      ]),
    );
  });
});
