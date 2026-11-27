#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == --with-display ]]; then
  shift
  # Manage the authority file as the test user and preserve Playwright's exit status.
  display_dir=$(mktemp -d)
  trap 'rm -rf -- "$display_dir"' EXIT
  xvfb-run -a -f "$display_dir/Xauthority" -s "-screen 0 1280x720x24 -nolisten tcp" "$@"
  exit
fi

# The agent binary must already include the built viewer. Prepare the executable
# outside the namespace so Cargo and browser installation can reach the network.
if [[ ${1:-} != --inside ]]; then
  if [[ $# != 1 || ! -x $1 ]]; then
    echo "Usage: $0 path/to/prebuilt/nanoctl" >&2
    exit 2
  fi
  gate_script=$(realpath "$0")
  agent=$(realpath "$1")
  bun=$(command -v bun)
  node=$(command -v node)
  if unshare --user --map-root-user --net -- bash -c '
    ip link set lo up && ip link add probe0 type veth peer name probe1 &&
      tc qdisc add dev lo root netem limit 1
  ' >/dev/null 2>&1; then
    exec unshare --user --map-root-user --net -- bash "$gate_script" --inside "$agent" "$bun" "$node"
  fi
  exec sudo -n -E unshare --net -- bash "$gate_script" --inside "$agent" "$bun" "$node"
fi

agent=$2
bun=$3
node=$4
ip link set lo up
# Chromium needs an ordinary host ICE address. Traffic to this locally owned
# address still traverses loopback and the UDP filter in this namespace.
ip link add gate0 type veth peer name gate1
ip addr add 100.64.0.2/24 dev gate0
ip link set gate0 up
ip link set gate1 up

tc qdisc add dev lo root handle 1: prio
tc qdisc add dev lo parent 1:1 handle 10: netem \
  limit 128 delay 25ms 8ms loss 5% reorder 25% rate 3mbit
tc filter add dev lo protocol ip parent 1: prio 1 flower ip_proto udp classid 1:1
tc filter add dev lo protocol ipv6 parent 1: prio 2 flower ip_proto udp classid 1:1
tc -s -d qdisc show dev lo

cd "$(dirname "$0")/.."
# sudo can restore a system-only PATH. The Playwright web server also launches Bun.
args=(env "PATH=$(dirname "$bun"):$PATH" NANOCTL_BROWSER_MEDIA=1 NANOCTL_BROWSER_BAD_NETWORK=1
  "NANOCTL_BROWSER_AGENT_BIN=$agent"
  bash "$(realpath "$0")" --with-display
  "$node" node_modules/@playwright/test/cli.js test
  -c tests/browser/playwright.config.ts bad-network.spec.ts --project chromium)
if [[ -n ${SUDO_USER:-} ]]; then
  exec runuser -u "$SUDO_USER" --preserve-environment -- "${args[@]}"
fi
exec "${args[@]}"
