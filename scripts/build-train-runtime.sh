#!/usr/bin/env bash
# Builds a HOT-Step tool at the pinned commit for Linux: the adapter trainer
# (ace-train), the listening captioner (ace-caption), and the audio-to-MIDI
# transcriber (ace-midi) — each with its own ggml libraries, which carry
# HOT-Step's patches and never share a folder with the engine's.
# Linux port of scripts/build-train-runtime.ps1 (see git history).
#
# Usage:
#   ./scripts/build-train-runtime.sh --output <dir> [--tool music-train|music-midi|ace-caption]
#       [--arch universal|native]
#
# <dir> receives the tool binary, its libggml*.so* and runtime.json.
# BF16 training needs Ampere or newer; transcription runs on Turing too.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT=""
TOOL="music-train"
ARCH="universal"

while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUTPUT="$2"; shift 2 ;;
    --tool) TOOL="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
[ -n "$OUTPUT" ] || { echo "error: --output <dir> is required" >&2; exit 1; }
case "$TOOL" in music-train|music-midi|ace-caption) ;; *) echo "error: --tool must be music-train, music-midi or ace-caption" >&2; exit 1 ;; esac
case "$ARCH" in universal|native) ;; *) echo "error: --arch must be universal|native" >&2; exit 1 ;; esac

for tool in git cmake ninja gcc nvcc; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required on PATH (the trainer is CUDA only)" >&2; exit 1; }
done

# The trainer/midi manifests pin the same HOT-Step commit; the captioner
# ships from that tree too.
MANIFEST="$TOOL"
[ "$TOOL" = "ace-caption" ] && MANIFEST="music-train"
SOURCE="$REPO_ROOT/engines/$MANIFEST-source.json"
TARGET="$TOOL"
[ "$TOOL" = "music-train" ] && TARGET="ace-train"
[ "$TOOL" = "music-midi" ] && TARGET="ace-midi"
SOURCEDIR="$(python3 -c "import json; print(json.load(open('$SOURCE')).get('source_dir', 'engine'))")"

"$REPO_ROOT/scripts/sync-train-source.sh" "$MANIFEST"
COMMIT="$(python3 -c "import json; print(json.load(open('$SOURCE'))['commit'])")"
WORKTREE="${YUE_ENGINE_BUILD_ROOT:-$HOME/.cache/yue2-studio}/hotstep-${COMMIT:0:8}"
SRC="$WORKTREE/$SOURCEDIR"

NPROC="$(nproc 2>/dev/null || echo 4)"
CCACHE_FLAG="-DGGML_CCACHE=OFF"
command -v ccache >/dev/null && CCACHE_FLAG="-DGGML_CCACHE=ON"

# CUDA 13 runs Turing (75) and newer; older cards live in a CUDA 12 toolkit
# this script does not require. `native` builds for this card only.
if [ "$ARCH" = "native" ]; then
  ARCH_FLAG="-DCMAKE_CUDA_ARCHITECTURES=native"
else
  ARCH_FLAG="-DCMAKE_CUDA_ARCHITECTURES=75-real;80-real;86-real;89-real;90-real;120a-real;121-real;120-virtual"
fi

BUILD_DIR="$SRC/build-$TARGET-$ARCH"
echo "=== cmake configure $TARGET ($ARCH) ==="
cmake -S "$SRC" -B "$BUILD_DIR" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  "$CCACHE_FLAG" -DGGML_NATIVE=OFF -DGGML_CUDA=ON "$ARCH_FLAG"
echo "=== cmake build $TARGET ==="
cmake --build "$BUILD_DIR" --target "$TARGET" --parallel "$NPROC"

BUILT="$BUILD_DIR/$TARGET"
[ -x "$BUILT" ] || { echo "error: the build completed without $TARGET" >&2; exit 1; }

OUT="$(realpath -m "$OUTPUT")"
[ "$OUT" != "/" ] || { echo "error: refusing a drive-root output directory" >&2; exit 1; }
mkdir -p "$OUT"
cp -f "$BUILT" "$OUT/$TARGET"
# -d keeps the versioned .so symlinks as links
while IFS= read -r lib; do cp -df "$lib" "$OUT/"; done < <(find "$BUILD_DIR" -name 'libggml*.so*' -o -name 'libllama*.so*' | sort -u)
cp -f "$WORKTREE/$SOURCEDIR/LICENSE" "$OUT/LICENSE-HOT-Step.txt" 2>/dev/null || cp -f "$WORKTREE/LICENSE" "$OUT/LICENSE-HOT-Step.txt" 2>/dev/null || true

python3 - "$OUT" "$COMMIT" "$ARCH" "$TARGET" <<'PYEOF'
import json, sys
out, commit, arch, target = sys.argv[1:5]
json.dump({"commit": commit, "cuda_architecture": arch, "runtime": target},
          open(f"{out}/runtime.json", "w"), indent=2)
print(json.dumps({"runtime": f"{out}/{target}"}))
PYEOF
echo "staged in $OUT"
