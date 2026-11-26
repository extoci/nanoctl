import { expect, test } from "bun:test";
import { longestGap, mostInOneSecond } from "../tests/browser/bad-network-evidence";

test("counts both a stalled start and a silent pipeline exit in the observation window", () => {
  expect(longestGap([], 10, 1_010)).toBe(1_000);
  expect(longestGap([0, 550, 600, 1_100], 10, 1_010)).toBe(540);
  expect(longestGap([10, 20, 30], 10, 1_010)).toBe(980);
});

test("finds a keyframe burst across a wall-clock second boundary", () => {
  expect(mostInOneSecond([])).toBe(0);
  expect(mostInOneSecond([400, 1_010, 1_100, 1_399, 1_400])).toBe(4);
});
