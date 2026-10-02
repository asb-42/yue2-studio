#!/usr/bin/env bash
# Syncs the pinned yue2.cpp source (engines/yue2-cpp-source.json) into a build
# worktree. Linux port of the Sync-PinnedSource step of
# scripts/build-yue-runtime.ps1.
#
# One checkout per pinned commit: moving to a new commit leaves the build
# directories in place, so Ninja recompiles only what changed instead of all
# of ggml and its CUDA kernels.
#
# Usage: ./scripts/sync-yue-source.sh [worktree-dir]
# Default worktree: ${YUE_ENGINE_BUILD_ROOT:-$HOME/.cache/yue2-studio}/yue2-engine
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_SOURCE="$REPO_ROOT/engines/yue2-cpp-source.json"

REPOSITORY="$(python3 -c "import json; print(json.load(open('$ENGINE_SOURCE'))['repository'])")"
COMMIT="$(python3 -c "import json; print(json.load(open('$ENGINE_SOURCE'))['commit'])")"

WORKTREE="${1:-${YUE_ENGINE_BUILD_ROOT:-$HOME/.cache/yue2-studio}/yue2-engine}"

for tool in git cmake; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required on PATH" >&2; exit 1; }
done

if [ ! -d "$WORKTREE/.git" ]; then
  echo "cloning $REPOSITORY into $WORKTREE"
  git clone --recurse-submodules "$REPOSITORY" "$WORKTREE"
fi
echo "fetching $COMMIT"
git -C "$WORKTREE" fetch origin "$COMMIT"
git -C "$WORKTREE" checkout --detach "$COMMIT"
git -C "$WORKTREE" submodule update --init --recursive
echo "synced: $(git -C "$WORKTREE" rev-parse --short HEAD) in $WORKTREE"
