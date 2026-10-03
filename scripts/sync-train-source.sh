#!/usr/bin/env bash
# Syncs a pinned HOT-Step source (engines/<tool>-source.json) into a build
# worktree. Linux port of the sync step of scripts/build-train-runtime.ps1
# (removed with the Windows shell; see git history).
#
# Usage: ./scripts/sync-train-source.sh <music-train|music-midi> [worktree-dir]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="${1:-music-train}"
case "$TOOL" in music-train|music-midi) ;; *) echo "error: tool must be music-train or music-midi" >&2; exit 1 ;; esac

SOURCE="$REPO_ROOT/engines/$TOOL-source.json"
[ -f "$SOURCE" ] || { echo "error: no $SOURCE" >&2; exit 1; }
REPOSITORY="$(python3 -c "import json; print(json.load(open('$SOURCE'))['repository'])")"
COMMIT="$(python3 -c "import json; print(json.load(open('$SOURCE'))['commit'])")"
SHORT="${COMMIT:0:8}"

WORKTREE="${2:-${YUE_ENGINE_BUILD_ROOT:-$HOME/.cache/yue2-studio}/hotstep-$SHORT}"

command -v git >/dev/null || { echo "error: git is required on PATH" >&2; exit 1; }

if [ ! -d "$WORKTREE/.git" ]; then
  echo "cloning $REPOSITORY into $WORKTREE"
  git clone "$REPOSITORY" "$WORKTREE"
fi
echo "fetching $COMMIT"
git -C "$WORKTREE" fetch origin "$COMMIT"
git -C "$WORKTREE" checkout --detach "$COMMIT"
# ggml carries the training patches; the VST3 SDK is only needed for the
# build tree to configure.
git -C "$WORKTREE" submodule update --init --recursive engine/ggml engine/vendor/vst3sdk
echo "synced: $(git -C "$WORKTREE" rev-parse --short HEAD) in $WORKTREE"
