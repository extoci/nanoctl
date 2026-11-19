# Security

## Trust and threats

nanoctl 3 has no hosted nanoctl control plane, enrollment endpoint, device secret or
hosted signaling database. A host serves its own viewer and signaling only on Tailscale
addresses. Tailscale's identity/network infrastructure and the host OS remain trusted.
WebRTC DTLS-SRTP/SCTP runs inside WireGuard; a DERP relay does not decrypt that traffic.

Threats include unauthorized tailnet users, malicious browser origins, DNS rebinding,
untrusted local users, compromised clients/hosts, and stale signals after replacement.
A permitted control session has the interactive user's desktop privileges. nanoctl
does not isolate applications or protect an already compromised endpoint.

## Authorization

The agent obtains the connecting peer through `tailscale whois --json <ip:port>`.
Tagged callers must first match `tailnet.allow_tags`, even if they have a capability.
Then policy is:

1. Capability `extoci.lol/cap/nanoctl`: any `{"control":true}` entry grants control;
   otherwise a present capability (`[{}]` or `[{"control":false}]`) is view-only.
2. An allowed tag grants control for a tagged caller without a capability.
3. A login in `tailnet.allow_users` grants control. If that list is empty, only
   the owner of the host's Tailscale node is allowed by default.
4. Otherwise deny (403).

Network grants/ACLs must independently allow the connection. Example to merge into
your tailnet policy (replace the destination with the host's Tailscale IP):

```json
{
  "grants": [
    {
      "src": ["alice@example.com"],
      "dst": ["100.100.1.2"],
      "ip": ["tcp:7337", "udp:*"],
      "app": {
        "extoci.lol/cap/nanoctl": [{ "control": true }]
      }
    }
  ]
}
```

Use `{"control":false}` for view-only. TCP carries the viewer and signaling; UDP
allows dynamic WebRTC host ports. Add allowed source tags to the host config when
using tagged controller nodes. Capabilities come from whois, not browser headers.

## HTTPS and browser boundary

Enable HTTPS certificates in the Tailscale admin console. Default `tls="auto"` uses
`tailscale cert`, owner-only certificate/key storage, daily renewal checks and renewal
within 14 days of expiry. Failed renewal retains an existing valid cert; without one
the agent warns and falls back to HTTP. HTTP is still WireGuard-encrypted on the
tailnet but does not provide a normal browser secure context.

Host must identify this node (tailnet IP, MagicDNS FQDN or host name). WebSocket
Origin must match that Host and actual HTTP/HTTPS listener scheme. The viewer uses
only same-origin assets/connections, a restrictive CSP without unsafe-eval or remote
script origins, no-store API responses, nosniff and anti-framing headers.

## Loopback is not an identity boundary

`serve_loopback=false` by default. **Only enable it on single-user machines fronted
by tailscale serve.** Local OS users can forge `Tailscale-User-Login`; Host/Origin
checks do not stop a malicious local process. Opt-in serve traffic accepts HTTP or
HTTPS Origin for the same Host without trusting X-Forwarded-Proto.

`NANOCTL_DEV_LOOPBACK_USER` is an explicitly insecure test-only bypass, including
loopback ICE. Never set it for production.

Installers preserve legacy config but ignore removed keys; deletion of old platform
keyring credentials is best-effort, not remote revocation. Keep OS and Tailscale
patches current and restrict who can change the binary, config and service.
