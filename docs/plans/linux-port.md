# Linux port plan — YuE2 Studio fork (no Tauri, no Windows-only code)

Status: draft plan, not started.
Date: 2026-10-02.
Upstream inspected: YuE2 Studio 3.4.0 (`desktop/src-tauri/tauri.conf.json:4`), MIT studio code, non-MIT models/assets (see §0.4).

## 0. Goal, non-goals, constraints

### 0.1 Goal

A Linux fork that runs the studio as:

```text
browser (vite build served statically or vite dev)
  → music-server (Axum on 127.0.0.1:8791)
    → yue-server (Linux native GGUF engine on 127.0.0.1:18087)
    → optional Linux sidecars: llama-server, whisper backend, music-train, music-midi/ace-caption
```

No Tauri, no NSIS/updater, no WebView2, no DirectML, no VST3 host in v1.

### 0.2 Non-goals for v1

- No Tauri window, auto-update, NSIS installer, `latest.json` signing flow.
- No VST3 support (`crates/music-server/src/vst.rs`, `scripts/build-vst-host.ps1`).
- No DirectML path; Linux GPU = NVIDIA/CUDA, else Vulkan/CPU where the engine already supports it.
- No Windows portable/`%LOCALAPPDATA%`/registry/`explorer.exe` behaviour.
- No feature parity for Winamp shaped-window mode; keep it as a normal web page.

### 0.3 Hard constraints (from `AGENTS.md` + upstream README)

- [ ] No Python or Node.js in the runtime path.
- [ ] Model engines stay adapters; do not hardcode one model’s behaviour into UI/server code.
- [ ] Capability-level provider choice stays: music, ASR, LLM, cover art independently Local vs OpenRouter.
- [ ] OpenRouter model IDs stay in configuration, not source.
- [ ] Every local engine keeps install state + capability metadata + cancellation + progress before UI exposes it.
- [ ] Weights, API keys, media, caches remain runtime user data; never commit them.

### 0.4 Licensing notes (do not regress in the fork)

Upstream code is MIT, but the runnable system is not all-MIT:

- [ ] Keep `LICENSE` (MIT) for fork code.
- [ ] YuE2-3B + VAE: CC BY-NC 4.0 with individual-creator permission; SheetSage2 + decoder companion: CC BY-NC 4.0 without it (`README.md:555-562`).
- [ ] MuScriptor weights: CC BY-NC 4.0 (`crates/music-server/src/midi.rs:1-10`).
- [ ] `audioMotion-analyzer`: AGPL-3.0; YuE2-ComfyUI port: Apache-2.0 (`licenses/YuE2-ComfyUI-Apache-2.0.txt`); `signal` MIDI editor: MIT; A320U SoundFonts: GPL-2.0 (`licenses/A320U-soundfont-GPL-2.0.txt`).
- [ ] Document the same in fork README + keep `licenses/`.

## 1. Architecture map (what stays, what goes)

Keep:

- [ ] `crates/music-server` — Axum service; already has standalone `src/main.rs:6-8` (`music_server::serve()`).
- [ ] `crates/music-core`, `crates/music-engine`, `crates/audio-post` — mostly portable; Windows parts are `cfg(windows)`-gated.
- [ ] `app/` React UI — only 3 Tauri touch points (see §9).
- [ ] `engines/*.json` manifests as version pins; add Linux build metadata alongside Windows.

Drop or stub for v1:

- [x] `desktop/` Tauri shell deleted (DONE: `git rm -r desktop/`, all 5 `scripts/*.ps1`, stale `app/README.md`); Windows PE bundle self-check removed with it (Linux uses the `ldd` step instead).
- [ ] `scripts/*.ps1` Windows build/release scripts.
- [ ] DirectML, Win32 job objects, WebView2 args, `SetWindowRgn`, `explorer.exe`, `LOCALAPPDATA`, VC runtime, `LoadLibraryExW` preload.

Vite already proxies to the service (`app/vite.config.ts:12-39`), so headless dev is proven:

```sh
cargo run -p music-server       # 127.0.0.1:8791
npm --prefix app run dev        # 127.0.0.1:3791
```

## 2. Workstream 0 — fork scaffolding

- [x] Create fork branch/repo; keep upstream remote for cherry-picks.
- [ ] Add `docs/plans/linux-port.md` (this file) + per-stream issues if using trackers.
- [ ] Add `docs/plans/linux-acceptance.md` checklist (or use §11 here as source of truth).
- [x] Decide versioning: `3.4.0-linux.1` in `app/package.json` (fork of upstream 3.4.0; `__APP_VERSION__` reads it since `desktop/` is gone).
- [x] Linux CI: `.github/workflows/ci.yml` runs `cargo test --workspace --locked` + UI build/types/tests on ubuntu-latest.
- [x] Pin Linux toolchain: `rust-toolchain.toml` channel `stable-x86_64-pc-windows-msvc` → `stable` (was blocking rustup entirely on Linux). Verified with Rust 1.99 / Node 22.
- [ ] Document system deps (build-essential, cmake, ninja, CUDA toolkit optional, Vulkan SDK optional, Node 20+, pkg-config, OpenSSL/rustls notes).

Verification:

- [ ] Fresh Linux checkout passes `cargo test --workspace` (after §3 cfg fixes) and `npm --prefix app test`.

