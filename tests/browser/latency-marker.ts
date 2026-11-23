export function markerBits(sequence: number, input: number): number[] {
  const checksum = ((sequence >>> 8) ^ sequence ^ input ^ 0xa5) & 0xff;
  const word = (sequence << 16) | (input << 8) | checksum;
  return [1, 0, ...Array.from({ length: 32 }, (_, i) => (word >>> (31 - i)) & 1), 0, 1];
}

export function readMarker(bits: number[]): { sequence: number; input: number } | undefined {
  if (bits.length !== 36 || bits[0] !== 1 || bits[1] !== 0 || bits[34] !== 0 || bits[35] !== 1)
    return;
  let word = 0;
  for (let i = 2; i < 34; i++) {
    if (bits[i] < 0) return;
    word = (word << 1) | bits[i];
  }
  const sequence = word >>> 16;
  const input = (word >>> 8) & 0xff;
  if ((word & 0xff) !== (((sequence >>> 8) ^ sequence ^ input ^ 0xa5) & 0xff)) return;
  return { sequence, input };
}

export function readMarkerRow(pixels: Uint8ClampedArray) {
  const width = pixels.length / 4;
  let left = -1;
  let right = -1;
  for (let x = 0; x < width; x++) {
    const red = pixels[x * 4];
    const green = pixels[x * 4 + 1];
    const blue = pixels[x * 4 + 2];
    if (red < 80 && green > 150 && blue > 150) left = x;
    if (left >= 0 && red > 150 && green < 80 && blue > 150) {
      right = x;
      break;
    }
  }
  if (left < 0 || right - left < 144) return;
  return readMarker(
    Array.from({ length: 36 }, (_, i) => {
      const x = Math.floor(left + ((right - left) * (i + 0.5)) / 36);
      const red = pixels[x * 4];
      return red > 192 ? 1 : red < 64 ? 0 : -1;
    }),
  );
}

export function summarize(samples: number[]) {
  if (samples.length === 0) throw new Error("No latency samples");
  const sorted = [...samples].sort((a, b) => a - b);
  const quantile = (p: number) => sorted[Math.ceil(sorted.length * p) - 1];
  return {
    count: sorted.length,
    p50: quantile(0.5),
    p95: quantile(0.95),
    p99: quantile(0.99),
    max: sorted[sorted.length - 1],
  };
}
