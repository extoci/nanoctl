import type { AccessUnit } from "@nanoctl/protocol";

export type FrameSequencerOutcome = {
  /** Complete access units that are now safe to decode in emitted order. */
  readonly emitted: AccessUnit[];
  /** Set exactly when the machine enters the awaiting-keyframe state, not on every dropped delta. */
  readonly requestKeyframe: boolean;
  readonly duplicatesDropped: number;
  readonly unreferenceableDropped: number;
  readonly buffered: number;
};

const MODULO_HALF = 0x80000000;
const MAX_REORDER_BYTES = 4 * 1024 * 1024;

function validSequence(sequence: number): boolean {
  return Number.isInteger(sequence) && sequence >= 0 && sequence <= 0xffffffff;
}

function sequenceDistance(sequence: number, expected: number): number {
  return (sequence - expected) >>> 0;
}

function emptyOutcome(): {
  emitted: AccessUnit[];
  requestKeyframe: boolean;
  duplicatesDropped: number;
  unreferenceableDropped: number;
  buffered: number;
} {
  return {
    emitted: [],
    requestKeyframe: false,
    duplicatesDropped: 0,
    unreferenceableDropped: 0,
    buffered: 0,
  };
}

/**
 * Orders complete access units before decode without unbounded jitter buffering.
 *
 * The video delta lane is unordered and loss-tolerant. Because every delta depends on
 * the previous delta, a future frame may be held only long enough for a tiny reorder
 * window; once the bounded window overflows, the machine declares the reference chain
 * broken exactly once and waits for a keyframe instead of decoding corrupt output.
 */
export class FrameSequencer {
  readonly reorderCapacity: number;
  private expectedSequence: number | undefined;
  private awaitingKeyframe = false;
  private readonly reorderBuffer: AccessUnit[] = [];
  private reorderBytes = 0;

  constructor(reorderCapacity = 16) {
    if (!Number.isInteger(reorderCapacity) || reorderCapacity < 0 || reorderCapacity > 16)
      throw new Error("reorder capacity must be an integer between 0 and 16");
    this.reorderCapacity = reorderCapacity;
  }

  get waitingForReference(): boolean {
    return this.awaitingKeyframe;
  }

  isBehindWatermark(next: number): boolean {
    if (this.awaitingKeyframe) return false;
    if (this.expectedSequence === undefined) return true;
    const distance = sequenceDistance(next, this.expectedSequence);
    return distance > 0 && distance < MODULO_HALF;
  }

  requireKeyframe(): void {
    this.reorderBuffer.length = 0;
    this.reorderBytes = 0;
    this.awaitingKeyframe = true;
  }

  push(unit: AccessUnit): FrameSequencerOutcome {
    if (!validSequence(unit.streamSequence)) throw new Error("invalid access-unit sequence");
    const outcome = emptyOutcome();
    if (unit.keyframe) {
      if (
        this.expectedSequence !== undefined &&
        sequenceDistance(unit.streamSequence, this.expectedSequence) >= MODULO_HALF
      ) {
        outcome.duplicatesDropped = 1;
        outcome.buffered = this.reorderBuffer.length;
        return outcome;
      }
      // Reference and delta frames travel on independent lanes, so a delta that
      // references this keyframe may validly arrive before the keyframe itself.
      // Discard only older generations; keep newer buffered units and drain them
      // once the keyframe establishes the reference.
      for (let index = this.reorderBuffer.length - 1; index >= 0; index -= 1) {
        const buffered = this.reorderBuffer[index];
        if (
          buffered === undefined ||
          sequenceDistance(buffered.streamSequence, unit.streamSequence) >= MODULO_HALF
        ) {
          this.reorderBuffer.splice(index, 1);
          this.reorderBytes -= buffered?.payload.length ?? 0;
        }
      }
      this.awaitingKeyframe = false;
      this.expectedSequence = (unit.streamSequence + 1) >>> 0;
      outcome.emitted.push(unit);
      this.drainContiguous(outcome.emitted);
      outcome.buffered = this.reorderBuffer.length;
      return outcome;
    }

    if (this.awaitingKeyframe || this.expectedSequence === undefined) {
      outcome.unreferenceableDropped += 1;
      if (!this.awaitingKeyframe) {
        this.awaitingKeyframe = true;
        outcome.requestKeyframe = true;
      }
      outcome.buffered = this.reorderBuffer.length;
      return outcome;
    }

    const expected = this.expectedSequence;
    const distance = sequenceDistance(unit.streamSequence, expected);
    if (distance === 0) {
      outcome.emitted.push(unit);
      this.expectedSequence = (expected + 1) >>> 0;
      this.drainContiguous(outcome.emitted);
      outcome.buffered = this.reorderBuffer.length;
      return outcome;
    }

    const isFuture = distance < MODULO_HALF;
    if (!isFuture) {
      outcome.duplicatesDropped += 1;
      outcome.buffered = this.reorderBuffer.length;
      return outcome;
    }
    if (this.reorderBuffer.some((buffered) => buffered.streamSequence === unit.streamSequence)) {
      outcome.duplicatesDropped += 1;
      outcome.buffered = this.reorderBuffer.length;
      return outcome;
    }
    if (
      this.reorderBuffer.length >= this.reorderCapacity ||
      this.reorderBytes + unit.payload.length > MAX_REORDER_BYTES
    ) {
      outcome.unreferenceableDropped += this.reorderBuffer.length + 1;
      this.requireKeyframe();
      outcome.requestKeyframe = true;
      outcome.buffered = 0;
      return outcome;
    }

    const insertAt = this.reorderBuffer.findIndex(
      (buffered) => sequenceDistance(buffered.streamSequence, unit.streamSequence) < MODULO_HALF,
    );
    if (insertAt === -1) this.reorderBuffer.push(unit);
    else this.reorderBuffer.splice(insertAt, 0, unit);
    this.reorderBytes += unit.payload.length;
    outcome.buffered = this.reorderBuffer.length;
    return outcome;
  }

  private drainContiguous(emitted: AccessUnit[]): void {
    for (;;) {
      const expected = this.expectedSequence;
      if (expected === undefined) return;
      const index = this.reorderBuffer.findIndex(
        (buffered) => buffered.streamSequence === expected,
      );
      if (index === -1) return;
      const next = this.reorderBuffer[index];
      if (next === undefined) return;
      this.reorderBuffer.splice(index, 1);
      this.reorderBytes -= next.payload.length;
      emitted.push(next);
      this.expectedSequence = (expected + 1) >>> 0;
    }
  }
}
