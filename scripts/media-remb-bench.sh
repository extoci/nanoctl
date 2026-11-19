#!/usr/bin/env bash
set -euo pipefail
if [[ $# -lt 3 ]]; then
  echo "usage: $0 BINARY OUTPUT_DIR PREFIX [idle motion throttled idle-motion recovery]" >&2
  exit 2
fi
binary=$(realpath "$1")
out=$(realpath -m "$2")
prefix=$3
shift 3
if [[ $# == 0 ]]; then set -- idle motion throttled idle-motion recovery; fi
mkdir -p "$out"
export DISPLAY=${DISPLAY:-:0} NANOCTL_DEV_LOOPBACK_USER=bench RUST_LOG=error NANOCTL_BENCH_CHROME_REMB=1
scene='' old='' agent='' shaped=0
mode=$(xrandr --current | awk '/\*/ {print $1; exit}')
output=$(xrandr --current | awk '/ connected/ {print $1; exit}')
cleanup() {
  if [[ -n $agent ]]; then kill "$agent" 2>/dev/null || true; wait "$agent" 2>/dev/null || true; fi
  for pid in "$scene" "$old"; do
    if [[ -n $pid ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  done
  if [[ $shaped == 1 ]]; then sudo -n tc qdisc del dev lo root || true; fi
  xrandr --output "$output" --mode "$mode"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
tc qdisc show dev lo | rg -q 'qdisc noqueue'
xrandr --output "$output" --mode 1920x1080
for scenario in "$@"; do
  case "$scenario" in idle|motion|throttled|idle-motion|recovery) ;; *) exit 2 ;; esac
  source='testsrc2=size=1920x1080:rate=60'
  seconds=30
  if [[ $scenario == idle || $scenario == idle-motion ]]; then source='color=c=0x203040:size=1920x1080:rate=1'; fi
  ffplay -hide_banner -loglevel error -f lavfi -i "$source" -an -noborder -fs -window_title nanoctl-bench > "$out/$prefix-$scenario-scene.log" 2>&1 &
  scene=$!
  sleep 2
  kill -0 "$scene"
  if [[ $scenario == throttled || $scenario == recovery ]]; then
    sudo -n tc qdisc add dev lo root netem rate 2mbit delay 40ms loss 2%
    shaped=1
  fi
  if [[ $scenario == recovery ]]; then seconds=50; fi
  ready="$out/$prefix-$scenario.ready"
  rm -f "$ready"
  NANOCTL_BENCH_READY_FILE="$ready" timeout 120 "$binary" --log-file "$out/$prefix-$scenario-agent.log" media-bench --seconds "$seconds" --json > "$out/$prefix-$scenario.json" 2> "$out/$prefix-$scenario.stderr" &
  agent=$!
  until [[ -f $ready ]]; do kill -0 "$agent"; sleep .05; done
  if [[ $scenario == idle-motion ]]; then
    sleep 10
    old=$scene
    ffplay -hide_banner -loglevel error -f lavfi -i 'testsrc2=size=1920x1080:rate=60' -an -noborder -fs -window_title nanoctl-bench-motion > "$out/$prefix-$scenario-motion-scene.log" 2>&1 &
    scene=$!
    sleep .3
    kill "$old"; wait "$old" 2>/dev/null || true
    old=
  fi
  if [[ $scenario == recovery ]]; then
    sleep 20
    sudo -n tc qdisc del dev lo root
    shaped=0
  fi
  wait "$agent"
  agent=
  python3 -m json.tool "$out/$prefix-$scenario.json" >/dev/null
  tc -s qdisc show dev lo > "$out/$prefix-$scenario-qdisc.txt"
  if [[ $shaped == 1 ]]; then sudo -n tc qdisc del dev lo root; shaped=0; fi
  kill "$scene"; wait "$scene" 2>/dev/null || true
  scene=
done
