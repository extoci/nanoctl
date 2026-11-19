#!/bin/sh
set -eu
# shellcheck source=packaging/tailnet.sh
. "$(dirname "$0")/../tailnet.sh"

[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Uninstall nanoctl as the desktop user, not root." >&2
  exit 1
}
install_root="$HOME/Library/Application Support/nanoctl"
binary_path="$install_root/bin/nanoctl"
agent_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"

launchctl bootout "gui/$(id -u)/dev.nanoctl.agent" 2>/dev/null || true
if [ -x "$binary_path" ]; then
  remove_legacy_credential "$binary_path"
fi
rm -f "$agent_path" "$binary_path"
rmdir "$install_root/bin" "$install_root" 2>/dev/null || true
printf '%s\n' "nanoctl LaunchAgent removed; configuration preserved."
