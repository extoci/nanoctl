import { expect, test } from "bun:test";
import { MAX_CONTROL_MESSAGE_BYTES, parseClipboardMessage } from "@nanoctl/protocol";
import { ClipboardSync } from "./clipboard-sync";

test("clipboard protocol rejects malformed, binary and oversized messages", () => {
  for (const value of [
    null,
    new Uint8Array(),
    "bad",
    "null",
    '{"type":"clipboard","text":2}',
    " ".repeat(MAX_CONTROL_MESSAGE_BYTES),
  ])
    expect(parseClipboardMessage(value)).toBeNull();
  expect(parseClipboardMessage('{"type":"clipboard","text":""}')).toEqual({
    type: "clipboard",
    text: "",
  });
});

test("sync suppresses echoes and writes only latest pending host text on focus", async () => {
  let focused = false;
  let local = "local";
  const sent: string[] = [];
  const writes: string[] = [];
  const sync = new ClipboardSync({
    focused: () => focused,
    read: async () => local,
    write: async (text) => {
      writes.push(text);
      local = text;
    },
    send: (message) => {
      sent.push(message);
      return true;
    },
  });
  sync.receive('{"type":"clipboard","text":"old"}');
  sync.receive('{"type":"clipboard","text":"host"}');
  expect(writes).toEqual([]);
  focused = true;
  await sync.focus();
  await sync.focus();
  expect(writes).toEqual(["host"]);
  expect(sent).toEqual([]);
  sync.local("");
  sync.local("");
  expect(sent).toEqual(['{"type":"clipboard","text":""}']);
  sync.local("\n".repeat(MAX_CONTROL_MESSAGE_BYTES));
  expect(sent).toHaveLength(1);
});

test("permission failures are ignored and pending host writes can retry", async () => {
  let denied = true;
  const writes: string[] = [];
  const sync = new ClipboardSync({
    focused: () => true,
    read: async () => {
      throw new Error("denied");
    },
    write: async (text) => {
      if (denied) throw new Error("denied");
      writes.push(text);
    },
    send: () => false,
  });
  await sync.focus();
  sync.receive('{"type":"clipboard","text":"host"}');
  await sync.focus();
  denied = false;
  await sync.focus();
  expect(writes).toEqual(["host"]);
});
