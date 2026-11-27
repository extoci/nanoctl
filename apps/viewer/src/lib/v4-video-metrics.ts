export class V4VideoMetrics {
  private previousArrival: { sequence: number; captureUs: number; arrivalMs: number } | undefined;
  private previousDelivered: number | undefined;
  private deliveredFrames = 0;
  private skippedFrames = 0;
  private jitterMs: number | null = null;
  private resolution: string | null = null;
  private decodeTotalMs = 0;
  private decodeSamples = 0;

  received(sequence: number, captureUs: number, arrivalMs: number): void {
    const previous = this.previousArrival;
    if (previous) {
      const distance = (sequence - previous.sequence) >>> 0;
      if (distance === 0 || distance >= 0x80000000) return;
      if (distance === 1 && captureUs >= previous.captureUs) {
        const variation = Math.abs(
          arrivalMs - previous.arrivalMs - (captureUs - previous.captureUs) / 1_000,
        );
        const jitter = this.jitterMs ?? 0;
        this.jitterMs = jitter + (variation - jitter) / 16;
      }
    }
    this.previousArrival = { sequence, captureUs, arrivalMs };
  }

  delivered(sequence: number): void {
    const previous = this.previousDelivered;
    if (previous !== undefined) {
      const distance = (sequence - previous) >>> 0;
      if (distance === 0 || distance >= 0x80000000) return;
      this.skippedFrames += distance - 1;
    }
    this.deliveredFrames += 1;
    this.previousDelivered = sequence;
  }

  decoded(width: number, height: number, elapsedMs: number | undefined): void {
    this.resolution = `${width}×${height}`;
    if (elapsedMs !== undefined) {
      this.decodeTotalMs += elapsedMs;
      this.decodeSamples += 1;
    }
  }

  snapshot() {
    const total = this.deliveredFrames + this.skippedFrames;
    return {
      resolution: this.resolution,
      jitterMs: this.jitterMs,
      skippedFrames: total > 0 ? this.skippedFrames : null,
      skippedPercent: total > 0 ? (100 * this.skippedFrames) / total : null,
      decodeMs: this.decodeSamples > 0 ? this.decodeTotalMs / this.decodeSamples : null,
      decodeSamples: this.decodeSamples,
    };
  }
}

export function selectedIceLink(report: RTCStatsReport) {
  let pair: RTCIceCandidatePairStats | undefined;
  const nominated: RTCIceCandidatePairStats[] = [];
  for (const stats of report.values()) {
    if (stats.type === "transport") {
      const selected = report.get((stats as RTCTransportStats).selectedCandidatePairId ?? "");
      if (selected?.type === "candidate-pair") pair = selected as RTCIceCandidatePairStats;
    }
    if (stats.type === "candidate-pair" && stats.nominated && stats.state === "succeeded") {
      nominated.push(stats as RTCIceCandidatePairStats);
    }
  }
  if (!pair && nominated.length === 1) pair = nominated[0];
  const local = pair ? report.get(pair.localCandidateId)?.candidateType : undefined;
  const remote = pair ? report.get(pair.remoteCandidateId)?.candidateType : undefined;
  const rtt = pair?.currentRoundTripTime;
  return {
    rttMs: typeof rtt === "number" && Number.isFinite(rtt) && rtt >= 0 ? rtt * 1_000 : null,
    route:
      local === "relay" || remote === "relay" ? "relay" : local && remote ? "direct" : "unknown",
  };
}

export type V4VideoDebugSnapshot = ReturnType<V4VideoMetrics["snapshot"]> &
  ReturnType<typeof selectedIceLink>;

export const v4DebugVideo = new WeakMap<
  HTMLVideoElement,
  { metrics: V4VideoMetrics; readIceStats: () => Promise<RTCStatsReport | undefined> }
>();

export async function readV4VideoDebug(
  video: HTMLVideoElement,
): Promise<V4VideoDebugSnapshot | undefined> {
  const current = v4DebugVideo.get(video);
  if (!current) return;
  const report = await current.readIceStats().catch(() => undefined);
  if (v4DebugVideo.get(video) !== current) return;
  return {
    ...current.metrics.snapshot(),
    ...(report ? selectedIceLink(report) : { rttMs: null, route: "unknown" }),
  };
}
