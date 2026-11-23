import { describe, expect, test } from "bun:test";
import { comboMessages, connectSteps, connectionQuality, keyCombos } from "./session-ui";

const metrics = {
  resolution: "1920x1080",
  fps: 60,
  bitrateKbps: 8000,
  rttMs: 8,
  packetsLost: 0,
  framesDropped: 0,
  route: "direct" as const,
};

describe("session ui", () => {
  test("rates connection quality", () => {
    expect(connectionQuality(null)).toBeNull();
    expect(connectionQuality(metrics)).toBe("good");
    expect(connectionQuality({ ...metrics, route: "relay" })).toBe("fair");
    expect(connectionQuality({ ...metrics, rttMs: 90 })).toBe("fair");
    expect(connectionQuality({ ...metrics, rttMs: 180 })).toBe("poor");
  });

  test("does not flag an idle desktop as a bad connection", () => {
    for (const fps of [null, 0, 5, 12]) {
      expect(connectionQuality({ ...metrics, fps, rttMs: 1 })).toBe("good");
      expect(connectionQuality({ ...metrics, fps, rttMs: 180 })).toBe("poor");
    }
  });

  test("combos press modifiers, then the key, then release in reverse", () => {
    const combo = keyCombos("macos").find((c) => c.label === "Screenshot")!;
    const messages = comboMessages(combo).map((m) =>
      m.type === "key" ? `${m.action}:${m.code}:${m.modifiers}` : m.type,
    );
    expect(messages).toEqual([
      "down:MetaLeft:8",
      "down:ShiftLeft:9",
      "down:Digit3:9",
      "up:Digit3:9",
      "up:ShiftLeft:8",
      "up:MetaLeft:0",
    ]);
    expect(keyCombos("windows").map((c) => c.label)).toContain("Win key");
  });

  test("connect steps track progress", () => {
    expect(connectSteps(false, "negotiating", false).map((s) => s.state)).toEqual([
      "active",
      "pending",
      "pending",
    ]);
    expect(connectSteps(true, "connected", false).map((s) => s.state)).toEqual([
      "done",
      "done",
      "active",
    ]);
  });
});
