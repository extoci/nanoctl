export type VideoSample = {
  id: string;
  bytes: number;
  frames: number | undefined;
  timestamp: number;
};

export function videoRates(
  current: VideoSample,
  previous: VideoSample | null,
  reportedFps?: number,
) {
  const elapsed = previous ? current.timestamp - previous.timestamp : 0;
  const comparable =
    previous !== null &&
    current.id === previous.id &&
    elapsed > 0 &&
    current.bytes >= previous.bytes;
  const frames =
    comparable &&
    current.frames !== undefined &&
    previous.frames !== undefined &&
    current.frames >= previous.frames
      ? ((current.frames - previous.frames) * 1_000) / elapsed
      : null;
  return {
    fps: reportedFps ?? frames,
    bitrateKbps: comparable ? ((current.bytes - previous.bytes) * 8) / elapsed : null,
  };
}
