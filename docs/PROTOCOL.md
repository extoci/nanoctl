# Protocol

All requests go to the host agent, normally over tailnet HTTPS on port 7337.
Every request and WebSocket upgrade is authorized through Tailscale whois.
There are no bearer credentials, enrollment, polling or hosted signaling.
Types and validators live in `packages/protocol`.

## HTTP

- `GET /` and hashed static assets: embedded viewer, CSP and anti-framing headers.
- `GET /v1/info`: no-store JSON:

  ```json
  {
    "hostName": "desktop",
    "platform": "linux",
    "version": "3.0.0-alpha.11",
    "displays": [
      {
        "id": "59",
        "name": "Display",
        "width": 1920,
        "height": 1080,
        "scaleFactor": 1,
        "primary": true
      }
    ],
    "remoteInput": true,
    "capabilities": {
      "protocolVersion": 2,
      "platform": "linux",
      "architecture": "x64",
      "codecs": ["av1", "hevc", "h264"],
      "encoderBackends": ["software-openh264"],
      "ready": true,
      "readiness": "ready",
      "input": true,
      "remoteInput": true,
      "binaryInput": true,
      "clipboard": true,
      "systemAudio": true,
      "displays": []
    },
    "viewer": {
      "identity": {
        "login": "owner@example.com",
        "displayName": "Owner",
        "tags": []
      },
      "mayControl": true
    }
  }
  ```

  Actual `capabilities.displays` repeats the enumerated display list.
  `remoteInput` reflects config, viewer policy and platform availability.

- Unauthorized requests return 403 with an explanation including the identity seen.

## WebSocket

Upgrade `GET /v1/connect` using `wss:` for HTTPS, otherwise `ws:`, with matching Host
and Origin. Each message/frame is capped at 64 KiB. One socket is one session.

Server hello has shape `{type:"hello", sessionId:string, host:HostInfo}`, where
`host` is the complete info **object**, not a serialized string.
Both directions exchange an object-valued v2 envelope:

```json
{
  "type": "signal",
  "envelope": {
    "version": 2,
    "sessionId": "<hello sessionId>",
    "controllerEpoch": "<browser epoch>",
    "hostEpoch": "",
    "generation": 0,
    "sequence": 1,
    "sender": "controller",
    "sentAt": 1700000000000,
    "payload": { "type": "offer", "sdp": "..." }
  }
}
```

The first browser signal must be an offer. Preserve the controller epoch and increase
sequence numbers; host replies use their own nonempty host epoch and `sender:"host"`.
Ignore stale session/epoch/generation traffic. ICE restart stays within the live
lifecycle; socket reconnect creates a new session.

Payloads: `offer` / `answer` with `sdp`; `ice-candidate` with `candidate`,
`sdpMid`, `sdpMLineIndex`, `usernameFragment`; `ice-complete`; `end` with `reason`.
Terminal socket messages are `{"type":"end","reason":"replaced"}` (or another reason).
A newer authorized socket replaces the active session. Socket closure ends the session.

## Media and input

WebRTC uses empty ICE servers, tailnet host candidates, AV1, HEVC or H.264 RTP (the
host answers with the first offered codec in `capabilities.codecs`), DTLS-SRTP video and
SCTP data channels. No application-level TURN/STUN is used; Tailscale may relay its
underlying encrypted transport through DERP. Reliable control carries discrete commands;
the lossy pointer lane avoids accumulating stale motion. View-only sessions never enable
host input or clipboard access. System audio is available to view-only sessions too.

A `quality` control message may include optional positive `maxWidth`/`maxHeight`
physical-pixel caps in addition to rate caps. They describe the viewer's rendered video
viewport. Hosts clamp them to configured encode dimensions, fit the source aspect ratio
inside them, scale the bitrate ceiling by the measured 0.8 power of the viewport/source pixel
ratio, and force a keyframe when they change. Omitted dimensions preserve the configured
encode size, so older iOS and web viewers remain compatible.

### Video over data channel

A viewer may also create `nanoctl.video.v1` (unordered, `maxPacketLifeTime: 80`) and
`nanoctl.video-idr.v1` (reliable, ordered) to receive compressed access units directly,
bypassing the RTP jitter buffer. H.264 and HEVC payloads use Annex-B; AV1 payloads use
low-overhead OBU temporal units (the formats WebCodecs consumes). Each message is
`[u32 seq][u8 flags][optional u16 part][optional u16 parts][optional u64 sent_ms][payload chunk]`
with all integers little-endian. Flags bit0 marks a keyframe, bit1 marks fragmentation (the
part fields are present), and bit2 marks a sender timestamp. Unfragmented delta frames
normally cost only five header bytes; keyframes and every sixteenth delta carry `sent_ms`
for transit telemetry. `parts` splits large access units into ~48 KiB chunks.
Deltas ride the unordered bounded-retransmit channel and keyframes the reliable one. A sequence gap breaks
the reference chain: the viewer drops post-gap deltas and sends the text `KF` on the
video channel until a keyframe arrives. While `nanoctl.video.v1` is open the agent
suppresses delta-frame writes on the RTP track automatically — a viewer decoding the
data channel drops the RTP copy, so those samples are duplicate bandwidth. Keyframes
still go to RTP, which keeps RTCP round-trip and loss reports meaningful for adaptive
bitrate. When the video channel closes, the agent resumes full RTP delivery and forces
a keyframe so the resumed track's reference chain is fresh.

### System audio

