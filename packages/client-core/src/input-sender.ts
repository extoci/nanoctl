import {
  encodeInputMessage,
  type GamepadState,
  type InputMessage,
  type KeyIdentity,
  type NormalizedPoint,
  type PointerButton,
} from "@nanoctl/protocol";

export type V4InputChannel = {
  readonly bufferedAmount: number;
  readonly readyState: RTCDataChannelState;
  send(data: ArrayBufferView<ArrayBuffer>): void;
};
export type V4InputSendResult = "sent" | "closed" | "backpressured";

const CONTROL_BUFFER_BUDGET = 64 * 1024;
const MOTION_BUFFER_BUDGET = 256;

function channelBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  const copy = new Uint8Array(new ArrayBuffer(bytes.byteLength));
  copy.set(bytes);
  return copy;
}

type ControlPayload =
  | {
      readonly kind: "pointerButton";
      readonly down: boolean;
      readonly button: PointerButton;
      readonly point: NormalizedPoint;
    }
  | {
      readonly kind: "pointerWheel";
      readonly point: NormalizedPoint;
      readonly deltaX: number;
      readonly deltaY: number;
    }
  | {
      readonly kind: "key";
      readonly down: boolean;
      readonly browserRepeat: boolean;
      readonly identity: KeyIdentity;
    }
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "releaseAll" }
  | { readonly kind: "heartbeat" };

/**
 * Browser-independent v4 input lane writer. Control, pointer motion, and
 * gamepad motion each own an independent u32 sequence: one analog stream must
 * never make the other stream's newest state look stale. Loss-tolerant motion
 * is never queued behind a saturated SCTP buffer; stale analog state is
 * dropped instead of delayed. Reliable control is bounded, and saturation is
 * surfaced to the caller because silently dropping discrete input would
 * violate ordering semantics.
 */
export class V4InputSender {
  private controlSequence = 1;
  private pointerSequence = 1;
  private gamepadSequence = 1;

  constructor(
    private readonly control: V4InputChannel,
    private readonly motion: V4InputChannel,
  ) {}

  pointerMove(point: NormalizedPoint): V4InputSendResult {
    return this.sendMotion({ kind: "pointerMove", sequence: this.nextPointerSequence(), point });
  }

  gamepadSnapshot(pads: readonly GamepadState[]): V4InputSendResult {
    return this.sendMotion({
      kind: "gamepadSnapshot",
      sequence: this.nextGamepadSequence(),
      pads,
    });
  }

  pointerButton(down: boolean, button: PointerButton, point: NormalizedPoint): V4InputSendResult {
    return this.sendControl({ kind: "pointerButton", down, button, point });
  }

  pointerWheel(point: NormalizedPoint, deltaX: number, deltaY: number): V4InputSendResult {
    return this.sendControl({ kind: "pointerWheel", point, deltaX, deltaY });
  }

  key(down: boolean, browserRepeat: boolean, identity: KeyIdentity): V4InputSendResult {
    return this.sendControl({ kind: "key", down, browserRepeat, identity });
  }

  text(text: string): V4InputSendResult {
    return this.sendControl({ kind: "text", text });
  }

  heartbeat(): V4InputSendResult {
    return this.sendControl({ kind: "heartbeat" });
  }

  /** Safety input is allowed past the normal control budget: a 6-byte release
   * must still be attempted when the lane is congested. */
  releaseAll(): V4InputSendResult {
    return this.sendControl({ kind: "releaseAll" }, true);
  }

  private sendControl(payload: ControlPayload, safetyCritical = false): V4InputSendResult {
    if (this.control.readyState !== "open") return "closed";
    const message = {
      ...payload,
      sequence: this.nextControlSequence(),
    } as InputMessage;
    const bytes = encodeInputMessage(message);
    if (!safetyCritical && this.control.bufferedAmount + bytes.byteLength > CONTROL_BUFFER_BUDGET) {
      return "backpressured";
    }
    this.control.send(channelBytes(bytes));
    return "sent";
  }

  private sendMotion(message: InputMessage): V4InputSendResult {
    if (this.motion.readyState !== "open") return "closed";
    const bytes = encodeInputMessage(message);
    if (this.motion.bufferedAmount + bytes.byteLength > MOTION_BUFFER_BUDGET) {
      return "backpressured";
    }
    this.motion.send(channelBytes(bytes));
    return "sent";
  }

  private nextControlSequence(): number {
    const sequence = this.controlSequence;
    this.controlSequence = (this.controlSequence + 1) >>> 0;
    return sequence;
  }

  private nextPointerSequence(): number {
    const sequence = this.pointerSequence;
    this.pointerSequence = (this.pointerSequence + 1) >>> 0;
    return sequence;
  }

  private nextGamepadSequence(): number {
    const sequence = this.gamepadSequence;
    this.gamepadSequence = (this.gamepadSequence + 1) >>> 0;
    return sequence;
  }
}
