#!/usr/bin/env bash
# Run the target viewer with a virtual environment
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
VENV="${DIR}/.venv"

if [ ! -d "${VENV}" ]; then
    echo "Creating virtual environment..."
    python3 -m venv "${VENV}"
    "${VENV}/bin/pip" install --quiet -r "${DIR}/requirements.txt"
fi

exec "${VENV}/bin/python3" "${DIR}/target_viewer.py" "$@"
