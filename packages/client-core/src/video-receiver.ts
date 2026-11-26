import { encodeVideoControlMessage, type AccessUnit } from "@nanoctl/protocol";
import { VideoStreamProcessor } from "./video";

export type V4VideoReceiverOptions = {
  readonly videoDelta: RTCDataChannel;
  readonly videoReference: RTCDataChannel;
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
export class V4VideoReceiver {
  private readonly processor = new VideoStreamProcessor();
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
    try {
      const outcome = this.processor.pushEncodedChunk(messageBytes(data));
      for (const unit of outcome.emitted) {
        this.options.onAccessUnit(unit);
      }
      if (outcome.requestKeyframe) {
        this.requestKeyframe();
      }
    } catch (error) {
      this.options.onError?.(error);
    }
  }

  requestKeyframe(): void {
    this.options.sendKeyframeRequest(encodeVideoControlMessage({ kind: "keyframeRequest" }));
  }

  dispose(): void {
    if (this.options.videoDelta.onmessage === this.deltaHandler) {
      this.options.videoDelta.onmessage = null;
    }
    if (this.options.videoReference.onmessage === this.referenceHandler) {
      this.options.videoReference.onmessage = null;
    }
  }
}
