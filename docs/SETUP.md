# Host setup and removal

Release packages wrap the platform registrations in `packaging/`. Until signed packages are
produced, build the agent with `cargo build --release --features media`, enroll it as the
interactive desktop user, run `nanoctl doctor`, and then register the background agent.

Before registration, exercise native capture and encoding on the interactive desktop:

```shell
nanoctl media-smoke --require-hardware --seconds 30
```

Use `--json` when collecting the acceptance record from a signed release candidate. Screen-capture
permission prompts must be completed by the candidate identity before this command can pass.

The enrollment user and background-agent user must be identical. Screen capture, input permission,
and the OS credential entry belong to that interactive identity. None of the v1 registrations opens
an inbound port.

## Windows 11

Open an elevated PowerShell in the enrolled user's session:

```powershell
$configPath = ((.\nanoctl.exe paths) -replace '^config=', '')
.\packaging\windows\install-service.ps1 `
  -BinaryPath .\nanoctl.exe `
  -ConfigPath $configPath
```

Despite the compatibility filename, the script registers a headless highest-privilege Scheduled
Task for the current interactive user. A LocalSystem Windows service runs in Session 0 and cannot
capture or inject into that user's desktop or read their Credential Manager item. The task starts
at logon and restarts after failure. The signed installer supplies the exact generated config path.

Remove it from the same user session with `uninstall-agent.ps1`. Removal deletes the local
credential and configuration; revoke the device in the dashboard as well if it is still listed.

## macOS 14+

Grant Screen Recording and Accessibility to the signed nanoctl identity, then run:

```sh
packaging/macos/install-agent.sh target/release/nanoctl
```

This installs a per-user LaunchAgent because TCC grants and interactive capture are user-session
scoped. Remove it with `packaging/macos/uninstall-agent.sh`. An unsigned locally built binary is for
development only; changing its code signature may invalidate TCC approval.

## Linux

For an enrolled graphical user:

```sh
packaging/linux/install-user-service.sh target/release/nanoctl
```

This installs and enables a hardened systemd user service. Wayland capture/input may still require
portal interaction and compositor support. Remove it with
`packaging/linux/uninstall-user-service.sh`.

## Verification after setup

1. Reboot or sign out/in and confirm the background registration is running.
2. Run `nanoctl doctor --json` as the enrolled user without printing any credential.
3. Confirm the dashboard reports the device online within 45 seconds.
4. Connect from a separate network, verify pointer, wheel, keyboard, fullscreen, and explicit end.
5. Revoke from the dashboard and verify no new session can start.
6. Uninstall, confirm the registration and credential are gone, and retain no diagnostic logs
   unless the owner explicitly requested them.

## Signed updates

Linux and macOS packages include `update-user-service.sh` or `update-agent.sh`. Each accepts the
downloaded signed manifest and the base64 Ed25519 publisher public key installed through the trusted
package channel. The script stops the user service, verifies and stages the target-specific
artifact, activates it atomically, restarts, runs `doctor`, and either commits the update or restores
the previous binary.

For inspection without changing files:

```sh
nanoctl verify-update manifest.json --public-key "$NANOCTL_UPDATE_PUBLIC_KEY"
```

Do not copy a public key from the update server itself: that would make a compromised distribution
server its own trust anchor. On Windows, `update-agent.ps1` performs the same transaction outside
the stopped Scheduled Task because a running Windows image cannot safely replace itself. An
unsigned local build must not be mixed with the signed update channel.
