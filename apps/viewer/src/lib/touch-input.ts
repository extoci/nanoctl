import type { ControlMessage } from "@nanoctl/protocol";

export type Position = { x: number; y: number };
export type VideoGeometry = { left: number; top: number; width: number; height: number };
export type TouchSample = Position & { id: number; t: number };
export type TouchMode = "trackpad" | "direct";
export type Zoom = { scale: number; offsetX: number; offsetY: number };
const clamp = (n: number, min = 0, max = 1) => Math.max(min, Math.min(max, n));

/** The rect is the transformed element rect, including CSS zoom/pan. */
export function videoGeometry(
  rect: VideoGeometry,
  width: number,
  height: number,
): VideoGeometry | null {
  if (width <= 0 || height <= 0 || rect.width <= 0 || rect.height <= 0) return null;
  const scale = Math.min(rect.width / width, rect.height / height);
  const w = width * scale;
  const h = height * scale;
  return {
    left: rect.left + (rect.width - w) / 2,
    top: rect.top + (rect.height - h) / 2,
    width: w,
    height: h,
  };
}

export function normalizedPosition(point: Position, geometry: VideoGeometry): Position {
  return {
    x: clamp((point.x - geometry.left) / geometry.width),
    y: clamp((point.y - geometry.top) / geometry.height),
  };
}

type Clock = {
  setTimeout: (fn: () => void, ms: number) => unknown;
  clearTimeout: (id: unknown) => void;
};
type Options = {
  mode: TouchMode;
  geometry: () => VideoGeometry | null;
  send: (message: ControlMessage) => unknown;
  onCursor?: (position: Position) => void;
  onZoom?: (zoom: Zoom) => void;
  clock?: Clock;
};
type Finger = { start: TouchSample; last: TouchSample };
const distance = (a: Position, b: Position) => Math.hypot(a.x - b.x, a.y - b.y);

export class TouchGestures {
  cursor: Position = { x: 0.5, y: 0.5 };
  zoom: Zoom = { scale: 1, offsetX: 0, offsetY: 0 };
  private fingers = new Map<number, Finger>();
  private count = 0;
  private started = 0;
  private moved = false;
  private consumed = false;
  private held = false;
  private lastTap = -Infinity;
  private lastTwoTap = -Infinity;
  private timer: unknown;
  private decision: "pending" | "scroll" | "pinch" = "pending";
  private pair: { center: Position; distance: number; zoom: Zoom } | null = null;
  private scroll = { x: 0, y: 0 };
  private clock: Clock;

  constructor(private options: Options) {
    this.clock = options.clock ?? {
      setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
      clearTimeout: (id) => globalThis.clearTimeout(id as ReturnType<typeof setTimeout>),
    };
  }

  down(sample: TouchSample): void {
    if (this.fingers.has(sample.id)) return;
    if (!this.fingers.size) {
      this.started = sample.t;
      this.count = 0;
      this.moved = this.consumed = false;
      this.decision = "pending";
      this.scroll = { x: 0, y: 0 };
    }
    this.fingers.set(sample.id, { start: sample, last: sample });
    this.count = Math.max(this.count, this.fingers.size);
    this.clearTimer();
    if (this.count === 1) {
      if (this.options.mode === "trackpad" && sample.t - this.lastTap < 300) {
        this.press();
        this.consumed = true;
        this.lastTap = -Infinity;
      } else if (this.options.mode === "direct") {
        this.timer = this.clock.setTimeout(() => {
          if (this.fingers.size !== 1 || this.moved) return;
          this.locate(sample);
          this.click(2);
          this.consumed = true;
        }, 500);
      }
    } else {
      this.release();
      if (this.fingers.size === 2) this.pair = { ...this.pairPosition(), zoom: { ...this.zoom } };
    }
  }

