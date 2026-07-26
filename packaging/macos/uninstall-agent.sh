#!/bin/sh
set -eu

[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Uninstall nanoctl as the enrolled desktop user, not root." >&2
  exit 1
}
install_root="$HOME/Library/Application Support/nanoctl"
binary_path="$install_root/bin/nanoctl"
agent_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"

launchctl bootout "gui/$(id -u)/dev.nanoctl.agent" 2>/dev/null || true
if [ -x "$binary_path" ]; then
  "$binary_path" unenroll
fi
rm -f "$agent_path" "$binary_path"
rmdir "$install_root/bin" "$install_root" 2>/dev/null || true
printf '%s\n' "nanoctl LaunchAgent and local enrollment removed."
