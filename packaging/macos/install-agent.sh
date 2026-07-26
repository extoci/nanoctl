#!/bin/sh
set -eu

source_binary=${1:?usage: install-agent.sh /path/to/nanoctl}
install_root="$HOME/Library/Application Support/nanoctl"
binary_path="$install_root/bin/nanoctl"
agent_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"

install -d -m 700 "$install_root/bin" "$HOME/Library/LaunchAgents"
install -m 755 "$source_binary" "$binary_path"
install -m 600 "$(dirname "$0")/dev.nanoctl.agent.plist" "$agent_path"
/usr/libexec/PlistBuddy -c "Set :ProgramArguments:0 $binary_path" "$agent_path"

launchctl bootout "gui/$(id -u)/dev.nanoctl.agent" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$agent_path"
launchctl kickstart -k "gui/$(id -u)/dev.nanoctl.agent"
printf '%s\n' "nanoctl LaunchAgent installed and started."
