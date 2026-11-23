# Platforms

All platforms need an interactive desktop user, Tailscale installed/running, and
network policy allowing the agent's TCP port (7337 by default) plus WebRTC UDP.
No platform needs nanoctl enrollment or a keyring credential.
The agent discovers the Tailscale CLI on PATH, at the standard macOS app path or
Windows Program Files path, with `tailnet.tailscale_cli` as an override.

## Windows

Target Windows 10 1903+ / Windows 11. The public installer places the binary under
`%LOCALAPPDATA%\nanoctl` and registers a hidden, non-elevated interactive-user
Scheduled Task via `wscript.exe`. Logs are owner-protected. PID/version/token ready
markers and startup stability checks gate transactional upgrades.

The managed `packaging/windows/install-service.ps1` helper supports Program Files
deployments but still runs capture/input in the interactive user's task, not Session 0.
Config/task owner checks reject cross-user migration; legacy Administrators-owned
config repair requires elevating the same user. Existing config remains unchanged.
Old Windows Credential Manager entries are deleted best-effort.

Windows.Graphics.Capture supplies frames, enigo supplies input, and hardware Media
Foundation H.264 is preferred. Secure desktop, sign-in screens
and elevated application control remain restricted. Direct zero-copy D3D11 capture
is not a claimed capability.

## macOS

Target macOS 14+. A per-user LaunchAgent keeps capture/input in the correct login
session. Grant Screen Recording and Accessibility to the installed binary; permission
cannot be silently granted by the installer. Tailscale's CLI can be discovered at
`/Applications/Tailscale.app/Contents/MacOS/Tailscale`.

Native capture feeds real-time hardware VideoToolbox H.264. The pipeline
still copies captured frames; direct ScreenCaptureKit zero-copy is not claimed.
Legacy Keychain entry deletion is best-effort and is not needed for normal operation.

## Linux

The installer registers a systemd user service. Run it in the graphical user's session,
not as root. X11 and PipeWire/Wayland capture support depend on the desktop environment
and permissions. Wayland portal input/restore-token integration is not a blanket promise
of unattended input support. X11 is a weaker input-isolation environment.

The encoder tries VA-API first, then NVIDIA NVENC. VA-API needs a writable render
device and an H.264 encode-capable driver. NVENC needs an encode-capable NVIDIA GPU
and driver libraries (`libcuda.so.1` and `libnvidia-encode.so.1`) supporting the
NVENC API used at build time; a CUDA toolkit is not required at runtime. To build the
Linux media binary, install `libva-dev` and `libffmpeg-nvenc-dev` on Debian/Ubuntu
(libva and nv-codec-headers on other distributions). Direct DMA-BUF zero-copy is not claimed.
Legacy Linux keyutils credentials are invalidated best-effort when `keyctl` is available.

## Encoder selection

`quality.encoder = "auto"` (default) tries the native encoder first, then uses
OpenH264 if hardware is unavailable or fails during encoding or reconfiguration.
After a switch, the replacement starts with a new H.264 keyframe and stays active
for that session. Set `"hardware"` to fail instead of using OpenH264, or
`"software"` to skip hardware entirely. `nanoctl doctor` reports the selected
backend at probe time; the media smoke report shows the backend that actually encoded.

## Browser and HTTPS

Use a current WebRTC/H.264-capable browser; automated coverage is Chromium.
Native HTTPS uses `tailscale cert` after HTTPS is enabled in the tailnet admin console,
with warned HTTP fallback if issuance fails. WireGuard encrypts either, but keyboard lock
requires a supporting browser, secure context and fullscreen.
The viewer adapts to missing capture/input capability and view-only authorization.
See [setup](SETUP.md), [security](SECURITY.md) and [testing](TESTING.md).
