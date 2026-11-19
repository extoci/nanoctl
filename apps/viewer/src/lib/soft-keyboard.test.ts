import { expect, test } from "bun:test";
import { MAX_CONTROL_MESSAGE_BYTES, type ControlMessage } from "@nanoctl/protocol";
import { inputMessages, keyMessages, StickyModifiers } from "./soft-keyboard";

test("text, paste and replacement/composition commits use one text message", () => {
  for (const type of [
    "insertText",
    "insertReplacementText",
    "insertCompositionText",
    "insertFromComposition",
    "insertFromPaste",
  ]) {
    expect(inputMessages(type, "a😀")).toEqual([{ type: "text", text: "a😀" }]);
  }
});
test("deletion and line breaks map to named keys", () => {
  expect(inputMessages("deleteContentBackward", null, 2)).toEqual(
    keyMessages("Backspace", "Backspace", 2),
  );
  for (const type of ["insertLineBreak", "insertParagraph"])
    expect(inputMessages(type, null)).toEqual(keyMessages("Enter", "Enter"));
});
test("200 characters use one commit and long escaped Unicode pastes split without loss", () => {
  const short = "x".repeat(200);
  expect(inputMessages("insertText", short)).toEqual([{ type: "text", text: short }]);
  const text = '😀漢\n"\\'.repeat(20_000);
  const messages = inputMessages("insertFromPaste", text);
  expect(messages.length).toBeGreaterThan(1);
  expect(messages.map((m) => (m.type === "text" ? m.text : "")).join("")).toBe(text);
  for (const message of messages) {
    expect(new TextEncoder().encode(JSON.stringify(message)).length).toBeLessThanOrEqual(
      MAX_CONTROL_MESSAGE_BYTES,
    );
    if (message.type === "text") expect(message.text.isWellFormed()).toBe(true);
  }
});
test("modifier shortcut preserves a Unicode first character and leaves empty commits latched", () => {
  expect(inputMessages("insertText", "😀tail", 2)).toEqual([
    ...keyMessages("😀", "", 2),
    { type: "text", text: "tail" },
  ]);
  expect(inputMessages("insertText", "", 2)).toEqual([]);
});
test("empty, unsupported and invalid input types do not emit keys", () => {
  expect(inputMessages("insertText", null)).toEqual([]);
  expect(inputMessages("insertText", "")).toEqual([]);
  expect(inputMessages("historyUndo", "x")).toEqual([]);
  expect(inputMessages("", "x")).toEqual([]);
});
test("latched modifiers surround only the next key, including multi-character input", () => {
  const messages: ControlMessage[] = [];
  const sticky = new StickyModifiers((message) => messages.push(message));
  sticky.tap("Control", 0);
  sticky.dispatch(inputMessages("insertText", "ab", sticky.mask));
  expect(messages).toEqual([
    {
      type: "key",
      action: "down",
      key: "Control",
      code: "ControlLeft",
      modifiers: 2,
      repeat: false,
    },
    ...keyMessages("a", "", 2),
    { type: "key", action: "up", key: "Control", code: "ControlLeft", modifiers: 0, repeat: false },
    { type: "text", text: "b" },
  ]);
  expect(sticky.mask).toBe(0);
});
test("double tap locks until another tap; reset releases every modifier", () => {
  const messages: ControlMessage[] = [];
  const sticky = new StickyModifiers((message) => messages.push(message));
  sticky.tap("Shift", 0);
  sticky.tap("Shift", 100);
  expect(sticky.state.get("Shift")).toBe("locked");
  sticky.dispatch(keyMessages("ArrowLeft", "ArrowLeft", sticky.mask));
  expect(sticky.mask).toBe(1);
  sticky.tap("Shift", 200);
  expect(sticky.mask).toBe(0);
  sticky.tap("Alt", 300);
  sticky.tap("Meta", 400);
  sticky.reset();
  expect(sticky.mask).toBe(0);
  expect(messages.slice(-2)).toEqual([
    { type: "key", action: "up", key: "Alt", code: "AltLeft", modifiers: 8, repeat: false },
    { type: "key", action: "up", key: "Meta", code: "MetaLeft", modifiers: 0, repeat: false },
  ]);
  const count = messages.length;
  sticky.reset();
  expect(messages.length).toBe(count);
});
test("unsupported inputs do not consume a latch and a slow second tap releases it", () => {
  const sticky = new StickyModifiers(() => {});
  sticky.tap("Control", 0);
  sticky.dispatch([]);
  expect(sticky.mask).toBe(2);
  sticky.tap("Control", 500);
  expect(sticky.mask).toBe(0);
});
