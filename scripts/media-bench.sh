#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 BEFORE_BINARY AFTER_BINARY OUTPUT_DIR [SECONDS>=30]" >&2
  exit 2
fi
before=$(realpath "$1")
after=$(realpath "$2")
out=$(realpath -m "$3")
seconds=${4:-30}
[[ $seconds =~ ^[0-9]+$ && $seconds -ge 30 ]]
mkdir -p "$out"
export DISPLAY=${DISPLAY:-:0}
export NANOCTL_DEV_LOOPBACK_USER=bench
export RUST_LOG=error
scene_pid=
shaped=0
old_mode=$(xrandr --current | awk '/\*/ {print $1; exit}')
output=$(xrandr --current | awk '/ connected/ {print $1; exit}')
cleanup() {
  if [[ $shaped == 1 ]]; then sudo -n tc qdisc del dev lo root; fi
  if [[ -n $scene_pid ]]; then kill "$scene_pid" 2>/dev/null || true; wait "$scene_pid" 2>/dev/null || true; fi
  xrandr --output "$output" --mode "$old_mode"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
tc qdisc show dev lo | tee "$out/qdisc-before.txt"
tc qdisc show dev lo | rg -q 'qdisc noqueue'
xrandr --output "$output" --mode 1920x1080
{
  date -u
  uname -a
  getconf _NPROCESSORS_ONLN
  getconf CLK_TCK
  printf 'DISPLAY=%s NANOCTL_DEV_LOOPBACK_USER=%s RUST_LOG=%s\n' "$DISPLAY" "$NANOCTL_DEV_LOOPBACK_USER" "$RUST_LOG"
  printf 'before=%s\nafter=%s\nseconds=%s\n' "$before" "$after" "$seconds"
  xrandr --current
  ffplay -version | head -1
} > "$out/environment.txt"
for scenario in idle motion throttled; do
  if [[ $scenario == idle ]]; then
    source='color=c=0x203040:size=1920x1080:rate=1'
  else
    source='testsrc2=size=1920x1080:rate=60'
  fi
  ffplay -hide_banner -loglevel error -f lavfi -i "$source" -an -noborder -fs -window_title nanoctl-bench > "$out/$scenario-scene.log" 2>&1 &
  scene_pid=$!
  sleep 2
  kill -0 "$scene_pid"
  for revision in before after; do
    binary=$before
    if [[ $revision == after ]]; then binary=$after; fi
    if [[ $scenario == throttled ]]; then
      sudo -n tc qdisc add dev lo root netem rate 2mbit delay 40ms loss 2%
      shaped=1
    fi
    printf '%s %s %s\n' "$(date -u +%FT%TZ)" "$revision" "$scenario"
    timeout "$((seconds + 60))" "$binary" --log-file "$out/$revision-$scenario-agent.log" media-bench --seconds "$seconds" --json > "$out/$revision-$scenario.json" 2> "$out/$revision-$scenario.stderr"
    python3 -m json.tool "$out/$revision-$scenario.json" > /dev/null
    tc -s qdisc show dev lo > "$out/$revision-$scenario-qdisc.txt"
    if [[ $shaped == 1 ]]; then
      sudo -n tc qdisc del dev lo root
      shaped=0
    fi
  done
  kill "$scene_pid"
  wait "$scene_pid" 2>/dev/null || true
  scene_pid=
done
tc qdisc show dev lo | tee "$out/qdisc-after.txt"
