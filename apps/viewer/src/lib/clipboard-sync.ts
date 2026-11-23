import { MAX_CONTROL_MESSAGE_BYTES, parseClipboardMessage } from "@nanoctl/protocol";

export class ClipboardSync {
  private last: string | undefined;
  private pending: string | undefined;
  private queue: Promise<void> = Promise.resolve();

  constructor(
    private readonly io: {
      focused: () => boolean;
      read: () => Promise<string>;
      write: (text: string) => Promise<void>;
      send: (message: string) => boolean;
    },
  ) {}

  local(text: string): void {
    if (text === this.last) return;
    const message = JSON.stringify({ type: "clipboard", text });
    if (new TextEncoder().encode(message).length >= MAX_CONTROL_MESSAGE_BYTES) return;
    if (this.io.send(message)) this.last = text;
  }

  receive(data: unknown): void {
    const message = parseClipboardMessage(data);
    if (!message) return;
    this.last = message.text;
    this.pending = message.text;
    if (this.io.focused()) void this.focus();
  }

  focus(): Promise<void> {
    this.queue = this.queue
      .then(async () => {
        if (!this.io.focused()) return;
        if (this.pending !== undefined) {
          const text = this.pending;
          await this.io.write(text);
          if (this.pending === text) this.pending = undefined;
        } else {
          const text = await this.io.read();
          if (this.pending === undefined && this.io.focused()) this.local(text);
        }
      })
      .catch(() => {});
    return this.queue;
  }
}
