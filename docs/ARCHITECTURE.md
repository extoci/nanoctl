# Architecture

[ADR 0004](adr/0004-tailnet-native.md) is the design authority for nanoctl 3.

```text
Tailnet browser ── HTTPS + WebSocket ── Rust agent :7337
       └──────── WebRTC video / input ──────┘
                                      └── tailscale status / whois / cert
```

There is no nanoctl control plane, hosted signaling, enrollment, credential store,
Convex, Shoo, Vercel, STUN or TURN dependency. Tailscale itself handles network identity,
WireGuard transport, NAT traversal and, when needed, DERP relay.

## Host

- `tailnet.rs` wraps CLI discovery, status and briefly cached peer whois behind a
  testable `Tailnet` trait. The CLI is found on PATH, in the macOS app, in Windows
  Program Files, or through `tailnet.tailscale_cli`.
- `server.rs` binds only the node's tailnet addresses and discovers them periodically
  while Tailscale is unavailable. Loopback is off by default; there is no wildcard listener.
- `tls.rs` obtains the MagicDNS certificate with `tailscale cert`, stores it privately,
  checks daily and renews within 14 days of expiry. Native HTTPS is the default;
  issuance failure without a valid certificate produces a warned HTTP fallback.
- `service.rs` owns shutdown and the ready marker. One WebSocket owns one session;
  replacement or socket closure tears down and joins media/input work.
- `rtc.rs` retains v2 epochs, ICE generations, input lanes and bounded recovery.
  ICE has no servers and only gathers tailnet host candidates.
- The media pipeline captures the interactive desktop, bounds its newest-frame queue,
  encodes the negotiated codec (AV1, HEVC or H.264) and sends RTP. Input requires config, platform capability and viewer permission.

## Viewer

`apps/viewer` is a static React SPA built with Bun into hashed assets in `dist`.
The agent's `build.rs` embeds them; no separate web server or remote runtime assets
are needed. `GET /v1/info` supplies host name, displays, capabilities and the viewer's
tailnet identity. `/v1/connect` carries direct WebSocket signaling.

The moved session engine owns peer lifecycle, reliable control, lossy pointer traffic,
epochs and generations. Socket loss triggers bounded reconnect with a new session;
`replaced` requires an explicit Reconnect. Stats use inbound RTP byte/frame deltas,
show unknown rates before a baseline, and retain fractional rates on static desktops.

## Trust boundaries

Tailnet reachability is necessary but not sufficient: the agent also requires
owner/allow-list/capability authorization. Tagged callers require an allowed tag.
Host validation rejects DNS rebinding and WebSocket Origin must match the listener.
Only single-user machines fronted by `tailscale serve` should opt into loopback header
trust; local OS users can forge that identity. See [security](SECURITY.md).
