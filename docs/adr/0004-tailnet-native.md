# ADR 0004: Tailnet-native nanoctl (3.0)

## Status

Accepted. Supersedes ADR 0002 (Convex control plane + TURN fallback).

## Decision

nanoctl no longer has a hosted control plane. Every host agent serves its own controller directly
to the owner's Tailscale network (tailnet). There is no Convex, Shoo, Vercel, TURN, enrollment code,
or device credential. Installing the agent on a machine that is on your tailnet is the whole setup;
you open `https://<machine>.<tailnet>.ts.net:7337` from any of
your tailnet devices.

```text
Browser on tailnet device ── HTTP + WebSocket signaling ──> nanoctl agent (tailnet IP :7337)
          │                                                       │
          └──────── WebRTC (DTLS-SRTP / SCTP) over WireGuard ─────┘
                                                                  │
                                               tailscaled LocalAPI / CLI (whois, status)
```

## Identity and authorization

- The agent listens only on its Tailscale addresses (100.64.0.0/10, fd7a:115c:a1e0::/48).
  Loopback is disabled by default and is an explicit opt-in for single-user machines fronted by
  `tailscale serve`. It never binds a wildcard address by default.
- Every HTTP request and WebSocket upgrade is attributed to a tailnet identity with tailscaled's
  `whois` for the connecting address. Requests without a tailnet identity are rejected.
- With `tailnet.serve_loopback = true`, loopback requests are accepted only when they carry the
  `Tailscale-User-Login` header that `tailscale serve` injects (it strips client-supplied copies).
  Any local OS user can forge this header, so this mode must not be enabled on multi-user machines.
- Policy, first match wins:
  1. Tailscale grant capability `extoci.lol/cap/nanoctl` on the peer: `[{"control": true}]` grants
     control, `[{"control": false}]` or `[{}]` grants view-only.
  2. `allow_users` in config (login names) grants control.
  3. Empty `allow_users`: the user who owns this host's node is allowed (control).
  4. Tagged (non-user) peers are rejected unless their tag is in `allow_tags`.
- DNS rebinding / CSRF: `Host` must be one of this node's tailnet IPs, MagicDNS FQDN, or short
  host name (or loopback for serve); WebSocket `Origin` must equal the request's own origin,
  using the actual listener's HTTP/HTTPS scheme. Opt-in serve loopback requests with a serve
  identity header accept either HTTP or HTTPS origins for the same Host, without trusting
  `X-Forwarded-Proto`.

## Native HTTPS

`tailnet.tls = "auto"` is the default secure-context path. The agent obtains a certificate for
its validated MagicDNS FQDN using `tailscale cert --cert-file <path> --key-file <path> <dnsname>`,
stores the certificate and private key in its private configuration directory (owner-only
permissions), and serves rustls HTTPS on the tailnet IPs at port 7337. It checks daily and renews
within 14 days of expiry. A failed renewal retains an existing valid certificate; without one,
issuance failure falls back to HTTP with a warning. Doctor reports the fallback and instructions
to enable HTTPS certificates in Tailscale's DNS settings and grant certificate-issuance permission.
Doctor and `nanoctl url` print the HTTPS MagicDNS URL when a usable certificate is available.

```toml
[tailnet]
port = 7337
tls = "auto" # Set to "off" for plain HTTP.
# Only enable on single-user machines when fronted by tailscale serve.
serve_loopback = false
```

Opt-in loopback listeners remain plain HTTP for `tailscale serve` to proxy. Native HTTPS does
not need loopback or trusted identity headers: tailnet connections still authenticate with whois.

## Signaling

One WebSocket (`/v1/connect`) is one controller session. The server sends `hello` (session id, host
info, displays, capabilities, viewer identity). Both sides exchange the existing v2 signal
envelopes wrapped as `{"type":"signal","envelope":...}`, and `{"type":"end","reason":...}`.
Closing the socket ends the session. One active session per host; a newer authorized connection
replaces the older one, which receives `end` with reason `replaced`.

## Media path

WebRTC is kept for its congestion control, browser hardware decode, and lossy pointer lane. ICE
uses host candidates on tailnet addresses only: no STUN, no TURN. WireGuard already provides
NAT traversal (direct or DERP). Browser mDNS candidates are not resolved; the agent learns the
browser through peer-reflexive candidates on the connectivity check.

## Consequences

- Cost is zero beyond the user's own Tailscale plan; there is nothing to host.
- Reachability equals tailnet reachability; Tailscale ACLs/grants are the access policy.
- Plain HTTP on the tailnet is encrypted by WireGuard. Secure-context browser APIs (keyboard lock,
  clipboard) use native HTTPS by default. `tailscale serve --bg 7337` remains an optional
  alternative when loopback trust is explicitly enabled on a single-user machine.
