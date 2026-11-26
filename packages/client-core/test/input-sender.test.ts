import { expect, test } from "bun:test";
import { parseInputMessage } from "@nanoctl/protocol";
import { V4InputSender, type V4InputChannel } from "../src/input-sender";

class FakeChannel implements V4InputChannel {
  readyState: RTCDataChannelState = "open";
  bufferedAmount = 0;
  readonly sent: Uint8Array[] = [];

  send(data: ArrayBufferView<ArrayBuffer>): void {
    this.sent.push(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    this.bufferedAmount += data.byteLength;
  }
}

test("routes discrete input to control and analog state to motion with independent sequences", () => {
  const control = new FakeChannel();
  const motion = new FakeChannel();
  const sender = new V4InputSender(control, motion);

  expect(sender.key(true, false, { code: "KeyA", key: "a" })).toBe("sent");
  expect(sender.pointerMove({ x: 1, y: 2 })).toBe("sent");
  expect(sender.heartbeat()).toBe("sent");
  expect(sender.gamepadSnapshot([])).toBe("sent");

  const key = parseInputMessage(control.sent[0]!);
  const heartbeat = parseInputMessage(control.sent[1]!);
  const move = parseInputMessage(motion.sent[0]!);
  const gamepad = parseInputMessage(motion.sent[1]!);
  expect(key).toMatchObject({ kind: "key", sequence: 1 });
  expect(heartbeat).toMatchObject({ kind: "heartbeat", sequence: 2 });
  expect(move).toMatchObject({ kind: "pointerMove", sequence: 1 });
  expect(gamepad).toMatchObject({ kind: "gamepadSnapshot", sequence: 2 });
});

test("drops loss-tolerant motion instead of queueing it behind a full SCTP buffer", () => {
  const control = new FakeChannel();
  const motion = new FakeChannel();
  const sender = new V4InputSender(control, motion);
  motion.bufferedAmount = 300;

  expect(sender.pointerMove({ x: 1, y: 2 })).toBe("backpressured");
  expect(sender.gamepadSnapshot([])).toBe("backpressured");
  expect(motion.sent).toHaveLength(0);
});

test("release remains attempted when control is backpressured", () => {
  const control = new FakeChannel();
  const motion = new FakeChannel();
  const sender = new V4InputSender(control, motion);
  control.bufferedAmount = 64 * 1024;

  expect(sender.key(true, false, { code: "KeyA", key: "a" })).toBe("backpressured");
  expect(sender.releaseAll()).toBe("sent");
  expect(control.sent).toHaveLength(1);
  expect(parseInputMessage(control.sent[0]!)).toMatchObject({ kind: "releaseAll" });
});

test("closed lanes report closed without writing", () => {
  const control = new FakeChannel();
  const motion = new FakeChannel();
  const sender = new V4InputSender(control, motion);
  control.readyState = "closed";
  motion.readyState = "closed";

  expect(sender.releaseAll()).toBe("closed");
  expect(sender.pointerMove({ x: 0, y: 0 })).toBe("closed");
  expect(control.sent).toHaveLength(0);
  expect(motion.sent).toHaveLength(0);
});
