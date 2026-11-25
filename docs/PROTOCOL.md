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
    "version": "3.0.0-alpha.6",
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
      "clipboard": true,
      "systemAudio": true,
      "displays": []
    },
    "viewer": {
      "identity": { "login": "owner@example.com", "displayName": "Owner", "tags": [] },
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

### Video over data channel

A viewer may also create `nanoctl.video.v1` (unordered, `maxRetransmits: 0`) and
`nanoctl.video-idr.v1` (reliable, ordered) to receive Annex-B access units directly,
bypassing the RTP jitter buffer. Each message is
`[u32 seq][u8 flags (bit0 = keyframe)][u16 part][u16 parts][u64 sent_ms][payload chunk]`
with all integers little-endian; `parts` splits large access units into ~48 KiB chunks.
Deltas ride the unreliable channel and keyframes the reliable one. A sequence gap breaks
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

### Gamepads

When `capabilities.gamepad` is true, an opted-in viewer with control permission sends
`{"type":"gamepad","pads":[[0,4096,128,0,0,0,0,0]]}` on the reliable control channel,
or on the unordered pointer channel (`nanoctl.pointer.v1`) — snapshots carry full
state, so the unordered lane drops stale reports instead of delaying fresh ones. The
host keeps the latest pointer move and the latest gamepad snapshot in separate slots,
so a per-frame snapshot never replaces the pointer move sent in the same frame.
Each entry is `[slot, buttons, leftTrigger, rightTrigger, leftX, leftY, rightX, rightY]`:
slots are 0–3, buttons use XInput flags, triggers are unsigned 8-bit values, and axes
are signed 16-bit values with positive Y up. For PlayStation controllers, the viewer adds
an optional `dualShock4` array to the snapshot with one
`{"slot":0,"leftTriggerPressed":false,"rightTriggerPressed":false,"touchpadPressed":false}`
entry per PlayStation pad. The host presents those slots as DualShock 4 controllers and
all other slots as Xbox 360 controllers. The original `pads` entries are unchanged, so
older hosts can ignore the extension. Each message is a full snapshot; the host unplugs
slots not present. An empty snapshot detaches all pads. The viewer sends
changes and a 500 ms keepalive while forwarding; after 2 seconds without a snapshot
the host neutralizes the current pads. Channel or session teardown unplugs them.
