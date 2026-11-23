# Operations

There is no hosted nanoctl service, database, signaling queue, TURN credential or
enrollment endpoint to operate. Each host serves its own embedded viewer over Tailscale.

## Routine checks

```sh
tailscale status
nanoctl doctor
nanoctl url
nanoctl paths
nanoctl config
```

Doctor checks config, CLI discovery, Tailscale state, addresses, MagicDNS, TLS URL,
capture, encoder and input. Missing Tailscale does not prevent installation or the
background agent waiting for it; doctor correctly reports that the host is not ready.
The URL command may fail until Tailscale is running.

Enable HTTPS in the Tailscale admin console's DNS settings. With `tls="auto"`,
`tailscale cert` issues a private native-HTTPS certificate, checked daily and renewed
within 14 days of expiry. A failed renewal retains a valid certificate. If issuance
fails and no valid certificate exists, inspect the warning, grant certificate permission
to the agent user, and fix HTTPS configuration; the agent falls back to HTTP meanwhile.

## Service and upgrades

- Linux: `systemctl --user status nanoctl.service`;
  logs: `journalctl --user -u nanoctl.service`.
- macOS: `launchctl print gui/$(id -u)/dev.nanoctl.agent`;
  registration: `~/Library/LaunchAgents/dev.nanoctl.agent.plist`.
- Windows: `Get-ScheduledTask -TaskName "nanoctl Agent"`;
  logs: `%LOCALAPPDATA%\nanoctl\agent.log`.

## Debugging

`nanoctl debug` prints a Markdown report to paste into an issue or coding agent: version,
OS, doctor checks, codecs, encoder backends, displays, redacted configuration and the last
400 lines of the agent log (`--lines N` to change, `--output FILE` to also save it). The agent
always keeps an info-level log, independent of console verbosity, at
`<local data dir>/nanoctl/logs/diagnostics.log` (2 MB, one rotated `.log.1`); `RUST_LOG`
changes its level. In the viewer, Details → Copy debug info copies the same host report plus
browser codec support, the last two minutes of stats and the session event timeline; it
downloads a file instead when the clipboard is unavailable. The host report requires control
permission.

Run the release installer again as the same desktop user. Download checksums,
atomic binary activation, service registration and startup rollback remain enforced.
Windows uses PID/version/token ready-file validation and a stability window; Unix
one-command installs verify that the same service PID survives startup.
Low-level packaging helpers retain signed-manifest stage/activate/commit/rollback.
Config stays in place; v2 credentials are removed best-effort only after success.
No keyring or setup-code interaction is required.

## Troubleshooting

| Symptom                   | Check                                                                                                         |
| ------------------------- | ------------------------------------------------------------------------------------------------------------- |
| No URL / no listener      | Tailscale installed, running and signed in; `tailscale status --json`; agent CLI path                         |
| 403                       | Identity in response; node owner, `allow_users`, allowed source tags, capability grant; [policy](SECURITY.md) |
| WebSocket rejected        | Same-origin page; correct Host, listener scheme and port; avoid arbitrary proxies                             |
| Negotiates but no video   | Host capture permissions, available display, UDP firewall/policy and encoder diagnostic                       |
| Keyboard lock unavailable | Use native HTTPS, a supporting browser and fullscreen                                                         |
| Another viewer took over  | Expected single-session behavior; reconnect deliberately                                                      |
| Very low FPS/kbps         | Compare multiple stats samples while changing the desktop; idle capture can be sparse                         |

The stats route is the WebRTC candidate type, not proof that Tailscale itself is using
a direct path instead of DERP. Use Tailscale diagnostics for the underlying route.
Never set `NANOCTL_DEV_LOOPBACK_USER` in a deployed service. Keep loopback serving off
unless deliberately accepting the single-user `tailscale serve` trust tradeoff.