When `capabilities.systemAudio` is true, the viewer offers an additional recvonly audio
transceiver. The agent sends a separate Opus RTP track: 48 kHz stereo, 20 ms frames,
128 kbps restricted-lowdelay encoding, fmtp `minptime=10;useinbandfec=1;stereo=1`.
RTCP is drained independently, and bounded audio queues drop stale work rather than
blocking video. Capture starts after the peer connects and stops with the session.

`features.system_audio` defaults to true. Configuration files saved by control-plane releases
(they contain `control_plane_url`) pinned it to false; that saved value is treated as unset.
Linux requires `parec` and a reachable
PulseAudio/PipeWire-pulse server; capture reads `@DEFAULT_MONITOR@`, not the microphone.
Windows uses WASAPI output-device loopback. macOS system audio is unsupported and
advertised false. Opus is bundled statically, so no runtime libopus installation is needed.
The browser starts muted; **Tap for sound** or **Enable sound** opts in for the session, and
**Mute sound** disables playback again. If the browser blocks playback, **Tap to resume sound**
retries it with a gesture. Audio is host-to-viewer
only and does not require permission to control the host.

### Clipboard

Text clipboard sync uses a viewer-created reliable ordered SCTP channel labeled
`nanoctl.clipboard.v1`. Both directions carry JSON `{"type":"clipboard","text":"..."}`.
The serialized UTF-8 message must be smaller than 64 KiB. Invalid, binary, non-string
text and oversized messages are ignored; oversized host clipboard text is logged and skipped.
Sync requires `features.clipboard` (default true), a media build with an available OS
clipboard backend, and permission to control the host. View-only sessions get neither direction.
The agent keeps its clipboard owner thread alive and polls every 350 ms. Viewer-originated
text is not echoed back. The browser writes host text when focused, retaining only the latest
pending text otherwise. It reads local text on paste and focus/visibility regain, suppressing
echoes. Clipboard permissions and a secure browser context are required; denied browser APIs
are silently ignored. A remote paste shortcut sends local clipboard text before its key event.

### Compact input

Input uses compact binary wire v3 when `capabilities.binaryInput` is true. SCTP datagrams already preserve message boundaries, so byte zero is the kind; there is no magic or version framing. Integers are little-endian except wheel deltas, which use the exact significant big-endian bytes of each IEEE-754 f64 with trailing zero bytes omitted; a packed nibble byte stores the two 0–8 byte lengths. Malformed binary messages are invalid input.

Kinds: `1` pointer motion with normalized unsigned u16 `x`/`y` (5 bytes); `2` full gamepad snapshot — u8 sequence, 4-bit slot mask, then 13 bytes per masked slot in ascending order: u16 buttons, u8 triggers, four i16 axes, and DualShock flags (bit 0 present, bits 1/2/3 left-trigger/right-trigger/touchpad); `3` pointer button down/up (6 bytes); `4` wheel input with exact compact f64 deltas (6–22 bytes); `5` keyboard input — flags byte (bit0 up, bit1 repeat, bit2 named-key form), followed by either a named-key id or the UTF-8 `key` bytes; `6` text input with a u16 byte length; `7` release-all (1 byte); `8` optional keepalive probe (1 byte — current clients do not send periodic application probes because SCTP and media traffic already keep the association alive); `9` audio enablement (2 bytes); `10` display selection with a u8 identifier length. Kind `11` is a one-byte gamepad heartbeat: it proves the viewer is alive without replacing controller state. Kind `12` is a sparse gamepad snapshot with the same sequence and slot-mask header; each masked slot starts with a field mask and encodes only nonzero fields (bit0 buttons as unsigned LEB128, bits1/2 nonzero triggers, bits3–6 nonzero i16 axes little-endian, bit7 DualShock flags). Unchanged pads mix heartbeats with occasional reliable full snapshots so state cannot wedge inputs; hosts neutralize active pads after 750 ms without reports. JSON input forms are not part of wire v3.

### Gamepads

When `capabilities.gamepad` is true, an opted-in viewer with control permission sends
binary gamepad snapshots with a u8 sequence; stale or reordered snapshots are discarded.
Kind-2 full snapshots rebase all four slots and unplug slots absent from the mask. They ride
the reliable control channel (`nanoctl.control.v1`) because they are the baseline for sparse
state. Kind-12 sparse snapshots and heartbeats ride the unordered bounded-retransmit gamepad channel
(`nanoctl.gamepad.v1`, `maxPacketLifeTime: 100`); sparse snapshots name only slots whose state differs from the last full baseline,
and omitted slots keep their current host state. Each sparse slot entry remains absolute
rather than delta-encoded: zero-valued buttons, triggers, and axes are omitted behind the
field mask, so any received entry completely repairs that slot even after earlier packet
loss. Empty detach snapshots also use the reliable control channel because they are one-shot
control transitions rather than repeatable state. The host keeps the latest pointer move and
latest gamepad snapshot in separate slots, so a per-frame snapshot never replaces the
pointer move sent in the same frame.

Slots are 0–3, buttons use XInput flags, triggers are unsigned 8-bit values, and axes are
signed 16-bit values with positive Y up. The host presents slots with DualShock flags as
DualShock 4 controllers and all other slots as Xbox 360 controllers. The viewer sends a
change immediately and repeats it only briefly as a final-state anchor while bounded SCTP
retransmission repairs loss, then keeps an unchanged pad alive with heartbeats every 250 ms and reliable full
snapshots at least every 2 s; after 750 ms without a report the host neutralizes the current
pads. Sparse snapshots received before any full baseline are ignored without consuming their
sequence, so a delayed reliable baseline can still reset the stream. Channel or session
teardown unplugs all pads.
