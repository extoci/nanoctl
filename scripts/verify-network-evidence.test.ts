import { describe, expect, test } from "bun:test";
import { requiredNetworkScenarios, validateNetworkEvidence } from "./verify-network-evidence";

function record(scenario: string) {
  return {
    schema_version: 1,
    passed: true,
    agent_version: "1.0.0",
    web_version: "1.0.0",
    scenario,
    duration_seconds: 300,
    setup_milliseconds: 450,
    recovery_milliseconds: 0,
    samples: 300,
    selected_route: "direct",
    average_bitrate_kbps: 8_000,
    p95_rtt_milliseconds: 20,
    p95_input_to_photon_milliseconds: 80,
    packets_lost: 0,
    frames_dropped: 0,
  };
}

describe("physical network evidence", () => {
  test("accepts one complete record for every required scenario", () => {
    expect(validateNetworkEvidence(requiredNetworkScenarios.map(record))).toEqual([]);
  });

  test("rejects failed, short, duplicate, inconsistent, and missing evidence", () => {
    const records = requiredNetworkScenarios.slice(0, -1).map(record);
    records[0] = {
      ...records[0],
      passed: false,
      duration_seconds: 30,
      samples: 0,
    };
    records[1] = { ...records[1], web_version: "other" };
    records.push(record("same-lan"));
    expect(validateNetworkEvidence(records)).toEqual(
      expect.arrayContaining([
        "record 1: scenario did not pass",
        "record 1: duration_seconds is below 300",
        "record 1: invalid samples",
        "record 2: web_version differs from the other records",
        "record 16: duplicate scenario same-lan",
        "missing scenario turn-loss",
      ]),
    );
  });
});
