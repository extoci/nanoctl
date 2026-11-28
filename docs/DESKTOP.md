# Windows desktop

nanoctl's Windows desktop app is a native GPUI shell. It is branded **nanoctl** and uses `planck` only as its internal codename. The binary is `nanoctl-desktop` so it does not collide with the host-agent `nanoctl` binary.

## Scope

This first desktop iteration is intentionally thin:

- discovers nanoctl agents on the tailnet;
- presents them in a native host list;
- opens the selected agent's secure embedded browser viewer for the streaming session;
- keeps experimental behavior behind Labs.

It does not yet duplicate the browser viewer's WebRTC media pipeline in native code. The browser viewer remains the streaming implementation.

## Build and test

```powershell
bun run test:desktop
bun run build:desktop
```

The release executable is `crates/nanoctl-planck/target/release/nanoctl-desktop.exe`. Windows release candidates publish `nanoctl-desktop-windows-x64.exe` and `nanoctl-desktop-windows-arm64.exe` beside the agent artifacts.

## Labs

Labs experiments are upcoming alpha features that users can enable independently. They are off by default, persist only in the desktop app's local configuration, and can be restricted by platform. Unknown experiment keys are preserved so older clients do not erase settings written by newer clients.

The first experiment is `windows.tailnet-discovery`. With Labs off, the app probes only the local device. With the experiment enabled on Windows, it also probes online Tailscale peers for nanoctl agents on TCP port 7337.

Configuration is stored below the user's roaming application-data directory, normally `%APPDATA%
anoctl\desktop\config\config.toml` on Windows.