## 3. Workstream 1 — Rust workspace: compile clean on Linux, keep Windows intact where cheap

Files: `Cargo.toml:41`, `crates/music-engine/Cargo.toml:15-16`, `crates/music-core/Cargo.toml:12-13`.

- [ ] Keep `windows-sys` behind `target.'cfg(windows)'.dependencies` only (already the case in engine/core; workspace root lists it unconditionally at `Cargo.toml:41` — move it behind `cfg(windows)` or into the crates that need it).
- [ ] Audit every `cfg(windows)` site; ensure each has a Linux behaviour, not just a no-op where behaviour matters:
  - [ ] `crates/music-engine/src/process_group.rs:19-94` — Windows job object; Linux keeps `bind_children_to_this_process() -> false`. Decide process-group strategy: `Drop` supervisors already stop children; add best-effort Linux process-group kill (e.g. `setpgid`/`killpg` or ensure `stop()` is called on shutdown) without pulling `libc` into more crates than needed (`music-engine` already uses `libc` for SIGTERM at `yue_server.rs:494-501` — reuse that pattern).
  - [ ] `crates/music-core/src/process.rs:19-34` — `adopt()` is noop on Linux; acceptable v1 if supervisors stop children; document the gap (hard-kill leaves engine alive) or implement Linux group.
  - [ ] `crates/music-engine/src/yue_server.rs:26-29,468-478,480-501` — executable name + graceful shutdown already split (`CTRL_BREAK` vs `SIGTERM`); keep.
  - [ ] `crates/music-server/src/hardware.rs:143-153,214-227` — `CREATE_NO_WINDOW` + registry adapter; keep Linux stubs compiling, replace logic in §5.
  - [ ] `crates/music-server/src/assistant_runtime.rs:331,804-812` — `llama-server.exe` vs `llama-server`, `hide_console`; already split; keep.
  - [ ] `crates/music-server/src/vst.rs:224-231` — keep `hide_window` noop on Linux even though VST host is dropped (or delete module in fork; see §8).
  - [ ] `crates/music-server/src/lyrics_sync.rs:895-910` — `LoadLibraryExW` preload; keep Windows-only, add Linux comment (DirectML `.dll` goes away anyway).
  - [ ] `crates/music-server/src/saving.rs:220-230` — `explorer.exe` + `raw_arg`; Linux currently passes `path` to a binary still named `explorer.exe` — fix in §8/`saving` task (use `xdg-open` like `lib.rs:2369,6397` already does).
- [ ] Fix `ort` features per-target: `ort` at `Cargo.toml:37` enables `cuda,directml`. DirectML is Windows-only. Make features target-dependent (e.g. no `directml` on Linux) or document that Linux build uses `cuda` (+ CPU) only.
- [ ] Run `cargo clippy --workspace --all-targets` on Linux; fix new warnings in touched code.

Verification:

- [x] `cargo test --workspace` green on Linux (CPU-only, no models). 2026-10-02: 17 + 2 + 16 + 281 passed, 4 ignored (live-download/model tests).
- [x] `npm --prefix app test` green (19 files / 68 tests); `vite build` succeeds.
- [ ] No `windows-sys` in Linux `cargo tree` for the server binary (or only as target-specific).

## 4. Workstream 2 — `yue-server` Linux engine build + runtime loading

This is the critical path. Upstream ships `yue-server.exe` + `ggml*.dll` + `cuda12|cuda13/ggml-cuda.dll` + VC runtime + downloaded cuBLAS (`README.md:426-452`, `scripts/build-yue-runtime.ps1`, `crates/music-server/src/engine_runtime.rs`, `crates/music-engine/src/yue_server.rs:201-239`).