  move(sample: TouchSample): void {
    const finger = this.fingers.get(sample.id);
    if (!finger) return;
    const previous = finger.last;
    finger.last = sample;
    if (distance(finger.start, sample) >= 10) {
      this.moved = true;
      this.clearTimer();
    }
    if (this.count === 1) {
      if (this.consumed && !this.held) return;
      if (this.options.mode === "direct") {
        if (!this.moved) return;
        if (!this.held) {
          this.locate(finger.start);
          this.press();
        }
        this.locate(sample);
      } else {
        const geometry = this.options.geometry();
        if (!geometry) return;
        const speed = distance(previous, sample) / Math.max(1, sample.t - previous.t);
        const gain = 1 + Math.min(0.5, speed / 4);
        this.cursor = {
          x: clamp(this.cursor.x + ((sample.x - previous.x) / geometry.width) * gain),
          y: clamp(this.cursor.y + ((sample.y - previous.y) / geometry.height) * gain),
        };
        this.options.onCursor?.(this.cursor);
        this.emit("move");
      }
      return;
    }
    if (this.count !== 2 || this.fingers.size !== 2 || !this.pair) return;
    const current = this.pairPosition();
    const ratio = current.distance / Math.max(1, this.pair.distance);
    const dx = current.center.x - this.pair.center.x;
    const dy = current.center.y - this.pair.center.y;
    if (this.decision === "pending") {
      if (Math.abs(ratio - 1) > 0.15 && Math.hypot(dx, dy) < 15) this.decision = "pinch";
      else if (Math.hypot(dx, dy) >= 15) this.decision = "scroll";
      else return;
      this.moved = true;
      this.consumed = true;
    }
    if (this.decision === "pinch") {
      const scale = clamp(this.pair.zoom.scale * ratio, 1, 4);
      this.zoom =
        scale < 1.05
          ? { scale: 1, offsetX: 0, offsetY: 0 }
          : { scale, offsetX: this.pair.zoom.offsetX + dx, offsetY: this.pair.zoom.offsetY + dy };
      this.options.onZoom?.(this.zoom);
    } else {
      this.scroll.x -= dx * 5;
      this.scroll.y -= dy * 5;
      const deltaX = Math.trunc(this.scroll.x / 100) * 100;
      const deltaY = Math.trunc(this.scroll.y / 100) * 100;
      if (deltaX || deltaY)
        this.options.send({ type: "pointer", action: "wheel", ...this.cursor, deltaX, deltaY });
      this.scroll.x -= deltaX;
      this.scroll.y -= deltaY;
      this.pair.center = current.center;
    }
  }

  up(sample: TouchSample): void {
    if (!this.fingers.has(sample.id)) return;
    this.move(sample);
    this.clearTimer();
    this.release();
    this.fingers.delete(sample.id);
    if (this.fingers.size) return;
    if (!this.moved && !this.consumed && sample.t - this.started < 250) {
      if (this.options.mode === "direct") this.locate(sample);
      if (this.count === 2 && sample.t - this.lastTwoTap < 300) {
        this.zoom = { scale: 1, offsetX: 0, offsetY: 0 };
        this.options.onZoom?.(this.zoom);
        this.lastTwoTap = -Infinity;
      } else {
        this.click(this.count === 3 ? 1 : this.count === 2 ? 2 : 0);
        if (this.count === 1) this.lastTap = sample.t;
        if (this.count === 2) this.lastTwoTap = sample.t;
      }
    }
    this.pair = null;
  }

  cancel(): void {
    this.reset();
  }
  reset(): void {
    this.clearTimer();
    this.release();
    this.fingers.clear();
    this.pair = null;
    this.lastTap = this.lastTwoTap = -Infinity;
  }
  private clearTimer(): void {
    if (this.timer !== undefined) this.clock.clearTimeout(this.timer);
    this.timer = undefined;
  }
  private pairPosition() {
    const [a, b] = [...this.fingers.values()].map((finger) => finger.last);
    return { center: { x: (a!.x + b!.x) / 2, y: (a!.y + b!.y) / 2 }, distance: distance(a!, b!) };
  }
  private locate(point: Position): void {
    const geometry = this.options.geometry();
    if (!geometry) return;
    this.cursor = normalizedPosition(point, geometry);
    this.options.onCursor?.(this.cursor);
    this.emit("move");
  }
  private emit(action: "move" | "down" | "up", button: 0 | 1 | 2 = 0): void {
    this.options.send({ type: "pointer", action, ...this.cursor, button });
  }
  private press(): void {
    this.emit("down");
    this.held = true;
  }
  private release(): void {
    if (this.held) this.emit("up");
    this.held = false;
  }
  private click(button: 0 | 1 | 2): void {
    this.emit("down", button);
    this.emit("up", button);
  }
}

export function attachTouchInput(video: HTMLVideoElement, gestures: TouchGestures): () => void {
  const active = new Set<number>();
  const listener = (event: PointerEvent) => {
    if (event.pointerType !== "touch") return;
    event.preventDefault();
    const sample = { id: event.pointerId, x: event.clientX, y: event.clientY, t: event.timeStamp };
    if (event.type === "lostpointercapture" && !active.has(event.pointerId)) return;
    if (event.type === "pointerdown") {
      active.add(event.pointerId);
      video.setPointerCapture(event.pointerId);
      gestures.down(sample);
    } else if (event.type === "pointermove") gestures.move(sample);
    else if (event.type === "pointerup") {
      active.delete(event.pointerId);
      gestures.up(sample);
    } else {
      active.clear();
      gestures.cancel();
    }
  };
  const events = [
    "pointerdown",
    "pointermove",
    "pointerup",
    "pointercancel",
    "lostpointercapture",
  ] as const;
  for (const type of events) video.addEventListener(type, listener, { passive: false });
  const reset = () => {
    active.clear();
    gestures.reset();
  };
  window.addEventListener("blur", reset);
  document.addEventListener("visibilitychange", reset);
  return () => {
    for (const type of events) video.removeEventListener(type, listener);
    window.removeEventListener("blur", reset);
    document.removeEventListener("visibilitychange", reset);
    gestures.reset();
  };
}
