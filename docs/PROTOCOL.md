# Protocol v2

New writers emit `version: 2`. Native readers retain bounded v1 parsing for cleanup and diagnostics
during rollout, mapping a missing controller epoch/ICE generation to `""`/`0`; v1 rows are never
admitted to an active v2 negotiation. The control plane does not admit a v1 agent into a new v2
session. Unknown versions are rejected, not guessed. JSON is UTF-8 and bounded before parsing.
Persistent schema types live in Convex; wire types live in `@nanoctl/protocol` and the Rust mirror.

## Signaling envelope

```json
{
  "version": 2,
  "sessionId": "convex-session-id",
  "controllerEpoch": "browser-peer-uuid",
  "generation": 0,
  "sequence": 1680000000000123,
  "sender": "controller",
  "sentAt": 1710000000000,
  "payload": { "type": "offer", "sdp": "v=0..." }
}
```

`controllerEpoch` identifies one browser peer and changes on refresh/replacement. The control plane
authoritatively claims the current epoch with a compare-and-swap against the epoch observed by the
caller, and the agent echoes it so delayed signals from an old controller cannot change the new peer
or session state. The native peer uses the same compare-and-swap claim for one host epoch and
generation; delayed answers from an old agent process are rejected.
`generation` is the monotonic ICE restart number inside that controller epoch. It gates SDP,
candidates, and end-of-candidates; an older generation can never mutate the current peer. Sequences
are monotonic within a live sender epoch and are idempotent at the control plane. Host epochs use a
fresh safe JSON-number seed after an agent restart, preventing a new answer from colliding with a
previous in-memory peer. The browser replays a bounded recent host window by database insertion time,
uses an exact indexed lookup for the current answer, and never treats a random seed as a cross-process
cursor. A native restart gets a new host epoch; the browser drops that peer's bootstrap answer and
performs an ICE restart before applying the replacement answer. Gaps are allowed because ICE
candidates may race HTTP retries. Messages outside the session deadline are rejected.

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
session identify protocol v2; unknown message variants are ignored and every message is capped at
64 KiB. The browser drops obsolete pointer motion above a 64 KiB lossy-channel backlog. A reliable
control backlog above 256 KiB is terminal: the browser disables input, closes the peer, and ends the
control-plane session instead of allowing key, wheel, or keepalive traffic to grow without bound.
The host input watchdog releases held state when keepalives stop.

Capabilities distinguish a reachable device from a controllable one. `remoteInput: false` is a
valid view-only configuration: the dashboard may connect for video, but the controller keeps the
input lane disabled and does not show a false “control enabled” state.

Normalized coordinates are finite numbers clamped to `[0, 1]` and map to the selected display’s
pixel bounds plus its virtual-desktop origin. Display identifiers come from the authenticated
agent’s bounded capability document. A reliable display-selection message validates the target and
optimistically updates input geometry, then replaces the capture recorder and forces an IDR; capture
replacement failure retains the existing stream and sends the prior geometry back to the input
worker. A reliable `release` message releases all held keys and buttons immediately.
The browser sends it on control disable, blur, visibility loss, and teardown. A 2-second input
watchdog is the final backstop.

Keyboard events use DOM `code` for physical location plus `key` for meaning. The agent maps `code`
using the active OS layout and treats text input separately in future protocol versions. Modifier
bits are Shift=1, Control=2, Alt=4, Meta=8. `Ctrl+Alt+Shift+Escape` disables remote input, releases
held state, and exits fullscreen locally without sending the chord to the host. Clipboard variants
are reserved and rejected in v2.

## Codec negotiation

Browser offer order is a preference, not a command. Agent intersects it with hardware/software
capability and local policy. H.264 packetization-mode 1 is the mandatory v2 baseline. VP9 and AV1
are optional. The agent never shells out with remote-provided encoder names or arguments.

The selected codec, profile, pixel format, resolution, target bitrate, max FPS, display, and audio
state are reported on the reliable channel. Adaptation changes are reported so diagnostics can
distinguish congestion from capture failure.

## Compatibility

v2 is additive at the parser boundary. A v1 envelope is accepted by native readers with an empty
controller epoch and generation zero so bounded old rows can be inspected safely, but it cannot pass
the v2 claim/signaling gates. All new browser and agent messages carry explicit controller and host
epochs. Device readiness and session creation require protocol v2; a v1 device remains visible but
the dashboard labels it `upgrade nanoctl to 2.0` instead of silently attempting an unsafe downgrade.
New variants or changed semantics require a new protocol version. Security fixes may tighten
validation without a version increment. The control plane remains a bounded, authenticated
rendezvous service; it never handles media bytes.