- [x] Reproduce pinned source sync on Linux: `scripts/sync-yue-source.sh` (clone pinned commit + submodules; DONE 2026-10-02, 71 MB checkout).
- [x] Write `scripts/build-yue-runtime.sh` (DONE 2026-10-02): `cuda|vulkan|cpu|all` backends, `universal|native` arches, optional CUDA 12 companion, release layout with `runtime.json`. Corrections from probing: CUDA 13 nvcc rejects pre-Turing (52–70 live in the CUDA 12 build only); universal CUDA 13 list is `75;80;86;89;90;120a;121;120-virtual`; no Vulkan SDK / no second toolkit on this box → single CUDA 13 build first.
- [x] Full `yue-server` Linux build + `GET /health` on 127.0.0.1:18087 (DONE 2026-10-02: `cuda/universal` built clean on GB10/CUDA 13 — 161 MB staged with symlinks preserved; `ldd` clean; `--help` runs).
- [x] FIRST LINUX SONG (DONE 2026-10-02): 4 GB Light set downloaded via the service, engine self-test `ok (78.6 ms)`, `KV backend: CUDA0`, 30 s folk-pop track composed + rendered + imported in ~1 min → tagged 320 kbps MP3 + cover JPG in the library.
- Lessons: (a) unified-memory cards report `[N/A]` VRAM → new `nvidia_name()` fallback keeps `nvidia:true` + CUDA detect (else Auto never tries CUDA); (b) CUDA 13 arch list `75;80;86;89;90;120a;121;120-virtual` (52–70 rejected by nvcc 13); (c) on Linux `adopt()` is a no-op — killing the service orphans `yue-server` on its port; find holders with `ss -ltnp`, never `pkill -f` with the literal path in the same command (it matches your own shell); (d) `cargo run` wrapper deaths are not service deaths — run the binary directly for long sessions.
- [x] `run-linux.sh` picks up `dist/yue2-cpp` via `YUE_ENGINE_ROOT` automatically.
- [x] `cuda_backend()` filename is per-OS now (`ggml-cuda.dll` vs `libggml-cuda.so`, `music-engine/src/yue_server.rs`); `EXECUTABLE` was already per-OS. Bundle layout + `scripts/build-yue-runtime.sh` still open.
- [x] `engine_runtime.rs`: per-OS library names (`.dll` vs versioned `.so` SONAMEs), no VC runtime on Linux, no cuBLAS download on Linux (system CUDA toolkit; Auto falls through gracefully). `is_on_the_search_path` still PATH-based — `LD_LIBRARY_PATH`/`ldconfig` extension still open.
- [x] Wire engine bundle root + data root via existing envs — `scripts/run-linux.sh` sets `YUE_STUDIO_DATA_ROOT` (XDG), `YUE_MODELS_ROOT`, `YUE_STUDIO_SETTINGS_PATH`, `YUE_ENGINE_*`, `LD_LIBRARY_PATH` for `/usr/local/cuda/lib64`. Service boot smoke-tested on Linux (`/health`, `/setup/status` answer; engine correctly reports absent).
- [ ] Write `scripts/build-yue-runtime.sh` mirroring `Invoke-CMakeBuild` flags (`build-yue-runtime.ps1:86-122`):
  - [ ] `all` equivalent: `-DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN=ON -DGGML_CUDA=ON`, `GGML_NATIVE=OFF`, explicit `CMAKE_CUDA_ARCHITECTURES` (same lists: CUDA13 `75-real;80-real;86-real;89-real;90-real;120a-real;120-virtual`; CUDA12 extra `52-real;60-real;61-real;70-real` per `Invoke-Cuda12Build`).
  - [ ] Keep two CUDA backends if feasible on Linux (`cuda13/`, `cuda12/` folders with `libggml-cuda.so`); else start with single-CUDA + Vulkan + CPU and record the gap for Maxwell/Pascal/Volta + old drivers.
  - [ ] Drop MSVC/PDB/VSLANG/vswhere/redist steps (`build-yue-runtime.ps1:60-70,102-117,205-215`); use GCC/Clang + Ninja + `ccache` if present.
  - [ ] Emit `runtime.json` stamp with `runtime: yue-server` (Linux equivalent of `build-yue-runtime.ps1:217-226`).
- [ ] Decide bundle layout, e.g. `<bundle>/yue-server`, `<bundle>/lib/libggml*.so`, `<bundle>/cuda13|12/libggml-cuda.so`. Update:
  - [ ] `YueServerLocation::executable`/`locate_bundled_executable` (`yue_server.rs:399-412`) — already tries `EXECUTABLE`, `bin/`, `build/Release`, `build/`; ensure Linux output dir matches one of these or extend candidates.
  - [ ] `EXECUTABLE` const (`yue_server.rs:26-29`) already `yue-server` on non-Windows — keep.
  - [ ] `cuda_backend()` (`yue_server.rs:205-216`) — replace `ggml-cuda.dll` with `libggml-cuda.so` (and folder layout); keep dev-build fallback (beside executable) semantics.
  - [ ] `strip_verbatim` (`yue_server.rs:431-437`) — keep (noop on Linux).
  - [ ] `YUE_CUDA_BACKEND` + `GGML_BACKEND` env passing (`yue_server.rs:269-289`) — verify against Linux yue2.cpp (`GGML_BACKEND=CUDA0|Vulkan0|CPU` stays valid).
- [ ] Replace `engine_runtime.rs` Windows logic:
  - [ ] `CUDA13_LIBRARIES`/`CUDA12_LIBRARIES` (`engine_runtime.rs:29-33`) → Linux SONAMEs (e.g. `libcublas.so.13`, `libcublasLt.so.13`, … — confirm via `ldd` on built `libggml-cuda.so`, mirroring the `dumpbin /IMPORTS` doctrine in comments at `engine_runtime.rs:169-176`).
  - [ ] `CUBLAS13`/`CUBLAS12` assets (`engine_runtime.rs:44-79`) — Windows `developer.download.nvidia.com/.../windows-x86_64/...` zips → Linux `linux-x86_64` redist tarballs or “use system CUDA toolkit” (preferred on Linux: depend on toolkit/CUDA packages instead of downloading 400–550 MB).
  - [ ] `VC_RUNTIME_LIBRARIES` + `vc_runtime_missing()` (`engine_runtime.rs:35-38,126-132`) → Linux: no-op (or check `libgomp`/`libstdc++` only if actually needed).
  - [ ] `is_on_the_search_path()` (`engine_runtime.rs:164-167`) → keep but check `LD_LIBRARY_PATH` + `ldconfig -p` in addition to `PATH` (Linux loader does not search `PATH`).
  - [ ] `imported_libraries`/`unresolved_dependencies`/`is_provided_by_the_system` (`engine_runtime.rs:176-297`) — `#[cfg(test)]` PE parsers; gate to Windows or add ELF (`ldd`) equivalent test for the staged Linux bundle.
