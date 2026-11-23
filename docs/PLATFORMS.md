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
Foundation H.264 is preferred with OpenH264 fallback. Secure desktop, sign-in screens
and elevated application control remain restricted. Direct zero-copy D3D11 capture
is not a claimed capability.

## macOS

Target macOS 14+. A per-user LaunchAgent keeps capture/input in the correct login
session. Grant Screen Recording and Accessibility to the installed binary; permission
cannot be silently granted by the installer. Tailscale's CLI can be discovered at
`/Applications/Tailscale.app/Contents/MacOS/Tailscale`.

macOS 15+ prefers low-queue-depth ScreenCaptureKit, including cursor and click highlights.
macOS 14 and ScreenCaptureKit startup failures use the existing AVCapture backend.
Both paths copy frames and feed real-time VideoToolbox H.264, with OpenH264 fallback.
Legacy Keychain entry deletion is best-effort and is not needed for normal operation.

## Linux

The installer registers a systemd user service. Run it in the graphical user's session,
not as root. X11 and PipeWire/Wayland capture support depend on the desktop environment
and permissions. Wayland portal input/restore-token integration is not a blanket promise
of unattended input support. X11 is a weaker input-isolation environment.

VA-API H.264 needs a compatible driver/render device and libva development headers
when building. OpenH264 provides the software fallback; `quality.encoder="hardware"`
requires hardware instead of falling back. Direct DMA-BUF zero-copy is not claimed.
Legacy Linux keyutils credentials are invalidated best-effort when `keyctl` is available.

## Browser and HTTPS

Use a current WebRTC/H.264-capable browser; automated coverage is Chromium.
Native HTTPS uses `tailscale cert` after HTTPS is enabled in the tailnet admin console,
with warned HTTP fallback if issuance fails. WireGuard encrypts either, but keyboard lock
requires a supporting browser, secure context and fullscreen.
The viewer adapts to missing capture/input capability and view-only authorization.
See [setup](SETUP.md), [security](SECURITY.md) and [testing](TESTING.md).
