import { expect, test } from "bun:test";
import type { ControlMessage } from "@nanoctl/protocol";
import { KeySendQueue, KEY_SEND_INTERVAL_MS, MAX_PENDING_KEYS } from "./key-send-queue";
import { inputMessages, keyMessages, MAX_INPUT_CHARACTERS, StickyModifiers } from "./soft-keyboard";

function setup() {
  let now = 0;
  let id = 0;
  let ready = true;
  let fail = false;
  const timers = new Map<number, { at: number; callback: () => void }>();
  const sent: { t: number; message: ControlMessage }[] = [];
  const queue = new KeySendQueue({
    send: (message) => {
      if (fail) return false;
      sent.push({ t: now, message });
      return true;
    },
    release: (message) => {
      sent.push({ t: now, message });
    },
    ready: () => ready,
    clock: {
      setTimeout: (callback, delay) => {
        timers.set(++id, { at: now + delay, callback });
        return id;
      },
      clearTimeout: (timer) => {
        timers.delete(timer as number);
      },
    },
  });
  const advance = (duration: number) => {
    const end = now + duration;
    while (true) {
      const next = [...timers].sort((a, b) => a[1].at - b[1].at)[0];
      if (!next || next[1].at > end) break;
      now = next[1].at;
      timers.delete(next[0]);
      next[1].callback();
    }
    now = end;
  };
  const enqueue = (messages: ControlMessage[]) => {
    for (const message of messages) if (message.type === "key") queue.enqueue(message);
  };
  return {
    queue,
    sent,
    advance,
    enqueue,
    ready: (value: boolean) => {
      ready = value;
    },
    fail: () => {
      fail = true;
    },
  };
}

test("200 characters arrive in order, adjacent down/up, at most two messages per 100ms window", () => {
  const s = setup();
  const text = Array.from({ length: 200 }, (_, i) => String.fromCodePoint(0x400 + i)).join("");
  const messages = inputMessages("insertText", text);
  s.enqueue(messages);
  expect(s.sent).toEqual([]);
  s.advance(200 * KEY_SEND_INTERVAL_MS);
  expect(s.sent.map(({ message }) => message)).toEqual(messages);
  for (let i = 0; i < s.sent.length; i += 2) {
    expect(s.sent[i]!.t).toBe(s.sent[i + 1]!.t);
    const start = s.sent[i]!.t;
    expect(s.sent.filter(({ t }) => t >= start && t < start + 100).length).toBeLessThanOrEqual(2);
    expect(s.sent.filter(({ t }) => t >= start && t < start + 1000).length).toBeLessThanOrEqual(20);
  }
});
test("sticky combinations use the same budget and preserve character pairs", () => {
  const s = setup();
  const sticky = new StickyModifiers((message) => {
    if (message.type === "key") s.queue.enqueue(message);
  });
  sticky.tap("Control", 0);
  sticky.tap("Alt", 1);
  sticky.dispatch(inputMessages("insertText", "ab", sticky.mask));
  s.advance(500);
  expect(
    s.sent.map(({ message }) => message.type === "key" && [message.key, message.action]),
  ).toEqual([
    ["Control", "down"],
    ["Alt", "down"],
    ["a", "down"],
    ["a", "up"],
    ["Control", "up"],
    ["Alt", "up"],
    ["b", "down"],
    ["b", "up"],
  ]);
  for (let i = 0; i < s.sent.length; i += 2) expect(s.sent[i]!.t).toBe(s.sent[i + 1]!.t);
});
test("buffered channel pauses without accumulating catch-up batches", () => {
  const s = setup();
  s.ready(false);
  s.enqueue(inputMessages("insertText", "abc"));
  s.advance(1000);
  expect(s.sent).toEqual([]);
  s.ready(true);
  s.advance(100);
  expect(s.sent.length).toBe(2);
  s.advance(200);
  expect(s.sent.map(({ message }) => message)).toEqual(inputMessages("insertText", "abc"));
});
test("reset drops pending keys and releases only already transmitted presses", () => {
  const s = setup();
  const [down, up] = keyMessages("Control", "ControlLeft", 2);
  s.enqueue([down!]);
  s.advance(100);
  s.enqueue(inputMessages("insertText", "pending"));
  s.enqueue([up!]);
  s.queue.reset();
  s.advance(1000);
  if (up?.type !== "key") throw new Error("Expected key up");
  expect(s.sent.map(({ message }) => message)).toEqual([down!, { ...up, modifiers: 0 }]);
  s.queue.reset();
  expect(s.sent.length).toBe(2);
});
test("send failure releases any successfully sent down and discards remaining keys", () => {
  const s = setup();
  s.enqueue([keyMessages("Shift", "ShiftLeft")[0]!]);
  s.advance(100);
  s.enqueue(inputMessages("insertText", "abc"));
  s.fail();
  s.advance(1000);
  expect(s.sent.map(({ message }) => message.type === "key" && message.action)).toEqual([
    "down",
    "up",
  ]);
});
test("long pastes and the pending queue are bounded", () => {
  expect(inputMessages("insertText", "a".repeat(10000)).length).toBe(MAX_INPUT_CHARACTERS * 2);
  const s = setup();
  const down = keyMessages("a")[0]!;
  for (let i = 0; i < MAX_PENDING_KEYS; i++)
    if (down.type === "key") expect(s.queue.enqueue(down)).toBe(true);
  if (down.type === "key") expect(s.queue.enqueue(down)).toBe(false);
  s.advance(1000);
  expect(s.sent).toEqual([]);
});
