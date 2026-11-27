// Finalize sequence gaps only after the reorder window has passed. Track
// complete access units even when the decoder is waiting for a keyframe.
const REORDER_TOLERANCE = 16;

export class VideoLossWindow {
  private latestSequence: number | undefined;
  private latestIndex = 0;
  private firstUnscored = 0;
  private readonly pending = new Set<number>();
  private received = 0;
  private missing = 0;

  observe(sequence: number): void {
    const distance = this.latestSequence === undefined ? 0 : (sequence - this.latestSequence) | 0;
    const index = this.latestIndex + distance;
    if (index < this.firstUnscored) return;
    this.pending.add(index);
    if (this.latestSequence === undefined || distance > 0) {
      this.latestSequence = sequence;
      this.latestIndex = index;
    }
    const settled = this.latestIndex - REORDER_TOLERANCE;
    if (settled < this.firstUnscored) return;
    let arrived = 0;
    for (const pending of this.pending) {
      if (pending <= settled) {
        arrived += 1;
        this.pending.delete(pending);
      }
    }
    this.received += arrived;
    this.missing += settled - this.firstUnscored + 1 - arrived;
    this.firstUnscored = settled + 1;
  }

  take(): number | undefined {
    const total = this.received + this.missing;
    if (total === 0) return;
    const loss = this.missing / total;
    this.received = 0;
    this.missing = 0;
    return loss;
  }
}
