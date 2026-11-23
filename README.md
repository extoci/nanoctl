# nanoctl

Fast browser-based remote desktop, served by your own machine over Tailscale.

## Tailnet host picker

The viewer header lists other online nanoctl hosts on your tailnet so you can jump between machines.
Discovery probes each online Tailscale peer on the configured agent port (400ms timeout), caches
results for 15 seconds, and refreshes the picker every 30 seconds. Each destination still applies
its own authentication policy. Set `tailnet.discover_peers = false` to disable discovery.

**3.0 alpha is tailnet-native.** Each Rust agent embeds its static browser viewer.
There is no hosted nanoctl control plane, account, enrollment code, device credential,
STUN or TURN service. Tailscale supplies identity, reachability and WireGuard encryption;
WebRTC carries video and input between your browser and the host.

## Get started

1. [Install Tailscale](https://tailscale.com/download) on the host and controller device,
   sign into your tailnet, and leave Tailscale running.
2. Run the nanoctl installer as the host's interactive desktop user:

   **Linux / macOS**

   ```sh
   curl -fsSL https://github.com/extoci/nanoctl/releases/latest/download/install.sh | sh
   ```

   **Windows PowerShell**

   ```powershell
   irm https://github.com/extoci/nanoctl/releases/latest/download/install.ps1 | iex
   ```

3. Open the printed URL from a browser on your tailnet, normally
   `https://<host>.<tailnet>.ts.net:7337/`.

During prereleases, use the install script from the selected release and set
`NANOCTL_VERSION=v3.0.0-alpha.1` (PowerShell: `$env:NANOCTL_VERSION="v3.0.0-alpha.1"`).
Run the installer again to upgrade. Downloads are checksum-verified; failed startup
restores the previous installation. Existing config is preserved and legacy credentials
are removed best-effort.

Enable HTTPS certificates in the **Tailscale admin console → DNS → HTTPS Certificates**.
The agent runs `tailscale cert` automatically, privately stores and renews the certificate,
and serves native HTTPS. If issuance is unavailable, it warns and falls back to HTTP
over the encrypted tailnet. Some browser APIs then require HTTPS. See [setup](docs/SETUP.md).

## Access

By default only the user who owns the host's Tailscale node can connect and control it.
`tailnet.allow_users`, `tailnet.allow_tags`, or the grant capability
`extoci.lol/cap/nanoctl` extend access; grants can make a viewer view-only.
Tailscale network policy must also permit TCP 7337 and WebRTC UDP traffic.
See [security and sample grants](docs/SECURITY.md).

The viewer auto-connects, displays the host and your tailnet identity, and includes a
display picker, input toggle, fullscreen, secure-context keyboard lock and stream stats.
Only one viewer is active at a time; a newer viewer replaces the older one.
Press **Ctrl+Alt+Shift+Esc** to release remote input.

## Develop

```sh
bun install --frozen-lockfile
bun run check
bun run build:agent
```

The release build embeds `apps/viewer/dist`; build the viewer before invoking Cargo directly.
Cargo alone still builds without Bun and serves a placeholder if the viewer was not built.
Native media dependencies and browser checks are described in [testing](docs/TESTING.md).

- [Architecture](docs/ARCHITECTURE.md) · [Protocol](docs/PROTOCOL.md)
- [Operations](docs/OPERATIONS.md) · [Platforms](docs/PLATFORMS.md)
- [Product](docs/PRODUCT.md) · [ADR 0004](docs/adr/0004-tailnet-native.md)

MIT licensed.
