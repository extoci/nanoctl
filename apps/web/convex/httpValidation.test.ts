import { describe, expect, test } from "bun:test";
import { boundedCapabilities, parseEnrollmentInput } from "./httpValidation";

describe("agent HTTP input validation", () => {
  test("normalizes a valid enrollment request", () => {
    expect(
      parseEnrollmentInput({
        code: "abcde-fghjk-mnpqr-stvwx",
        name: "  Studio   desktop ",
        platform: "linux",
        architecture: "x64",
        agentVersion: " 1.0.0 ",
        capabilities: { displays: [] },
      }),
    ).toEqual({
      code: "ABCDE-FGHJK-MNPQR-STVWX",
      name: "Studio desktop",
      platform: "linux",
      architecture: "x64",
      agentVersion: "1.0.0",
      capabilitiesJson: '{"displays":[]}',
    });
  });

  test("rejects malformed codes and oversized identity fields", () => {
    const base = {
      code: "ABCDE-FGHJK-MNPQR-STVWX",
      name: "Desktop",
      platform: "linux",
      architecture: "x64",
      agentVersion: "1.0.0",
      capabilities: {},
    };
    expect(parseEnrollmentInput({ ...base, code: "not-a-code" })).toBeNull();
    expect(parseEnrollmentInput({ ...base, name: "x".repeat(81) })).toBeNull();
    expect(parseEnrollmentInput({ ...base, agentVersion: "x".repeat(33) })).toBeNull();
  });

  test("bounds capabilities by encoded bytes without truncating JSON", () => {
    expect(boundedCapabilities({ input: true })).toBe('{"input":true}');
    expect(boundedCapabilities({ value: "😀".repeat(20_000) })).toBeNull();
  });
});
