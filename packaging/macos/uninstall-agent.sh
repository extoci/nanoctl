#!/bin/sh
set -eu

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
