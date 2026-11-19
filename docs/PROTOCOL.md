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
    "version": "3.0.0-alpha.1",
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
      "codecs": ["h264"],
      "encoderBackends": ["software-openh264"],
      "ready": true,
      "readiness": "ready",
      "input": true,
      "remoteInput": true,
      "clipboard": false,
      "systemAudio": false,
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

WebRTC uses empty ICE servers, tailnet host candidates, H.264 RTP, DTLS-SRTP video and
SCTP data channels. No application-level TURN/STUN is used; Tailscale may relay its
underlying encrypted transport through DERP. Reliable control carries discrete commands;
the lossy pointer lane avoids accumulating stale motion. View-only sessions never enable
host input. Audio and clipboard are not implemented.
