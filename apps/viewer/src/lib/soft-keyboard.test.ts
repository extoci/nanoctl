import { expect, test } from "bun:test";
import type { ControlMessage } from "@nanoctl/protocol";
import { inputMessages, keyMessages, StickyModifiers } from "./soft-keyboard";

test("text and replacement/composition commits map Unicode characters to pairs", () => {
  for (const type of [
    "insertText",
    "insertReplacementText",
    "insertCompositionText",
    "insertFromComposition",
  ]) {
    expect(inputMessages(type, "a😀", 6)).toEqual([
      ...keyMessages("a", "", 6),
      ...keyMessages("😀", "", 6),
    ]);
  }
});
test("deletion and line breaks map to named keys", () => {
  expect(inputMessages("deleteContentBackward", null, 2)).toEqual(
    keyMessages("Backspace", "Backspace", 2),
  );
  for (const type of ["insertLineBreak", "insertParagraph"])
    expect(inputMessages(type, null)).toEqual(keyMessages("Enter", "Enter"));
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
    ...keyMessages("b"),
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
