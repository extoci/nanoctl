import { expect, test } from "bun:test";
import {
  earlyMediaExits,
  longestGap,
  mostInOneSecond,
} from "../tests/browser/bad-network-evidence";

test("counts both a stalled start and a silent pipeline exit in the observation window", () => {
  expect(longestGap([], 10, 1_010)).toBe(1_000);
  expect(longestGap([0, 550, 600, 1_100], 10, 1_010)).toBe(540);
  expect(longestGap([10, 20, 30], 10, 1_010)).toBe(980);
});

test("finds a keyframe burst across a wall-clock second boundary", () => {
  expect(mostInOneSecond([])).toBe(0);
  expect(mostInOneSecond([400, 1_010, 1_100, 1_399, 1_400])).toBe(4);
});

test("allows an explicit viewer end during attach and flags active pipeline exits", () => {
  const startupEnd = "2026-11-27T14:21:40.088Z INFO nanoctl::v4_media: v4 media pipeline stopped";
  const viewerEnd =
    "2026-11-27T14:21:40.093Z INFO nanoctl::status: Viewer disconnected reason=viewer-ended";
  const activeStop = "2026-11-27T14:21:49.002Z INFO nanoctl::v4_media: v4 media pipeline stopped";
  const start = Date.parse("2026-11-27T14:21:41.000Z");
  expect(earlyMediaExits([startupEnd, viewerEnd, activeStop], start)).toEqual([activeStop]);

  const failed = viewerEnd.replace("reason=viewer-ended", "reason=media-failed");
  expect(earlyMediaExits([startupEnd, failed], start)).toEqual([startupEnd, failed]);
  expect(earlyMediaExits([startupEnd], start)).toEqual([startupEnd]);
});