- [ ] Wire engine bundle root + data root via existing envs: `YUE_MODELS_ROOT`, `YUE_STUDIO_DATA_ROOT`, `YUE_STUDIO_SETTINGS_PATH`, `YUE_ENGINE_BASE_URL`/`HOST`/`PORT` (`desktop/src-tauri/src/lib.rs:76-124`, `music-server/src/main.rs` + `model_manager.rs:144`, `library.rs:112`, `request_log.rs:26-28`). Linux fork needs a small launcher (shell script or systemd unit, see §10) that sets these instead of the Tauri shell.
- [ ] Document VRAM tiers unchanged (`hardware.rs:123-135`); verify Linux VRAM numbers still come from `nvidia-smi` (yes — keep).

Verification:

- [ ] `./scripts/build-yue-runtime.sh --backend all` produces runnable `yue-server` on a CUDA machine.
- [ ] `GET /health` on `127.0.0.1:18087` answers; studio supervisor `ensure_started`/`stop` works (graceful SIGTERM path at `yue_server.rs:494-501`).
- [ ] `ldd yue-server` + `ldd libggml-cuda.so` show no unexpected missing libs; add the Linux bundle test from above.

## 5. Workstream 3 — hardware detection on Linux

File: `crates/music-server/src/hardware.rs`.

- [ ] Keep `nvidia-smi` probes (`nvidia_smi`, `nvidia_cuda_device`, `nvidia_cards`, `chosen_card`, `CUDA_VISIBLE_DEVICES`) — they work on Linux.
- [ ] Re-evaluate `CUDA13_DRIVER=580` / `CUDA12_DRIVER=525` (`hardware.rs:56-57`): these are Windows driver majors from NVIDIA tables. Confirm Linux equivalents (Linux 580-series exists for CUDA 13, but verify; adjust constants or split per-OS).
- [x] Replace `display_adapter()` registry scan (`hardware.rs:214-227`, `best_adapter`) on Linux: DRM render-node presence + sysfs vendor map (NVIDIA/AMD/Intel), VRAM unknown (CPU-safe tiers). The name being present is what offers the Vulkan device chain. Added `linux_vendor_name` unit test.
- [x] `OnnxFlavour::uses_directml` forced false on Linux; DirectML live tests (`directml_live`) gated to `#[cfg(all(test, windows))` — this also fixed a Linux test-link failure (`-lkernel32`).
  - [ ] (a) v1-minimal: accept `None` → Vulkan still offered? No — `device_chain` and `uses_vulkan` key off `gpu_name.is_some()` (`lib.rs:503-512,525-541`). So minimal fix needed: provide *some* Linux GPU presence check (e.g. `lspci`, `/sys/class/drm`, `vulkaninfo`, `rocm-smi`/`nvidia-smi` absence + DRM render node).
  - [ ] (b) fuller: parse `lspci -mm` or DRM `device/vendor`; map to name + best-effort VRAM (VRAM on Linux iGPU/dGPU without NVML is hard — report name with `total_vram_gb=0` and keep tiers CPU-safe, or read `mem_info_vram_total` over hwmon where available).
- [ ] Keep `accumulates_in_fp16()` + `clamp_fp16` Vulkan logic (`hardware.rs:80-82`, `lib.rs:543-558`) — still correct on Linux AMD/Vulkan.
- [ ] Keep `resources.rs` `nvidia-smi` GPU snapshot (`resources.rs:95-114`); on non-NVIDIA Linux it yields empty `gpus` — acceptable v1, surface as “no NVIDIA telemetry” rather than zeros (already the pattern).

Verification:

- [ ] On NVIDIA Linux: `hardware()` reports name/VRAM/`cuda` build/recommended profile correctly.
- [ ] On AMD/Intel-only Linux: engine offers Vulkan (not stuck on CPU-only because `gpu_name` was `None`).
- [ ] Unit tests `hardware.rs:257-312` still pass; add Linux `display_adapter` tests with fixtures.

## 6. Workstream 4 — ONNX Runtime (Parakeet, HT-Demucs, Beat This!) on Linux

Files: `crates/music-server/src/lyrics_sync.rs:170-761,840-910`, `crates/music-server/src/separation.rs`, `crates/music-server/src/audio_facts.rs`, `Cargo.toml:37`, `crates/music-server/Cargo.toml:45,51`.

- [ ] Drop DirectML assets (`onnxruntime-directml`, `directml` at `lyrics_sync.rs:734-761`, `DIRECTML_ASSETS`, `OnnxCard::DirectMl`, `with_card` DirectML branch at `lyrics_sync.rs:854-868`) or gate to Windows. Linux `OnnxCard` = `Cuda | (Cpu implied by None)`.
- [x] Replace ORT assets with Linux builds (DONE 2026-10-02, URLs verified against the v1.30.0 release page):
  - `onnxruntime` CPU: `onnxruntime-linux-{x64,aarch64}-1.30.0.tgz` (per `target_arch`); marker `libonnxruntime.so`.
  - `onnxruntime-cuda`: `onnxruntime-linux-x64-gpu_cuda12-1.30.0.tgz`; hidden on aarch64 (no upstream `linux-aarch64-gpu` build).
  - `cuda-cublas/cudart/cufft/cudnn` Windows zips: hidden on Linux — the provider loads the system CUDA 12 toolkit + cuDNN instead.
  - `onnxruntime-directml`/`directml`: hidden on Linux.
