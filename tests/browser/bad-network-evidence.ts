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
