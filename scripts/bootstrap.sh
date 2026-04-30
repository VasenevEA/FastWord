#!/usr/bin/env bash
# Bootstrap FastWord sidecar: create ~/.fastword/venv, install mlx-whisper,
# copy the sidecar script. Idempotent — re-run to upgrade.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$HOME/.fastword"
VENV="$DEST/venv"
SIDECAR_DST="$DEST/sidecar"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "FastWord requires macOS." >&2
    exit 1
fi

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "Warning: MLX targets Apple Silicon. Intel Macs are unsupported." >&2
fi

mkdir -p "$DEST" "$SIDECAR_DST"

PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
    echo "python3 not found. Install Python 3.11+ (e.g. brew install python@3.11)." >&2
    exit 1
fi

if [[ ! -d "$VENV" ]]; then
    echo "Creating venv at $VENV"
    "$PYTHON_BIN" -m venv "$VENV"
fi

# shellcheck disable=SC1091
source "$VENV/bin/activate"
pip install --upgrade pip
pip install -r "$REPO_ROOT/sidecar/requirements.txt"

cp "$REPO_ROOT/sidecar/sidecar.py" "$SIDECAR_DST/sidecar.py"

echo
echo "FastWord sidecar installed."
echo "  python: $VENV/bin/python3"
echo "  script: $SIDECAR_DST/sidecar.py"
echo
echo "First run will download the MLX Whisper model (~1.5GB)."
