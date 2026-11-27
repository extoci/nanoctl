import {
  VideoReassembler,
  parseVideoChunk,
  type AccessUnit,
  type VideoChunk,
} from "@nanoctl/protocol";
import { FrameSequencer, type FrameSequencerOutcome } from "./frame-sequencer";

export type VideoInputOutcome = FrameSequencerOutcome & {
  readonly droppedIncompleteSequence?: number;
  readonly received?: AccessUnit;
};

function incompleteOutcome(buffered: number): VideoInputOutcome {
  return {
    emitted: [],
    requestKeyframe: false,
    duplicatesDropped: 0,
    unreferenceableDropped: 0,
    buffered,
  };
}

/**
 * Browser-independent video input pipeline: canonical wire parse -> bounded reassembly
 * -> bounded reference ordering. Malformed bytes throw before mutating either stage.
 */
export class VideoStreamProcessor {
  private readonly reassembler = new VideoReassembler();
  private readonly sequencer: FrameSequencer;

  constructor(reorderCapacity?: number) {
    this.sequencer = new FrameSequencer(reorderCapacity);
  }

  get waitingForReference(): boolean {
    return this.sequencer.waitingForReference;
  }

  isBehindWatermark(next: number): boolean {
    return this.sequencer.isBehindWatermark(next);
  }

  requireKeyframe(): void {
    this.sequencer.requireKeyframe();
  }

  pushEncodedChunk(bytes: Uint8Array): VideoInputOutcome {
    return this.pushChunk(parseVideoChunk(bytes));
  }

  pushChunk(chunk: VideoChunk): VideoInputOutcome {
    const reassembled = this.reassembler.push(chunk);
    if (reassembled.complete === undefined) {
      return {
        ...incompleteOutcome(this.sequencer.bufferedFrames),
        droppedIncompleteSequence: reassembled.droppedIncompleteSequence,
      };
    }
    const sequenced = this.sequencer.push(reassembled.complete);
    return {
      ...sequenced,
      received: reassembled.complete,
      droppedIncompleteSequence: reassembled.droppedIncompleteSequence,
    };
  }
}
