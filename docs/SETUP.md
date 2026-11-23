# Setup

1. [Install Tailscale](https://tailscale.com/download) on the host and browser device.
   Sign into your tailnet and make sure Tailscale is running.
2. Run the [installer](../README.md#get-started) as the interactive desktop user,
   not root, LocalSystem, or a different Windows administrator.
3. Open the printed URL from your tailnet browser. The viewer auto-connects.

No nanoctl account, setup code, enrollment or keyring unlock is needed. Installers warn
but still install if Tailscale is missing/stopped; the agent waits for it to become
available. Run `nanoctl url` once Tailscale is running.

The Linux installer uses a systemd user service, macOS a LaunchAgent, and Windows
a hidden per-user Scheduled Task. Allow Screen Recording and Accessibility on macOS.
The desktop user must be signed in. See [platform constraints](PLATFORMS.md).

## HTTPS

Enable **DNS → HTTPS Certificates** in the Tailscale admin console and use MagicDNS.
Default `tailnet.tls="auto"` invokes `tailscale cert` for the node's validated FQDN.
Grant the desktop user certificate-issuance permission if tailscaled requires it.
Certificates and keys are stored privately; renewal is automatic within 14 days of expiry.

The normal URL is `https://<host>.<tailnet>.ts.net:7337/`. Failed issuance without a
valid existing certificate warns and falls back to `http://<host>.<tailnet>.ts.net:7337/`.
WireGuard still encrypts HTTP over the tailnet, but keyboard lock and other browser
secure-context features need HTTPS. Use `nanoctl doctor` to see the fallback reason
and `nanoctl url` for the URL to open. `tls="off"` explicitly disables native TLS.

## Configuration and access

Defaults require no config file. `nanoctl paths` prints the platform config path;
`nanoctl config` prints the effective config. Example optional configuration:

```toml
[tailnet]
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
Permit TCP 7337 and dynamic WebRTC UDP traffic through tailnet policy and host firewalls.
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
