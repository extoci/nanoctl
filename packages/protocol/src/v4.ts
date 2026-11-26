export const V4_WIRE_VERSION = 4 as const;
export const MAX_KEY_FIELD_BYTES = 64;
export const MAX_TEXT_BYTES = 1024;
export const MAX_GAMEPADS = 4;
export const MAX_VIDEO_CHUNK_BYTES = 48 * 1024;
export const MAX_VIDEO_PARTS = 512;
export const MAX_ACCESS_UNIT_BYTES = 16 * 1024 * 1024;
export const VIDEO_HEADER_BYTES = 20;

export type WireErrorCode =
  | "empty"
  | "truncated"
  | "unsupportedVersion"
  | "unknownKind"
  | "invalidLength"
  | "invalidFlags"
  | "invalidUtf8"
  | "invalidValue";

export class WireError extends Error {
  readonly code: WireErrorCode;

  constructor(code: WireErrorCode, message: string) {
    super(message);
    this.name = "WireError";
    this.code = code;
  }
}

function fail(code: WireErrorCode, message: string): never {
  throw new WireError(code, message);
}

function byteAt(bytes: Uint8Array, offset: number): number {
  const value = bytes[offset];
  if (value === undefined) fail("truncated", "truncated wire message");
  return value;
}

function u16At(bytes: Uint8Array, offset: number): number {
  if (offset + 2 > bytes.length) fail("truncated", "truncated wire message");
  return byteAt(bytes, offset) | (byteAt(bytes, offset + 1) << 8);
}

function u32At(bytes: Uint8Array, offset: number): number {
  if (offset + 4 > bytes.length) fail("truncated", "truncated wire message");
  return (
    (byteAt(bytes, offset) |
      (byteAt(bytes, offset + 1) << 8) |
      (byteAt(bytes, offset + 2) << 16) |
      (byteAt(bytes, offset + 3) << 24)) >>>
    0
  );
}

function u64At(bytes: Uint8Array, offset: number): number {
  if (offset + 8 > bytes.length) fail("truncated", "truncated wire message");
  let value = 0n;
  for (let index = 0; index < 8; index += 1) {
    value |= BigInt(byteAt(bytes, offset + index)) << BigInt(index * 8);
  }
  if (value > BigInt(Number.MAX_SAFE_INTEGER))
    fail("invalidValue", "u64 timestamp exceeds safe JavaScript integer range");
  return Number(value);
}

function f32At(bytes: Uint8Array, offset: number): number {
  if (offset + 4 > bytes.length) fail("truncated", "truncated wire message");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return view.getFloat32(offset, true);
}

class ByteWriter {
  private readonly out: number[] = [];

  u8(value: number): void {
    this.out.push(value & 0xff);
  }

  u16(value: number): void {
    this.u8(value);
    this.u8(value >>> 8);
  }

  u32(value: number): void {
    this.u8(value);
    this.u8(value >>> 8);
    this.u8(value >>> 16);
    this.u8(value >>> 24);
  }

  u64(value: number): void {
    let remaining = BigInt(value);
    for (let index = 0; index < 8; index += 1) {
      this.u8(Number(remaining & 0xffn));
      remaining >>= 8n;
    }
  }

  f32(value: number): void {
    const bytes = new Uint8Array(4);
    new DataView(bytes.buffer).setFloat32(0, value, true);
    for (const byte of bytes) this.u8(byte);
  }

  bytes(value: Uint8Array): void {
    for (const byte of value) this.u8(byte);
  }

  finish(): Uint8Array {
    return Uint8Array.from(this.out);
  }
}

function versioned(bytes: Uint8Array, minKind: number, maxKind: number): number {
  if (bytes.length === 0) fail("empty", "empty wire message");
  if (bytes.length < 2) fail("truncated", "truncated wire message");
  const version = byteAt(bytes, 0);
  if (version !== V4_WIRE_VERSION)
    fail("unsupportedVersion", `unsupported wire version ${version}`);
  const kind = byteAt(bytes, 1);
  if (kind < minKind || kind > maxKind) fail("unknownKind", `unknown wire message kind ${kind}`);
  return kind;
}

function utf8(bytes: Uint8Array, offset: number, length: number): string {
  if (offset + length > bytes.length) fail("truncated", "truncated wire message");
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(
      bytes.subarray(offset, offset + length),
    );
  } catch {
    fail("invalidUtf8", "wire message is not valid UTF-8");
  }
}

function validUint(value: number, bits: 8 | 16 | 32): boolean {
  return Number.isInteger(value) && value >= 0 && value < 2 ** bits;
}

