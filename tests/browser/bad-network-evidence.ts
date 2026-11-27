export function longestGap(timestamps: readonly number[], start: number, end: number): number {
  let previous = start;
  let longest = 0;
  for (const stamp of timestamps) {
    if (stamp < start || stamp > end) continue;
    longest = Math.max(longest, stamp - previous);
    previous = stamp;
  }
  return Math.max(longest, end - previous);
}

export function mostInOneSecond(timestamps: readonly number[]): number {
  let first = 0;
  let peak = 0;
  for (let last = 0; last < timestamps.length; last += 1) {
    while (timestamps[last]! - timestamps[first]! >= 1_000) first += 1;
    peak = Math.max(peak, last - first + 1);
  }
  return peak;
}

export function earlyMediaExits(lines: readonly string[], sampleStartEpochMs: number): string[] {
  const events = lines.filter(
    (line) => line.includes("v4 media pipeline ") || line.includes("Viewer disconnected reason="),
  );
  const timestamp = (line: string) => Date.parse(line.split(" ", 1)[0] ?? "");
  return events.filter((line, index) => {
    if (line.includes("reason=media-failed")) return true;
    if (!line.includes("v4 media pipeline stopped")) return false;
    const next = events[index + 1];
    const endedDuringAttach =
      next !== undefined &&
      next.includes("Viewer disconnected reason=viewer-ended") &&
      timestamp(line) < sampleStartEpochMs &&
      timestamp(next) < sampleStartEpochMs;
    return !endedDuringAttach;
  });
}
