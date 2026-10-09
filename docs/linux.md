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

First run needs Rust (`cargo`, via rustup) and, for the UI modes, Node 22+
(`npm`); the launcher errors clearly when either is missing and installs
`app/node_modules` (gitignored) itself via `npm ci`. In dev mode the browser
goes to `:3791` (`:8791` answers only the API); if vite never starts, the
launcher's EXIT trap stops the service too, so both ports stay dead.

With `--build` (or `YUE_UI_DIR=/path/to/app/dist` set manually) the service
serves the built interface on its own port: open `http://127.0.0.1:8791/`
directly, no dev server needed. Same-origin, so the visualiser popup,
`saving`, and downloads all work without CORS or proxying.

## LAN access

The service binds loopback unless told otherwise. For another computer on
your network:

```sh
YUE_BIND_ADDR=0.0.0.0 ./scripts/run-linux.sh --build   # or export it
```

Binding is not access: browsers off this computer still need network access
enabled with its key (loopback stays keyless). From the studio computer:

```sh
curl -X PUT http://127.0.0.1:8791/v1/network \
  -H 'Content-Type: application/json' -d '{"enabled": true}'
curl http://127.0.0.1:8791/v1/network   # shows the 64-char key (loopback only)
```

Then open `http://<this-machine>:8791/?key=<key>` once (it becomes a
cookie afterwards); API clients send `Authorization: Bearer <key>`. Without
the key the LAN gets 403 with instructions. To turn it off again:
`PUT /v1/network {"enabled": false}` (loopback only).

LAN limits (browser security, not this fork): `AudioWorklet` — the piano-roll
editor's synth and the visualiser pop-out feed — is only exposed on
localhost or HTTPS, never on plain LAN `http`. Over LAN those two say so
plainly instead of crashing; use `http://localhost:8791` (SSH tunnel) or
serve HTTPS for them. Everything else, MIDI preview included, works wherever
the page loads.

First access without touching the terminal twice: when the service starts
bound off-loopback with access already on, it prints the full
`http://<this-machine>:8791?key=…` URL once to its log; enabling access via
the API logs it too. Afterwards the key lives in Settings and in
`GET /v1/network` on loopback only — it is never shown to the network.

System packages: `build-essential cmake pkg-config git curl python3 node 22+`.
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

## Style by ear (auto-describe)

The optional listening pack (MOSS-Music + Beat This!) lets the studio hear a
song and write its style caption itself — genre, vocals, instruments, mood —
with the tempo measured from the recording. It is **not** installed by
default; the Training page shows an "Auto-describe songs (optional)" card with
a one-click download (~10.5 GB) when the pack is missing.

Once installed, the style-by-ear path is used automatically when:

- **Create → Cover → Cover a song** — the source song is heard and the style
  is written from what was heard (the `describeByEarHint` in the cover flow
  explains this).
- **Library → song menu → Edit metadata** — the "Style by ear" action
  re-hears the song and rewrites the style.

The heard style is a draft: MOSS can hallucinate vocals on instrumental
tracks, so the pipeline now distrusts heard vocal claims for instrumentals
and requires the caption to start with "instrumental" when the track has
no vocals. The assistant (llama.cpp) polishes the caption into the final
YuE2 style line.

### The model wants to sing

Worth knowing before you spend GPU time on it: **"instrumental" in the style,
and `[instrumental]` in the lyrics, are advisory.** They reliably stop *words*
being sung — and then the model vocalises instead: hums, vowel shapes, an
"mmm" choir. Measured on 2026-10-10, adding the Instrumental LoRA
(`yue2-instrumental`, AR 1.0) made no difference: isolated vocal stem rms
0.103 without it, 0.120 with it. Only the **score** binds — melody on the
`Ins` line with `Vocal` silent gives a genuinely instrumental render.

So when a track needs intelligible sung lyrics, leave "instrumental" out; when
it needs no singing, supply a score. See
[`docs/plans/2026-10-10_choir.md`](2026-10-10_choir.md) §7a.

Also note what the ASR check does and does not prove: `karaoke.instrumental`
means *no intelligible words*, not *no singing*. A track with a tenor verse and
a female chorus passes it. Judge by ear or by vocal-stem energy.

Four slow waltzes (3/4, BED3 recipe: melody on `Ins`, `Vocal` silent) came back
3-of-4 clean — jazz, country and chanson have silent vocals stems (rms 0.0000,
0.0003, 0.0011), while the ballroom waltz vocalises around 25–35 s (vocals
rms 0.0674, blocks at 0.20/0.14). Same recipe, same session: whether it sings
is per-render luck, not a setting. The dialect spells 3/4 natively (`M:3/4`
is 24 L-units a bar, `z24` a full-bar rest); a bare "waltz" in the style risks
Viennese tempo, so name the type and the BPM (84–104 in the measured set).

### Stems from generated music are not a multitrack

HT-Demucs splits a generated track into six stems, and they are **not** clean
isolations the way a real multitrack session is: an "other" stem meant to hold
an organ also carries fragments of the vocalised singing, and the artefacts you
hear in a stacked stem are usually those bleed-throughs rather than anything
the processing did. The likely reason is plain: source-separation models are
trained mostly on finished mixes, not on isolated stems. Treat this as
principled and currently unavoidable — plan mixes around it rather than
assuming stem surgery is clean.

### The chain that makes the harmonizer usable: stems → stack → remix

A processing run reads either the track's own file (`"source": "mix"`) or one
stem of it (`"source": {"stem": "vocals"}`), and a `remix` puts the stems back
together afterwards, the processed audio standing in for the stem it came from.
The UI offers both in the processing panel, once a track has been split.

That order is not decoration. The harmonizer shifts *everything* it is given:
on a whole mix every instrument moves at once and the voice's formants move
with it, which is why it only sounds right on a single stem. A remix of a
whole mix would sum the untouched mix back over the processed one, so the
server refuses it — a remix needs a stem as its source, and stems on disk.

An untouched remix gives the mix back. Measured on a 95.976 s track: the six
stems sum to a peak of 1.0212, the remix is that sum held at 0.9900, and the
result matches the scaled sum to 0.000000. Which is what makes the per-stem
levels worth trusting — a stem turned down is heard as turned down.

The bleed-through above still applies to the result: a stacked stem carries
fragments of the others, and `remix` puts them back.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `YUE_STUDIO_DATA_ROOT` | `${XDG_DATA_HOME:-~/.local/share}/yue2-studio` | everything the studio keeps; point it at `./workdir` to keep data beside the checkout as one portable piece (gitignored) |
|---|---|---|
| `YUE_BIND_ADDR` | loopback (or setup wildcard) | bind address; `0.0.0.0` opens the LAN (still key-gated, see above) |
| `YUE_TLS_PORT` | unset (HTTP only) | HTTPS port serving the whole studio (self-signed cert in `$DATA/tls/`; makes LAN browsers secure-context) |
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