- [x] `machine_runtime()`/`has_cuda_runtime()` use the `.so` name; `has_cuda_libraries()` on Linux checks the provider `.so` + system cuBLAS/cudart/cuDNN via `ldconfig -p`/`LD_LIBRARY_PATH` (new, unit-tested).
- [x] `point_ort_at()`: `LD_LIBRARY_PATH` on Linux (loader never reads `PATH`); DirectML preload stays Windows-only.
- [x] Downloader + assistant runtime unpack `.tar.gz` (new `tar` dep, `extract_tgz` preserving relative symlinks, `extract_archive` dispatcher; verified against the real 1.30.0 tarball: links resolve, headers excluded).
- [x] `separation_assets` panel rows built from the catalogue on Linux (`system-cuda` display row for the toolkit requirement); `separation_status` already went through the gated paths.
- [ ] Live GPU run of Parakeet/separation on Linux (needs models + ORT download; CPU ORT + Parakeet run is the next verification).
- [ ] Update `machine_runtime()`/`has_cuda_libraries()`/`has_directml_libraries()` (`lyrics_sync.rs:1019-1081`) for `.so` names (`libonnxruntime.so*`, `libonnxruntime_providers_cuda.so`, `libcublasLt.so.12`, `libcudart.so.12`, `libcudnn.so.9` — confirm against actual ORT 1.30.0 Linux layout).
- [ ] Update `point_ort_at()` (`lyrics_sync.rs:885-893`): `PATH` prepend + `DirectML.dll` preload is Windows logic; on Linux set `ORT_DYLIB_PATH` + `LD_LIBRARY_PATH` to the runtime dir (no `LoadLibraryExW`).
- [ ] Keep `ort` `load-dynamic` pattern (`lyrics_sync.rs:1009-1013`); verify `parakeet-rs` dynamic loading works against Linux ORT (it should — same mechanism, different lib name).
- [ ] `audio_facts.rs` (Beat This!) and `separation.rs` (HT-Demucs) call `with_card`/`onnx_card` — no logic change needed once `OnnxCard` is Linux-correct; re-test model I/O shapes unchanged.
- [ ] `OnnxFlavour` (`lyrics_sync.rs:766-805`): `Cuda|Cpu|Auto` stays; `uses_directml` goes away on Linux; `card()` returns `Cuda` only when `hardware().cuda.is_some()`.

Verification:

- [ ] Parakeet int8 words test with `YUE_DATA_ROOT`/`YUE_TEST_*` (see ignored tests at `lyrics_sync.rs:2020-2190`) on Linux CPU; then CUDA if available.
- [ ] Separation overlap/WAV unit tests (`separation.rs:317-348`) pass; one real HT-Demucs run on Linux.
- [ ] Beat This! tempo run on Linux (CPU at least).

## 7. Workstream 5 — Whisper (karaoke fallback) on Linux

File: `crates/music-server/src/lyrics_sync.rs:69-73,170-210,962-999,1184-1230`.

- [x] Whisper on Linux: DECIDED — not offered in v1 (DONE 2026-10-02). Its standalone runtime + cuBLAS/cuDNN + all `whisper-*` model assets are hidden from `asset()`/status on Linux; `karaoke_install` refuses `"whisper"` with "use Parakeet or OpenRouter" (live-verified). Parakeet (platform-neutral weights) + OpenRouter cover karaoke.
- [x] Windows-exe guard (DONE 2026-10-02): central refusal in `downloads::install_all`/`install` on Linux for any asset whose marker is `.exe`/`.dll` (`music-train`, `ace-caption`, `music-midi`, ...), naming the `*_BIN` override escape hatch. Live-verified via MIDI install (endpoint is fire-and-forget by design; refusal lands in the log). Weights stay manually placeable per the documented folder layout.
- [ ] Keep model file assets (`Systran/faster-whisper-*`, `deepdml/...-turbo-ct2`) — they are platform-neutral; only the runtime changes.
- [ ] `whisper_binary()` via `locate_binary(root, &[WHISPER_RUNTIME_DIR], "whisper-faster")` (`lyrics_sync.rs:962-966`, `downloads.rs:606`) — update expected binary name/path for Linux choice.
- [ ] If keeping CTranslate2-CUDA Whisper on Linux, document CUDA 11 vs 12 mismatch (Whisper runtime wants CUDA 11 libs while ORT wants CUDA 12) — same split upstream already ships; on Linux prefer system packages or a single CUDA generation.

Verification:

- [ ] One end-to-end karaoke run per kept backend (Parakeet mandatory; Whisper optional; OpenRouter with user key).

## 8. Workstream 6 — writing assistant (`llama-server`) on Linux

File: `crates/music-server/src/assistant_runtime.rs:61-211,319-346`.

