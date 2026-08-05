# Host setup

Create a setup code in the nanoctl dashboard, then run the installer as the desktop user that will
share this computer.

Linux and macOS:

```sh
curl -fsSL https://extoci.lol/nanoctl/install | sh
```

Windows PowerShell:

```powershell
irm https://extoci.lol/nanoctl/install | iex
```

The installer selects the correct x64 or arm64 executable, verifies its SHA-256 checksum, asks for
the setup code on first install, and starts the background agent. Run the same command again to
update to the latest release without enrolling again. It also adds `nanoctl` to the current user's
PATH for new terminals.

Windows prints the resolved release and target before downloading, for example
`Downloading nanoctl 1.0.17 for windows-x64...`, verifies that the executable reports that exact
version, and prints the installed command path when the upgrade completes. A failed health-gated
upgrade restores the previous task and executable rather than claiming that the new version is
installed.

If automatic platform selection is undesirable, use
`https://extoci.lol/nanoctl/install.sh` or
`https://extoci.lol/nanoctl/install.ps1` explicitly.

Linux uses a systemd user service, macOS uses a per-user LaunchAgent, and Windows uses a hidden,
non-elevated Scheduled Task launched through the windowless Windows Script Host (`wscript.exe`).
The agent must run in the enrolled interactive user's session so it can access that user's
credential store, screen, and input APIs. It does not open an inbound port. The registration starts
at login and restarts after failures. While idle, the agent does not capture the screen or initialize
a media session; it only performs lightweight control-plane checks while waiting for an authorized
session. Windows runtime
output is written to `%LOCALAPPDATA%\nanoctl\agent.log`; no console window is opened by the task.

Running the installer again is an in-place, health-gated upgrade. It preserves the existing
configuration and enrollment, migrates an older explicit `--config` task path when needed, and
retains the previous executable until the new task survives startup. If an upgrade fails, rerun the
same installer; the transaction restores the previous binary and task automatically.

Configurations created by an older elevated setup may be owned by `BUILTIN\Administrators`. The
installer accepts that legacy owner only when run by an administrator and grants the enrolled user
explicit access; it still rejects a task or configuration belonging to another Windows user.

macOS may ask for Screen Recording and Accessibility. Wayland may ask through its desktop portal.
Complete those prompts as the user who ran the installer.

## Verification

Run:

```sh
nanoctl doctor
nanoctl media-smoke --require-hardware --seconds 30
```

Then confirm the dashboard reports the device online and test a connection from another network.
Use `--json` when collecting a machine-readable acceptance record.

## Advanced and development setup

The installer supports these environment overrides:

- `NANOCTL_VERSION=v1.2.3` installs a specific release instead of `latest`.
- `NANOCTL_REPOSITORY=owner/repository` downloads from another GitHub repository.
- `NANOCTL_CONTROL_PLANE=https://example.convex.site` enrolls against another deployment.
- `NANOCTL_ENROLL_CODE=...` supplies the setup code without an interactive prompt.
- `NANOCTL_BINARY=/path/to/nanoctl` uses a local Unix executable instead of downloading.

The scripts under `packaging/` remain low-level development and removal helpers. Normal
installation and updates should use the one-command installer.
