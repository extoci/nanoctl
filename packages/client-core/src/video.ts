import { VideoReassembler, parseVideoChunk, type VideoChunk } from "@nanoctl/protocol";
import { FrameSequencer, type FrameSequencerOutcome } from "./frame-sequencer";

export type VideoInputOutcome = FrameSequencerOutcome & {
  readonly droppedIncompleteSequence?: number;
};

function emptyOutcome(): VideoInputOutcome {
  return {
    emitted: [],
    requestKeyframe: false,
    duplicatesDropped: 0,
    unreferenceableDropped: 0,
    buffered: 0,
  };
}

/**
 * Browser-independent video input pipeline: canonical wire parse -> bounded reassembly
 * -> bounded reference ordering. Malformed bytes throw before mutating either stage.
 */
export class VideoStreamProcessor {
  private readonly reassembler = new VideoReassembler();
  private readonly sequencer: FrameSequencer;

  constructor(reorderCapacity = 2) {
    this.sequencer = new FrameSequencer(reorderCapacity);
  }

  pushEncodedChunk(bytes: Uint8Array): VideoInputOutcome {
    return this.pushChunk(parseVideoChunk(bytes));
  }

  pushChunk(chunk: VideoChunk): VideoInputOutcome {
    const reassembled = this.reassembler.push(chunk);
    if (reassembled.complete === undefined) {
      return {
        ...emptyOutcome(),
        droppedIncompleteSequence: reassembled.droppedIncompleteSequence,
      };
    }
    const sequenced = this.sequencer.push(reassembled.complete);
    return {
      ...sequenced,
      droppedIncompleteSequence: reassembled.droppedIncompleteSequence,
    };
  }
}
