#!/usr/bin/env bash
# Builds the pinned yue2.cpp engine (engines/yue2-cpp-source.json) for Linux.
# Linux port of scripts/build-yue-runtime.ps1.
#
# Usage:
#   ./scripts/build-yue-runtime.sh --output <dir> [--backend auto|cuda|vulkan|cpu|all]
#       [--arch universal|native] [--cuda12-root <dir>]
#
# Layout of <dir> mirrors the Windows release, with .so in place of .dll:
#   yue-server, libggml*.so [.so deps], [cuda13/|cuda12/]libggml-cuda.so, runtime.json
# A single-CUDA build keeps libggml-cuda.so beside the executable (the
# supervisor's developer-build case); a dual build keeps one per folder and
# names it in YUE_CUDA_BACKEND.
#
# Toolchains: nvcc for cuda, $VULKAN_SDK + glslc for vulkan, GCC/Clang + CMake
# + Ninja always. No Vulkan SDK here means no Vulkan backend (warn, continue
# with the rest) unless --backend demands it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT=""
BACKEND="auto"
ARCH="universal"
CUDA12_ROOT="${CUDA_PATH_V12_9:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUTPUT="$2"; shift 2 ;;
    --backend) BACKEND="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    --cuda12-root) CUDA12_ROOT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
[ -n "$OUTPUT" ] || { echo "error: --output <dir> is required" >&2; exit 1; }
case "$BACKEND" in auto|cuda|vulkan|cpu|all) ;; *) echo "error: --backend must be auto|cuda|vulkan|cpu|all" >&2; exit 1 ;; esac
case "$ARCH" in universal|native) ;; *) echo "error: --arch must be universal|native" >&2; exit 1 ;; esac

for tool in git cmake ninja gcc; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required on PATH" >&2; exit 1; }
done

ENGINE_SOURCE="$REPO_ROOT/engines/yue2-cpp-source.json"
WORKTREE="${YUE_ENGINE_BUILD_ROOT:-$HOME/.cache/yue2-studio}/yue2-engine"
"$REPO_ROOT/scripts/sync-yue-source.sh" "$WORKTREE"

COMMIT="$(python3 -c "import json; print(json.load(open('$ENGINE_SOURCE'))['commit'])")"
NPROC="$(nproc 2>/dev/null || echo 4)"
CCACHE_FLAG="-DGGML_CCACHE=OFF"
command -v ccache >/dev/null && CCACHE_FLAG="-DGGML_CCACHE=ON"

has_nvcc() { command -v nvcc >/dev/null; }
has_vulkan() { [ -n "${VULKAN_SDK:-}" ] && [ -f "$VULKAN_SDK/include/vulkan/vulkan.h" ] && command -v glslc >/dev/null; }

if [ "$BACKEND" = "auto" ]; then
  if has_nvcc; then BACKEND="cuda"
  elif has_vulkan; then BACKEND="vulkan"
  else BACKEND="cpu"
  fi
fi
if { [ "$BACKEND" = "cuda" ] || [ "$BACKEND" = "all" ]; } && ! has_nvcc; then
  echo "error: the CUDA build requires nvcc on PATH" >&2; exit 1
fi
if { [ "$BACKEND" = "vulkan" ] || [ "$BACKEND" = "all" ]; } && ! has_vulkan; then
  if [ "$BACKEND" = "all" ]; then
    echo "warning: no Vulkan SDK ($VULKAN_SDK) with glslc; building CUDA + CPU only" >&2
    BACKEND="cuda"
  else
    echo "error: the Vulkan build requires VULKAN_SDK with headers and glslc" >&2; exit 1
  fi
fi

# GGML_NATIVE=OFF plus an explicit architecture list: a binary that runs on
# other people's CPUs and on Maxwell through Blackwell. The CUDA 13 line
# carries Turing and newer (121 covers Blackwell cards whose driver reports
# 12.x, e.g. GB10); the CUDA 12 companion below covers Maxwell-Pascal-Volta
# and old drivers. `native` builds for this card only (fast iteration).
# Echoes one cmake -D argument per line for mapfile slurping. CUDA 13 runs
# Turing (75) and newer only - Maxwell through Volta (52-70) fail its nvcc
# and live in the CUDA 12 companion build instead. 121 covers Blackwell
# cards whose driver reports 12.x (e.g. GB10).
cuda_arch_args() {
  if [ "$ARCH" = "native" ]; then
    echo "-DCMAKE_CUDA_ARCHITECTURES=native"
    return
  fi
  echo "-DGGML_NATIVE=OFF"
  echo "-DCMAKE_CUDA_ARCHITECTURES=75-real;80-real;86-real;89-real;90-real;120a-real;121-real;120-virtual"
}

build_tree() {
  # $1 = build dir name, $2 = "targets" (space-separated cmake --target args,
  # empty for the whole tree), remaining args = extra cmake defines.
  local dirname="$1" targets="$2"; shift 2
  echo "=== cmake configure $dirname ==="
  cmake -S "$WORKTREE" -B "$WORKTREE/$dirname" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    "$CCACHE_FLAG" "$@" 
  echo "=== cmake build $dirname ==="
  if [ -n "$targets" ]; then
    # shellcheck disable=SC2086
    cmake --build "$WORKTREE/$dirname" --target $targets --parallel "$NPROC"
  else
    cmake --build "$WORKTREE/$dirname" --parallel "$NPROC"
  fi
}

