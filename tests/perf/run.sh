#!/usr/bin/env bash
# Mayara performance test harness.
#
# Spawns mayara under `samply` for each (workload × compression) combination,
# drives it with the Python spoke_viewer in load-driver mode, and saves
# labelled profile files for later comparison.
#
# Designed to run unchanged on both macOS (Apple Silicon) and Linux (the
# Raspberry Pi deploys). The mayara binary path is configurable so the
# same script can profile binaries built at different git refs — the
# typical use case is comparing a perf change against its parent commit
# on the same host.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

BINARY="$ROOT_DIR/target/release/mayara-server"
LABEL="HEAD"
OUTPUT_DIR="$SCRIPT_DIR/results"
WORKLOADS="emulator,navico,furuno,raymarine"
COMPRESSION="both"
DURATION=30
PORT=6504
CLIENTS=1

usage() {
  cat <<EOF
Usage: $0 [OPTIONS]

  --binary PATH         mayara-server binary (default: $BINARY)
  --label NAME          label prefix for output files (default: $LABEL)
  --output-dir DIR      output directory (default: $OUTPUT_DIR)
  --workloads LIST      comma-separated subset of: emulator,live,navico,furuno,raymarine
                        (default: $WORKLOADS; "live" discovers real radars on
                        the local network — requires one to be powered up)
  --compression MODE    both|on|off (default: $COMPRESSION)
  --duration SECONDS    client read duration per run (default: $DURATION)
  --port PORT           mayara listen port (default: $PORT)
  --clients N           parallel clients per run (default: $CLIENTS)
  -h, --help            this message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)       BINARY="$2"; shift 2;;
    --label)        LABEL="$2"; shift 2;;
    --output-dir)   OUTPUT_DIR="$2"; shift 2;;
    --workloads)    WORKLOADS="$2"; shift 2;;
    --compression)  COMPRESSION="$2"; shift 2;;
    --duration)     DURATION="$2"; shift 2;;
    --port)         PORT="$2"; shift 2;;
    --clients)      CLIENTS="$2"; shift 2;;
    -h|--help)      usage; exit 0;;
    *)              echo "Unknown argument: $1" >&2; usage >&2; exit 1;;
  esac
done

workload_args() {
  case "$1" in
    emulator)  echo "--emulator" ;;
    live)      echo "--transmit" ;;
    navico)    echo "--pcap $ROOT_DIR/testdata/pcap/navico-halo24.pcap.gz" ;;
    furuno)    echo "--pcap $ROOT_DIR/testdata/pcap/furuno-drs4dnxt.pcap.gz" ;;
    raymarine) echo "--pcap $ROOT_DIR/testdata/pcap/raymarine-quantum.pcap.gz" ;;
    *)         return 1 ;;
  esac
}

modes_for() {
  case "$1" in
    both) echo "deflate nodeflate";;
    on)   echo "deflate";;
    off)  echo "nodeflate";;
    *)    echo "Invalid --compression: $1" >&2; exit 1;;
  esac
}

flag_for_mode() {
  case "$1" in
    nodeflate) echo "--no-websocket-compression";;
    *)         echo "";;
  esac
}

[[ -x "$BINARY" ]] || { echo "Binary not executable: $BINARY" >&2; exit 1; }
command -v samply >/dev/null || { echo "samply not installed (cargo install samply)" >&2; exit 1; }

# On Linux, samply needs perf_event_paranoid <= 1 for non-root sampling.
if [[ "$(uname -s)" == "Linux" ]]; then
  paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 99)
  if (( paranoid > 1 )); then
    echo "Warning: /proc/sys/kernel/perf_event_paranoid=$paranoid (need <=1 for samply)."
    echo "  sudo sysctl kernel.perf_event_paranoid=1"
    exit 1
  fi
fi

mkdir -p "$OUTPUT_DIR"

# Verify pcap fixtures for selected pcap workloads
for w in $(echo "$WORKLOADS" | tr ',' ' '); do
  args=$(workload_args "$w") || { echo "Unknown workload: $w" >&2; exit 1; }
  if [[ "$args" == --pcap* ]]; then
    pcap=$(echo "$args" | awk '{print $2}')
    if [[ ! -f "$pcap" ]]; then
      echo "Missing fixture: $pcap" >&2
      echo "Run: make fixtures (from $ROOT_DIR)" >&2
      exit 1
    fi
  fi
