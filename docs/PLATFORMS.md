# Platform implementation

## Windows

Target Windows 11. Capture uses Windows.Graphics.Capture with Desktop Duplication fallback for
supported session types. Prefer D3D11 textures through Media Foundation hardware encoders to avoid
GPU-to-CPU copies. Input uses SendInput in the interactive user session. The service supervisor and
per-user capture process communicate over an ACL-protected named pipe; Session 0 cannot directly
capture the user desktop.

The MSI installs the supervisor, registers automatic delayed start and recovery, and adds no public
firewall listener. The per-user bootstrap explains capture permission/readiness. UAC secure desktop
and Windows sign-in screens remain inaccessible.

## macOS

Target macOS 14+. ScreenCaptureKit provides displays/windows and system audio where available.
VideoToolbox provides low-latency H.264/HEVC-family hardware primitives, but v1 sends browser-safe
H.264. CGEvent injection requires Accessibility permission. ScreenCaptureKit requires Screen
Recording permission. The signed/notarized package installs a LaunchDaemon plus a LaunchAgent
because TCC permission is tied to an interactive code identity/session.

The setup tool opens the correct System Settings pages and verifies grants. It cannot click consent
for the user. Fast user switching and lock behavior are tested explicitly.

## Linux

Wayland capture uses xdg-desktop-portal + PipeWire; input uses RemoteDesktop portal where the
compositor supports it. Portal restore tokens are stored as non-secret configuration and may still
require renewed consent. Hardware encoding prefers VA-API, then platform-specific alternatives, then
bounded software x264.

X11 capture uses XDamage/XShm and input uses XTest. X11 cannot provide strong per-application input
isolation, so the diagnostics and UI label it as a weaker environment. A systemd system service
supervises a user-session process over a protected Unix socket. Supported reference desktops are
GNOME and KDE on current distributions; other compositors may be view-only.

## Packaging contract

Packages are built for x64 and arm64, signed per platform, and include:

- agent/service binaries and license notices;
- service manifests with restart backoff and sane resource limits;
- a `doctor` command and uninstall path;
- no embedded deployment secrets or static TURN password;
- atomic update support with signature verification and rollback.

Uninstall stops the service, revokes local credentials where reachable, removes binaries and service
registration, and preserves only an optional redacted diagnostic log after explicit consent.