- [x] Replace the five `llama.cpp` Windows zips (`llama-cuda`, `llama-cuda-runtime`, `llama-cuda12`, `llama-cuda12-runtime`, `llama-vulkan`, `llama-cpu` at `assistant_runtime.rs:122-198`, build `b11236`) with Linux builds (DONE 2026-10-02, names/sizes verified against the b11236 release page; same pin, no re-pin needed):
  - x64: `llama-b11236-bin-ubuntu-{x64,cuda-13.4-x64,cuda-12.8-x64,vulkan-x64}.tar.gz` + matching `cudart-llama-b11236-bin-ubuntu-*.tar.gz`.
  - arm64: `...-ubuntu-{arm64,cuda-13.4-arm64,vulkan-arm64}.tar.gz`; no CUDA 12 arm64 build exists, so those entries name the CUDA 13 arm64 archives (documented in-table).
  - cudart marker is `libcudart` on Linux (`cudart64` on Windows); tarballs verified to hold exactly the 3 needed `.so.13` files beside the server.
- [x] Download + `start()` Gemma GGUF on Linux; `/health` then one `assistant/write` round-trip (DONE 2026-10-02 on ARM64/CUDA: `llama-b11236-bin-ubuntu-cuda-13.4-arm64` + cudart + `gemma-4-e4b-q4_0` = 5.9 GB; sidecar healthy in 3.5 s with CUDA libs mapped; `assistant/write` returned a parsed lyrics draft in ~1 s).
- [x] Live GPU-path bonus (DONE 2026-10-02): Parakeet karaoke on the generated song — word-perfect 4-line alignment in 3.5 s on CPU ORT; HT-Demucs 6 stems separated into valid 16-bit WAV library tracks.
- [ ] `server_binary_for()` already picks `llama-server` on non-Windows (`assistant_runtime.rs:331`) — keep; verify `cuda|cuda12|vulkan|cpu` folder selection via `card_flavour()` (`assistant_runtime.rs:203-211`) still matches new asset `unzip_into` dirs.
- [ ] `spawn()` flags (`assistant_runtime.rs:612-705`) are platform-neutral (`--model/--host/--port/--ctx-size/--n-gpu-layers/--jinja/--reasoning*`); keep. `hide_console` noop on Linux already.
- [ ] Keep `extract_zip` flattening (`assistant_runtime.rs:779-797`) if Linux releases stay zips; handle `.tar.gz` if you pin those instead.

Verification:

- [ ] Download + `start()` Gemma GGUF on Linux; `/health` then one `assistant/write` round-trip.

## 9. Workstream 7 — training / MIDI / captioner sidecars

Files: `crates/music-server/src/training.rs:316-457`, `crates/music-server/src/midi.rs:28-86`, `engines/music-train-source.json`, `engines/music-midi-source.json`, `crates/music-engine/src/yue_train.rs`.

- [ ] All three are Windows-only release zips today: `music-train-cuda-windows-x64.zip` (`shipped_as: music-train.exe`), `music-midi-cuda-windows-x64.zip` (`music-midi.exe`), `ace-caption-windows-x64.zip` (`ace-caption.exe` + `ggml*.dll` pick list at `training.rs:379-396`).
- [ ] For each, choose BUILD or DROP for v1 (record in this file):
  - [ ] (a) Build Linux binaries from pinned HOT-Step-CPP commits (`music-train-source.json:repository/commit`, `music-midi-source.json`) with CUDA arches matching §4 (`music-midi-source.json:11` already lists `75-real;80-real;86-real;89-real;90-real;120a-real;120-virtual`).
  - [ ] (b) v1-drop with clean “not available on Linux” status (training already reports `pack_status`/`pack_ready`; MIDI reports `tool_installed`; keep those honest).
- [ ] If building: add `scripts/build-train-runtime.sh`, `scripts/build-midi-runtime.sh` (ports of `build-train-runtime.ps1`, `build-midi-editor.ps1` analogues), publish `*-linux-x64` assets, extend `pack()`/`listen_pack()`/`tool_asset()` URL tables with per-OS selection (keep Windows entries for upstream merges).
- [ ] `YUE_TRAIN_BIN` / `YUE_CAPTION_BIN` / `YUE_MIDI_BIN` env overrides (`training.rs:500-518`, `midi.rs:129-133`) already enable dev builds — keep; they are the Linux dev path before release assets exist.
- [ ] `CAPTIONER_PICK` DLL list (`training.rs:383-396`) → Linux `.so` list if captioner is built.
- [ ] Training weights (`yue_train::TRAINING_FILES`, MOSS-Music, `beat_this.onnx` at `training.rs:402-456`) are platform-neutral downloads — keep.

Verification:

- [ ] If built: one MIDI transcribe + one tiny training run (or `--help`/smoke + unit tests) on Linux.
- [ ] If dropped: UI/API reports “not installed / not supported on Linux” without stack traces; all other flows unaffected.

## 10. Workstream 8 — server Linux behaviours: paths, dialogs, reveal, encodings

