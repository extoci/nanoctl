/** Compares idle video keepalive wire cost with sequence-watermark heartbeats. */
const TRANSPORT_OVERHEAD = 20 + 8 + 37 + 12 + 16; // IPv4/UDP/DTLS/SCTP/DATA
const IDLE_KEEPALIVE_SECONDS = 2;
const SAMPLE_SECONDS = 60;

// Measured with ffmpeg/libx264 fast + zerolatency at CRF 18 for repeated
// identical 1920x1080 frames: every post-IDR unchanged access unit is 201
// bytes. One in every thirty-two idle frames also carries the eight-byte
// timestamp because timestamps are emitted every sixteenth sequence number.
const UNCHANGED_AU_BYTES = 201;
const FRAME_HEADER_BYTES = 5;
const TIMESTAMP_AMORTIZED_BYTES = 8 / 32;
const HEARTBEAT_BYTES = 5;

const samples = SAMPLE_SECONDS / IDLE_KEEPALIVE_SECONDS;
const before =
  samples *
  (UNCHANGED_AU_BYTES + FRAME_HEADER_BYTES + TIMESTAMP_AMORTIZED_BYTES + TRANSPORT_OVERHEAD);
const after = samples * (HEARTBEAT_BYTES + TRANSPORT_OVERHEAD);
const kbps = (bytes: number) => (bytes * 8) / SAMPLE_SECONDS / 1000;
const saved = (1 - after / before) * 100;

console.log(
  `unchanged 1080p desktop idle: ${kbps(before).toFixed(3)} -> ${kbps(after).toFixed(3)} kbps (${saved.toFixed(1)}% less)`,
);
if (after >= before) process.exitCode = 1;
