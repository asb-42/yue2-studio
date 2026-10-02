#!/usr/bin/env bash
# Packages a Linux release tarball. No installers, no signing, no updater:
# a directory with the service, the built UI, the engine bundle and the
# licences, plus a stamp file saying what it is.
#
# Usage: ./scripts/package-linux.sh [--output <file>] [--engine <dir>]
#   --engine defaults to dist/yue2-cpp (see build-yue-runtime.sh).
#   The tarball unpacks into ./yue2-studio-linux-<version>/ and runs with:
#     YUE_ENGINE_ROOT=./engine ./music-server   (UI at http://127.0.0.1:8791/)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT=""
ENGINE="$REPO_ROOT/dist/yue2-cpp"

while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUTPUT="$2"; shift 2 ;;
    --engine) ENGINE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

VERSION="$(python3 -c "import json; print(json.load(open('$REPO_ROOT/app/package.json'))['version'])")"
[ -n "$VERSION" ] || { echo "error: no version in app/package.json" >&2; exit 1; }
OUTPUT="${OUTPUT:-$REPO_ROOT/yue2-studio-linux-$VERSION.tar.gz}"

export PATH="$HOME/.cargo/bin:$PATH"
echo "=== building service (release) ==="
cargo build --release --locked -p music-server
echo "=== building UI ==="
npm --prefix "$REPO_ROOT/app" run build --silent

[ -x "$ENGINE/yue-server" ] || { echo "error: no engine at $ENGINE (run build-yue-runtime.sh first, or --engine <dir>)" >&2; exit 1; }
[ -f "$REPO_ROOT/app/dist/index.html" ] || { echo "error: no UI build at app/dist" >&2; exit 1; }

STAGE="$(mktemp -d)/yue2-studio-linux-$VERSION"
mkdir -p "$STAGE/engine" "$STAGE/ui"
cp "$REPO_ROOT/target/release/music-server" "$STAGE/"
cp -r "$REPO_ROOT/app/dist/." "$STAGE/ui/"
cp -r "$ENGINE/." "$STAGE/engine/"
cp "$REPO_ROOT/LICENSE" "$STAGE/"
cp -r "$REPO_ROOT/licenses" "$STAGE/" 2>/dev/null || true
cp "$REPO_ROOT/docs/linux.md" "$STAGE/README-linux.md"
cat > "$STAGE/run.sh" <<EOF
#!/usr/bin/env bash
# YuE2 Studio $VERSION for Linux. Data (models, library, settings) lives in
# \${YUE_STUDIO_DATA_ROOT:-\$HOME/.local/share/yue2-studio}; weights download
# on first use. Open http://127.0.0.1:8791/ (or YUE_BIND_ADDR=0.0.0.0 for LAN).
cd "\$(dirname "\$0")"
export YUE_ENGINE_ROOT="\$PWD/engine" YUE_UI_DIR="\$PWD/ui"
export LD_LIBRARY_PATH="\$PWD/engine\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
exec ./music-server
EOF
chmod +x "$STAGE/run.sh"
python3 - "$STAGE" "$VERSION" <<'PYEOF'
import json, sys, subprocess
stage, version = sys.argv[1:3]
commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
json.dump({"version": version, "commit": commit or "unknown",
           "service": "./music-server", "ui": "./ui", "engine": "./engine/yue-server"},
          open(f"{stage}/release.json", "w"), indent=2)
PYEOF
tar -czf "$OUTPUT" -C "$(dirname "$STAGE")" "$(basename "$STAGE")"
rm -rf "$(dirname "$STAGE")"
echo "packaged: $OUTPUT"
tar tzf "$OUTPUT" | head -12
