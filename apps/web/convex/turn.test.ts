import { describe, expect, test } from "bun:test";

import { parseTurnUrls } from "./turn";

describe("TURN relay configuration", () => {
  test("accepts bounded TURN and TURNS endpoints", () => {
    expect(parseTurnUrls("turn:one.example:3478?transport=udp, turns:two.example:5349")).toEqual([
      "turn:one.example:3478?transport=udp",
      "turns:two.example:5349",
    ]);
  });

  test("rejects credentials and unrelated schemes", () => {
    expect(() => parseTurnUrls("https://turn.example")).toThrow();
    expect(() => parseTurnUrls("turn:user@turn.example")).toThrow();
  });
});
