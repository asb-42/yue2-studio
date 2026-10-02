#!/usr/bin/env bash
# Linux fork launcher: the studio service plus the web UI, no Tauri.
#
# Usage:
#   ./scripts/run-linux.sh            # debug service + vite dev UI
#   ./scripts/run-linux.sh --build    # release service + static vite build
#   ./scripts/run-linux.sh --service-only
#
# Environment overrides (all optional):
#   YUE_BIND_ADDR           bind address; 0.0.0.0 opens the LAN (still key-gated:
#                         enable network access from loopback, see docs/linux.md)
#   YUE_STUDIO_DATA_ROOT  default: ${XDG_DATA_HOME:-~/.local/share}/yue2-studio
#   YUE_MODELS_ROOT       default: $DATA_ROOT/models/yue2-cpp
#   YUE_ENGINE_BIN        explicit yue-server binary for a developer build
#   YUE_TRAIN_BIN / YUE_MIDI_BIN / YUE_CAPTION_BIN as in the service
#   CUDA_VISIBLE_DEVICES  passed through to every CUDA process
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="dev"
for arg in "$@"; do
  case "$arg" in
    --build) MODE="build" ;;
    --service-only) MODE="service-only" ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

export PATH="$HOME/.cargo/bin:$PATH"

DATA_ROOT="${YUE_STUDIO_DATA_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/yue2-studio}"
export YUE_STUDIO_DATA_ROOT="$DATA_ROOT"
export YUE_MODELS_ROOT="${YUE_MODELS_ROOT:-$DATA_ROOT/models/yue2-cpp}"
export YUE_STUDIO_SETTINGS_PATH="${YUE_STUDIO_SETTINGS_PATH:-$DATA_ROOT/studio-settings.json}"
export YUE_ENGINE_HOST="${YUE_ENGINE_HOST:-127.0.0.1}"
export YUE_ENGINE_PORT="${YUE_ENGINE_PORT:-18087}"
export YUE_ENGINE_BASE_URL="${YUE_ENGINE_BASE_URL:-http://$YUE_ENGINE_HOST:$YUE_ENGINE_PORT}"
# A build of scripts/build-yue-runtime.sh beside this checkout is picked up
# automatically; YUE_ENGINE_ROOT / YUE_ENGINE_BIN override it explicitly.
if [ -z "${YUE_ENGINE_ROOT:-}" ] && [ -z "${YUE_ENGINE_BIN:-}" ] && [ -x "$REPO_ROOT/dist/yue2-cpp/yue-server" ]; then
  export YUE_ENGINE_ROOT="$REPO_ROOT/dist/yue2-cpp"
fi

# A Linux engine needs the system CUDA toolkit on the loader path; without it
# the CUDA backend fails to load and Auto falls through to Vulkan/CPU.
if [ -d /usr/local/cuda/lib64 ] && [[ ":${LD_LIBRARY_PATH:-}:" != *":/usr/local/cuda/lib64:"* ]]; then
  export LD_LIBRARY_PATH="/usr/local/cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

mkdir -p "$DATA_ROOT"

if [ "$MODE" = "build" ] || [ "$MODE" = "service-only" ]; then
  cargo build --release -p music-server
  SERVICE_BIN="$REPO_ROOT/target/release/music-server"
else
  cargo build -p music-server
  SERVICE_BIN="$REPO_ROOT/target/debug/music-server"
fi

if [ "$MODE" = "service-only" ]; then
  echo "data:    $DATA_ROOT"
  echo "service: http://127.0.0.1:8791 (health: /health, app: /v1/..., agents: /mcp)"
  exec "$SERVICE_BIN"
fi

"$SERVICE_BIN" &
SERVICE_PID=$!
trap 'kill $SERVICE_PID 2>/dev/null' EXIT

# Wait for the Axum service before opening the UI onto it.
for _ in $(seq 1 100); do
  if curl -sf http://127.0.0.1:8791/health >/dev/null 2>&1; then
    break
  fi
  sleep 0.3
done

if [ "$MODE" = "build" ]; then
  npm --prefix "$REPO_ROOT/app" run build
  export YUE_UI_DIR="$REPO_ROOT/app/dist"
  echo "UI:      http://127.0.0.1:8791/ (served by the service from $YUE_UI_DIR)"
  echo "         (needs YUE_UI_DIR set when run directly: export YUE_UI_DIR=$REPO_ROOT/app/dist)"
  wait "$SERVICE_PID"
else
  echo "data:    $DATA_ROOT"
  echo "service: http://127.0.0.1:8791"
  echo "UI:      http://127.0.0.1:3791"
  (cd "$REPO_ROOT" && npm --prefix app run dev)
fi
