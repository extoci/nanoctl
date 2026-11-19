import { MAX_CONTROL_MESSAGE_BYTES, type ControlMessage } from "@nanoctl/protocol";

export const SENTINEL = "\u200b";
export function textMessages(text: string): ControlMessage[] {
  const messages: ControlMessage[] = [];
  const encoder = new TextEncoder();
  const overhead = encoder.encode(JSON.stringify({ type: "text", text: "" })).length;
  let chunk = "";
  let bytes = overhead;
  for (const character of text) {
    const size = encoder.encode(JSON.stringify(character)).length - 2;
    if (bytes + size > MAX_CONTROL_MESSAGE_BYTES) {
      messages.push({ type: "text", text: chunk });
      chunk = "";
      bytes = overhead;
    }
    chunk += character;
    bytes += size;
  }
  if (chunk) messages.push({ type: "text", text: chunk });
  return messages;
}
export const namedKeys: Record<string, string> = {
  Backspace: "Backspace",
  Enter: "Enter",
  Tab: "Tab",
  Escape: "Escape",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Delete: "Delete",
};
export function keyMessages(key: string, code = "", modifiers = 0): ControlMessage[] {
  return ["down", "up"].map((action) => ({
    type: "key",
    action: action as "down" | "up",
    key,
    code,
    modifiers,
    repeat: false,
  }));
}
export function inputMessages(
  inputType: string,
  data: string | null,
  modifiers = 0,
): ControlMessage[] {
  if (inputType === "deleteContentBackward")
    return keyMessages("Backspace", "Backspace", modifiers);
  if (inputType === "insertLineBreak" || inputType === "insertParagraph")
    return keyMessages("Enter", "Enter", modifiers);
  if (
    ![
      "insertText",
      "insertReplacementText",
      "insertCompositionText",
      "insertFromComposition",
      "insertFromPaste",
    ].includes(inputType)
  )
    return [];
  const text = data ?? "";
  if (!modifiers || !text) return textMessages(text);
  const first = String.fromCodePoint(text.codePointAt(0)!);
  return [...keyMessages(first, "", modifiers), ...textMessages(text.slice(first.length))];
}

export const modifierKeys = {
  Shift: { code: "ShiftLeft", bit: 1 },
  Control: { code: "ControlLeft", bit: 2 },
  Alt: { code: "AltLeft", bit: 4 },
  Meta: { code: "MetaLeft", bit: 8 },
} as const;
export type Modifier = keyof typeof modifierKeys;
export class StickyModifiers {
  readonly state = new Map<Modifier, "latched" | "locked">();
  private lastTap: { key: Modifier; t: number } | null = null;
  constructor(private send: (message: ControlMessage) => unknown) {}
  get mask(): number {
    return [...this.state.keys()].reduce((mask, key) => mask | modifierKeys[key].bit, 0);
  }
  tap(key: Modifier, t: number): void {
    if (this.state.get(key) === "latched" && this.lastTap?.key === key && t - this.lastTap.t < 300)
      this.state.set(key, "locked");
    else if (this.state.has(key)) this.release(key);
    else {
      this.state.set(key, "latched");
      this.send({
        type: "key",
        action: "down",
        key,
        code: modifierKeys[key].code,
        modifiers: this.mask,
        repeat: false,
      });
    }
    this.lastTap = { key, t };
  }
  dispatch(messages: ControlMessage[]): void {
    if (!messages.length) return;
    for (const message of messages) {
      this.send(message.type === "key" ? { ...message, modifiers: this.mask } : message);
      if (message.type === "key" && message.action === "up") {
        for (const [key, state] of this.state) if (state === "latched") this.release(key);
      }
    }
    this.lastTap = null;
  }
  reset(): void {
    for (const key of this.state.keys()) this.release(key);
    this.lastTap = null;
  }
  private release(key: Modifier): void {
    this.state.delete(key);
    this.send({
      type: "key",
      action: "up",
      key,
      code: modifierKeys[key].code,
      modifiers: this.mask,
      repeat: false,
    });
  }
}