function validI16(value: number): boolean {
  return Number.isInteger(value) && value >= -32768 && value <= 32767;
}

function validTimestamp(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

export type NormalizedPoint = { readonly x: number; readonly y: number };
export type PointerButton = "left" | "middle" | "right";
export type ControllerKind = "xbox360" | "dualshock4" | "dualsense";
export type KeyIdentity = { readonly code: string; readonly key: string };
export type GamepadState = {
  readonly slot: number;
  readonly kind: ControllerKind;
  readonly buttons: number;
  readonly leftTrigger: number;
  readonly rightTrigger: number;
  readonly leftX: number;
  readonly leftY: number;
  readonly rightX: number;
  readonly rightY: number;
};

export type InputMessage =
  | { readonly kind: "pointerMove"; readonly sequence: number; readonly point: NormalizedPoint }
  | {
      readonly kind: "pointerButton";
      readonly sequence: number;
      readonly down: boolean;
      readonly button: PointerButton;
      readonly point: NormalizedPoint;
    }
  | {
      readonly kind: "pointerWheel";
      readonly sequence: number;
      readonly point: NormalizedPoint;
      readonly deltaX: number;
      readonly deltaY: number;
    }
  | {
      readonly kind: "key";
      readonly sequence: number;
      readonly down: boolean;
      readonly browserRepeat: boolean;
      readonly identity: KeyIdentity;
    }
  | { readonly kind: "text"; readonly sequence: number; readonly text: string }
  | { readonly kind: "releaseAll"; readonly sequence: number }
  | { readonly kind: "heartbeat"; readonly sequence: number }
  | {
      readonly kind: "gamepadSnapshot";
      readonly sequence: number;
      readonly pads: readonly GamepadState[];
    };

const pointerButtons: Record<PointerButton, number> = { left: 0, middle: 1, right: 2 };
const controllerKinds: Record<ControllerKind, number> = { xbox360: 0, dualshock4: 1, dualsense: 2 };

function pointerButtonFromWire(value: number): PointerButton {
  if (value === 0) return "left";
  if (value === 1) return "middle";
  if (value === 2) return "right";
  fail("invalidValue", "unknown pointer button");
}

function controllerKindFromWire(value: number): ControllerKind {
  if (value === 0) return "xbox360";
  if (value === 1) return "dualshock4";
  if (value === 2) return "dualsense";
  fail("invalidValue", "unknown controller kind");
}

function encodeText(value: string): Uint8Array {
  return new TextEncoder().encode(value);
}

function validateInput(message: InputMessage): void {
  if (!validUint(message.sequence, 32)) fail("invalidValue", "invalid input sequence");
  switch (message.kind) {
    case "pointerMove":
      if (!validUint(message.point.x, 16) || !validUint(message.point.y, 16))
        fail("invalidValue", "invalid normalized point");
      break;
    case "pointerButton":
      if (!validUint(message.point.x, 16) || !validUint(message.point.y, 16))
        fail("invalidValue", "invalid normalized point");
      break;
    case "pointerWheel":
      if (!validUint(message.point.x, 16) || !validUint(message.point.y, 16))
        fail("invalidValue", "invalid normalized point");
      if (!Number.isFinite(message.deltaX) || !Number.isFinite(message.deltaY))
        fail("invalidValue", "non-finite wheel delta");
      break;
    case "key":
      if (
        encodeText(message.identity.code).length > MAX_KEY_FIELD_BYTES ||
        encodeText(message.identity.key).length === 0 ||
        encodeText(message.identity.key).length > MAX_KEY_FIELD_BYTES
      )
        fail("invalidLength", "invalid key field length");
      break;
    case "text":
      if (encodeText(message.text).length === 0 || encodeText(message.text).length > MAX_TEXT_BYTES)
        fail("invalidLength", "invalid text length");
      break;
    case "gamepadSnapshot": {
      if (message.pads.length > MAX_GAMEPADS) fail("invalidLength", "too many gamepads");
      const slots = new Set<number>();
      for (const pad of message.pads) {
        if (
          !validUint(pad.slot, 8) ||
          pad.slot >= MAX_GAMEPADS ||
          slots.has(pad.slot) ||
          !validUint(pad.buttons, 16) ||
          !validUint(pad.leftTrigger, 8) ||
          !validUint(pad.rightTrigger, 8) ||
          !validI16(pad.leftX) ||
          !validI16(pad.leftY) ||
          !validI16(pad.rightX) ||
          !validI16(pad.rightY)
        )
          fail("invalidValue", "invalid gamepad state");
        slots.add(pad.slot);
      }
      break;
    }
    default:
      break;
  }
}

export function encodeInputMessage(message: InputMessage): Uint8Array {
  validateInput(message);
  const out = new ByteWriter();
  out.u8(V4_WIRE_VERSION);
  switch (message.kind) {
    case "pointerMove":
      out.u8(0x01);
      out.u32(message.sequence);
      out.u16(message.point.x);
      out.u16(message.point.y);
      break;
    case "pointerButton":
      out.u8(0x02);
      out.u32(message.sequence);
      out.u8(message.down ? 1 : 0);
      out.u8(pointerButtons[message.button]);
      out.u16(message.point.x);
      out.u16(message.point.y);
      break;
    case "pointerWheel":
      out.u8(0x03);
      out.u32(message.sequence);
      out.u16(message.point.x);
      out.u16(message.point.y);
      out.f32(message.deltaX);
      out.f32(message.deltaY);
      break;
    case "key": {
      out.u8(0x04);
      out.u32(message.sequence);
      out.u8((message.down ? 0 : 1) | (message.browserRepeat ? 2 : 0));
      const code = encodeText(message.identity.code);
      const key = encodeText(message.identity.key);
      out.u8(code.length);
      out.u8(key.length);
      out.bytes(code);
      out.bytes(key);
      break;
    }
    case "text": {
      out.u8(0x05);
      out.u32(message.sequence);
      const text = encodeText(message.text);
      out.u16(text.length);
      out.bytes(text);
      break;
    }
    case "releaseAll":
      out.u8(0x06);
      out.u32(message.sequence);
      break;
    case "heartbeat":
      out.u8(0x07);
      out.u32(message.sequence);
      break;
    case "gamepadSnapshot":
      out.u8(0x08);
      out.u32(message.sequence);
      out.u8(message.pads.length);
      for (const pad of message.pads) {
        out.u8(pad.slot);
        out.u8(controllerKinds[pad.kind]);
        out.u16(pad.buttons);
        out.u8(pad.leftTrigger);
        out.u8(pad.rightTrigger);
        out.u16(pad.leftX);
        out.u16(pad.leftY);
        out.u16(pad.rightX);
        out.u16(pad.rightY);
      }
      break;
  }
  return out.finish();
}

function pointAt(bytes: Uint8Array, offset: number): NormalizedPoint {
  return { x: u16At(bytes, offset), y: u16At(bytes, offset + 2) };
}

export function parseInputMessage(bytes: Uint8Array): InputMessage {
  const kind = versioned(bytes, 0x01, 0x08);
  const sequence = u32At(bytes, 2);
  switch (kind) {
    case 0x01:
      if (bytes.length !== 10) fail("invalidLength", "invalid pointer move length");
      return { kind: "pointerMove", sequence, point: pointAt(bytes, 6) };
    case 0x02: {
      if (bytes.length !== 12) fail("invalidLength", "invalid pointer button length");
      const action = byteAt(bytes, 6);
      if (action !== 0 && action !== 1) fail("invalidValue", "invalid pointer button action");
      return {
        kind: "pointerButton",
        sequence,
        down: action === 1,
        button: pointerButtonFromWire(byteAt(bytes, 7)),
        point: pointAt(bytes, 8),
      };
    }
    case 0x03: {
      if (bytes.length !== 18) fail("invalidLength", "invalid pointer wheel length");
      const deltaX = f32At(bytes, 10);
      const deltaY = f32At(bytes, 14);
      if (!Number.isFinite(deltaX) || !Number.isFinite(deltaY))
        fail("invalidValue", "non-finite wheel delta");
      return { kind: "pointerWheel", sequence, point: pointAt(bytes, 6), deltaX, deltaY };
    }
    case 0x04: {
      if (bytes.length < 9) fail("truncated", "truncated key message");
      const flags = byteAt(bytes, 6);
      if ((flags & ~3) !== 0) fail("invalidFlags", "invalid key flags");
      const codeLength = byteAt(bytes, 7);
      const keyLength = byteAt(bytes, 8);
      if (
        codeLength > MAX_KEY_FIELD_BYTES ||
        keyLength === 0 ||
        keyLength > MAX_KEY_FIELD_BYTES ||
        bytes.length !== 9 + codeLength + keyLength
      )
        fail("invalidLength", "invalid key length");
      return {
        kind: "key",
        sequence,
        down: (flags & 1) === 0,
        browserRepeat: (flags & 2) !== 0,
        identity: {
          code: utf8(bytes, 9, codeLength),
          key: utf8(bytes, 9 + codeLength, keyLength),
        },
      };
    }
    case 0x05: {
      if (bytes.length < 8) fail("truncated", "truncated text message");
      const length = u16At(bytes, 6);
      if (length === 0 || length > MAX_TEXT_BYTES || bytes.length !== 8 + length)
        fail("invalidLength", "invalid text length");
      return { kind: "text", sequence, text: utf8(bytes, 8, length) };
    }
    case 0x06:
      if (bytes.length !== 6) fail("invalidLength", "invalid release-all length");
      return { kind: "releaseAll", sequence };
    case 0x07:
      if (bytes.length !== 6) fail("invalidLength", "invalid heartbeat length");
      return { kind: "heartbeat", sequence };
    case 0x08: {
      if (bytes.length < 7) fail("truncated", "truncated gamepad snapshot");
      const count = byteAt(bytes, 6);
      if (count > MAX_GAMEPADS || bytes.length !== 7 + count * 14)
        fail("invalidLength", "invalid gamepad snapshot length");
      const slots = new Set<number>();
      const pads: GamepadState[] = [];
      for (let index = 0; index < count; index += 1) {
        const offset = 7 + index * 14;
        const slot = byteAt(bytes, offset);
        if (slot >= MAX_GAMEPADS || slots.has(slot))
          fail("invalidValue", "duplicate or out-of-range gamepad slot");
        slots.add(slot);
        pads.push({
          slot,
          kind: controllerKindFromWire(byteAt(bytes, offset + 1)),
          buttons: u16At(bytes, offset + 2),
          leftTrigger: byteAt(bytes, offset + 4),
          rightTrigger: byteAt(bytes, offset + 5),
          leftX: (u16At(bytes, offset + 6) << 16) >> 16,
          leftY: (u16At(bytes, offset + 8) << 16) >> 16,
          rightX: (u16At(bytes, offset + 10) << 16) >> 16,
          rightY: (u16At(bytes, offset + 12) << 16) >> 16,
        });
      }
      return { kind: "gamepadSnapshot", sequence, pads };
    }
    default:
      fail("unknownKind", "unknown input kind");
  }
}

export type VideoCodec = "h264" | "hevc" | "av1";
export type VideoChunk = {
  readonly streamSequence: number;
  readonly part: number;
  readonly parts: number;
  readonly keyframe: boolean;
  readonly codec: VideoCodec;
  readonly captureTimeUs: number;
  readonly payload: Uint8Array;
};
export type AccessUnit = Omit<VideoChunk, "part" | "parts">;

const codecs: Record<VideoCodec, number> = { h264: 1, hevc: 2, av1: 3 };

function codecFromWire(value: number): VideoCodec {
  if (value === 1) return "h264";
  if (value === 2) return "hevc";
  if (value === 3) return "av1";
  fail("invalidValue", "unknown video codec");
}

function validateVideoChunk(chunk: VideoChunk): void {
  if (
    !validUint(chunk.streamSequence, 32) ||
    !validUint(chunk.part, 16) ||
    !validUint(chunk.parts, 16) ||
    !validTimestamp(chunk.captureTimeUs)
  )
    fail("invalidValue", "invalid video chunk metadata");
  if (chunk.parts === 0 || chunk.parts > MAX_VIDEO_PARTS || chunk.part >= chunk.parts)
    fail("invalidValue", "invalid video chunk part count");
  if (chunk.payload.length === 0 || chunk.payload.length > MAX_VIDEO_CHUNK_BYTES)
    fail("invalidLength", "invalid video chunk payload length");
}

export function encodeVideoChunk(chunk: VideoChunk): Uint8Array {
  validateVideoChunk(chunk);
  const out = new ByteWriter();
  out.u8(V4_WIRE_VERSION);
  out.u8(0x40);
  out.u32(chunk.streamSequence);
  out.u16(chunk.part);
  out.u16(chunk.parts);
  out.u8(codecs[chunk.codec]);
  out.u8(chunk.keyframe ? 1 : 0);
  out.u64(chunk.captureTimeUs);
  out.bytes(chunk.payload);
  return out.finish();
}

export function parseVideoChunk(bytes: Uint8Array): VideoChunk {
  versioned(bytes, 0x40, 0x40);
  if (bytes.length < VIDEO_HEADER_BYTES) fail("truncated", "truncated video chunk");
  const part = u16At(bytes, 6);
  const parts = u16At(bytes, 8);
  const flags = byteAt(bytes, 11);
  if ((flags & ~1) !== 0) fail("invalidFlags", "invalid video flags");
  const payloadLength = bytes.length - VIDEO_HEADER_BYTES;
  if (parts === 0 || parts > MAX_VIDEO_PARTS || part >= parts)
    fail("invalidValue", "invalid video chunk part count");
  if (payloadLength === 0 || payloadLength > MAX_VIDEO_CHUNK_BYTES)
    fail("invalidLength", "invalid video chunk payload length");
  return {
    streamSequence: u32At(bytes, 2),
    part,
    parts,
    codec: codecFromWire(byteAt(bytes, 10)),
    keyframe: (flags & 1) !== 0,
    captureTimeUs: u64At(bytes, 12),
    payload: bytes.slice(VIDEO_HEADER_BYTES),
  };
}

export type ReassemblyOutcome = {
  readonly droppedIncompleteSequence?: number;
  readonly complete?: AccessUnit;
};

type PartialAccessUnit = {
  readonly partCount: number;
  received: number;
  byteLength: number;
  readonly keyframe: boolean;
  readonly codec: VideoCodec;
  readonly captureTimeUs: number;
  readonly parts: (Uint8Array | undefined)[];
};

export class VideoReassembler {
  private currentSequence: number | undefined;
  private current: PartialAccessUnit | undefined;

  push(chunk: VideoChunk): ReassemblyOutcome {
    validateVideoChunk(chunk);
    const outcome: {
      droppedIncompleteSequence?: number;
      complete?: AccessUnit;
    } = {};
    if (this.currentSequence !== chunk.streamSequence) {
      if (this.currentSequence !== undefined && this.current !== undefined)
        outcome.droppedIncompleteSequence = this.currentSequence;
      this.currentSequence = chunk.streamSequence;
      this.current = {
        partCount: chunk.parts,
        received: 0,
        byteLength: 0,
        keyframe: chunk.keyframe,
        codec: chunk.codec,
        captureTimeUs: chunk.captureTimeUs,
        parts: Array.from({ length: chunk.parts }),
      };
    }
    const current = this.current;
    if (current === undefined) fail("invalidValue", "missing current access unit");
    if (
      current.partCount !== chunk.parts ||
      current.keyframe !== chunk.keyframe ||
      current.codec !== chunk.codec ||
      current.captureTimeUs !== chunk.captureTimeUs
    )
      fail("invalidValue", "inconsistent video access-unit metadata");
    if (current.parts[chunk.part] !== undefined) return outcome;
    current.byteLength += chunk.payload.length;
    if (current.byteLength > MAX_ACCESS_UNIT_BYTES) {
      this.current = undefined;
      this.currentSequence = undefined;
      fail("invalidValue", "video access unit exceeds size limit");
    }
    current.parts[chunk.part] = chunk.payload.slice();
    current.received += 1;
    if (current.received === current.partCount) {
      const payload = new Uint8Array(current.byteLength);
      let offset = 0;
      for (const part of current.parts) {
        if (part === undefined) fail("invalidValue", "missing complete access-unit part");
        payload.set(part, offset);
        offset += part.length;
      }
      outcome.complete = {
        streamSequence: chunk.streamSequence,
        keyframe: current.keyframe,
        codec: current.codec,
        captureTimeUs: current.captureTimeUs,
        payload,
      };
      this.current = undefined;
      this.currentSequence = undefined;
    }
    return outcome;
  }
}

export type ReferenceDecision = "accept" | "dropAwaitingKeyframe" | "requestKeyframe";

export class ReferenceTracker {
  private expectedSequence: number | undefined;
  private awaitingKeyframe = false;

  inspect(streamSequence: number, keyframe: boolean): ReferenceDecision {
    if (!validUint(streamSequence, 32)) fail("invalidValue", "invalid stream sequence");
    if (keyframe) {
      this.expectedSequence = (streamSequence + 1) >>> 0;
      this.awaitingKeyframe = false;
      return "accept";
    }
    if (this.awaitingKeyframe) return "dropAwaitingKeyframe";
    if (this.expectedSequence === undefined) {
      this.awaitingKeyframe = true;
      return "requestKeyframe";
    }
    if (streamSequence === this.expectedSequence) {
      this.expectedSequence = (this.expectedSequence + 1) >>> 0;
      return "accept";
    }
    this.awaitingKeyframe = true;
    return "requestKeyframe";
  }
}
