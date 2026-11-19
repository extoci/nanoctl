export const MAX_RELIABLE_CONTROL_BUFFER_BYTES = 256 * 1024;

export function reliableControlBufferIsSaturated(bufferedAmount: number): boolean {
  return !Number.isFinite(bufferedAmount) || bufferedAmount > MAX_RELIABLE_CONTROL_BUFFER_BYTES;
}
