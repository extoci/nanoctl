import { expect, test } from "bun:test";
import { VideoLossWindow } from "../src/video-loss";

test("reports missing access units after the reorder window, including across sequence wrap", () => {
  const loss = new VideoLossWindow();
  const observe = (offset: number) => loss.observe((0xfffffffc + offset) >>> 0);
  [0, 2, 1, 1, 4].forEach(observe);
  expect(loss.take()).toBeUndefined();
  for (let offset = 5; offset <= 20; offset += 1) observe(offset);
  expect(loss.take()).toBe(1 / 5);
  expect(loss.take()).toBeUndefined();
  observe(3);
  observe(21);
  expect(loss.take()).toBe(0);
});