- [x] Data roots: `scripts/run-linux.sh` sets Linux defaults (`XDG_DATA_HOME`-aware) for `YUE_STUDIO_DATA_ROOT`, `YUE_MODELS_ROOT`, `YUE_STUDIO_SETTINGS_PATH`, `YUE_ENGINE_*`. Still open: systemd unit + README docs.
- [x] Genuine portability bug fixed 2026-10-02: `Library::resolve_media` (`library.rs:221`) used `Path::file_name` directly, so a path stored on Windows (`Z:\...\song.mp3`, backslashes) never resolved on Linux. Now takes the last segment on either separator. Failing test `a_library_whose_folder_moved_still_finds_its_songs` passes.
- [ ] `studio_data_root()` / `engine_bundle_root()` / `ModelManager::from_environment()` — audit under Linux env only; ensure `library.rs:112` (`library.sqlite` + `media/`) lands under data root, not CWD.
- [x] Save flow (`saving.rs:27-79,110-168`): without Tauri there is no `set_save_dialog`. DONE (unchanged semantics): browser downloads directly (`saveFile.ts` already did); Tauri flow untouched. Added `POST /v1/files/play` (written-files allowlist) + `POST /v1/library/songs/{id}/play-external` (library containment) spawning VLC/mpv/mplayer/celluloid/totem or `YUE_MEDIA_PLAYER`, `xdg-open` last; no shell, detached spawn. Live-verified: import → play-external → 200 (override `/bin/true` and auto-detect `totem`).
- [x] UI: song menu + Files panel gained "Play in system player" (`playExternally`/`playExternallyHint` in all 5 languages), shown only when `isLocalService()` (new in `externalLinks.ts`: desktop shell, loopback API base, or same-origin localhost) so a remote browser never starts playback on the studio's computer.
- [x] Visualiser second window without Tauri: `window.open('visualizer.html')` fallback (same-origin popup shares localStorage state + BroadcastChannel feed; dead-popup polling); `visualizerWindow.tsx` uses document fullscreen + `window.close()` in browser, Tauri window calls lazy/dynamic. `@tauri-apps/api` static imports removed from `VisualizerPanel`/`visualizerWindow` (WinampMode keeps its inert-in-browser static imports; full Winamp-mode removal deferred — it is portable Webamp + MCP-wired, only its Win32 shaping was Windows-specific and that is already unreachable without the shell).
- [ ] Reveal/open-folder (`saving.rs:215-231`, `lib.rs:2362-2369,6387-6397`): fix the non-Windows `saving::reveal` branch (still spawns `explorer.exe`); use `xdg-open` like the other two sites.
- [x] `rfd` folder picker gated to Windows (`music-server/Cargo.toml` target-dep + `setup_adopt` requires explicit `{"path": ...}` on Linux). This also removed the `wayland-client` pkg-config build failure on headless Linux.
- [x] `saving::reveal` uses `xdg-open` (folder) on Linux instead of `explorer.exe`.
- [ ] MP3/LAME + tagging (`lib.rs:7624`, `id3`, `audio_pcm.rs` “no ffmpeg” comment): confirm whether LAME is linked/bundled or shell-out; ensure Linux provides it (system `libmp3lame` or bundled static) — check `audio_pcm.rs` + tagging code before closing this item.
- [ ] `symphonia`, `resvg`, `dicebear`, `zip/flate2`, `sysinfo`, `fs2` — all portable; just keep building on Linux.

Verification:

- [ ] Cold start with empty data root creates library/settings/models dirs in the right place.
- [ ] Save + reveal + open-data-directory all work from a plain browser (no Tauri).
- [ ] Library import/export round-trip incl. MP3 with tags + cover art.

## 11. Workstream 9 — frontend web-only mode (no Tauri APIs)

Tauri usage is small and already partially guarded:

- [ ] `app/services/externalLinks.ts:18-38` — uses `__TAURI__` bridge with `fallback(url)`; keep + test fallback in plain browser.
- [ ] `app/services/apiBase.ts:27` — already special-cases `tauri.localhost`; ensure dev proxy (`vite.config.ts:12-39`) and production same-origin serving both work.
- [ ] `app/components/player/VisualizerPanel.tsx:51-58` — `invoke('open_visualizer_window')` + `WebviewWindow.getByLabel('visualizer')`. Web fallback: `window.open('/visualizer.html')`; keep multi-window messaging working (visualizer is a second rollup input at `vite.config.ts:47-53`).
- [x] WinampMode OS window management removed (DONE: shaping/region/monitor/decorations/zoom-via-webview gone; fullscreen page with CSS zoom; `@tauri-apps/api` dropped from `package.json`/lock; `restoreWindowAfterReload` deleted). Player, skins, EQ carry-over, agent control unchanged.
- [x] Remaining UI shell traces removed (DONE): `isDesktop`/`__TAURI__` bridge gone, `openExternal` always opens a tab, `saveFile` always browser-downloads (+ download history in Files panel), visualizer is popup-only, network-access setting always shown, reveal gated on `isLocalService` like play.
- [ ] `app/visualizerWindow.tsx:4` — same treatment as WinampMode.
- [x] Remove hard version coupling: `app/vite.config.ts:7` reads `../desktop/src-tauri/tauri.conf.json` for `__APP_VERSION__`. Fork is deleting `desktop/`; read version from `app/package.json` (or a fork `version.json`) instead. Also `prebuild/predev` runs `node ../scripts/changelog.mjs` (`app/package.json:13-14`) — keep script or vendor it (it only needs Node at build time, which is allowed; the “no Node in runtime path” rule is about the shipped runtime). DONE 2026-10-02: falls back to `app/package.json` when the Tauri manifest is absent; `vite build` verified.
- [x] Decide static serving: DONE 2026-10-02 — Axum serves `app/dist` itself (`YUE_UI_DIR`, else `app/dist` beside CWD) through the existing `remote::interface` fallback with SPA routing and traversal guard (unit-tested); `run-linux.sh --build` sets it. Live-verified: `/` + JS + `visualizer.html` + SPA route 200, missing file 404, API intact.
- [x] LAN access (DONE 2026-10-02): `YUE_BIND_ADDR` bind parameter (default loopback); binding is not access — off-machine browsers still need network access + key via the existing guard. Live-verified on 192.168.178.50: 403 without key → enable via loopback → key → UI 200 + API 200. Startup logs a WARN with the exact enable commands when bound non-loopback with access off. Upstream `remote::interface` fallback + `set_asset_source` (Tauri asset resolver at `lib.rs:498-500`) needs a Linux equivalent for serving editor/UI assets — implement (a).
- [ ] Keep `@tauri-apps/api` dep only if dynamically imported; else drop from `app/package.json:24` in the fork to avoid confusion.

