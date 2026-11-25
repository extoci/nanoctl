import { describe, expect, test } from "bun:test";

import { validateMediaEvidence } from "./verify-media-evidence";

const targets = [
  ["linux", "x86_64"],
  ["linux", "aarch64"],
  ["macos", "aarch64"],
  ["windows", "x86_64"],
] as const;

function record(target_os: string, target_arch: string) {
  return {
    schema_version: 1,
    passed: true,
    agent_version: "1.0.0",
    target_os,
    target_arch,
    backend: "native hardware",
    requested_seconds: 1_800,
    elapsed_milliseconds: 1_800_000,
    frames: 108_000,
    encoded_bytes: 800_000_000,
    capture_to_encode_p50_milliseconds: 4,
    capture_to_encode_p95_milliseconds: 12,
    encode_p50_milliseconds: 3,
    encode_p95_milliseconds: 8,
    width: 1_920,
    height: 1_080,
    idr_frames: 120,
    sps_units: 120,
    pps_units: 120,
  };
}

describe("physical media evidence", () => {
  test("accepts one hardware record for every release target", () => {
    expect(validateMediaEvidence(targets.map(([os, arch]) => record(os, arch)))).toEqual([]);
  });

  test("rejects unsupported release targets", () => {
    expect(validateMediaEvidence([record("macos", "x86_64")])).toEqual(
      expect.arrayContaining(["record 1: unexpected target macos-x64"]),
    );
    expect(validateMediaEvidence([record("windows", "aarch64")])).toEqual(
      expect.arrayContaining(["record 1: unexpected target windows-arm64"]),
    );
  });

  test("rejects software, short, duplicate, and missing evidence", () => {
    const records = targets.slice(0, -1).map(([os, arch]) => record(os, arch));
    records[0] = {
      ...records[0],
      backend: "OpenH264",
      requested_seconds: 30,
    };
    records.push(record("linux", "x86_64"));
    expect(validateMediaEvidence(records)).toEqual(
      expect.arrayContaining([
        "record 1: requested_seconds is below 1800",
        "record 1: hardware encoder evidence is absent",
        "record 4: duplicate target linux-x64",
        "missing target windows-x64",
      ]),
    );
  });
});
