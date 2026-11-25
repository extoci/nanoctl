/** Compares the original fixed 17-byte data-channel video header with compact optional headers. */
const TRANSPORT_OVERHEAD = 20 + 8 + 37 + 12 + 16; // IPv4/UDP/DTLS/SCTP/DATA
const CHUNK = 48 * 1024;
const FPS = 60;
const SECONDS = 30;

const partsFor = (bytes: number) => Math.max(1, Math.ceil(bytes / CHUNK));
const oldBytes = (au: number) => partsFor(au) * (TRANSPORT_OVERHEAD + 17) + au;
const newBytes = (au: number, sequence: number, keyframe = false) => {
  const parts = partsFor(au);
  const timestamped = keyframe || sequence % 16 === 0;
  const header = 5 + (parts > 1 ? 4 : 0) + (timestamped ? 8 : 0);
  return parts * (TRANSPORT_OVERHEAD + header) + au;
};

for (const [name, deltaAu, keyframeAu] of [
  ["small desktop deltas", 700, 50_000],
  ["continuous gameplay motion", 2_500, 100_000],
  ["worst-case noise", 20_000, 150_000],
] as const) {
  let before = 0;
  let after = 0;
  for (let frame = 0; frame < FPS * SECONDS; frame++) {
    const keyframe = frame === 0;
    const au = keyframe ? keyframeAu : deltaAu;
    before += oldBytes(au);
    after += newBytes(au, frame, keyframe);
  }
  const saved = (1 - after / before) * 100;
  console.log(
    `${name}: ${((before * 8) / SECONDS / 1000).toFixed(1)} -> ${((after * 8) / SECONDS / 1000).toFixed(1)} kbps (${saved.toFixed(2)}% less)`,
  );
  if (after >= before) process.exitCode = 1;
}
