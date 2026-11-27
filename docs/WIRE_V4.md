# nanoctl wire protocol v4

This is the canonical clean-slate binary protocol implemented by `crates/nanoctl-core`.
It is intentionally incompatible with v2/v3. Datagram transports preserve message boundaries,
so messages have no outer length field or magic bytes. Every multi-byte integer is little-endian.

Every message starts with:

```text
u8 version = 4
u8 kind
```

Parsers must reject unknown versions/kinds, impossible lengths, invalid flags, invalid UTF-8,
out-of-range values, and non-finite floating-point values. A malformed message must not partially
mutate receiver state. The Rust implementation is the reference until generated cross-language
fixtures exist.

## Lanes

| Lane                         | Reliability              | Messages                                                                     |
| ---------------------------- | ------------------------ | ---------------------------------------------------------------------------- |
| `nanoctl.control.v4`         | reliable, ordered        | key, text, pointer button/wheel, release, heartbeat, future session commands |
| `nanoctl.motion.v4`          | unordered, loss-tolerant | pointer movement and gamepad latest-state snapshots                          |
| `nanoctl.video-delta.v4`     | unordered, loss-tolerant | delta chunks and reference chunks                                            |
| `nanoctl.video-reference.v4` | reliable, unordered      | keyframe video chunks and reference-recovery control                         |

Every independent input stream owns a `u32` sequence counter: reliable control is one stream, while
pointer motion and gamepad snapshots use separate counters even though they share the motion lane. On
loss-tolerant lanes receivers drop non-newer sequences within the same stream using wrapping sequence
comparison. The reliable control lane keeps order through the transport. Its sequence is diagnostic
and liveness metadata, not permission to reorder discrete input. A newer complete keyframe can be
used before an older one arrives. Receivers discard reference frames older than their decode position.

## Input messages

Input kinds are `0x01..=0x08`. After version and kind, every input message carries `u32 sequence`.

### Pointer move — kind `0x01`, 10 bytes

```text
u16 x  # normalized 0..=65535 over the selected remote desktop width
u16 y  # normalized 0..=65535 over the selected remote desktop height
```

### Pointer button — kind `0x02`, 12 bytes

```text
u8 action  # 0 = up, 1 = down
u8 button  # 0 = left, 1 = middle, 2 = right
u16 x
u16 y
```

### Pointer wheel — kind `0x03`, 18 bytes

```text
u16 x
u16 y
f32 delta_x  # logical wheel units, positive right
f32 delta_y  # logical wheel units, positive down
```

### Key — kind `0x04`, variable length

```text
u8 flags       # bit 0 set = key up, clear = key down; bit 1 = browser-generated repeat
u8 code_len    # <= 64 bytes
u8 key_len     # 1..=64 bytes
u8[code_len] code  # UTF-8 web keyboard code, may be empty
u8[key_len] key    # UTF-8 web keyboard key value
```

The host ignores browser-generated repeats for input injection. While a non-modifier key is held,
the host deterministically synthesizes typematic repeats after 500 ms at 33 ms intervals. Delayed
host ticks skip missed deadlines; they never emit catch-up bursts.

### Text — kind `0x05`, variable length

```text
u16 len  # 1..=1024 bytes
u8[len] text  # UTF-8
```

### Release all — kind `0x06`, 6 bytes

No payload beyond the common header and sequence. It is idempotent. Disconnect, permission loss,
lane failure, or input liveness timeout has the same host effect: release all held keys/buttons and
neutralize gamepads.

### Heartbeat — kind `0x07`, 6 bytes

No payload beyond the common header and sequence. Input state times out after 2 seconds without any
valid input-lane message. Heartbeats keep an otherwise idle but still focused control session live.

### Gamepad snapshot — kind `0x08`, variable length

```text
u8 count  # 0..=4
entry[count]
```

Each entry is 14 bytes:

