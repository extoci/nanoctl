#!/bin/sh
set -eu

manifest_path=${1:?usage: update-agent.sh /path/to/manifest.json PUBLIC_KEY_BASE64}
public_key=${2:?usage: update-agent.sh /path/to/manifest.json PUBLIC_KEY_BASE64}
[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Update nanoctl as the enrolled desktop user, not root." >&2
  exit 1
}
install_root="$HOME/Library/Application Support/nanoctl"
binary_path="$install_root/bin/nanoctl"
agent_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"
service_target="gui/$(id -u)/dev.nanoctl.agent"
activated=0
completed=0

cleanup() {
  if [ "$completed" -eq 1 ]; then
    return
  fi
  launchctl bootout "$service_target" 2>/dev/null || true
  if [ "$activated" -eq 1 ] || [ -f "$binary_path.previous" ]; then
    "$binary_path" rollback-update 2>/dev/null || true
  fi
  launchctl bootstrap "gui/$(id -u)" "$agent_path" 2>/dev/null || true
  launchctl kickstart -k "$service_target" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

launchctl bootout "$service_target" 2>/dev/null || true
"$binary_path" stage-update "$manifest_path" --public-key "$public_key"
"$binary_path" activate-update "$manifest_path" --public-key "$public_key"
activated=1
launchctl bootstrap "gui/$(id -u)" "$agent_path"
launchctl kickstart -k "$service_target"

if "$binary_path" doctor; then
  "$binary_path" commit-update
  completed=1
  trap - EXIT HUP INT TERM
  printf '%s\n' "nanoctl update activated and committed."
  exit 0
fi

printf '%s\n' "nanoctl update failed its health check and was rolled back." >&2
exit 1
