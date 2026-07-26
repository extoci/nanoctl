# Protocol v1

All control-plane envelopes carry `version: 1`. Unknown versions are rejected, not guessed. JSON is
UTF-8 and bounded before parsing. Persistent schema types live in Convex; wire types live in
`@nanoctl/protocol` and the Rust mirror.

## Signaling envelope

```json
{
  "version": 1,
  "sessionId": "convex-session-id",
  "sequence": 0,
  "sender": "controller",
  "sentAt": 1710000000000,
  "payload": { "type": "offer", "sdp": "v=0..." }
}
```

Sequences are strictly increasing independently for `controller` and `host`. Duplicate tuples
`(session, sender, sequence)` are idempotent. Gaps are allowed because ICE candidates may race HTTP
retries. Messages outside the session deadline are rejected.

Payloads are `offer`, `answer`, `ice-candidate`, `ice-complete`, and `end`. An ICE restart is a new
controller `offer`, not a distinct wire variant. SDP is at most 1 MB, a candidate 8 KiB, and a
reason 512 characters. SDP codec/media lines are validated again by the WebRTC implementation.

## Agent HTTP status semantics

Authenticated agent endpoints return `401` or `403` only when the credential is invalid, disabled,
or revoked. The host treats those responses as a terminal enrollment failure and stops serving.
`429` means temporary throttling, while `5xx` and transport failures are transient; the service
retains its enrollment and retries on its normal bounded polling schedule. The shared HTTP abuse
ceiling permits the documented 250 ms advanced polling interval, and signaling also has a tighter
per-session limit.

The browser ends a session with an authenticated control-plane mutation before deliberate
navigation. If it disappears without doing so, the host gives ICE restart 15 seconds to recover;
a peer that remains failed publishes a terminal `end` signal and closes locally. Convex then marks
the session failed, so an abandoned browser cannot reserve a device for the full session TTL.

## Data channels

`nanoctl.control.v1` is reliable and ordered. It carries key/button transitions, pointer
button/wheel events, release, and keepalive messages. `nanoctl.pointer.v1` is unordered with zero
retransmissions and carries pointer movement samples. Channel labels and the authenticated WebRTC
session identify protocol v1; unknown message variants are ignored and every message is capped at
64 KiB. The browser drops obsolete pointer motion above a 64 KiB lossy-channel backlog. A reliable
control backlog above 256 KiB is terminal: the browser disables input, closes the peer, and ends the
control-plane session instead of allowing key, wheel, or keepalive traffic to grow without bound.
The host input watchdog releases held state when keepalives stop.

Normalized coordinates are finite numbers clamped to `[0, 1]` and map to the selected display’s
pixel bounds plus its virtual-desktop origin. Display identifiers come from the authenticated
agent’s bounded capability document. A reliable display-selection message first updates input
geometry, then replaces the capture recorder and forces an IDR; capture replacement failure retains
the existing stream. A reliable `release` message releases all held keys and buttons immediately.
The browser sends it on control disable, blur, visibility loss, and teardown. A 2-second input
watchdog is the final backstop.

Keyboard events use DOM `code` for physical location plus `key` for meaning. The agent maps `code`
using the active OS layout and treats text input separately in future protocol versions. Modifier
bits are Shift=1, Control=2, Alt=4, Meta=8. `Ctrl+Alt+Shift+Escape` disables remote input, releases
held state, and exits fullscreen locally without sending the chord to the host. Clipboard variants
are reserved and rejected in v1.

## Codec negotiation

Browser offer order is a preference, not a command. Agent intersects it with hardware/software
capability and local policy. H.264 packetization-mode 1 is the mandatory v1 baseline. VP9 and AV1
are optional. The agent never shells out with remote-provided encoder names or arguments.

The selected codec, profile, pixel format, resolution, target bitrate, max FPS, display, and audio
state are reported on the reliable channel. Adaptation changes are reported so diagnostics can
distinguish congestion from capture failure.

## Compatibility

Additive optional fields are allowed within v1. New variants or changed semantics require v2.
Peers advertise minimum and maximum supported versions; there is no downgrade below the minimum.
Security fixes may tighten validation without a version increment.
