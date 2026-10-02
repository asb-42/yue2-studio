# YuE2 Studio — Linux fork notes

This fork runs the studio without Tauri: the Rust/Axum service plus a plain
browser UI. Windows-only pieces (Tauri shell, NSIS installer/updater,
WebView2, DirectML, VST3 host, Win32 window shaping, Explorer verbs) are
dropped; system playback goes to VLC/mpv/mplayer through
`POST /v1/library/songs/{id}/play-external`.

## Quick start (development)

```sh
./scripts/run-linux.sh            # debug service (127.0.0.1:8791) + vite UI (127.0.0.1:3791)
./scripts/run-linux.sh --service-only
./scripts/run-linux.sh --build    # release service + static vite build, UI served by the service itself
```

With `--build` (or `YUE_UI_DIR=/path/to/app/dist` set manually) the service
serves the built interface on its own port: open `http://127.0.0.1:8791/`
directly, no dev server needed. Same-origin, so the visualiser popup,
`saving`, and downloads all work without CORS or proxying.

System packages: `build-essential cmake pkg-config git curl python3 node 20+`.
`ninja` is used when present (no sudo? bootstrap it: see below).
Optional: CUDA toolkit 12 or 13 (`nvcc`), Vulkan SDK + `glslc`.

No sudo and no ninja? One-time bootstrap into `~/.local/bin`:

```sh
curl -sL -o /tmp/ninja.tar.gz https://github.com/ninja-build/ninja/archive/refs/tags/v1.12.1.tar.gz
mkdir -p /tmp/ninja-src && tar xzf /tmp/ninja.tar.gz -C /tmp/ninja-src --strip-components=1
(cd /tmp/ninja-src && ./configure.py --bootstrap) && cp /tmp/ninja-src/ninja ~/.local/bin/
```

## Engine build

```sh
./scripts/sync-yue-source.sh                              # pinned yue2.cpp checkout
./scripts/build-yue-runtime.sh --output dist/yue2-cpp --backend cuda --arch universal
```

* `--backend auto|cuda|vulkan|cpu|all` (`all` = runtime-loaded CUDA + Vulkan + CPU, like the Windows release).
* `--arch universal|native`. Universal CUDA 13 targets Turing→Blackwell
  (`75;80;86;89;90;120a;121`); Maxwell–Volta need the CUDA 12 companion
  (`--cuda12-root <toolkit>`), because CUDA 13 nvcc rejects them.
* `run-linux.sh` picks up `dist/yue2-cpp` automatically (`YUE_ENGINE_ROOT`);
  `YUE_ENGINE_BIN` points at a single binary explicitly.

Without an engine the service still boots (setup screens, library, MCP);
generation waits for one.

## Runtimes the service downloads itself (first use)

| Capability | Linux source | Notes |
|---|---|---|
| ONNX Runtime CPU | `onnxruntime-linux-{x64,aarch64}-1.30.0.tgz` | Parakeet, HT-Demucs, Beat This! |
| ONNX Runtime CUDA | `onnxruntime-linux-x64-gpu_cuda12-1.30.0.tgz` | x64 only; needs system CUDA 12 + cuDNN 9 |
| CUDA provider libs | system toolkit (`ldconfig`/`LD_LIBRARY_PATH`) | nothing downloaded |
| llama.cpp assistant | `llama-b11236-bin-ubuntu-*.tar.gz` (+ `cudart-…`) | x64 + arm64, CUDA 13/12, Vulkan, CPU |
| Models/weights | same Hugging Face revisions as Windows | GGUF, faster-whisper files, MuScriptor, MOSS, trainer weights |

Deliberately unavailable on Linux v1 (clear API errors, never broken
downloads): Whisper standalone (use Parakeet or OpenRouter), DirectML,
`music-train` / `ace-caption` / `music-midi` executables (build the pinned
HOT-Step commit from source and point `YUE_TRAIN_BIN` / `YUE_CAPTION_BIN` /
`YUE_MIDI_BIN` at the binaries; weights can be placed by hand per the
documented `data/` layout).

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `YUE_UI_DIR` | `app/dist` beside CWD (set by launcher) | built web interface served by the service itself |
|---|---|---|
| `YUE_STUDIO_DATA_ROOT` | `${XDG_DATA_HOME:-~/.local/share}/yue2-studio` | library, media, settings, logs, downloads |
| `YUE_MODELS_ROOT` | `$DATA/models/yue2-cpp` | YuE2 weights |
| `YUE_STUDIO_SETTINGS_PATH` | `$DATA/studio-settings.json` | settings |
| `YUE_ENGINE_ROOT` / `YUE_ENGINE_BIN` | `dist/yue2-cpp` (via launcher) | engine bundle / binary |
| `YUE_ENGINE_HOST` / `YUE_ENGINE_PORT` | `127.0.0.1` / `18087` | engine endpoint |
| `YUE_MEDIA_PLAYER` | auto (`vlc,mpv,mplayer,celluloid,totem`, else `xdg-open`) | external playback |
| `CUDA_VISIBLE_DEVICES` | — | passed through to CUDA processes |
| `YUE_TRAIN_BIN` / `YUE_CAPTION_BIN` / `YUE_MIDI_BIN` | — | source-built sidecars |

Ports: service `8791` (HTTP + MCP at `/mcp`), engine `18087`, vite dev `3791`.

## Known limitations (v1)

* Vulkan engine backend needs a Vulkan SDK at build time; CPU backend always available.
* `nvidia-smi` on unified-memory cards (e.g. GB10) reports no VRAM figure, so model-set recommendation falls back to `light` — override in setup.
* ARM64 Linux has CPU ONNX + CPU/Vulkan/CUDA13 llama builds, but no CUDA ORT build upstream: GPU Parakeet/separation need x64.
* No auto-update, no installer: pull + rebuild.
