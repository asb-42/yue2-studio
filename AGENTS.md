# YuE2 Studio — Linux fork guidelines

This fork is a headless-first Linux studio: Rust/Axum service (serving its own
browser UI), native C++/CUDA engines, native Linux desktop integration. The
Tauri shell, Windows installers, and all Windows-only code were removed;
see README.md for the full attribution and porting notes.

- Do not add Python or Node.js to the runtime path.
- Do not reintroduce Windows-only dependencies (Tauri, DirectML, VST3 host,
  WebView2, NSIS). Platform code stays behind `cfg(windows)` only where the
  Windows build is still maintained alongside — otherwise delete it.
- Model engines are adapters; no UI or server code may hardcode one model's behaviour.
- Provider choice is capability-level: music, ASR, LLM and cover art can independently be Local or OpenRouter.
- Keep OpenRouter model identifiers in provider configuration, not source code.
- Every local engine must expose install state, capability metadata, cancellation and progress before it reaches the UI.
- Model weights, API keys, media and caches are runtime user data; never commit them.
  `.gitignore` refuses `*.gguf`, `*.safetensors`, `*.onnx`, `*.pt`, `*.ckpt`.
- Binding is not access: `YUE_BIND_ADDR` may open the LAN, but off-machine
  browsers always go through the network key guard. Never log keys except the
  one-time first-access URL for the operator.
