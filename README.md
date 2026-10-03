<div align="center">

<img src="docs/logo.png" alt="" width="112" height="112" />

# YuE2 Studio — Linux fork

**Full songs with an editable score, generated on your own GPU. No Tauri, no Windows — a Rust service, a browser UI, and native Linux playback.**

**This is a fork.** YuE2 Studio is the work of
[Ilya Timonin (timoncool)](https://github.com/timoncool), built on
[yue2.cpp](https://github.com/ServeurpersoCom/yue2.cpp) by
[Serveurperso](https://github.com/ServeurpersoCom) and the YuE2 model family
by [M-A-P](https://huggingface.co/m-a-p). The original Windows-first project
lives at **[timoncool/YuE2-Studio](https://github.com/timoncool/YuE2-Studio)** —
start there for the Windows installer, the project page, and the samples.
Everything below describes only what this fork changes.

[![License](https://img.shields.io/github/license/asb-42/yue2-studio?style=flat-square)](LICENSE)
[![CI](https://github.com/asb-42/yue2-studio/actions/workflows/ci.yml/badge.svg)](https://github.com/asb-42/yue2-studio/actions)

</div>

## What was ported, what changed, and why

The upstream studio is a Tauri 2 desktop app for Windows: one `.exe` holding
a WebView window, an Axum service, and a C++/CUDA engine with Windows DLLs,
DirectML, VST3, and Win32 window shaping. This fork keeps the parts that are
portable and replaces the parts that are Windows:

| Upstream (Windows) | This fork (Linux) | Why |
|---|---|---|
| Tauri window + WebView2 | Plain browser UI, served by the service itself (`YUE_UI_DIR`) | No WebView2 on Linux; same-origin serving removes CORS/proxying entirely |
| NSIS installer, portable zip, auto-updater | `scripts/run-linux.sh`, `scripts/package-linux.sh` tarball | No installer framework; Debian packaging comes later, deliberately |
| `yue-server.exe` + `ggml-*.dll` + downloaded cuBLAS + VC redist | `yue-server` + `libggml-*.so`, system CUDA toolkit | The Linux loader resolves `.so` from the system; nothing to download |
| DirectML for AMD/Intel ONNX | CPU fallback; CUDA where available | DirectML is Windows-only |
| Whisper standalone runtime | Parakeet + OpenRouter for karaoke | The standalone ships Windows builds only |
| `music-train` / `music-midi` / `ace-caption` `.exe` | Refused with a build-from-source message (`YUE_TRAIN_BIN` etc. escape hatches) | No Linux builds published; weights stay placeable by hand |
| Win32 shaped-window Winamp mode | Full-page Webamp player (skins, EQ, MilkDrop, agent control kept) | `SetWindowRgn` has no Linux equivalent; only the OS shaping is gone |
| Explorer reveal, Win32 save dialog | `xdg-open`, browser downloads, VLC/mpv/mplayer playback | Native Linux desktop integration |
| Loopback-only bind | `YUE_BIND_ADDR` (e.g. `0.0.0.0`) + the existing key guard | LAN access; binding is not access, the key still gates the network |

Unchanged: the song engine and its protocol, the model catalogue and pins,
the library format, the MCP server (`/mcp`, 162 tools), the score/MIDI
machinery, karaoke alignment, cover art, and the React UI feature set.

## Supported architectures

| | x86_64 Linux | aarch64 Linux |
|---|---|---|
| Service + UI | yes | yes (verified) |
| `yue-server` CUDA | yes (Turing→Blackwell, CUDA 13; Maxwell–Volta need a CUDA 12 toolkit build) | yes (verified on GB10, CUDA 13) |
| `yue-server` CPU | yes | yes |
| Vulkan backend | build with a Vulkan SDK (`--backend all`) | same |
| ONNX (Parakeet, stems, tempo) | CPU + CUDA 12 | CPU only (no upstream `linux-aarch64-gpu` build) |
| llama.cpp assistant | CPU, CUDA 13/12, Vulkan | CPU, CUDA 13, Vulkan |
| Training / MIDI / captioner exes | build HOT-Step from source | same |

CPU-only machines work everywhere, slowly. Unified-memory NVIDIA cards
(GB10) are detected despite reporting no VRAM figure; the model-set
recommendation conservatively falls back to `light` there.

## Quick start

```sh
./scripts/run-linux.sh            # service on 127.0.0.1:8791 + vite UI on 127.0.0.1:3791
./scripts/run-linux.sh --build    # release service; UI at http://127.0.0.1:8791/ directly
./scripts/run-linux.sh --service-only
```

System packages: `build-essential cmake pkg-config git curl python3 node 22+`.
Optional: CUDA toolkit 12 or 13 (`nvcc`), Vulkan SDK. Without `sudo`,
bootstrap ninja into `~/.local/bin` (see `docs/linux.md`).

```sh
./scripts/sync-yue-source.sh                              # pinned yue2.cpp checkout
./scripts/build-yue-runtime.sh --output dist/yue2-cpp --backend cuda --arch universal
```

Then choose the Light set in setup (or any set your VRAM fits), press
download, and Create. The first verified Linux song — a 30 s folk-pop track
on CUDA — took about a minute on GB10; Parakeet karaoke aligned it
word-perfect in seconds, and HT-Demucs split six stems.

Everything the studio owns stays under `YUE_STUDIO_DATA_ROOT`
(`~/.local/share/yue2-studio` by default): models, songs, settings, logs.
Weights, API keys, media and caches are runtime data and are never committed
(`.gitignore` refuses `*.gguf`, `*.safetensors`, `*.onnx`, `*.pt`, `*.ckpt`).

## LAN access

```sh
YUE_BIND_ADDR=0.0.0.0 ./scripts/run-linux.sh --build
curl -X PUT http://127.0.0.1:8791/v1/network -H 'Content-Type: application/json' -d '{"enabled": true}'
curl http://127.0.0.1:8791/v1/network   # the key (loopback only)
```

Open `http://<machine>:8791/?key=<key>` once; API clients send
`Authorization: Bearer <key>`. The service prints the first-access URL to its
log at startup and on enable. Without the key the LAN gets 403.

### No key at hand: the SSH tunnel

From the other computer (Linux, macOS, or Windows PowerShell — OpenSSH is
built in everywhere now):

```sh
ssh -N -L 8791:localhost:8791 user@192.168.178.50
# open http://localhost:8791/ — no key needed, the tunnel counts as local
```

Keep it alive across network naps with `autossh -M 0` in place of `ssh`,
or just leave a second terminal tab open. The tunnel also solves the one
LAN limitation below without any certificates.

### LAN limitation: AudioWorklet needs a secure context

The piano-roll editor's synth and the visualiser pop-out feed use
`AudioWorklet`, which browsers expose only on `localhost`/HTTPS — never on
plain `http://192.168.x.x`. Those two say so plainly instead of crashing;
use the SSH tunnel above or the HTTPS option below for them. Everything
else, MIDI preview included, works over plain LAN http.

### LAN over HTTPS (no tunnel, one click-through)

```sh
YUE_BIND_ADDR=0.0.0.0 YUE_TLS_PORT=8792 ./scripts/run-linux.sh --build
```

The service generates a self-signed certificate on first start (kept in the
data root, `tls/`), prints its SHA-256 fingerprint to the log, and serves
the whole studio — UI and API — on the TLS port alongside plain HTTP.
Open `https://<machine>:8792/`, check the fingerprint matches the log once,
accept, and the browser treats the page as secure: editor and visualiser feed
work.

## Drive it from an agent (MCP)

As upstream: while the service runs, `http://127.0.0.1:8791/mcp`
(`studio_status` first, skill in `docs/mcp-skill.md`).

## Models

Same catalogue and pins as upstream 3.4.0: YuE2 GGUFs from
[Serveurperso/YuE2-GGUF](https://huggingface.co/Serveurperso/YuE2-GGUF) at
`64b030e`, the decoder companion from
[Mothersuperior/yue2-mothersuperior-realaudio-tokenizer-v4](https://huggingface.co/Mothersuperior/yue2-mothersuperior-realaudio-tokenizer-v4)
at `e2e63d8`, checked by size and SHA-256.

| Your GPU | Set | Download |
| --- | --- | --- |
| 12 GB VRAM and above | Full native — BF16 backbone | 9.8 GB |
| 8 GB and above | Quality — Q8_0 backbone | 5.1 GB |
| 7 GB and above | Balanced — Q6_K backbone | 4.1 GB |
| 5.5 GB and above | Light — Q5_K_M backbone | 3.8 GB |

The ONNX Runtime (1.30.0) and llama.cpp (b11236) builds come from their
upstream releases for Ubuntu (x64 and arm64), verified in
`crates/music-server/src/lyrics_sync.rs` and `assistant_runtime.rs`.
Parakeet, HT-Demucs, MuScriptor, MOSS-Music and trainer weights are fetched
from the same mirrors as upstream.

## Architecture

```text
browser ─── YuE2 Studio service (Rust/Axum on 127.0.0.1:8791, UI included)
                    ├─ yue-server   (C++/CUDA, GGUF, 127.0.0.1:18087)
                    ├─ llama-server (assistant sidecar, optional)
                    └─ music-train / music-midi (source builds, optional)
```

No Python and no Node.js in the runtime path. Model weights are never part
of a release. Full details: [`docs/linux.md`](docs/linux.md),
[`docs/plans/linux-port.md`](docs/plans/linux-port.md).

## License

The studio code is MIT ([LICENSE](LICENSE)), and so is yue2.cpp —
both kept verbatim from upstream, whose copyright stands. **The models are
not.** The YuE2-3B and YuE2 VAE weights are **CC BY-NC 4.0 with an
individual-creator permission**
([MODEL_LICENSE](https://github.com/multimodal-art-projection/YuE/blob/main/MODEL_LICENSE)):
personal users, content creators and musicians acting on their own may use
them free of charge and publish, sell, license or otherwise monetize the
songs they make. SheetSage2 and the decoder companion are **CC BY-NC 4.0**
without that permission; MuScriptor weights are **CC BY-NC 4.0**.

Bundled components under other licences, as upstream: the visualiser's
spectrum looks ([audioMotion-analyzer](https://github.com/hvianna/audioMotion-analyzer),
**AGPL-3.0**); score/MIDI import ported from
[YuE2-ComfyUI](https://github.com/pytraveler/YuE2-ComfyUI) (**Apache-2.0**,
kept in `licenses/`); the MIDI editor ([signal](https://github.com/ryohey/signal),
**MIT**) with A320U SoundFonts (**GPL-2.0**, kept in `licenses/`).
Acknowledgements to all upstream authors in full: M-A-P, Mothersuperior,
Serveurperso, scragnog (HOT-Step), and every author credited in the
upstream README's acknowledgements, which this fork inherits.

What changed in this fork versus upstream is tracked per commit from
`feb80d1` on; `CHANGELOG.md` carries the upstream history with fork entries
appended separately below.