Verification:

- [ ] `npm --prefix app test` green; `vite build` output served to a plain Chromium/Firefox with zero `__TAURI__` present: create → library → player → EQ/visualiser → Winamp mode (unshaped) → save → MIDI editor all smoke-tested.
- [ ] No console errors from Tauri imports when Tauri is absent.

## 12. Workstream 10 — scripts, packaging, docs

- [x] Port needed scripts to `scripts/*.sh`: DONE `scripts/run-linux.sh` (dev: set envs, run service + vite; `--build`, `--service-only`), `sync-yue-source.sh`, `build-yue-runtime.sh`, `package-linux.sh` (tarball: service + UI + engine + licenses + `run.sh` + `release.json`). Tarball VERIFIED 2026-10-02: clean unpack + `./run.sh` boots, serves UI, fresh root shows first_run with no models and a healthy-but-idle engine bundle. No Debian packaging yet — deliberate, pending testing.
- [ ] Delete or archive `scripts/*.ps1`, `desktop/` NSIS bits, `tauri.release.conf.template.json`, `installer-*.ns*` in the fork (or keep `desktop/` untouched-but-ignored; deletion is cleaner for a “drop Windows completely” fork).
- [ ] Write fork README section: system deps, NVIDIA driver/CUDA requirements, bundle layout, env vars, ports (`8791` service, `18087` engine, `3791` vite dev), data dirs, model download behaviour (same catalogue, Linux runtimes), what is intentionally missing (Tauri/VST/DirectML/Whisper?/training? per §§7-8 decisions).
- [ ] Update `llms.txt` / `docs/mcp-skill.md` service URLs if they change (default: unchanged `http://127.0.0.1:8791/mcp`).
- [ ] Keep `CHANGELOG.md` fork entries separate from upstream.

## 13. Acceptance criteria (v1 done)

- [ ] `cargo test --workspace` + `npm --prefix app test` green on clean Linux.
- [ ] One-command Linux dev: service + UI via `scripts/run-linux.sh` (or documented two commands).
- [ ] NVIDIA GPU Linux: download model set → Create (full song + score) → replay/re-render → stems → karaoke (Parakeet) → MIDI transcribe → library/cover/MP3 export, all from plain browser.
- [ ] CPU-only Linux: generation still functions (slow) or fails with a clear message; separation/karaoke fall back to CPU builds.
- [ ] No references to `C:\`, `explorer.exe`, `LOCALAPPDATA`, `.dll`, `WebView2`, DirectML, NSIS in Linux runtime paths/logs (grep gate in CI).
- [ ] No committed weights/keys/media; `git status` clean of runtime data.
- [ ] MCP (`/mcp`, `studio_status`) reachable at `127.0.0.1:8791` for agents.

## 14. Suggested build order (dependencies first)

1. §2 scaffolding + §3 workspace compiles on Linux (fast, unblocks everything).
2. §4 engine build + supervisor health (critical path; mock UI with curl).
3. §5 hardware presence + §10 `run-linux.sh` + static UI serving (§11 partial) → first song on NVIDIA.
4. §6 ORT Linux → Parakeet/separation/tempo.
5. §9 web-only UI + §8 save/reveal/paths → full browser loop.
6. §7 Whisper/assistant/train/MIDI per BUILD/DROP decisions.
7. §10 packaging + docs + CI grep gates.

## 15. Open questions for fork owner

- [ ] Single CUDA backend or dual `cuda12`+`cuda13` on Linux? (Dual covers old cards/drivers; single is simpler.)
- [ ] System CUDA/ORT/llama.cpp vs studio-downloaded runtimes on Linux? (System packages fit Linux norms; downloads fit upstream UX.)
- [ ] Which sidecars are IN for v1: Whisper? training? MIDI? captioner/MOSS? (Recommendation: Parakeet + OpenRouter first; Whisper + assistant next; training/MIDI only if HOT-Step builds cleanly.)
- [ ] dewasa Single static-origin serving from Axum vs nginx/Caddy reverse proxy?
- [ ] Target distros/packages: tarball only, or `.deb`/AppImage/systemd unit as well?
