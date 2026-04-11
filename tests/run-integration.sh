#!/usr/bin/env bash
set -euo pipefail

PORT="${MAYARA_TEST_PORT:-6502}"
BASE_URL="http://localhost:${PORT}"
SERVER="./target/release/mayara-server"
LOG_FILE=$(mktemp)

# Build release binary
echo "Building release binary..."
cargo build --release --quiet

# Start emulator server, capture log to extract PID
echo "Starting emulator on port ${PORT}..."
"${SERVER}" --emulator --port "${PORT}" >"${LOG_FILE}" 2>&1 &

cleanup() {
    echo "Stopping server..."
    curl -s "${BASE_URL}/quit" >/dev/null 2>&1 || true
    rm -f "${LOG_FILE}"
}
trap cleanup EXIT

# Wait for server to be ready and extract PID from log
SERVER_PID=""
for i in $(seq 1 30); do
    if grep -q "Starting HTTP web server on port ${PORT}" "${LOG_FILE}" 2>/dev/null; then
        SERVER_PID=$(grep "Starting HTTP web server on port ${PORT}" "${LOG_FILE}" | sed 's/.*(pid \([0-9]*\)).*/\1/')
        break
    fi
    sleep 0.2
done

if [ -z "${SERVER_PID}" ]; then
    echo "Server did not start in time. Log output:"
    cat "${LOG_FILE}"
    exit 1
fi

echo "Server ready (pid ${SERVER_PID}), running tests..."

# Run all tests including integration tests
MAYARA_TEST_URL="${BASE_URL}" \
MAYARA_TEST_WS_URL="ws://localhost:${PORT}" \
cargo test -- --include-ignored

echo "All tests passed."
