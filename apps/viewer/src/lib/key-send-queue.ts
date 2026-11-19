import type { ControlMessage } from "@nanoctl/protocol";

type KeyMessage = Extract<ControlMessage, { type: "key" }>;
export const KEY_SEND_INTERVAL_MS = 100;
export const KEY_MESSAGES_PER_TICK = 2;
export const MAX_PENDING_KEYS = 1024;
type Timer = {
  setTimeout: (callback: () => void, delay: number) => unknown;
  clearTimeout: (timer: unknown) => void;
};

/** Small reliable batches leave headroom in the agent's 16-message native-input queue. */
export class KeySendQueue {
  private pending: KeyMessage[] = [];
  private pressed = new Map<string, KeyMessage>();
  private timer: unknown;
  constructor(
    private options: {
      send: (message: KeyMessage) => boolean;
      release: (message: KeyMessage) => void;
      ready: () => boolean;
      clock?: Timer;
    },
  ) {}
  private get clock(): Timer {
    return (
      this.options.clock ?? {
        setTimeout: (fn, delay) => globalThis.setTimeout(fn, delay),
        clearTimeout: (timer) => globalThis.clearTimeout(timer as ReturnType<typeof setTimeout>),
      }
    );
  }
  enqueue(message: KeyMessage): boolean {
    if (this.pending.length >= MAX_PENDING_KEYS) {
      this.reset();
      return false;
    }
    this.pending.push(message);
    this.schedule();
    return true;
  }
  reset(): void {
    if (this.timer !== undefined) this.clock.clearTimeout(this.timer);
    this.timer = undefined;
    this.pending = [];
    const pressed = [...this.pressed.values()];
    this.pressed.clear();
    for (const message of pressed)
      this.options.release({ ...message, action: "up", modifiers: 0, repeat: false });
  }
  private schedule(): void {
    if (this.timer !== undefined || !this.pending.length) return;
    this.timer = this.clock.setTimeout(() => {
      this.timer = undefined;
      this.tick();
    }, KEY_SEND_INTERVAL_MS);
  }
  private tick(): void {
    if (!this.options.ready()) {
      this.schedule();
      return;
    }
    let budget = KEY_MESSAGES_PER_TICK;
    while (this.pending.length && budget > 0) {
      const first = this.pending[0]!;
      const next = this.pending[1];
      const pair =
        first.action === "down" &&
        next?.action === "up" &&
        first.key === next.key &&
        first.code === next.code;
      const count = pair ? 2 : 1;
      if (count > budget) break;
      for (let i = 0; i < count; i++) {
        const message = this.pending.shift()!;
        if (!this.options.send(message)) {
          this.reset();
          return;
        }
        const id = message.code || message.key;
        if (message.action === "down") this.pressed.set(id, message);
        else this.pressed.delete(id);
      }
      budget -= count;
    }
    this.schedule();
  }
}