mapfile -t CUDA_ARCH < <(cuda_arch_args)

case "$BACKEND" in
  cuda)
    build_tree "build-cuda-$ARCH" "yue-server" -DGGML_CUDA=ON "${CUDA_ARCH[@]}"
    BIN_DIR="$WORKTREE/build-cuda-$ARCH"
    ;;
  vulkan)
    build_tree "build-vulkan" "yue-server" -DGGML_NATIVE=OFF -DGGML_VULKAN=ON
    BIN_DIR="$WORKTREE/build-vulkan"
    ;;
  cpu)
    build_tree "build-cpu" "yue-server" -DGGML_NATIVE=OFF -DGGML_CPU_ALL_VARIANTS=ON
    BIN_DIR="$WORKTREE/build-cpu"
    ;;
  all)
    # Runtime-loaded backends (the Windows `all` arrangement): the whole tree,
    # as upstream's buildall does, because naming yue-server alone would skip
    # the backend libraries nothing links against.
    build_tree "build-all-$ARCH" "" -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN=ON -DGGML_CUDA=ON "${CUDA_ARCH[@]}"
    BIN_DIR="$WORKTREE/build-all-$ARCH"
    ;;
esac

# The CUDA 12 companion for cards CUDA 13 dropped and drivers older than 580:
# ggml-cuda alone, from the same source. Needs a CUDA 12 toolkit; without one
# the single CUDA 13 build above is the whole runtime.
CUDA12_DIR=""
if [ "$BACKEND" = "all" ] && [ "$ARCH" = "universal" ] && [ -n "$CUDA12_ROOT" ]; then
  if [ ! -x "$CUDA12_ROOT/bin/nvcc" ]; then
    echo "error: no nvcc under '$CUDA12_ROOT/bin'; set --cuda12-root to a CUDA 12 toolkit" >&2; exit 1
  fi
  CUDA12_ROOT_FWD="$(echo "$CUDA12_ROOT" | sed 's|\\|/|g')"
  echo "=== cmake build cuda12 backend ==="
  # shellcheck disable=SC2086
  PATH="$CUDA12_ROOT/bin:$PATH" CUDA_PATH="$CUDA12_ROOT" \
    cmake -S "$WORKTREE" -B "$WORKTREE/build-cuda12-universal" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    $CCACHE_FLAG -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CUDA=ON -DGGML_VULKAN=OFF \
    "-DCMAKE_CUDA_ARCHITECTURES=52-real;60-real;61-real;70-real;75-real;80-real;86-real;89-real;90-real;120a-real" \
    "-DCMAKE_CUDA_COMPILER=$CUDA12_ROOT_FWD/bin/nvcc" "-DCUDAToolkit_ROOT=$CUDA12_ROOT_FWD" \
    "-DCMAKE_CUDA_FLAGS=-Wno-deprecated-gpu-targets"
  cmake --build "$WORKTREE/build-cuda12-universal" --target ggml-cuda --parallel "$NPROC"
  CUDA12_DIR="$(dirname "$(find "$WORKTREE/build-cuda12-universal" -name 'libggml-cuda.so' | head -1)")"
  [ -n "$CUDA12_DIR" ] || { echo "error: the CUDA 12 build completed without libggml-cuda.so" >&2; exit 1; }
elif [ "$BACKEND" = "all" ] && [ "$ARCH" = "universal" ]; then
  echo "warning: no CUDA 12 toolkit (--cuda12-root); single CUDA 13 runtime only" >&2
fi

# Stage the runtime.
YUE_SERVER="$(find "$BIN_DIR" -maxdepth 3 -name 'yue-server' -type f | head -1)"
[ -n "$YUE_SERVER" ] || { echo "error: the build completed without yue-server" >&2; exit 1; }
OUT="$(realpath -m "$OUTPUT")"
[ "$OUT" != "/" ] || { echo "error: refusing a drive-root output directory" >&2; exit 1; }
mkdir -p "$OUT"
cp -f "$YUE_SERVER" "$OUT/"
# -d keeps the versioned .so symlinks as links: dereferencing them triples a
# 165 MB backend into three copies.
while IFS= read -r lib; do cp -df "$lib" "$OUT/"; done < <(find "$BIN_DIR" -name 'libggml*.so*' -o -name 'libllama*.so*' | sort -u)

if [ -n "$CUDA12_DIR" ]; then
  # Each CUDA backend in a folder of its own, none beside the executable: the
  # studio names the one the card and its driver run in YUE_CUDA_BACKEND.
  mkdir -p "$OUT/cuda13" "$OUT/cuda12"
  mv -f "$OUT"/libggml-cuda.so* "$OUT/cuda13/" 2>/dev/null || true
  cp -df "$CUDA12_DIR"/libggml-cuda.so* "$OUT/cuda12/"
fi

python3 - "$OUT" "$COMMIT" "$BACKEND" "$ARCH" <<'PYEOF'
import json, sys
out, commit, backend, arch = sys.argv[1:5]
builds = ["cuda13", "cuda12"] if backend == "all" and arch == "universal" and __import__("os").path.isdir(f"{out}/cuda12") else []
json.dump({"commit": commit, "backend": backend, "cuda_architecture": arch,
           "cuda_builds": builds, "runtime": "yue-server"},
          open(f"{out}/runtime.json", "w"), indent=2)
print(json.dumps({"runtime": f"{out}/yue-server", "cuda_builds": builds}))
PYEOF
echo "staged in $OUT"
