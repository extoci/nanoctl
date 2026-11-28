# Windows desktop

nanoctl's Windows desktop app is a native GPUI shell. It is branded **nanoctl** and uses `planck` only as its internal codename. The binary is `nanoctl-desktop` so it does not collide with the host-agent `nanoctl` binary.

## Scope

This first desktop iteration is intentionally thin:

- discovers nanoctl agents on the tailnet;
- presents them in a native host list;
- opens the selected agent's secure embedded browser viewer for the streaming session;
- reports the local host agent's installation and running state;
- starts, stops, restarts and opens the configuration for the local host agent;
- keeps experimental client and host behavior behind Labs.

It does not yet duplicate the browser viewer's WebRTC media pipeline in native code. The browser viewer remains the streaming implementation.

## Build and test

```powershell
bun run test:desktop
bun run build:desktop
```

The release executable is `crates/nanoctl-planck/target/release/nanoctl-desktop.exe`. Windows release candidates publish `nanoctl-desktop-windows-x64.exe` beside the agent artifacts.

## Labs

Labs experiments are upcoming alpha features that users can enable independently. They are off by default and can be restricted by platform. Client experiments persist in the desktop app's local configuration. Host experiments persist in the host agent's configuration and require an agent restart before the media pipeline picks them up. Unknown experiment keys are preserved in both files so older clients do not erase settings written by newer clients.

The first client experiment is `windows.tailnet-discovery`. With Labs off, the app probes only the local device. With the experiment enabled on Windows, it also probes online Tailscale peers for nanoctl agents on TCP port 7337.

The first host experiment is `windows.strict-dxgi-capture`. Windows normally tries the low-latency DXGI duplication capture path and falls back to legacy continuous capture if duplication is unavailable. The experiment removes that fallback so alpha testers can expose DXGI failures instead of silently using the slower path.

Desktop configuration is stored below the user's roaming application-data directory, normally `%APPDATA%\nanoctl\desktop\config\config.toml` on Windows. Host agent configuration is normally `%APPDATA%\nanoctl\nanoctl\config\config.toml`.
