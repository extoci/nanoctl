import { describe, expect, test } from "bun:test";
import {
  parseDeviceDisplays,
  parseDeviceReadiness,
  parseDeviceRemoteInput,
  parseDeviceReadinessState,
  parseSignalEnvelope,
} from "./lib";

function envelope(sender: "controller" | "host", type: string) {
  const payload =
    type === "offer" || type === "answer"
      ? { type, sdp: "v=0" }
      : type === "end"
        ? { type, reason: "done" }
        : { type };
  return JSON.stringify({
    version: 1,
    sessionId: "session",
    sequence: 7,
    sender,
    sentAt: 1,
    payload,
  });
}

describe("signal envelope indexing", () => {
  test("extracts an authenticated role-compatible kind", () => {
    expect(parseSignalEnvelope(envelope("controller", "offer"), "controller")).toEqual({
      version: 1,
      sessionId: "session",
      sender: "controller",
      controllerEpoch: "",
      hostEpoch: "",
      sentAt: 1,
      sequence: 7,
      generation: 0,
      kind: "offer",
    });
    expect(parseSignalEnvelope(envelope("host", "answer"), "host").kind).toBe("answer");
    expect(parseSignalEnvelope(envelope("host", "end"), "host").kind).toBe("end");
  });

  test("rejects role confusion and unknown payload kinds", () => {
    expect(() => parseSignalEnvelope(envelope("host", "offer"), "host")).toThrow();
    expect(() => parseSignalEnvelope(envelope("controller", "answer"), "controller")).toThrow();
    expect(() =>
      parseSignalEnvelope(envelope("controller", "renegotiate"), "controller"),
    ).toThrow();
    expect(() =>
      parseSignalEnvelope(
        JSON.stringify({
          ...JSON.parse(envelope("controller", "offer")),
          sentAt: 0,
          payload: { type: "offer" },
        }),
        "controller",
      ),
    ).toThrow();
  });
});

describe("device capability parsing", () => {
  test("accepts bounded display metadata and drops malformed rows", () => {
    const displays = parseDeviceDisplays(
      JSON.stringify({
        displays: [
          {
            id: "42",
            name: "Studio Display",
            width: 2560,
            height: 1440,
            scaleFactor: 2,
            primary: true,
          },
          { id: "", name: "invalid" },
        ],
      }),
    );
    expect(displays).toEqual([
      {
        id: "42",
        name: "Studio Display",
        width: 2560,
        height: 1440,
        scaleFactor: 2,
        primary: true,
      },
    ]);
    expect(parseDeviceDisplays("not json")).toEqual([]);
  });

  test("requires an explicit usable v2 media readiness capability", () => {
    const capabilities = JSON.stringify({
      protocolVersion: 1,
      codecs: ["h264"],
      ready: true,
      displays: [
        {
          id: "42",
          name: "Display",
          width: 1920,
          height: 1080,
          scaleFactor: 1,
          primary: true,
        },
      ],
    });
    expect(parseDeviceReadiness(capabilities)).toBe(false);
    expect(parseDeviceRemoteInput(capabilities)).toBe(false);
    expect(
      parseDeviceRemoteInput(JSON.stringify({ ...JSON.parse(capabilities), remoteInput: true })),
    ).toBe(true);
    expect(parseDeviceReadinessState(capabilities)).toBe("upgrade-required");
    expect(parseDeviceReadinessState(capabilities.replace('"ready":true', '"ready":false'))).toBe(
      "upgrade-required",
    );
    expect(parseDeviceReadiness("{}")).toBe(false);
    const v2Capabilities = JSON.stringify({
      ...JSON.parse(capabilities),
      protocolVersion: 2,
    });
    expect(
      parseDeviceReadinessState(
        JSON.stringify({ ...JSON.parse(v2Capabilities), readiness: "capture-permission" }),
      ),
    ).toBe("capture-permission");
    expect(
      parseDeviceReadinessState(
        JSON.stringify({ protocolVersion: 2, codecs: [], displays: [], ready: false }),
      ),
    ).toBe("no-display");
  });
});