```text
u8 slot       # 0..=3, unique within the snapshot
u8 kind       # 0 = Xbox 360, 1 = DualShock 4, 2 = DualSense
u16 buttons   # XInput-compatible button bitfield; bit 15 is the PlayStation touchpad click
u8 left_trigger
u8 right_trigger
i16 left_x    # positive right
i16 left_y    # positive up
i16 right_x
i16 right_y
```

A snapshot is the complete desired controller state. Empty snapshots detach all controllers. Because
motion lanes are unordered and loss-tolerant, stale snapshots are dropped rather than delayed or
replayed.

## Video chunks

Video uses kind `0x40`. The fixed header is 20 bytes:

```text
u32 stream_sequence  # one complete encoded access-unit sequence
u16 part             # zero-based chunk index
u16 parts            # 1..=512 chunks in this access unit
u8 codec             # 1 = H.264, 2 = HEVC, 3 = AV1
u8 flags             # bit 0 = keyframe/IDR
u64 capture_time_us  # monotonic capture-pipeline timestamp
u8[] payload         # 1..=49152 bytes; length comes from the datagram
```

All chunks in one access unit must carry identical sequence, part count, codec, keyframe flag, and
capture timestamp. Reassembly keeps up to three incomplete references and sixteen incomplete deltas,
with one shared 16 MiB payload budget. It ignores duplicate parts and evicts the oldest pending unit
of the same kind when its limit is reached. Delta fragments cannot evict a pending reference.
Complete single-chunk units do not evict pending fragments.
After the receiver advances its decode position, older fragments leave the reassembly buffer and
late copies of those fragments are discarded on arrival.

Over SCTP, the host sends up to 1,100 payload bytes in each message to avoid SCTP fragmentation.
It gives the outstanding reference 350 ms to drain. If it is still pending, the next recovery
keyframe goes on the expiring delta lane. Both copies must fit within the delta lane's byte budget.
The reliable lane takes a new reference once the previous one drains.

The host can send a second copy of a video chunk on the delta lane with the same sequence and
part index. Both copies count toward the send budget. When the reliable lane can accept a new
reference, the host sends its original there even if the delta lane has no room for the copy.

A keyframe resets the receiver's reference chain. The receiver starts its reorder timer when the
expected frame is incomplete or a fragment of a later frame arrives. When the timer expires, the
receiver requests a keyframe. Late deltas can still fill the gap and stop retries. If the reorder
window fills or the decoder loses its reference, the receiver discards dependent deltas and waits
for a fresh keyframe. The transport or session layer owns the request timers and rate limits.

After a gap persists for 70 ms, the viewer can request the missing delta on the reference lane.
Kind `0x43` carries its `u32 stream_sequence`. The host retains up to 16 recent deltas of up to
8 KiB each and resends a matching delta at most once within 500 ms of its original send. It skips
the resend if it would fill more than half of the delta lane. The 180 ms keyframe deadline still
applies if the delta does not arrive.

Before the first complete reference, the viewer sends a keyframe request if it has waited 350 ms.
It retries until a reference arrives, even if there have been no video messages.

## Size and memory limits

- Single video chunk payload: 48 KiB.
- Complete video access unit: 16 MiB.
- Gamepads: 4 slots.
- Text input message: 1 KiB.
- Key code/key fields: 64 bytes each.
- Reassembly holds at most three incomplete references and sixteen incomplete deltas, with 16 MiB of
  payload in total.

Implementations must enforce these limits before allocation wherever possible and must cap total
allocation even when lengths are internally consistent.

### Idle video watermark

The host sends `[0x04, 0x42, next_sequence: u32 little-endian]` on the reliable
video reference channel when video stops advancing. It identifies the sequence
after the last access unit accepted by the transport and does not consume a
sequence number. Receivers retain the highest watermark using wrapping sequence comparison. A
receiver gives late deltas the normal reorder grace period before requesting a keyframe if its
expected sequence is behind the watermark.
