export function sourceContextOptions(mode = "low-latency"): CanvasRenderingContext2DSettings {
  if (mode === "legacy") return {};
  if (mode === "low-latency") return { alpha: false, desynchronized: true };
  throw new Error(`Invalid source renderer: ${mode}`);
}