done

# Track the active samply pid so cleanup can SIGINT exactly the right
# child. Using `pkill -f "$BINARY"` would also match this script's own
# argv (which contains $BINARY as an arg) and kill ourselves.
SAMPLY_PID=""

cleanup() {
  if [[ -n "$SAMPLY_PID" ]] && kill -0 "$SAMPLY_PID" 2>/dev/null; then
    kill -INT "$SAMPLY_PID" 2>/dev/null || true
    sleep 1
    kill -9 "$SAMPLY_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

run_one() {
  local workload="$1"
  local mode="$2"
  local stem="${LABEL}-${workload}-${mode}"
  local profile="$OUTPUT_DIR/${stem}.json.gz"
  local logfile="$OUTPUT_DIR/${stem}.server.log"
  local client_log="$OUTPUT_DIR/${stem}.client.log"

  local args
  args=$(workload_args "$workload")
  local flag
  flag=$(flag_for_mode "$mode")

  echo
  echo "=== $LABEL / $workload / $mode ==="
  cleanup
  sleep 1
  rm -f "$profile" "$logfile" "$client_log"

  # shellcheck disable=SC2086
  samply record --save-only --unstable-presymbolicate \
    -o "$profile" -- \
    "$BINARY" $args --port "$PORT" $flag > "$logfile" 2>&1 &
  SAMPLY_PID=$!
  local samply_pid=$SAMPLY_PID

  # Wait for radar discovery — emulator is immediate, pcap can take
  # 30 s+ for the radar's discovery beacon to be dispatched.
  local discover_deadline=$(($(date +%s) + 90))
  while ! curl -fs "http://localhost:$PORT/signalk/v2/api/vessels/self/radars" 2>/dev/null \
          | grep -q '"name"'; do
    if (( $(date +%s) > discover_deadline )); then
      echo "  TIMEOUT: radar discovery (see $logfile)" >&2
      kill -9 "$samply_pid" 2>/dev/null || true
      return 1
    fi
    if ! kill -0 "$samply_pid" 2>/dev/null; then
      echo "  FAIL: mayara exited before radar discovery (see $logfile)" >&2
      return 1
    fi
    sleep 1
  done
  echo "  radar up, starting $CLIENTS client(s) for ${DURATION}s"

  # Launch parallel clients
  local client_pids=()
  local i
  for ((i=0; i<CLIENTS; i++)); do
    "$ROOT_DIR/client-examples/python-client/run.sh" \
      --url "http://localhost:$PORT" --duration "$DURATION" \
      > "${client_log%.log}.${i}.log" 2>&1 &
    client_pids+=($!)
  done

  for pid in "${client_pids[@]}"; do
    wait "$pid" 2>/dev/null || true
  done

  # SIGINT mayara directly via samply's child pid. samply forwards
  # SIGINT to its child unreliably (especially on Linux without a PTY),
  # so we look up samply's child ourselves. mayara's
  # tokio_graceful_shutdown handler then exits cleanly, which lets
  # samply finalise the profile.
  local mayara_pid
  mayara_pid=$(pgrep -P "$samply_pid" -n || true)
  if [[ -n "$mayara_pid" ]]; then
    kill -INT "$mayara_pid" 2>/dev/null || true
  fi
  wait "$samply_pid" 2>/dev/null || true
  SAMPLY_PID=""

  if [[ -f "$profile" ]]; then
    local size
    size=$(wc -c < "$profile" | tr -d ' ')
    echo "  saved $(basename "$profile") (${size} bytes)"
  else
    echo "  PROFILE FAILED (see $logfile)" >&2
    return 1
  fi
}

failures=0
for w in $(echo "$WORKLOADS" | tr ',' ' '); do
  for m in $(modes_for "$COMPRESSION"); do
    if ! run_one "$w" "$m"; then
      failures=$((failures + 1))
    fi
  done
done

echo
echo "Done. $failures failure(s). Profiles in $OUTPUT_DIR"
echo
echo "Analyze with:"
echo "  python3 $SCRIPT_DIR/analyze.py $OUTPUT_DIR/*.json.gz"

exit "$failures"
