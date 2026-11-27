import {
  encodeVideoControlMessage,
  parseVideoControlMessage,
  type AccessUnit,
} from "@nanoctl/protocol";
import { VideoStreamProcessor } from "./video";
import { VideoLossWindow } from "./video-loss";

export type V4VideoReceiverOptions = {
  readonly videoDelta: RTCDataChannel;
  readonly videoReference: RTCDataChannel;
  readonly onReceivedAccessUnit?: (unit: AccessUnit) => void;
  readonly onAccessUnit: (unit: AccessUnit) => void;
  readonly sendKeyframeRequest: (bytes: Uint8Array) => void;
  readonly onError?: (error: unknown) => void;
};

function messageBytes(data: unknown): Uint8Array {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  throw new Error("unsupported video data-channel message type");
}

/**
 * Browser-independent v4 video lane receiver. Both video lanes feed one bounded logical
 * stream processor; lane transport reliability is deliberately not part of the reference
 * decision. Decode output is emitted only in a valid reference chain.
 */
const REORDER_WAIT_MS = 180;
const KEYFRAME_RETRY_MS = 250;

export class V4VideoReceiver {
  private readonly processor = new VideoStreamProcessor();
  private reorderTimer: ReturnType<typeof setTimeout> | undefined;
  private reorderSequence: number | undefined;
  private keyframeTimer: ReturnType<typeof setTimeout> | undefined;
  private disposed = false;
  private watermark: number | undefined;
  private readonly linkLoss = new VideoLossWindow();
  private readonly deltaHandler = (event: MessageEvent) => this.handle(event.data);
  private readonly referenceHandler = (event: MessageEvent) => this.handle(event.data);

  constructor(private readonly options: V4VideoReceiverOptions) {
    for (const channel of [options.videoDelta, options.videoReference]) {
      channel.binaryType = "arraybuffer";
    }
    options.videoDelta.onmessage = this.deltaHandler;
    options.videoReference.onmessage = this.referenceHandler;
  }

  handle(data: unknown): void {
    if (this.disposed) return;
    try {
      const bytes = messageBytes(data);
      if (bytes[1] === 0x42) {
        const message = parseVideoControlMessage(bytes);
        if (message.kind === "sequenceWatermark") {
          const next = message.nextSequence;
          const previous = this.watermark;
          if (previous === undefined || ((next - previous) | 0) > 0) {
            this.watermark = next;
            this.updateReorderTimer();
          }
        }
        return;
      }
      const outcome = this.processor.pushEncodedChunk(bytes);
      if (outcome.received) {
        this.linkLoss.observe(outcome.received.streamSequence);
        this.options.onReceivedAccessUnit?.(outcome.received);
      }
      for (const unit of outcome.emitted) {
        if (unit.keyframe) this.stopKeyframeRetry();
        this.options.onAccessUnit(unit);
        if (this.processor.waitingForReference) return;
      }
      if (outcome.requestKeyframe) {
        this.requestKeyframe();
      } else {
        this.updateReorderTimer();
      }
    } catch (error) {
      this.options.onError?.(error);
    }
  }

  requestKeyframe(): void {
    if (this.disposed) return;
    this.processor.requireKeyframe();
    this.stopReorderTimer();
    this.sendKeyframeRequestWithRetry();
  }

  private sendKeyframeRequestWithRetry(): void {
    if (this.disposed || this.keyframeTimer !== undefined) return;
    this.keyframeTimer = setTimeout(() => {
      this.keyframeTimer = undefined;
      this.sendKeyframeRequestWithRetry();
    }, KEYFRAME_RETRY_MS);
    try {
      this.options.sendKeyframeRequest(encodeVideoControlMessage({ kind: "keyframeRequest" }));
    } catch (error) {
      this.options.onError?.(error);
    }
  }

  private updateReorderTimer(): void {
    const watermarkGap =
      this.watermark !== undefined && this.processor.isBehindWatermark(this.watermark);
    if (!this.processor.hasFrameGap && !watermarkGap) {
      this.stopReorderTimer();
      if (!this.processor.waitingForReference) this.stopKeyframeRetry();
      return;
    }
    if (this.keyframeTimer !== undefined) return;
    const expected = this.processor.nextExpectedSequence;
    if (this.reorderTimer !== undefined && this.reorderSequence === expected) return;
    this.stopReorderTimer();
    this.reorderSequence = expected;
    this.reorderTimer = setTimeout(() => {
      this.stopReorderTimer();
      // A deadline tells us to start recovery. Late deltas may still complete
      // the current chain before a new reference arrives.
      this.sendKeyframeRequestWithRetry();
    }, REORDER_WAIT_MS);
  }

  private stopReorderTimer(): void {
    clearTimeout(this.reorderTimer);
    this.reorderTimer = undefined;
    this.reorderSequence = undefined;
  }

  private stopKeyframeRetry(): void {
    clearTimeout(this.keyframeTimer);
    this.keyframeTimer = undefined;
  }

  takeLinkLoss(): number | undefined {
    return this.linkLoss.take();
  }

  dispose(): void {
    this.disposed = true;
    this.stopReorderTimer();
    this.stopKeyframeRetry();
    if (this.options.videoDelta.onmessage === this.deltaHandler) {
      this.options.videoDelta.onmessage = null;
    }
    if (this.options.videoReference.onmessage === this.referenceHandler) {
      this.options.videoReference.onmessage = null;
    }
  }
}
