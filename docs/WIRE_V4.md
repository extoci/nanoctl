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
| `nanoctl.video-delta.v4`     | unordered, loss-tolerant | non-keyframe video chunks                                                    |
| `nanoctl.video-reference.v4` | reliable, ordered        | keyframe video chunks and reference-recovery control                         |

Every sender-lane pair owns an independent `u32` sequence counter. On loss-tolerant lanes receivers
drop non-newer sequences using wrapping sequence comparison. Reliable lanes keep ordering through the
transport; their sequence is diagnostic and liveness metadata, not permission to reorder discrete input.

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
u16 buttons   # XInput-compatible button bitfield
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
capture timestamp. Reassembly keeps at most one incomplete access unit, rejects frames larger than
16 MiB, drops duplicate parts, and abandons an incomplete unit when a new sequence arrives.

A keyframe resets the receiver's reference chain. A missing or out-of-order delta invalidates
dependent deltas exactly once and produces a keyframe request decision; timers and rate limits belong
to the transport/session layer, not the wire parser.

## Size and memory limits

- Single video chunk payload: 48 KiB.
- Complete video access unit: 16 MiB.
- Gamepads: 4 slots.
- Text input message: 1 KiB.
- Key code/key fields: 64 bytes each.
- Reassembly holds at most one incomplete video access unit.

Implementations must enforce these limits before allocation wherever possible and must cap total
allocation even when lengths are internally consistent.
