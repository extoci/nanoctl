# Setup

1. [Install Tailscale](https://tailscale.com/download) on the browser device and sign in.
   Linux and macOS hosts also need system Tailscale. Windows releases can use embedded
   Tailscale instead.
2. Run the [installer](../README.md#get-started) as the interactive desktop user,
   not root, LocalSystem, or a different Windows administrator.
3. Open the printed URL from your tailnet browser. The viewer auto-connects.

No nanoctl account or setup code is needed. The host and viewer still need to
be on the same tailnet. Run `nanoctl url` once the host has joined.

The Linux installer uses a systemd user service, macOS a LaunchAgent, and Windows
a hidden per-user Scheduled Task. Allow Screen Recording and Accessibility on macOS.
The desktop user must be signed in. See [platform constraints](PLATFORMS.md).

## Embedded Tailscale on Windows

The Windows release includes a userspace Tailscale node. It does not install the
Tailscale app, a VPN driver, or a system-wide VPN. By default, nanoctl uses the
system backend if it finds the Tailscale CLI and the embedded backend otherwise.

1. Install nanoctl as your Windows desktop user. If an unusable Tailscale
   installation is still present, set `backend = "embedded"` under `[tailnet]` in
   the file shown by `nanoctl paths`, then restart nanoctl.
2. Run `nanoctl url` and open the Tailscale sign-in link when it appears. Sign in
   as the same Tailscale user you use on the iPhone. Approve the new device if
   your tailnet requires it. The embedded node is listed as `nanoctl-<pc-name>`.
3. Enable [MagicDNS and HTTPS Certificates](https://login.tailscale.com/admin/dns),
   then run `nanoctl url` again. With Tailscale connected on the iPhone, open the
   printed HTTPS URL in Safari. The [controller setup](../README.md#game-controllers)
   is separate: Windows still needs ViGEmBus for gamepad forwarding.

Allow TCP and IPv4 UDP on the configured agent port (default: 7337) in tailnet
policy; embedded WebRTC uses that same port for UDP. The agent does not bind
the viewer or media to a LAN interface. The node's identity persists in the
desktop user's local app data, so upgrades and restarts do not require sign-in.
To revoke access, remove the node in the [Tailscale admin console](https://login.tailscale.com/admin/machines).

Set `backend = "system"` to require the external client. An explicit
`tailscale_cli` or `serve_loopback` uses the system backend in auto mode.
`nanoctl licenses` prints notices for the embedded dependencies. Source builds
need Go (version in the embedded helper's module file) and Cargo features
`media,embedded-tailnet`; release users do not need Go installed.

## HTTPS

Enable **DNS → HTTPS Certificates** in the Tailscale admin console and use MagicDNS.
With system Tailscale, default `tailnet.tls="auto"` invokes `tailscale cert` for the node's
validated FQDN. Grant the desktop user certificate-issuance permission if tailscaled
requires it. Certificates and keys are stored privately and renewed automatically.
The embedded backend obtains and renews its certificate through its own Tailscale node.

The normal URL is `https://<host>.<tailnet>.ts.net:7337/`. Failed issuance without a
valid existing certificate warns and falls back to `http://<host>.<tailnet>.ts.net:7337/`
only on the system backend. The embedded backend does not silently switch to HTTP.
WireGuard still encrypts HTTP over the tailnet, but keyboard lock and other browser
secure-context features need HTTPS. Use `nanoctl doctor` to see the fallback reason
and `nanoctl url` for the URL to open. `tls="off"` explicitly disables native TLS.

## Configuration and access

Defaults require no config file. `nanoctl paths` prints the platform config path;
`nanoctl config` prints the effective config. Example optional configuration:

```toml
[tailnet]
backend = "auto"
port = 7337
tls = "auto"
allow_users = ["alice@example.com"]
allow_tags = []
# Only enable on single-user machines when fronted by tailscale serve.
serve_loopback = false
discover_peers = true # Probe online tailnet peers for the viewer host picker
# tailscale_cli = "/custom/path/to/tailscale"

[features]
remote_input = true
clipboard = true
system_audio = true
```

An empty `allow_users` allows the host node's owner by default. A nonempty list
replaces that default. Grants can authorize control or view-only through
`extoci.lol/cap/nanoctl`; tagged callers additionally require `allow_tags`.
See [security](SECURITY.md#authorization) for a sample tailnet `grants` policy.
Permit TCP 7337 and WebRTC UDP traffic through tailnet policy and host firewalls.
Restart the agent after config changes.

Clipboard and system audio default to enabled. Clipboard is restricted to controlling
viewers; system audio is available to view-only viewers too. Disable either feature above
to prevent its transport from being negotiated. Linux audio requires `pulseaudio-utils`
(`parec` and `pactl`) and a reachable PulseAudio or PipeWire-pulse server in the desktop
user session. Windows captures the default output device via WASAPI loopback. macOS
system audio is currently unsupported. Viewers must click **Enable sound** to hear audio.

Native HTTPS does not require `tailscale serve`. If you choose serve instead,
explicitly opt into loopback only on a single-user host: local users can forge its
identity header. Do not expose the listener through a public reverse proxy or Funnel.

## Upgrading from v2

Re-run the installer. It preserves the existing config, ignores removed legacy keys
with warnings, and best-effort deletes the old platform credential after a successful
upgrade. Do not delete config to work around a Tailscale warning. Checksum validation,
transactional activation/rollback and service startup health gates remain in place.
