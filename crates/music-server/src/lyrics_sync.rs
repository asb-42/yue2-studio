//! Karaoke timings for a finished track.
//!
//! The words are already known - YuE2 sang the lyrics it was given - but the
//! *timings* are not, and the video studio's karaoke layer and the player both
//! need them. This module produces an LRC file for a track using whichever
//! recogniser the user picked; like every optional extra here it is off by
//! default and downloads nothing on its own.
//!
//! Three backends, all of them explicit choices:
//!
//!   * **Whisper** - whisper.cpp run as a sidecar. It writes LRC itself, and
//!     the CUDA build is preferred over the CPU one when both are installed.
//!   * **Parakeet** - the NVIDIA TDT model, the same one Dub Studio uses.
//!   * **OpenRouter** - a cloud model, billed to the user's own key, asked for
//!     verbose output because plain text carries no timings.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use parakeet_rs::Transcriber;

pub use crate::downloads::Asset;
use crate::downloads::{AssetKind, Downloader};

/// Which recogniser produces the timings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrProvider {
    #[default]
    None,
    Whisper,
    Parakeet,
    OpenRouter,
}

/// Persisted with the rest of the studio settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LyricsSyncConfig {
    /// The karaoke switch. Off means the buttons do not appear at all.
    pub enabled: bool,
    pub provider: AsrProvider,
    /// Which downloaded Whisper model to run.
    pub whisper_model: Option<String>,
    /// Which OpenRouter speech-to-text model to call.
    pub openrouter_model: Option<String>,
    /// Let Whisper skip what its speech detector says is not speech. Off by
    /// default, and that is a decision rather than an oversight: the detector is
    /// trained on speech, and this studio's other half is singing, which it
    /// answers with silence. On the other hand it is the honest answer when
    /// there is nothing to hear.
    pub whisper_vad: bool,
    /// What the local recogniser runs on. It decides which runtime is
    /// downloaded as much as which one is loaded, so it belongs to the setting
    /// rather than to a guess made at load time.
    #[serde(default)]
    pub runtime: OnnxFlavour,
}

impl LyricsSyncConfig {
    pub fn available(&self) -> bool {
        self.enabled && self.provider != AsrProvider::None
    }
}

/// The reason the interface shows when a recogniser hears nothing sung: a
/// code, not a sentence, so it is put in the reader's language.
pub const NO_WORDS: &str = "karaoke.no-words";

/// Where the recogniser's binaries live once unpacked. CTranslate2 loads its
/// CUDA libraries from beside the executable, so they share one directory - the
/// way Dub Studio arranges it, and the reason its card mode works instead of
/// quietly falling back to the processor.
pub const WHISPER_RUNTIME_DIR: &str = "whisper";

/// The model sizes the recogniser knows, as `--model` names them.
pub const WHISPER_SIZES: &[&str] = &["tiny", "base", "small", "medium", "large-v3", "large-v3-turbo"];

/// The recogniser's own Linux build, pinned to the release that carries it.
const WHISPER_ENGINE_URL: &str =
    "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-ubuntu-arm64.tar.gz";
const WHISPER_ENGINE_BYTES: u64 = 4_608_377;

/// Silero's detector, as whisper.cpp's built-in voice activity detection wants
/// it, and the asset that brings it.
pub const WHISPER_VAD_ASSET: &str = "whisper-vad";
const VAD_MODEL_FILE: &str = "ggml-silero-v6.2.0.bin";

/// Phrases Whisper writes over music and silence instead of saying it heard
/// no words: the verbatim hallucinations of the "Bag of Hallucinations"
/// study (Barański et al., ICASSP 2025, MIT) and the per-language lists of
/// NVIDIA NeMo's Granary pipeline (Apache-2.0), as merged by
/// Scicom-AI/Whisper-Hallucination, kept for the studio's languages and a few
/// common ones, phrases of two words or more, normalised.
const HALLUCINATIONS: &str = include_str!("whisper_hallucinations.txt");

/// Where a batch was recognised: on the device chosen, or on the processor
/// after the card refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recognised {
    OnDevice,
    OnProcessor,
}

/// Words that only ever appear in a subtitler's credit, never in a song.
const CREDIT_MARKERS: &[&str] = &["dimatorzok", "субтитр", "amara.org", "untertitel", "sous-titr", "subtítulo", "sottotitol", "legendas por", "subtitles by", "字幕", "자막"];

fn normalised(text: &str) -> String {
    let lowered = text.to_lowercase().replace('ё', "е");
    lowered
        .split(|c: char| c.is_whitespace() || ".,!?…\"'«»“”„-–—:;()[]♪。、！？「」".contains(c))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a recognised segment is Whisper filling a gap rather than words
/// that were sung: a known hallucination said whole, a subtitler's credit, a
/// sound written as a caption ("ВЕСЕЛАЯ МУЗЫКА", "[Music]"), or no letters at
/// all. A segment is judged whole, so a sung line that merely contains such a
/// phrase stays.
pub fn is_hallucination(text: &str) -> bool {
    static KNOWN: std::sync::OnceLock<std::collections::HashSet<&'static str>> = std::sync::OnceLock::new();
    let trimmed = text.trim();
    if !trimmed.chars().any(char::is_alphabetic) {
        return true;
    }
    let bracketed = (trimmed.starts_with('[') && trimmed.ends_with(']')) || (trimmed.starts_with('(') && trimmed.ends_with(')')) || trimmed.starts_with('♪');
    let letters: Vec<char> = trimmed.chars().filter(|c| c.is_alphabetic()).collect();
    let shouted = letters.len() >= 3 && letters.iter().all(|c| !c.is_lowercase()) && letters.iter().any(|c| c.is_uppercase());
    if bracketed || shouted {
        return true;
    }
    let plain = normalised(trimmed);
    if CREDIT_MARKERS.iter().any(|marker| plain.contains(marker)) {
        return true;
    }
    KNOWN.get_or_init(|| HALLUCINATIONS.lines().filter(|line| !line.is_empty()).collect()).contains(plain.as_str())
}

/// Where whisper.cpp puts the transcript of `wav`: beside it, named after it
/// with `.json` in place of the extension - so `a.wav` answers at `a.wav.json`.
fn whisper_json_beside(wav: &Path) -> PathBuf {
    wav.with_extension("wav.json")
}

/// The words and their times out of whisper.cpp's JSON.
///
/// whisper.cpp hands back *tokens*, not words: a piece of a word at a time, in
/// the byte-pair encoding the model thinks in. A token that opens with a space
/// begins a word and the rest continue it, which is the only rule there is - so
/// a word is assembled here and carries the time of the token that opened it.
///
/// A token that came back without word timestamps becomes one long "word":
/// better a line placed roughly than a line dropped, and the written lyrics
/// are laid back over whatever times these are.
fn whisper_words_from_json(text: &str) -> Vec<(f64, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(segments) = value.get("transcription").and_then(|value| value.as_array()) else { return Vec::new() };
    let at = |token: &serde_json::Value| {
        token
            .get("offsets")
            .and_then(|value| value.get("from"))
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
            / 1000.0
    };
    let mut words: Vec<(f64, String)> = Vec::new();
    for segment in segments {
        if is_hallucination(segment.get("text").and_then(|value| value.as_str()).unwrap_or_default()) {
            continue;
        }
        match segment.get("tokens").and_then(|value| value.as_array()) {
            Some(list) if !list.is_empty() => {
                for token in list {
                    let piece = token.get("text").and_then(|value| value.as_str()).unwrap_or_default();
                    // `[_BEG_]` and the timestamp markers carry no word
                    if piece.starts_with("[_") || piece.starts_with("<|") {
                        continue;
                    }
                    let opens = piece.starts_with(' ');
                    let piece = piece.trim();
                    if piece.is_empty() {
                        continue;
                    }
                    match (opens, words.last_mut()) {
                        (true, _) | (false, None) => words.push((at(token), piece.to_string())),
                        (false, Some(last)) => last.1.push_str(piece),
                    }
                }
            }
            _ => {
                // No tokens: the segment text is all there is.
                let mut line = segment.get("text").and_then(|value| value.as_str()).unwrap_or_default().trim().to_string();
                line = line.replace("[_BEG_]", "").trim().to_string();
                if !line.is_empty() {
                    words.push((at(segment), line));
                }
            }
        }
    }
    words
}

/// Release locations per platform: Windows zips, Linux tarballs (verified
/// against the v1.30.0 release; sizes are display-only, the real figure comes
/// from the server at download time). The GPU build follows the engine's
/// choice of CUDA 12, the one its provider was built against.
#[cfg(windows)]
const ONNXRUNTIME_CPU_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-win-x64-1.30.0.zip";
#[cfg(all(not(windows), target_arch = "x86_64"))]
const ONNXRUNTIME_CPU_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-1.30.0.tgz";
#[cfg(all(not(windows), target_arch = "aarch64"))]
const ONNXRUNTIME_CPU_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-aarch64-1.30.0.tgz";
#[cfg(all(not(windows), not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const ONNXRUNTIME_CPU_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-1.30.0.tgz";
#[cfg(windows)]
const ONNXRUNTIME_CPU_FILE: &str = "runtime/onnxruntime.zip";
#[cfg(not(windows))]
const ONNXRUNTIME_CPU_FILE: &str = "runtime/onnxruntime.tgz";
#[cfg(windows)]
const ONNXRUNTIME_CPU_BYTES: u64 = 82_645_522;
#[cfg(all(not(windows), target_arch = "x86_64"))]
const ONNXRUNTIME_CPU_BYTES: u64 = 11_306_877;
#[cfg(all(not(windows), target_arch = "aarch64"))]
const ONNXRUNTIME_CPU_BYTES: u64 = 10_269_495;
#[cfg(all(not(windows), not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const ONNXRUNTIME_CPU_BYTES: u64 = 11_306_877;
/// The library name the loader binds: `ort` loads this at run time from the
/// runtime directory (`machine_runtime`), so it must name the file the
/// archive actually holds.
#[cfg(windows)]
const ONNXRUNTIME_LIBRARY: &str = "onnxruntime.dll";
#[cfg(not(windows))]
const ONNXRUNTIME_LIBRARY: &str = "libonnxruntime.so";
#[cfg(windows)]
const ONNXRUNTIME_CUDA_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-win-x64-gpu_cuda12-1.30.0.zip";
#[cfg(all(not(windows), target_arch = "x86_64"))]
const ONNXRUNTIME_CUDA_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-gpu_cuda12-1.30.0.tgz";
/// No `linux-aarch64-gpu` upstream: ARM CUDA falls back to the processor
/// build, and `asset` below hides this entry there.
#[cfg(all(not(windows), target_arch = "aarch64"))]
const ONNXRUNTIME_CUDA_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-gpu_cuda12-1.30.0.tgz";
#[cfg(all(not(windows), not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const ONNXRUNTIME_CUDA_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-gpu_cuda12-1.30.0.tgz";
#[cfg(windows)]
const ONNXRUNTIME_CUDA_FILE: &str = "runtime/onnxruntime-cuda.zip";
#[cfg(not(windows))]
const ONNXRUNTIME_CUDA_FILE: &str = "runtime/onnxruntime-cuda.tgz";
#[cfg(windows)]
const ONNXRUNTIME_CUDA_BYTES: u64 = 379_723_801;
#[cfg(not(windows))]
const ONNXRUNTIME_CUDA_BYTES: u64 = 439_354_926;
#[cfg(windows)]
const ONNXRUNTIME_CUDA_PROVIDER: &str = "onnxruntime_providers_cuda.dll";
#[cfg(not(windows))]
const ONNXRUNTIME_CUDA_PROVIDER: &str = "libonnxruntime_providers_cuda.so";

pub const ASSETS: &[Asset] = &[
    Asset {
        id: "whisper-engine",
        label: "Whisper (whisper.cpp)",
        kind: AssetKind::Runtime,
        url: WHISPER_ENGINE_URL,
        relative_path: "runtime/whisper-cpp.tar.gz",
        bytes: WHISPER_ENGINE_BYTES,
        unzip_into: Some(WHISPER_RUNTIME_DIR),
        marker: "whisper-cli",
        pick: &[],
        // The binary and every library it links, out of the archive's one
        // folder. Nothing in it is spare, so the four megabytes go in whole.
        keep: &["whisper-cli", "libwhisper", "libggml", "libparakeet"],
        vram_gb: None,
        note: "whisper.cpp's own build for this platform: word timestamps, and the --language flag that the Windows faster-whisper bundle cannot honour here.",
    },
    Asset {
        id: "whisper-vad",
        label: "Silero speech detector (for Whisper)",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin",
        relative_path: "models/whisper/ggml-silero-v6.2.0.bin",
        bytes: 885_098,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "Trained on speech, not on singing: on a choral passage it answers with silence, which is the truer thing to say.",
    },
    Asset {
        id: "whisper-tiny",
        label: "Whisper tiny",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        relative_path: "models/whisper/ggml-tiny.bin",
        bytes: 77691713,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(1),
        note: "The smallest there is. For a quick check, not for lyrics.",
    },
    Asset {
        id: "whisper-base",
        label: "Whisper base",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        relative_path: "models/whisper/ggml-base.bin",
        bytes: 147951465,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(1),
        note: "A good first choice on the processor.",
    },
    Asset {
        id: "whisper-small",
        label: "Whisper small",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        relative_path: "models/whisper/ggml-small.bin",
        bytes: 487601967,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(2),
        note: "Better with crowded audio, slower on the processor.",
    },
    Asset {
        id: "whisper-medium",
        label: "Whisper medium",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        relative_path: "models/whisper/ggml-medium.bin",
        bytes: 1533763069,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(5),
        note: "The last that fits a normal card comfortably.",
    },
    Asset {
        id: "whisper-large-v3",
        label: "Whisper large-v3",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        relative_path: "models/whisper/ggml-large-v3.bin",
        bytes: 3094623391,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(10),
        note: "The best there is, and the largest by far.",
    },
    Asset {
        id: "whisper-large-v3-turbo",
        label: "Whisper large-v3-turbo",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        relative_path: "models/whisper/ggml-large-v3-turbo.bin",
        bytes: 1623625531,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(6),
        note: "large-v3 quality at a third of the time.",
    },
    Asset {
        id: "parakeet-tdt-int8",
        label: "Parakeet TDT 0.6B v3 (int8)",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx",
        relative_path: "models/parakeet/encoder-model.int8.onnx",
        bytes: 652_183_999,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(2),
        note: "The encoder; the decoder and vocabulary come with it.",
    },
    // The fp32 encoder, exactly as Dub Studio fetches it: the graph and its
    // weights are two files, and the weights are the 2.4 GB half.
    Asset {
        id: "parakeet-tdt-fp32",
        label: "Parakeet TDT 0.6B v3 (fp32)",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx",
        relative_path: "models/parakeet-fp32/encoder-model.onnx",
        bytes: 41_770_866,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: Some(4),
        note: "Full precision: heavier than int8, and the most accurate of the two.",
    },
    Asset {
        id: "parakeet-tdt-fp32-weights",
        label: "Parakeet fp32 weights",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx.data",
        relative_path: "models/parakeet-fp32/encoder-model.onnx.data",
        bytes: 2_435_420_160,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "The weights the fp32 graph points at.",
    },
    Asset {
        id: "parakeet-decoder",
        label: "Parakeet decoder",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx",
        relative_path: "models/parakeet/decoder_joint-model.int8.onnx",
        bytes: 18_202_004,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "Required alongside the Parakeet encoder.",
    },
    Asset {
        id: "parakeet-features",
        label: "Parakeet feature extractor",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/nemo128.onnx",
        relative_path: "models/parakeet/nemo128.onnx",
        bytes: 139_764,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "The mel front end the encoder expects.",
    },
    Asset {
        id: "parakeet-vocab",
        label: "Parakeet vocabulary",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
        relative_path: "models/parakeet/vocab.txt",
        bytes: 93_939,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "Token table.",
    },
    Asset {
        id: "parakeet-config",
        label: "Parakeet configuration",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/config.json",
        relative_path: "models/parakeet/config.json",
        bytes: 97,
        unzip_into: None,
        marker: "",
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "Token table.",
    },
    Asset {
        id: "onnxruntime-cuda",
        label: "ONNX Runtime 1.30.0 · CUDA",
        kind: AssetKind::Runtime,
        url: ONNXRUNTIME_CUDA_URL,
        relative_path: ONNXRUNTIME_CUDA_FILE,
        bytes: ONNXRUNTIME_CUDA_BYTES,
        unzip_into: Some("onnx-cuda"),
        marker: ONNXRUNTIME_CUDA_PROVIDER,
        pick: &[],
        keep: &[],
        vram_gb: Some(2),
        note: "Runs the separator on an NVIDIA card instead of the processor. Needs CUDA 12.",
    },
    Asset {
        id: "cuda-cublas",
        label: "NVIDIA cuBLAS 12.9",
        kind: AssetKind::Runtime,
        url: "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.2.10-archive.zip",
        relative_path: "runtime/cuda-cublas.zip",
        bytes: 549_731_131,
        unzip_into: Some("onnx-cuda"),
        marker: "cublasLt64_12.dll",
        pick: &["cublasLt64_12.dll", "cublas64_12.dll"],
        keep: &[],
        vram_gb: None,
        note: "The linear algebra the CUDA provider is built on.",
    },
    Asset {
        id: "cuda-cudart",
        label: "NVIDIA CUDA runtime 12.9",
        kind: AssetKind::Runtime,
        url: "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip",
        relative_path: "runtime/cuda-cudart.zip",
        bytes: 3_521_238,
        unzip_into: Some("onnx-cuda"),
        marker: "cudart64_12.dll",
        pick: &["cudart64_12.dll"],
        keep: &[],
        vram_gb: None,
        note: "The CUDA runtime itself.",
    },
    Asset {
        id: "cuda-cufft",
        label: "NVIDIA cuFFT 11.4",
        kind: AssetKind::Runtime,
        url: "https://developer.download.nvidia.com/compute/cuda/redist/libcufft/windows-x86_64/libcufft-windows-x86_64-11.4.1.4-archive.zip",
        relative_path: "runtime/cuda-cufft.zip",
        bytes: 198_361_265,
        unzip_into: Some("onnx-cuda"),
        marker: "cufft64_11.dll",
        pick: &["cufft64_11.dll"],
        keep: &[],
        vram_gb: None,
        note: "The transforms the provider uses for spectral work.",
    },
    Asset {
        id: "cuda-cudnn",
        label: "NVIDIA cuDNN 9.25",
        kind: AssetKind::Runtime,
        url: "https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.25.0.15_cuda12-archive.zip",
        relative_path: "runtime/cuda-cudnn.zip",
        bytes: 1_904_452_100,
        unzip_into: Some("onnx-cuda"),
        marker: "cudnn64_9.dll",
        // Everything the convolution path loads, and nothing else: the
        // attention kernels alone are another 250 MB the separator never calls.
        pick: &[
            "cudnn64_9.dll",
            "cudnn_graph64_9.dll",
            "cudnn_ops64_9.dll",
            "cudnn_cnn64_9.dll",
            "cudnn_heuristic64_9.dll",
            "cudnn_engines_precompiled64_9.dll",
            "cudnn_engines_runtime_compiled64_9.dll",
            "cudnn_engines_tensor_ir64_9.dll",
            "cudnn_ext64_9.dll",
        ],
        keep: &[],
        vram_gb: None,
        note: "The convolution kernels the separator spends its time in.",
    },
    Asset {
        id: "onnxruntime",
        label: "ONNX Runtime 1.30.0",
        kind: AssetKind::Runtime,
        url: ONNXRUNTIME_CPU_URL,
        relative_path: ONNXRUNTIME_CPU_FILE,
        bytes: ONNXRUNTIME_CPU_BYTES,
        unzip_into: Some("onnx"),
        marker: ONNXRUNTIME_LIBRARY,
        pick: &[],
        keep: &[],
        vram_gb: None,
        note: "Parakeet runs on this; it is loaded at run time, not linked in.",
    },
    // The card path for every card that is not an NVIDIA one running CUDA:
    // DirectML over DirectX 12. 1.24.4 is the last DirectML build Microsoft
    // publishes; it speaks the same API level (24) as the builds above.
    Asset {
        id: "onnxruntime-directml",
        label: "ONNX Runtime 1.24.4 · DirectML",
        kind: AssetKind::Runtime,
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
        relative_path: "runtime/onnxruntime-directml.nupkg",
        bytes: 12_458_649,
        unzip_into: Some("onnx-dml"),
        marker: "onnxruntime.dll",
        pick: &["runtimes/win-x64/native/onnxruntime.dll", "runtimes/win-x64/native/onnxruntime_providers_shared.dll"],
        keep: &[],
        vram_gb: None,
        note: "Runs karaoke's Parakeet and the tempo model on an AMD or Intel card through DirectX 12.",
    },
    Asset {
        id: "directml",
        label: "DirectML 1.15.4",
        kind: AssetKind::Runtime,
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
        relative_path: "runtime/directml.nupkg",
        bytes: 202_292_617,
        unzip_into: Some("onnx-dml"),
        marker: "DirectML.dll",
        // the package carries the library for Xbox too, under the same name
        pick: &["bin/x64-win/DirectML.dll"],
        keep: &[],
        vram_gb: None,
        note: "The DirectML the runtime above is built for; the copy inside Windows is older.",
    },
];

/// Everything the DirectML path needs, in the order it is used.
pub const DIRECTML_ASSETS: [&str; 2] = ["onnxruntime-directml", "directml"];

/// Which build of the ONNX Runtime to load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnnxFlavour {
    /// The graphics card if its runtime is installed, otherwise the processor.
    Auto,
    /// The default: this studio exists for machines with an NVIDIA card, and
    /// the processor path is minutes where the card is seconds.
    #[default]
    Cuda,
    Cpu,
}

impl OnnxFlavour {
    /// Whether work runs on the card through CUDA: only an NVIDIA card with a
    /// driver that runs CUDA does. An AMD card was sent to the CUDA provider
    /// and failed, after being offered gigabytes of CUDA libraries.
    pub fn uses_cuda(self) -> bool {
        !matches!(self, OnnxFlavour::Cpu) && crate::hardware::hardware().cuda.is_some()
    }

    /// Whether work runs on the card through DirectML: every card CUDA does
    /// not run on - AMD, Intel, an NVIDIA card whose driver is too old - as
    /// long as there is a card at all. Windows-only: the DirectML runtime and
    /// its assets are Windows builds, so this is never the Linux card path.
    pub fn uses_directml(self) -> bool {
        if cfg!(not(windows)) {
            return false;
        }
        let hardware = crate::hardware::hardware();
        !matches!(self, OnnxFlavour::Cpu) && hardware.cuda.is_none() && hardware.gpu_name.is_some()
    }

    /// The card path this choice takes on this machine; none is the processor.
    pub fn card(self) -> Option<OnnxCard> {
        if self.uses_cuda() {
            Some(OnnxCard::Cuda)
        } else if self.uses_directml() {
            Some(OnnxCard::DirectMl)
        } else {
            None
        }
    }
}

/// How an ONNX Runtime build reaches the graphics card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnnxCard {
    Cuda,
    DirectMl,
}

/// The words Parakeet hears in one track, each with the second it starts.
fn parakeet_transcribe(model: &mut parakeet_rs::ParakeetTDT, audio: &Path) -> Result<Vec<(f64, String)>> {
    let samples = crate::audio_pcm::decode_mono_16k(audio).with_context(|| format!("decode {} for recognition", audio.display()))?;
    let result = model
        .transcribe_samples(samples, 16_000, 1, Some(parakeet_rs::TimestampMode::Words))
        .map_err(|error| anyhow!("Parakeet transcription failed: {error}"))?;
    let words: Vec<(f64, String)> = result
        .tokens
        .into_iter()
        .filter_map(|token| {
            let text = token.text.trim().to_string();
            (!text.is_empty()).then_some((token.start as f64, text))
        })
        .collect();
    if words.is_empty() {
        bail!(NO_WORDS);
    }
    Ok(words)
}

/// A DXGI adapter tests pin DirectML to - the integrated Radeon beside an
/// NVIDIA card - where the studio takes the fastest card.
#[cfg(test)]
static DIRECTML_ADAPTER: std::sync::OnceLock<i32> = std::sync::OnceLock::new();

/// Parakeet's sessions on the card, or no configuration for the processor.
fn parakeet_config(card: Option<OnnxCard>) -> Option<parakeet_rs::ExecutionConfig> {
    card.map(|card| parakeet_rs::ExecutionConfig::new().with_custom_configure(move |builder| Ok(with_card(builder, Some(card)).0)))
}

/// Whether the system CUDA libraries the provider loads are present.
/// Display helper for the separation panel; on Windows these ship as
/// downloads instead (see `has_cuda_libraries`).
#[cfg(not(windows))]
pub fn system_cuda_libraries_present() -> bool {
    system_cuda_provider_libs()
}

/// Whether the system loader finds the CUDA libraries the provider needs:
/// cuBLAS, the CUDA runtime and cuDNN. `ldconfig -p` knows the system library
/// cache; `LD_LIBRARY_PATH` covers toolkit installs outside it.
#[cfg(not(windows))]
fn system_cuda_provider_libs() -> bool {
    const WANTED: [&str; 3] = ["libcublasLt.so", "libcudart.so", "libcudnn.so"];
    let ldconfig = std::process::Command::new("ldconfig")
        .arg("-p")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let path_dirs: Vec<PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    WANTED.iter().all(|name| system_lib_present(name, &ldconfig, &path_dirs))
}

/// One SONAME in the loader cache or beside a `LD_LIBRARY_PATH` entry: the
/// cache prints `libcublasLt.so.12 (...) => /path`, directories hold the
/// versioned file the `.so` name would resolve to.
#[cfg(not(windows))]
fn system_lib_present(name: &str, ldconfig: &str, path_dirs: &[PathBuf]) -> bool {
    if ldconfig.lines().any(|line| line.trim_start().starts_with(name)) {
        return true;
    }
    path_dirs.iter().any(|directory| {
        std::fs::read_dir(directory)
            .map(|entries| entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with(name)))
            .unwrap_or(false)
    })
}

/// Puts a session on the card a path reaches: CUDA, or DirectML with what it
/// requires - no memory pattern, one operator at a time - on the fastest card
/// DirectX reports. Says whether the card took it: a card that refuses runs
/// the work on the processor and the caller reports that, instead of it
/// quietly taking ten times as long.
pub fn with_card(builder: ort::session::builder::SessionBuilder, card: Option<OnnxCard>) -> (ort::session::builder::SessionBuilder, bool) {
    let attempt = match card {
        None => return (builder, false),
        Some(OnnxCard::Cuda) => builder.clone().with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()]),
        Some(OnnxCard::DirectMl) => {
            let provider = ort::ep::DirectML::default()
                .with_performance_preference(ort::ep::directml::PerformancePreference::HighPerformance)
                .with_device_filter(ort::ep::directml::DeviceFilter::Gpu);
            #[cfg(test)]
            let provider = match DIRECTML_ADAPTER.get() {
                Some(&adapter) => provider.with_device_id(adapter),
                None => provider,
            };
            builder
                .clone()
                .with_memory_pattern(false)
                .and_then(|builder| builder.with_parallel_execution(false))
                .and_then(|builder| builder.with_execution_providers([provider.build().error_on_failure()]))
        }
    };
    match attempt {
        Ok(on_card) => (on_card, true),
        Err(error) => {
            eprintln!("[ERROR] the {card:?} provider did not register, the processor runs this instead: {error}");
            (builder, false)
        }
    }
}

/// Points `ort` at one ONNX Runtime build. On Windows a DLL's own dependencies
/// are resolved through the process search path, not through the folder it
/// came from, so the folder joins PATH. The DirectML build needs DirectML 1.15,
/// and System32 carries an older one that the search order reaches first: ours
/// is loaded by its full path beforehand, and a loaded module is what every
/// later load of that name gets.
fn point_ort_at(runtime: &Path, card: Option<OnnxCard>) {
    unsafe { std::env::set_var("ORT_DYLIB_PATH", runtime) };
    let Some(directory) = runtime.parent() else { return };
    // On Linux the loader never consults PATH; the runtime directory joins
    // LD_LIBRARY_PATH so bundled libraries resolve their own neighbours.
    #[cfg(not(windows))]
    {
        // The card needs no preloading on Linux: CUDA resolves through the
        // system loader, and DirectML does not exist here.
        let _ = card;
        let existing = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        unsafe { std::env::set_var("LD_LIBRARY_PATH", format!("{}:{existing}", directory.display())) };
        return;
    }
    #[cfg(windows)]
    {
        let existing = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", format!("{};{existing}", directory.display())) };
        if card == Some(OnnxCard::DirectMl) {
            preload(&directory.join("DirectML.dll"));
        }
    }
}

#[cfg(windows)]
fn preload(library: &Path) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut std::ffi::c_void, flags: u32) -> *mut std::ffi::c_void;
    }
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
    let wide: Vec<u16> = library.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) }.is_null() {
        eprintln!("[ERROR] could not load {}: {}", library.display(), std::io::Error::last_os_error());
    }
}

/// Every Parakeet file, because the model is useless without all of them.
pub const PARAKEET_ASSET_IDS: [&str; 5] =
    ["parakeet-tdt-int8", "parakeet-decoder", "parakeet-features", "parakeet-vocab", "parakeet-config"];

/// The same recogniser at full precision. The encoder is a graph plus a
/// separate weights file; everything else is shared with the int8 set.
pub const PARAKEET_FP32_ASSET_IDS: [&str; 6] = [
    "parakeet-tdt-fp32",
    "parakeet-tdt-fp32-weights",
    "parakeet-decoder",
    "parakeet-features",
    "parakeet-vocab",
    "parakeet-config",
];

pub fn asset(id: &str) -> Option<&'static Asset> {
    let found = ASSETS.iter().find(|asset| asset.id == id)?;
    asset_available(found).then_some(found)
}

/// Whether an asset is offered on this machine at all. Windows-only runtimes
/// stay in the table for the Windows build but are hidden on Linux: DirectML
/// and its ONNX build, and NVIDIA's Windows redistributables (on Linux the
/// CUDA provider loads the system toolkit instead). Whisper is *not* among
/// them: the recogniser is whisper.cpp, which ships a Linux build for this
/// platform, so its runtime and its models are offered here. Upstream ships
/// no `linux-aarch64-gpu` build, so the CUDA runtime is hidden on ARM Linux.
pub fn asset_available(asset: &Asset) -> bool {
    #[cfg(windows)]
    {
        let _ = asset;
        return true;
    }
    #[cfg(not(windows))]
    {
        if matches!(asset.id, "onnxruntime-directml" | "directml" | "cuda-cublas" | "cuda-cudart" | "cuda-cufft" | "cuda-cudnn") {
            return false;
        }
        #[cfg(target_arch = "aarch64")]
        if asset.id == "onnxruntime-cuda" {
            return false;
        }
        return true;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub provider: AsrProvider,
    pub root: String,
    /// True when the selected provider can actually run right now.
    pub ready: bool,
    pub whisper_binary: Option<String>,
    pub whisper_model: Option<String>,
    /// Whether the speech detector is asked for before each run.
    pub whisper_vad: bool,
    /// And whether it is on disk: a switch that is on without it would run
    /// without it and say nothing.
    pub whisper_vad_ready: bool,
    pub openrouter_model: Option<String>,
    /// What the recogniser runs on, so the page that downloads it can show the
    /// same choice that decides which files it fetches.
    pub runtime: OnnxFlavour,
    pub installed_models: Vec<String>,
    pub assets: Vec<crate::downloads::AssetStatus>,
    pub active_download: Option<crate::downloads::DownloadProgress>,
}

pub struct LyricsSync {
    downloader: Downloader,
}

impl LyricsSync {
    pub fn new(data_root: &Path) -> Self {
        Self { downloader: Downloader::new(data_root.join("karaoke")) }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// whisper.cpp's own binary. The build that ships here runs on the
    /// processor; a card build is a cmake run of the same sources, dropped
    /// into the same folder under this name.
    pub fn whisper_binary(&self) -> Option<PathBuf> {
        crate::downloads::locate_binary(self.downloader.root(), &[WHISPER_RUNTIME_DIR], "whisper-cli")
    }

    /// Where the GGML weights live, one file per size.
    fn whisper_model_dir(&self) -> PathBuf {
        self.downloader.root().join("models").join("whisper")
    }

    /// The size a model id stands for - `whisper-large-v3` is `large-v3`, which
    /// is the name the binary is given.
    fn whisper_size(id: &str) -> Option<&str> {
        let size = id.strip_prefix("whisper-")?;
        WHISPER_SIZES.contains(&size).then_some(size)
    }

    pub fn installed_models(&self) -> Vec<&'static Asset> {
        ASSETS
            .iter()
            .filter(|asset| asset.kind == AssetKind::Model && self.downloader.is_installed(asset))
            .collect()
    }

    /// Parakeet needs every one of its files and the ONNX Runtime library.
    pub fn parakeet_ready(&self) -> bool {
        self.onnxruntime_library().is_some()
            && PARAKEET_ASSET_IDS
                .iter()
                .all(|id| asset(id).is_some_and(|asset| self.downloader.is_installed(asset)))
    }

    /// Whether the Whisper model the configuration names is actually on disk.
    /// A faster-whisper model is a directory, not a file - and
    /// `whisper_model_path` has already checked that every file inside it
    /// arrived. Asking whether that path is a file said no to a complete
    /// installation, which is how a finished download still refused to run.
    /// Where Silero's speech detector sits, for the times it is asked for.
    fn whisper_vad_path(&self) -> PathBuf {
        self.whisper_model_dir().join(VAD_MODEL_FILE)
    }

    /// The detector is a second download, so switching it on without it is a
    /// refusal rather than a run that quietly does without it.
    pub fn whisper_vad_ready(&self) -> bool {
        ASSETS
            .iter()
            .find(|asset| asset.id == WHISPER_VAD_ASSET)
            .is_some_and(|asset| self.downloader.is_installed(asset))
            && self.whisper_vad_path().is_file()
    }

    /// A GGML model is one file, so "ready" is that the file is there - not
    /// that a directory is.
    pub fn whisper_model_ready(&self, config: &LyricsSyncConfig) -> bool {
        self.whisper_model_path(config).is_some_and(|path| path.is_file())
    }

    pub fn parakeet_dir(&self) -> PathBuf {
        self.downloader.root().join("models").join("parakeet")
    }

    /// `ort` loads this at run time; linking it would tie the build to one
    /// toolchain and one machine's libraries.
    pub fn onnxruntime_library(&self) -> Option<PathBuf> {
        self.machine_runtime().map(|(library, _)| library)
    }

    /// The ONNX Runtime this process loads, and the card it reaches. A process
    /// binds one build on first use, so it is chosen by the machine, not by a
    /// setting: the build for this machine's card when every file of it is
    /// there - it carries the processor provider too - else the processor build.
    pub fn machine_runtime(&self) -> Option<(PathBuf, Option<OnnxCard>)> {
        let cuda = self.downloader.runtime_dir("onnx-cuda").join(ONNXRUNTIME_LIBRARY);
        let directml = self.downloader.runtime_dir("onnx-dml").join(ONNXRUNTIME_LIBRARY);
        let cpu = self.downloader.runtime_dir("onnx").join(ONNXRUNTIME_LIBRARY);
        match OnnxFlavour::Auto.card() {
            Some(OnnxCard::Cuda) if self.has_cuda_libraries() => return Some((cuda, Some(OnnxCard::Cuda))),
            Some(OnnxCard::DirectMl) if self.has_directml_libraries() => return Some((directml, Some(OnnxCard::DirectMl))),
            _ => {}
        }
        [cpu, cuda, directml].into_iter().find(|library| library.is_file()).map(|library| (library, None))
    }

    /// The DirectML build and the DirectML it is built for, side by side.
    pub fn has_directml_libraries(&self) -> bool {
        let dir = self.downloader.runtime_dir("onnx-dml");
        ["onnxruntime.dll", "onnxruntime_providers_shared.dll", "DirectML.dll"].iter().all(|name| dir.join(name).is_file())
    }

    /// Whether every library the card's provider needs is installed.
    pub fn has_card_libraries(&self, card: OnnxCard) -> bool {
        match card {
            OnnxCard::Cuda => self.has_cuda_libraries(),
            OnnxCard::DirectMl => self.has_directml_libraries(),
        }
    }

    /// Binds `ort` to this machine's runtime - once per process, before
    /// anything touches `ort`, or it binds to whatever `onnxruntime.dll` the
    /// system happens to have - and says which card the bound build reaches.
    /// None when no runtime is installed. Every piece of work on ONNX asks
    /// here first, so all of it runs on the one build that was bound.
    pub fn bind_ort(&self) -> Option<Option<OnnxCard>> {
        static BOUND: std::sync::OnceLock<Option<OnnxCard>> = std::sync::OnceLock::new();
        if let Some(card) = BOUND.get() {
            return Some(*card);
        }
        let (library, card) = self.machine_runtime()?;
        Some(*BOUND.get_or_init(|| {
            point_ort_at(&library, card);
            card
        }))
    }

    /// The card work set to `runtime` runs on, with the machine's runtime
    /// bound first; none is the processor.
    pub fn onnx_card(&self, runtime: OnnxFlavour) -> Result<Option<OnnxCard>> {
        let bound = self.bind_ort().ok_or_else(|| anyhow!("the ONNX Runtime library is not installed"))?;
        Ok(if matches!(runtime, OnnxFlavour::Cpu) { None } else { bound })
    }

    pub fn has_cuda_runtime(&self) -> bool {
        self.downloader.runtime_dir("onnx-cuda").join(ONNXRUNTIME_LIBRARY).is_file()
    }

    /// The CUDA provider is a separate library, and on Windows it in turn
    /// needs cuBLAS, the CUDA runtime and cuDNN beside it. Without all of them
    /// the provider refuses to load and the run silently lands on the
    /// processor. On Linux those come from the system CUDA 12 toolkit and
    /// cuDNN instead of a download, so presence is read off the loader.
    pub fn has_cuda_libraries(&self) -> bool {
        let dir = self.downloader.runtime_dir("onnx-cuda");
        if !dir.join(ONNXRUNTIME_CUDA_PROVIDER).is_file() {
            return false;
        }
        #[cfg(windows)]
        {
            ["cublasLt64_12.dll", "cudart64_12.dll", "cudnn64_9.dll"].iter().all(|name| dir.join(name).is_file())
        }
        #[cfg(not(windows))]
        {
            system_cuda_provider_libs()
        }
    }

    /// A model counts as present only with every one of its files: a directory
    /// missing its tokenizer loads exactly as far as an error message.
    fn whisper_model_path(&self, config: &LyricsSyncConfig) -> Option<PathBuf> {
        let size = Self::whisper_size(config.whisper_model.as_deref()?)?;
        let asset = ASSETS
            .iter()
            .find(|asset| asset.id == format!("whisper-{size}"))?;
        self.downloader.is_installed(asset).then(|| self.whisper_model_dir().join(format!("ggml-{size}.bin")))
    }

    pub async fn status(&self, config: &LyricsSyncConfig) -> SyncStatus {
        let whisper_binary = self.whisper_binary();
        let ready = match config.provider {
            AsrProvider::None => false,
            AsrProvider::Whisper => whisper_binary.is_some() && self.whisper_model_path(config).is_some(),
            AsrProvider::Parakeet => self.parakeet_ready(),
            AsrProvider::OpenRouter => config.openrouter_model.as_deref().is_some_and(|model| !model.trim().is_empty()),
        };
        SyncStatus {
            enabled: config.enabled,
            provider: config.provider,
            runtime: config.runtime,
            root: self.downloader.root().display().to_string(),
            ready: config.enabled && ready,
            whisper_binary: whisper_binary.map(|path| path.display().to_string()),
            whisper_model: config.whisper_model.clone(),
            whisper_vad: config.whisper_vad,
            whisper_vad_ready: self.whisper_vad_ready(),
            openrouter_model: config.openrouter_model.clone(),
            installed_models: self.installed_models().iter().map(|asset| asset.id.to_string()).collect(),
            // Only what this machine can run: Windows-only runtimes are
            // hidden on Linux rather than offered as broken downloads.
            assets: self.downloader.status_of(&ASSETS.iter().copied().filter(asset_available).collect::<Vec<_>>()),
            active_download: self.downloader.active_for("karaoke").await,
        }
    }

    /// Runs Parakeet in this process and returns the words it hears, each with
    /// the second it starts. Same stack Dub Studio uses: parakeet-rs over ONNX
    /// Runtime, loaded from the DLL beside the models rather than linked in.
    pub fn parakeet_words(&self, runtime: OnnxFlavour, audio: &Path) -> Result<Vec<(f64, String)>> {
        let mut model = self.load_parakeet(self.onnx_card(runtime)?)?;
        parakeet_transcribe(&mut model, audio)
    }

    /// Parakeet on the card, or on the processor when there is none.
    fn load_parakeet(&self, card: Option<OnnxCard>) -> Result<parakeet_rs::ParakeetTDT> {
        if !self.parakeet_ready() {
            bail!("the Parakeet model is not fully downloaded");
        }
        parakeet_rs::ParakeetTDT::from_pretrained(self.parakeet_dir(), parakeet_config(card)).map_err(|error| anyhow!("load Parakeet: {error}"))
    }

    /// The words of several tracks from one load of the recogniser: Parakeet
    /// loaded once and run over each, Whisper started once with every file.
    /// `heard` gets each track's answer, with its index, the moment it is
    /// there, so a long dataset shows its lyrics song by song.
    pub fn words_many(
        &self,
        config: &LyricsSyncConfig,
        audio: &[PathBuf],
        language: Option<&str>,
        heard: &mut dyn FnMut(usize, Result<Vec<(f64, String)>>),
        cancel: &AtomicBool,
    ) -> Recognised {
        let failed = |heard: &mut dyn FnMut(usize, Result<Vec<(f64, String)>>), error: anyhow::Error| {
            for index in 0..audio.len() {
                heard(index, Err(anyhow!("{error:#}")));
            }
        };
        match config.provider {
            AsrProvider::Parakeet => {
                let mut model = match self.onnx_card(config.runtime).and_then(|card| self.load_parakeet(card)) {
                    Ok(model) => model,
                    Err(error) => {
                        failed(heard, error);
                        return Recognised::OnDevice;
                    }
                };
                for (index, path) in audio.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        return Recognised::OnDevice;
                    }
                    heard(index, parakeet_transcribe(&mut model, path));
                }
                Recognised::OnDevice
            }
            AsrProvider::Whisper => match self.whisper_many(config, audio, language, heard, cancel) {
                Ok(recognised) => recognised,
                Err(error) => {
                    failed(heard, error);
                    Recognised::OnDevice
                }
            },
            _ => {
                failed(heard, anyhow!("this recogniser does not run on this computer"));
                Recognised::OnDevice
            }
        }
    }

    /// One Whisper run over several tracks; the JSON it writes for each is
    /// named after the file it was given, and handed on as soon as it is whole.
    fn whisper_many(
        &self,
        config: &LyricsSyncConfig,
        audio: &[PathBuf],
        language: Option<&str>,
        heard: &mut dyn FnMut(usize, Result<Vec<(f64, String)>>),
        cancel: &AtomicBool,
    ) -> Result<Recognised> {
        let binary = self.whisper_binary().ok_or_else(|| anyhow!("the Whisper runtime is not installed"))?;
        let size = config
            .whisper_model
            .as_deref()
            .and_then(Self::whisper_size)
            .ok_or_else(|| anyhow!("no Whisper model is downloaded and selected"))?;
        if self.whisper_model_path(config).is_none() {
            bail!("the Whisper model {size} is not completely downloaded");
        }
        let work = self.downloader.root().join("work").join(format!("batch-{}", uuid::Uuid::now_v7()));
        let out_dir = work.join("out");
        fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
        let mut wavs: Vec<(usize, PathBuf)> = Vec::with_capacity(audio.len());
        let mut answered = vec![false; audio.len()];
        for (index, path) in audio.iter().enumerate() {
            let wav = work.join(format!("{index}.wav"));
            match crate::audio_pcm::write_wav16k_mono(path, &wav) {
                Ok(()) => wavs.push((index, wav)),
                Err(error) => {
                    answered[index] = true;
                    heard(index, Err(error.context(format!("decode {} for recognition", path.display()))));
                }
            }
        }
        // A track's JSON is taken once it parses: Whisper writes it whole when
        // that track is done, and a half-written one is simply read next time
        let mut take = |answered: &mut Vec<bool>| {
            for index in 0..audio.len() {
                if answered[index] {
                    continue;
                }
                let Some(path) = wavs.iter().find(|(slot, _)| *slot == index).map(|(_, wav)| wav) else { continue };
                let Ok(text) = fs::read_to_string(whisper_json_beside(path)) else { continue };
                if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                    continue;
                }
                answered[index] = true;
                let words = whisper_words_from_json(&text);
                heard(index, if words.is_empty() { Err(anyhow!(NO_WORDS)) } else { Ok(words) });
            }
        };
        let on_card = config.runtime.uses_cuda();
        let vad = config.whisper_vad && self.whisper_vad_ready();
        let all: Vec<PathBuf> = wavs.iter().map(|(_, wav)| wav.clone()).collect();
        let mut outcome = self.run_whisper_many(&binary, size, &all, &out_dir, language, on_card, vad, &mut || take(&mut answered), cancel);
        let mut recognised = Recognised::OnDevice;
        if outcome.as_ref().is_err_and(|error| error.to_string() != "cancelled") && on_card {
            // the card refused: the processor takes only what is still unheard,
            // and the page is told it ran there
            take(&mut answered);
            let refused = outcome.unwrap_err();
            let left: Vec<PathBuf> = wavs.iter().filter(|(index, _)| !answered[*index]).map(|(_, wav)| wav.clone()).collect();
            outcome = if left.is_empty() {
                Ok(())
            } else {
                recognised = Recognised::OnProcessor;
                self.run_whisper_many(&binary, size, &left, &out_dir, language, false, vad, &mut || take(&mut answered), cancel)
                    .with_context(|| format!("the card was tried first and refused: {refused}"))
            };
        }
        take(&mut answered);
        let run_error = outcome.err().map(|error| format!("{error:#}"));
        for (index, done) in answered.iter().enumerate() {
            if !done {
                heard(index, Err(anyhow!("{}", run_error.clone().unwrap_or_else(|| "whisper wrote no JSON for this track".into()))));
            }
        }
        fs::remove_dir_all(&work).ok();
        Ok(recognised)
    }

    /// Runs faster-whisper over one track and returns the words it heard.
    ///
    /// Purfview's standalone build, asked for JSON with word timestamps - the
    /// same recogniser Dub Studio uses. It replaced whisper.cpp, which was
    /// asked for an LRC file: that file is written line by line, word times had
    /// to be guessed out of it, and when the run failed the binary exited zero
    /// and wrote nothing at all, so the only thing anyone ever saw was
    /// "whisper-cli produced no LRC file".
    pub fn whisper_words(&self, config: &LyricsSyncConfig, audio: &Path, language: Option<&str>, _lyrics: &str) -> Result<Vec<(f64, String)>> {
        let binary = self.whisper_binary().ok_or_else(|| anyhow!("the Whisper runtime is not installed"))?;
        let size = config
            .whisper_model
            .as_deref()
            .and_then(Self::whisper_size)
            .ok_or_else(|| anyhow!("no Whisper model is downloaded and selected"))?;
        if self.whisper_model_path(config).is_none() {
            bail!("the Whisper model {size} is not completely downloaded");
        }

        let work = self.downloader.root().join("work");
        fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
        let stem = work.join(format!("sync-{}", uuid::Uuid::now_v7()));
        let wav = stem.with_extension("wav");
        crate::audio_pcm::write_wav16k_mono(audio, &wav)
            .with_context(|| format!("decode {} for recognition", audio.display()))?;
        let out_dir = stem.with_extension("out");
        fs::remove_dir_all(&out_dir).ok();
        fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;

        let on_card = config.runtime.uses_cuda();
        // Asked for and there: a switch that is on without the detector on disk
        // would run without it and say nothing, which is the one thing a
        // switch must not do.
        let vad = config.whisper_vad && self.whisper_vad_ready();
        let mut outcome = self.run_whisper(&binary, size, &wav, &out_dir, language, on_card, vad);
        // CTranslate2 fails inside itself on a machine without usable CUDA, so
        // the card is tried and the processor is the answer to its refusal -
        // once, and only in that direction.
        if outcome.is_err() && on_card {
            let refused = outcome.unwrap_err();
            fs::remove_dir_all(&out_dir).ok();
            fs::create_dir_all(&out_dir).ok();
            outcome = self
                .run_whisper(&binary, size, &wav, &out_dir, language, false, vad)
                .with_context(|| format!("the card was tried first and refused: {refused}"));
        }
        fs::remove_file(&wav).ok();

        if let Err(error) = outcome {
            fs::remove_dir_all(&out_dir).ok();
            return Err(error);
        }
        let json = whisper_json_beside(&wav);
        if !json.is_file() {
            fs::remove_dir_all(&out_dir).ok();
            bail!("whisper wrote no JSON beside {}", wav.display());
        }
        let text = fs::read_to_string(&json).with_context(|| format!("read {}", json.display()))?;
        let words = whisper_words_from_json(&text);
        fs::remove_dir_all(&out_dir).ok();
        if words.is_empty() {
            bail!(NO_WORDS);
        }
        Ok(words)
    }

    /// One run of the recogniser, with its complaints kept: a failure here is
    /// the only place that ever says why nothing was recognised.
    fn run_whisper(&self, binary: &Path, size: &str, wav: &Path, out_dir: &Path, language: Option<&str>, on_card: bool, vad: bool) -> Result<()> {
        self.run_whisper_many(binary, size, &[wav.to_path_buf()], out_dir, language, on_card, vad, &mut || {}, &AtomicBool::new(false))
    }

    /// Whisper over every file in one process; `poll` is called while it
    /// works, to pick up what it has written, and `cancel` stops it between polls.
    #[allow(clippy::too_many_arguments)]
    fn run_whisper_many(
        &self,
        binary: &Path,
        size: &str,
        wavs: &[PathBuf],
        out_dir: &Path,
        language: Option<&str>,
        on_card: bool,
        vad: bool,
        poll: &mut dyn FnMut(),
        cancel: &AtomicBool,
    ) -> Result<()> {
        // The model is a file, not a name: `-m` takes the GGML weights.
        let model = self
            .whisper_model_dir()
            .join(format!("ggml-{size}.bin"));
        let mut command = Command::new(binary);
        command
            .args(wavs)
            .arg("-m")
            .arg(&model)
            .arg("-l")
            // A language it was told beats one it has to guess, and "auto" is
            // not a language code - passing it as one is how a run comes back
            // empty. `auto` here is whisper.cpp's own request to detect.
            .arg(language.map(str::trim).filter(|code| !code.is_empty()).unwrap_or("auto"))
            .arg("-ojf")
            // No `-of`: whisper.cpp then writes `<input>.json` beside each
            // input, one per track. Naming one output instead would transcribe
            // only the first of a batch and drop the rest without a word.
            .arg("-np")
            // Music is not speech, and the tokens Whisper spends on it are the
            // ones that put credit rolls and lyric sheets where nobody sang.
            // Without this a run over a choral passage returns one phrase
            // repeated until the tape runs out.
            .arg("-sns");
        if vad {
            // The detector gets its own window over the audio and tells the
            // decoder which windows hold speech. Without it a run over music
            // invents words to fill the gaps; with it, a recording that is not
            // speech comes back empty, which is the true answer.
            command.arg("--vad").arg("-vm").arg(self.whisper_vad_path());
        }
        if on_card {
            command.arg("-ng").arg("0");
        }
        command
            // The weights are on this disk; a recogniser that goes looking for
            // them on the network is one that fails without one.
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdin(Stdio::null());
        // Its output goes to a file: a pipe nobody reads while it works through
        // a whole dataset fills up and stalls it.
        let log_path = out_dir.with_extension("log");
        let log = fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
        command.stdout(log.try_clone()?).stderr(log);
        // CTranslate2 and the CUDA libraries sit beside the binary, and that is
        // where they are found from.
        if let Some(directory) = binary.parent() {
            command.current_dir(directory);
        }
        hide_console(&mut command);

        let mut child = command.spawn().with_context(|| format!("run {}", binary.display()))?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if cancel.load(Ordering::Relaxed) {
                child.kill().ok();
                child.wait().ok();
                bail!("cancelled");
            }
            poll();
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        let output = fs::read_to_string(&log_path).unwrap_or_default();
        fs::remove_file(&log_path).ok();
        if status.success() {
            return Ok(());
        }
        let mut tail: Vec<&str> = output.lines().filter(|line| !line.trim().is_empty()).rev().take(8).collect();
        tail.reverse();
        bail!("whisper exited with {}: {}", status, tail.join(" | "))
    }
}

/// Groups a word stream into karaoke lines: a new line at a noticeable pause,
/// at sentence-ending punctuation, or once a line grows past comfortable
/// reading length. Ported from the segmentation Dub Studio uses for subtitles.
pub fn group_words(words: &[(f64, String)]) -> Vec<(f64, String)> {
    const PAUSE: f64 = 0.6;
    const MAX_CHARS: usize = 42;

    let mut lines: Vec<(f64, String)> = Vec::new();
    let mut start = 0.0;
    let mut current = String::new();
    let mut previous: Option<f64> = None;

    for (at, word) in words {
        let pause = previous.is_some_and(|last| at - last > PAUSE);
        let too_long = current.chars().count() + word.chars().count() + 1 > MAX_CHARS;
        if !current.is_empty() && (pause || too_long) {
            lines.push((start, std::mem::take(&mut current)));
        }
        if current.is_empty() {
            start = *at;
        } else {
            current.push(' ');
        }
        current.push_str(word);
        if word.ends_with(['.', '!', '?', '…']) {
            lines.push((start, std::mem::take(&mut current)));
        }
        previous = Some(*at);
    }
    if !current.is_empty() {
        lines.push((start, current));
    }
    merge_flashing_lines(lines)
}

/// ACE Step Studio merged LRC lines that begin less than two seconds apart so
/// they do not flash past unread. Karaoke here follows the same rule.
fn merge_flashing_lines(lines: Vec<(f64, String)>) -> Vec<(f64, String)> {
    const MIN_DISPLAY_SECONDS: f64 = 2.0;
    let mut merged: Vec<(f64, String)> = Vec::with_capacity(lines.len());
    for (start, text) in lines {
        match merged.last_mut() {
            Some((previous_start, previous_text)) if start - *previous_start < MIN_DISPLAY_SECONDS => {
                previous_text.push(' ');
                previous_text.push_str(&text);
            }
            _ => merged.push((start, text)),
        }
    }
    merged
}

/// The timed segments in an OpenAI-compatible verbose transcription. A model
/// that answered with plain text yields nothing, and the caller reports that
/// rather than inventing timings.
pub fn segments_from_verbose_json(body: &serde_json::Value) -> Vec<(f64, String)> {
    let mut words = Vec::new();
    if let Some(list) = body.get("words").and_then(|value| value.as_array()) {
        for entry in list {
            let (Some(start), Some(text)) = (entry.get("start").and_then(serde_json::Value::as_f64), entry.get("word").or_else(|| entry.get("text")).and_then(serde_json::Value::as_str)) else {
                continue;
            };
            words.push((start, text.trim().to_string()));
        }
    }
    if !words.is_empty() {
        return group_words(&words);
    }
    let mut segments = Vec::new();
    if let Some(list) = body.get("segments").and_then(|value| value.as_array()) {
        for entry in list {
            let (Some(start), Some(text)) = (entry.get("start").and_then(serde_json::Value::as_f64), entry.get("text").and_then(serde_json::Value::as_str)) else {
                continue;
            };
            segments.push((start, text.trim().to_string()));
        }
    }
    segments
}

/// One written line, with a time for every word in it.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedLine {
    pub start: f64,
    /// The words of the written line, each with the moment it is sung.
    pub words: Vec<(f64, String)>,
}

/// Enhanced LRC - the A2 format every karaoke player understands: a line time
/// followed by a time before each word. Without per-word times a player has
/// nothing to do but sweep the highlight linearly, which drifts away from the
/// singing within a line.
pub fn enhanced_lrc(lines: &[TimedLine]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.words.is_empty() {
            continue;
        }
        out.push_str(&format!("[{}]", stamp(line.start)));
        for (at, word) in &line.words {
            out.push_str(&format!("<{}>{} ", stamp(*at), word));
        }
        out.pop();
        out.push('\n');
    }
    out
}

fn stamp(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let minutes = (total as u64) / 60;
    let rest = total - (minutes * 60) as f64;
    format!("{minutes:02}:{rest:05.2}")
}

/// Puts the track's own lyrics on the recogniser's clock, word by word.
///
/// The line anchoring is the same as before; inside a line each written word
/// takes the time of the recognised word it matches, and words nobody matched
/// are spread across the gap in proportion to their length. That is what makes
/// a karaoke highlight land on the syllable instead of drifting through it.
pub fn align_lyrics_words(words: &[(f64, String)], lyrics: &str) -> Vec<TimedLine> {
    let anchored = align_lyrics(words, lyrics);
    if anchored.is_empty() {
        return Vec::new();
    }
    let heard: Vec<(f64, String)> = words.iter().map(|(at, word)| (*at, normalise(word))).collect();
    let mut timed: Vec<TimedLine> = Vec::with_capacity(anchored.len());

    for (index, (start, text)) in anchored.iter().enumerate() {
        let end = anchored.get(index + 1).map(|(next, _)| *next).unwrap_or_else(|| {
            // The last line runs to the last thing anyone heard.
            heard.last().map(|(at, _)| *at + 2.0).unwrap_or(start + 4.0)
        });
        let written: Vec<&str> = text.split_whitespace().collect();
        if written.is_empty() {
            continue;
        }

        // The recognised words that fall inside this line's span.
        let inside: Vec<&(f64, String)> = heard.iter().filter(|(at, _)| *at >= *start - 0.01 && *at < end).collect();
        let mut placed: Vec<Option<f64>> = vec![None; written.len()];
        let mut cursor = 0usize;
        for (position, word) in written.iter().enumerate() {
            let expected = normalise(word);
            if expected.is_empty() {
                continue;
            }
            if let Some(found) = inside[cursor..].iter().position(|(_, heard_word)| {
                // Compare by characters, never by bytes: slicing "неон" at a
                // byte index lands inside a letter and panics.
                heard_word == &expected || (expected.chars().count() > 3 && heard_word.starts_with(&stem(&expected)))
            }) {
                placed[position] = Some(inside[cursor + found].0);
                cursor += found + 1;
            }
        }

        // Anything unmatched is spread by length across the gap it sits in.
        let mut previous_time = *start;
        let mut position = 0usize;
        while position < written.len() {
            if let Some(at) = placed[position] {
                previous_time = at;
                position += 1;
                continue;
            }
            let gap_start = position;
            while position < written.len() && placed[position].is_none() {
                position += 1;
            }
            let next_time = placed.get(position).copied().flatten().unwrap_or(end);
            let span = (next_time - previous_time).max(0.05);
            let weight: usize = written[gap_start..position].iter().map(|word| word.chars().count().max(1)).sum();
            let mut used = 0usize;
            for offset in gap_start..position {
                let length = written[offset].chars().count().max(1);
                // Centred in its own share of the gap: a word placed exactly on
                // the previous word's time would highlight two words at once.
                let share = (used as f64 + length as f64 / 2.0) / weight as f64;
                placed[offset] = Some(previous_time + span * share);
                used += length;
            }
            previous_time = next_time;
        }

        timed.push(TimedLine {
            start: *start,
            words: written
                .iter()
                .zip(placed)
                .map(|(word, at)| (at.unwrap_or(*start), (*word).to_string()))
                .collect(),
        });
    }
    timed
}

/// Puts the track's own lyrics on the recogniser's clock.
///
/// The words are already known - the model sang what it was given - so the
/// recogniser is used for *timing only*, which is what every karaoke aligner
/// worth the name does. Its text is mistrusted: sung vocals are mis-heard
/// constantly, and printing that back as lyrics is how karaoke ends up
/// showing nonsense. All lines are placed at once by a monotonic alignment
/// that maximises how well they match overall: a line matched greedily to a
/// later repeat of itself would strand every line sung in between, so a line
/// that was not heard clearly is left out and filled in between its
/// neighbours instead. A repeated chorus consumes its occurrences in order.
pub fn align_lyrics(words: &[(f64, String)], lyrics: &str) -> Vec<(f64, String)> {
    let lines: Vec<&str> = lyrics
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !(line.starts_with('[') && line.ends_with(']')))
        .collect();
    if lines.is_empty() || words.is_empty() {
        return Vec::new();
    }

    let heard: Vec<(f64, String)> = words.iter().map(|(at, word)| (*at, normalise(word))).collect();
    let count = heard.len();
    // How well each line matches when it starts at each recognised word; below
    // the threshold a start is not a candidate at all.
    let spans: Vec<usize> = lines
        .iter()
        .map(|line| line.split_whitespace().filter(|word| !normalise(word).is_empty()).count())
        .collect();
    let scores: Vec<Vec<f64>> = lines
        .iter()
        .zip(&spans)
        .map(|(line, &span)| {
            let written: String = line.split_whitespace().map(normalise).collect();
            (0..count)
                .map(|start| {
                    if span == 0 {
                        return 0.0;
                    }
                    // A little slack in length, because the recogniser splits
                    // words differently from the page; each window is scored by
                    // what it misses and what it adds, so starting a word early
                    // costs as much as starting a word late.
                    let score = (span.saturating_sub(1).max(1)..=span + 2)
                        .filter(|length| start + length <= count)
                        .map(|length| {
                            let spoken: String = heard[start..start + length].iter().map(|(_, word)| word.as_str()).collect();
                            dice(&written, &spoken)
                        })
                        .fold(0.0, f64::max);
                    if score >= 0.45 { score } else { 0.0 }
                })
                .collect()
        })
        .collect();

    // best[p]: the best (total, sum of starts) so far with the next line free
    // to start at word p or later. Each line is either placed at some start
    // >= p, which moves p past it, or left out. A clearly recognised line
    // counts the same wherever it is heard best, and between equal totals the
    // earlier placement wins: a chorus the model sang twice in a row shows its
    // lines as they are first sung, not where the recogniser heard them best.
    let credit = |score: f64| if score >= 0.7 { 1.0 } else { score };
    let better = |left: (f64, usize), right: (f64, usize)| left.0 > right.0 + 1e-9 || ((left.0 - right.0).abs() <= 1e-9 && left.1 < right.1);
    let mut best: Vec<Option<(f64, usize)>> = vec![None; count + 1];
    best[0] = Some((0.0, 0));
    let mut back: Vec<Vec<(usize, Option<usize>)>> = Vec::with_capacity(lines.len());
    for (index, &span) in spans.iter().enumerate() {
        let mut next: Vec<Option<(f64, usize)>> = vec![None; count + 1];
        let mut choice = vec![(0usize, None); count + 1];
        for from in 0..=count {
            let Some(so_far) = best[from] else { continue };
            if next[from].is_none_or(|kept| better(so_far, kept)) {
                next[from] = Some(so_far);
                choice[from] = (from, None);
            }
            if span == 0 {
                continue;
            }
            for start in from..count {
                let score = scores[index][start];
                if score <= 0.0 {
                    continue;
                }
                let after = (start + span).min(count);
                let total = (so_far.0 + credit(score), so_far.1 + start);
                if next[after].is_none_or(|kept| better(total, kept)) {
                    next[after] = Some(total);
                    choice[after] = (from, Some(start));
                }
            }
        }
        best = next;
        back.push(choice);
    }

    let mut starts: Vec<Option<usize>> = vec![None; lines.len()];
    let mut position = (0..=count).fold(0, |kept, candidate| match (best[candidate], best[kept]) {
        (Some(offered), Some(held)) if better(offered, held) => candidate,
        (Some(_), None) => candidate,
        _ => kept,
    });
    for index in (0..lines.len()).rev() {
        let (from, start) = back[index][position];
        starts[index] = start;
        position = from;
    }

    // A clearly recognised line was credited in full, so its start may sit a
    // word early; settle it where it matches best, between its neighbours.
    for index in 0..lines.len() {
        let Some(start) = starts[index] else { continue };
        let floor = starts[..index].iter().rev().flatten().next().map(|previous| previous + 1).unwrap_or(0);
        let ceiling = starts[index + 1..].iter().flatten().next().copied().unwrap_or(count);
        let low = start.saturating_sub(2).max(floor);
        let high = (start + 2).min(ceiling.saturating_sub(1)).min(count.saturating_sub(1));
        let settled = (low..=high).fold(start, |kept, candidate| if scores[index][candidate] > scores[index][kept] { candidate } else { kept });
        starts[index] = Some(settled);
    }
    let mut placed: Vec<Option<f64>> = starts.iter().map(|start| start.map(|start| heard[start].0)).collect();

    interpolate(&lines, &mut placed, heard.first().map(|(at, _)| *at).unwrap_or(0.0), heard.last().map(|(at, _)| *at).unwrap_or(0.0));
    lines
        .iter()
        .zip(placed)
        .filter_map(|(line, at)| at.map(|at| (at, (*line).to_string())))
        .collect()
}

/// Lines the recogniser could not place are spread evenly between the ones it
/// could, so a karaoke file has no silent holes.
fn interpolate(lines: &[&str], placed: &mut [Option<f64>], first: f64, last: f64) {
    let mut index = 0;
    while index < lines.len() {
        if placed[index].is_some() {
            index += 1;
            continue;
        }
        let gap_start = index;
        while index < lines.len() && placed[index].is_none() {
            index += 1;
        }
        let before = gap_start.checked_sub(1).and_then(|previous| placed[previous]).unwrap_or(first);
        let after = placed.get(index).copied().flatten().unwrap_or(last.max(before));
        let steps = (index - gap_start + 1) as f64;
        for (offset, slot) in placed[gap_start..index].iter_mut().enumerate() {
            *slot = Some(before + (after - before) * ((offset + 1) as f64 / steps));
        }
    }
}

/// A word without its last character, for tolerating inflected endings.
fn stem(word: &str) -> String {
    let count = word.chars().count();
    word.chars().take(count.saturating_sub(1)).collect()
}

fn normalise(word: &str) -> String {
    word.chars().filter(|character| character.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The characters two strings share in order, over both their lengths: 1 for
/// the same text, lower for what either one misses or adds.
fn dice(expected: &str, heard: &str) -> f64 {
    let (left, right) = (expected.chars().count(), heard.chars().count());
    if left + right == 0 {
        return 0.0;
    }
    2.0 * similarity(expected, heard) * left as f64 / (left + right) as f64
}

/// How much of the written line the recogniser heard, compared character by
/// character rather than word by word.
///
/// Sung vocals come back mangled - "on the glass" as "arms the grass" - and a
/// word-level comparison scores that as a miss even though the line is plainly
/// the right one. Comparing characters in order tolerates the mangling, which
/// is what character-level aligners do for exactly this reason.
fn similarity(expected: &str, heard: &str) -> f64 {
    if expected.is_empty() || heard.is_empty() {
        return 0.0;
    }
    let left: Vec<char> = expected.chars().collect();
    let right: Vec<char> = heard.chars().collect();
    let mut previous = vec![0usize; right.len() + 1];
    let mut current = vec![0usize; right.len() + 1];
    for l in 0..left.len() {
        for r in 0..right.len() {
            current[r + 1] = if left[l] == right[r] { previous[r] + 1 } else { current[r].max(previous[r + 1]) };
        }
        std::mem::swap(&mut previous, &mut current);
        current.iter_mut().for_each(|value| *value = 0);
    }
    previous[right.len()] as f64 / left.len() as f64
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// whisper.cpp hands back byte-pair pieces, not words. A piece that opens
    /// with a space starts a word, the rest continue it, and the markers it
    /// puts in (`[_BEG_]`, timestamps) are not words at all.
    #[test]
    fn whisper_token_pieces_become_words_at_the_time_they_opened() {
        let json = r#"{
            "transcription": [
                {"offsets": {"from": 0, "to": 5000},
                 "text": " שלום עולם",
                 "tokens": [
                    {"text": "[_BEG_]", "offsets": {"from": 0, "to": 0}},
                    {"text": " ש",    "offsets": {"from": 80, "to": 630}},
                    {"text": "ל",     "offsets": {"from": 630, "to": 1200}},
                    {"text": "ום",    "offsets": {"from": 1200, "to": 2000}},
                    {"text": " ע",    "offsets": {"from": 2100, "to": 3000}},
                    {"text": "ו",     "offsets": {"from": 3000, "to": 3600}},
                    {"text": "לם",    "offsets": {"from": 3600, "to": 4400}}
                 ]}
            ]
        }"#;
        assert_eq!(
            whisper_words_from_json(json),
            vec![(0.08, "שלום".to_string()), (2.1, "עולם".to_string())],
            "two words, each with the time of the token that opened it"
        );
    }

    /// A segment with no token list still has its text, and dropping it would
    /// lose a line rather than place it roughly.
    #[test]
    fn a_segment_without_tokens_still_yields_its_line() {
        let json = r#"{"transcription": [
            {"offsets": {"from": 2000, "to": 4000}, "text": " [_BEG_] only text"}
        ]}"#;
        assert_eq!(whisper_words_from_json(json), vec![(2.0, "only text".to_string())]);
    }

    /// The format this reader is for is whisper.cpp's. It must not read
    /// faster-whisper's `segments` array as if it were the same thing and hand
    /// back an empty transcript without saying so.
    #[test]
    /// The switch is asked for and the detector is on disk. Anything else - on
    /// without it, or off - must run without it, because a switch that looks
    /// like it is doing something and is not is worse than no switch.
    #[test]
    fn the_speech_detector_is_only_asked_for_when_it_is_there() {
        let root = std::env::temp_dir().join(format!("vad-{}", uuid::Uuid::now_v7()));
        let sync = LyricsSync::new(&root);
        let config = LyricsSyncConfig {
            enabled: true,
            provider: AsrProvider::Whisper,
            whisper_model: Some("whisper-base".into()),
            whisper_vad: true,
            ..Default::default()
        };
        assert!(!sync.whisper_vad_ready(), "nothing is downloaded yet");
        // The detector is a plain file, so readiness is the file being there.
        fs::create_dir_all(sync.whisper_model_dir()).expect("model folder");
        fs::write(sync.whisper_vad_path(), b"not really a model").expect("detector");
        assert!(sync.whisper_vad_ready(), "the detector is on disk");
        let _ = config;
        fs::remove_dir_all(&root).ok();
    }

    /// Where whisper.cpp's transcript lands: beside the file it belongs to,
    /// named after it. This is the whole reason the batch path reads here - one
    /// `-of` for several inputs would transcribe only the first.
    #[test]
    fn a_transcript_sits_beside_its_audio_and_is_named_after_it() {
        assert_eq!(
            whisper_json_beside(Path::new("/tmp/work/3.wav")),
            Path::new("/tmp/work/3.wav.json")
        );
    }

    fn the_other_json_shape_reads_as_nothing_rather_than_guessing() {
        assert!(whisper_words_from_json(r#"{"segments": [{"text": "hello", "start": 0.0}]}"#).is_empty());
    }

    fn whisper_fillers_are_dropped_and_sung_lines_kept() {
        assert!(is_hallucination(" Субтитры создавал DimaTorzok"));
        assert!(is_hallucination("Продолжение следует..."));
        assert!(is_hallucination("Thanks for watching!"));
        assert!(is_hallucination("ご視聴ありがとうございました"));
        assert!(is_hallucination("ВЕСЕЛАЯ МУЗЫКА"));
        assert!(is_hallucination("[Music]"));
        assert!(is_hallucination(" 1."));
        assert!(!is_hallucination("Если б мне платили каждый раз,"));
        assert!(!is_hallucination("Спасибо, что ты рядом со мной"));
        assert!(!is_hallucination("Поехали!"));
        // whisper.cpp's shape: `transcription`, and tokens rather than words.
        let json = r#"{"transcription":[
            {"offsets":{"from":1000,"to":4000},"text":" Субтитры создавал DimaTorzok",
             "tokens":[{"text":"[_BEG_]","offsets":{"from":0,"to":0}},{"text":" Субтитры","offsets":{"from":1000,"to":4000}}]},
            {"offsets":{"from":5000,"to":5600},"text":" Тьма во мне",
             "tokens":[{"text":"[_BEG_]","offsets":{"from":0,"to":0}},{"text":" Тьма","offsets":{"from":5000,"to":5400}},{"text":" во","offsets":{"from":5400,"to":5500}},{"text":" мне","offsets":{"from":5500,"to":5600}}]}]}"#;
        let words = whisper_words_from_json(json);
        assert_eq!(words.iter().map(|(_, word)| word.as_str()).collect::<Vec<_>>(), ["Тьма", "во", "мне"]);
        assert_eq!(words[0].0, 5.0, "a word keeps the time of the token that opened it");
    }

    // The releases every runtime download is pinned to. Whisper is pinned to
    // the build id upstream names that release, which is not a semver: the tag
    // `v1.9.5` carries assets labelled `b5454`, and pinning to the tag would
    // 404. The cublas and cudnn pins are gone with the Windows bundle that
    // wanted them.
    const WHISPER_BUILD: &str = "whisper.cpp/releases/download/b5454";
    const CUBLAS_BUILD: &str = "12.9.2.10";
    const CUDART_BUILD: &str = "12.9.79";
    const CUFFT_BUILD: &str = "11.4.1.4";
    const CUDNN_BUILD: &str = "9.25.0.15";
    const ONNXRUNTIME_BUILD: &str = "v1.30.0";
    const ONNXRUNTIME_DIRECTML_BUILD: &str = "onnxruntime.directml/1.24.4/";
    const DIRECTML_BUILD: &str = "ai.directml/1.15.4/";

    impl TimedLine {
        fn text(&self) -> String {
            self.words.iter().map(|(_, word)| word.as_str()).collect::<Vec<_>>().join(" ")
        }
    }

    #[test]
    fn words_become_lines_at_pauses_and_never_flash_past() {
        let words = vec![
            (0.0, "neon".into()),
            (0.4, "on".into()),
            (0.8, "the".into()),
            (1.2, "glass".into()),
            // A long pause would start a new line, but it lands inside the two
            // second window ACE used, so it joins the line before it.
            (2.9, "driving".into()),
            (3.3, "home".into()),
            (12.0, "the".into()),
            (12.4, "engine".into()),
            (12.9, "dies".into()),
        ];
        let lines = group_words(&words);
        assert_eq!(
            lines,
            vec![
                (0.0, "neon on the glass".to_string()),
                (2.9, "driving home".to_string()),
                (12.0, "the engine dies".to_string()),
            ]
        );
    }

    #[test]
    fn a_verbose_transcription_yields_timed_lines_and_plain_text_yields_none() {
        let verbose = serde_json::json!({
            "text": "neon on the glass",
            "segments": [{ "start": 1.5, "text": " neon on the glass" }, { "start": 9.0, "text": "driving home" }]
        });
        assert_eq!(
            segments_from_verbose_json(&verbose),
            vec![(1.5, "neon on the glass".to_string()), (9.0, "driving home".to_string())]
        );
        assert!(segments_from_verbose_json(&serde_json::json!({ "text": "no timings here" })).is_empty());
    }

    #[test]
    fn the_written_lyrics_are_kept_and_only_the_timing_is_borrowed() {
        // What the recogniser heard: two words wrong, as sung vocals go.
        let heard = vec![
            (1.0, "neon".into()),
            (1.4, "arms".into()),   // "on" misheard
            (1.8, "the".into()),
            (2.2, "grass".into()),  // "glass" misheard
            (9.0, "driving".into()),
            (9.6, "home".into()),
        ];
        let lyrics = "[verse]
Neon on the glass
Driving home
";
        let lines = align_lyrics(&heard, lyrics);

        // The user's words, on the recogniser's clock - never the mishearing.
        assert_eq!(lines, vec![(1.0, "Neon on the glass".to_string()), (9.0, "Driving home".to_string())]);
    }

    #[test]
    fn every_word_gets_its_own_time_and_the_file_says_so() {
        let heard = vec![
            (1.0, "neon".into()),
            (1.4, "arms".into()),
            (1.8, "the".into()),
            (2.2, "glass".into()),
            (9.0, "driving".into()),
            (9.6, "home".into()),
        ];
        let lines = align_lyrics_words(&heard, "Neon on the glass
Driving home");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "Neon on the glass");
        // "Neon", "the" and "glass" were heard; "on" was not, so it lands
        // between the words either side of it rather than on the line start.
        assert_eq!(lines[0].words[0].0, 1.0);
        assert_eq!(lines[0].words[2].0, 1.8);
        assert_eq!(lines[0].words[3].0, 2.2);
        assert!(lines[0].words[1].0 > 1.0 && lines[0].words[1].0 < 1.8, "got {}", lines[0].words[1].0);

        let lrc = enhanced_lrc(&lines);
        assert!(lrc.starts_with("[00:01.00]<00:01.00>Neon <"), "{lrc}");
        assert!(lrc.contains("<00:09.00>Driving <00:09.60>home"), "{lrc}");
    }

    #[test]
    fn russian_lyrics_align_without_slicing_a_letter_in_half() {
        let heard = vec![
            (1.0, "неон".into()),
            (1.5, "дрожит".into()),
            (2.0, "на".into()),
            (2.4, "коже".into()),
        ];
        let lines = align_lyrics_words(&heard, "Неон дрожит на мокрой коже");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 5);
        assert_eq!(lines[0].words[0].0, 1.0);
        assert_eq!(lines[0].words[1].0, 1.5);
        assert!(enhanced_lrc(&lines).contains("<00:01.50>дрожит"));
    }

    #[test]
    fn a_line_nobody_could_place_is_filled_in_between_its_neighbours() {
        let heard = vec![(0.0, "first".into()), (10.0, "third".into())];
        let lines = align_lyrics(&heard, "First
Something entirely unheard
Third");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].0, 0.0);
        assert!(lines[1].0 > 0.0 && lines[1].0 < 10.0, "the middle line got {}", lines[1].0);
        assert_eq!(lines[2].0, 10.0);
    }

    #[test]
    fn the_whisper_runtime_is_pinned_and_every_asset_is_distinct() {
        for entry in ASSETS {
            assert!(entry.bytes > 0, "{} has no size", entry.id);
            assert!(entry.url.starts_with("https://"), "{} is not fetched over https", entry.id);
            assert_eq!(ASSETS.iter().filter(|other| other.id == entry.id).count(), 1);
            if entry.kind == AssetKind::Runtime {
                // Every runtime is pinned to an exact release - whisper.cpp to
                // its tag, ONNX Runtime to its version - so a working setup
                // keeps working.
                // NVIDIA's libraries are pinned by their own version in the
                // archive name, the same way the others are.
                let pinned = entry.url.contains(WHISPER_BUILD)
                    || entry.url.contains(ONNXRUNTIME_BUILD)
                    || entry.url.contains(ONNXRUNTIME_DIRECTML_BUILD)
                    || entry.url.contains(DIRECTML_BUILD)
                    || entry.url.contains(CUBLAS_BUILD)
                    || entry.url.contains(CUDART_BUILD)
                    || entry.url.contains(CUDNN_BUILD)
                    || entry.url.contains(CUFFT_BUILD);
                assert!(pinned, "{} is not pinned to a release", entry.id);
                assert!(!entry.marker.is_empty(), "{} has no proof of extraction", entry.id);
            }
        }
    }

    #[test]
    fn karaoke_stays_off_until_a_provider_is_chosen() {
        let mut config = LyricsSyncConfig::default();
        assert!(!config.available());
        config.enabled = true;
        assert!(!config.available());
        config.provider = AsrProvider::Whisper;
        assert!(config.available());
    }

    /// Windows-only downloads are hidden on Linux rather than offered as
    /// broken buttons: Whisper's standalone runtime with every one of its
    /// models, the DirectML pair, and NVIDIA's Windows redistributables (the
    /// CUDA provider reads the system toolkit there). Parakeet and both ONNX
    /// builds stay, except the CUDA build on ARM, which upstream does not
    /// publish for Linux.
    #[test]
    #[cfg(not(windows))]
    fn windows_only_runtimes_are_hidden_on_linux() {
        for id in [
            "onnxruntime-directml",
            "directml",
            "cuda-cublas",
            "cuda-cudart",
            "cuda-cufft",
            "cuda-cudnn",
        ] {
            assert!(asset(id).is_none(), "{id} is offered on Linux");
        }
        // Whisper used to be on that list with its two CUDA libraries, because
        // the runtime was a Windows build. It is whisper.cpp now, so it is
        // offered here - and the libraries it used to need are gone with it.
        for id in ["whisper-engine", "whisper-tiny", "whisper-large-v3-turbo"] {
            assert!(asset(id).is_some(), "{id} is hidden on Linux");
        }
        for id in ["whisper-cublas", "whisper-cudnn"] {
            assert!(!ASSETS.iter().any(|asset| asset.id == id), "{id} is still in the table");
        }
        for id in ["onnxruntime", "parakeet-tdt-int8", "parakeet-decoder", "parakeet-config"] {
            assert!(asset(id).is_some(), "{id} is hidden on Linux");
        }
        #[cfg(target_arch = "x86_64")]
        assert!(asset("onnxruntime-cuda").is_some());
        #[cfg(target_arch = "aarch64")]
        assert!(asset("onnxruntime-cuda").is_none());
    }

    /// The Linux runtime assets name tarballs holding the file the loader
    /// binds, not the Windows zips.
    #[test]
    #[cfg(not(windows))]
    fn linux_runtimes_are_tarballs_with_so_markers() {
        let cpu = asset("onnxruntime").expect("the CPU runtime");
        assert!(cpu.url.ends_with(".tgz"), "{}", cpu.url);
        assert!(cpu.relative_path.ends_with(".tgz"), "{}", cpu.relative_path);
        assert_eq!(cpu.marker, "libonnxruntime.so");
        #[cfg(target_arch = "x86_64")]
        {
            let cuda = asset("onnxruntime-cuda").expect("the CUDA runtime");
            assert!(cuda.url.ends_with(".tgz"), "{}", cuda.url);
            assert_eq!(cuda.marker, "libonnxruntime_providers_cuda.so");
        }
    }

    /// `ldconfig -p` lines (`name ... => path`) and versioned files beside
    /// `LD_LIBRARY_PATH` entries both count; anything else does not.
    #[test]
    #[cfg(not(windows))]
    fn the_loader_cache_and_the_library_path_are_both_read() {
        let cache = "823 libs found.\n\tlibcublasLt.so.12 (libc6,x86-64) => /lib/x86_64-linux-gnu/libcublasLt.so.12\n\tlibz.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libz.so.1\n";
        assert!(system_lib_present("libcublasLt.so", cache, &[]));
        assert!(!system_lib_present("libcudnn.so", cache, &[]));
        let dir = std::env::temp_dir().join(format!("ld-path-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libcudnn.so.9.25.0"), b"x").unwrap();
        assert!(system_lib_present("libcudnn.so", cache, &[dir.clone()]));
        assert!(!system_lib_present("libcudart.so", cache, &[dir.clone()]));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod live_recognition {
    use super::*;

    /// The whole recognition path against a real installation, when one is
    /// pointed at: decode, run the recogniser, read its JSON, get words with
    /// times. Checking the download and the binary separately is what let a
    /// finished installation still refuse to run.
    #[test]
    fn recognising_a_real_track_end_to_end() {
        let (Some(root), Some(track)) = (std::env::var_os("YUE_DATA_ROOT"), std::env::var_os("YUE_TEST_TRACK")) else { return };
        let sync = LyricsSync::new(std::path::Path::new(&root));
        let config = LyricsSyncConfig {
            enabled: true,
            provider: AsrProvider::Whisper,
            whisper_model: Some("whisper-large-v3".into()),
            runtime: OnnxFlavour::Cuda,
            ..Default::default()
        };
        assert!(sync.whisper_binary().is_some(), "the recogniser is not installed");
        assert!(sync.whisper_model_ready(&config), "the model is not considered ready");
        let words = sync
            .whisper_words(&config, std::path::Path::new(&track), Some("ru"), "")
            .expect("recognition");
        eprintln!("words: {}", words.len());
        for (at, word) in words.iter().take(8) {
            eprintln!("  {at:.2}s {word}");
        }
        assert!(!words.is_empty(), "nothing was recognised");
    }

    /// A chorus sung twice, heard worse the first time, and one of its lines
    /// not heard at all: nothing may jump to the second chorus and strand the
    /// verse sung in between.
    #[test]
    fn a_line_heard_better_later_does_not_strand_the_verse_before_it() {
        let heard = |from: f64, text: &str| -> Vec<(f64, String)> {
            text.split_whitespace().enumerate().map(|(i, w)| (from + i as f64 * 0.5, w.to_string())).collect()
        };
        let mut words = Vec::new();
        words.extend(heard(1.0, "walking down the road tonight"));
        words.extend(heard(10.0, "my truck talks back"));
        words.extend(heard(13.0, "fast down the dusty track"));
        words.extend(heard(16.0, "ive been part of it"));
        words.extend(heard(20.0, "took my girl out on a friday"));
        words.extend(heard(30.0, "my truck talks back"));
        words.extend(heard(33.0, "down the dusty track"));
        words.extend(heard(36.0, "feeling right tonight"));
        let lyrics = "[Verse 1]\nWalking down the road tonight\n\n[Chorus]\nMy truck talks back\nDown the dusty track\nFeeling right tonight\n\n[Verse 2]\nTook my girl out on a Friday\n\n[Chorus]\nMy truck talks back\nDown the dusty track\nFeeling right tonight";
        let lines = align_lyrics(&words, lyrics);
        let at = |index: usize| lines[index].0;
        assert_eq!(lines.len(), 8);
        assert!((at(2) - 13.5).abs() < 0.6, "the first chorus keeps its own line: {lines:?}");
        assert!(at(3) > at(2) && at(3) < at(4), "an unheard line sits between its neighbours: {lines:?}");
        assert!((at(4) - 20.0).abs() < 0.6, "the second verse is where it was sung: {lines:?}");
        assert!((at(6) - 33.0).abs() < 0.6 && (at(7) - 36.0).abs() < 0.6, "the second chorus is its own: {lines:?}");
    }

    /// The karaoke of a real track: Parakeet's words aligned to the lyrics in
    /// `YUE_TEST_LYRICS`, printed line by line with their start times.
    #[test]
    fn parakeet_karaoke_of_a_real_track() {
        let (Some(root), Some(track), Some(lyrics)) =
            (std::env::var_os("YUE_DATA_ROOT"), std::env::var_os("YUE_TEST_TRACK"), std::env::var_os("YUE_TEST_LYRICS"))
        else {
            return;
        };
        let sync = LyricsSync::new(std::path::Path::new(&root));
        let words = sync.parakeet_words(OnnxFlavour::Auto, std::path::Path::new(&track)).expect("recognition");
        let lyrics = std::fs::read_to_string(lyrics).expect("lyrics file");
        for (at, word) in &words {
            eprintln!("W {at:7.2} {word}");
        }
        for line in align_lyrics_words(&words, &lyrics) {
            eprintln!("{:7.1} {}", line.start, line.words.iter().map(|(_, word)| word.as_str()).collect::<Vec<_>>().join(" "));
        }
    }

    /// The same against Parakeet: what the recogniser actually heard, printed
    /// whole, so a generated song can be checked against its own lyrics, and
    /// how long loading and hearing took. `YUE_TEST_RUNTIME=cpu` keeps it on
    /// the processor, to hold the card's time against.
    #[test]
    fn parakeet_hears_a_real_track() {
        let (Some(root), Some(track)) = (std::env::var_os("YUE_DATA_ROOT"), std::env::var_os("YUE_TEST_TRACK")) else { return };
        let runtime = if std::env::var("YUE_TEST_RUNTIME").is_ok_and(|runtime| runtime == "cpu") { OnnxFlavour::Cpu } else { OnnxFlavour::Auto };
        let sync = LyricsSync::new(std::path::Path::new(&root));
        assert!(sync.parakeet_ready(), "Parakeet is not installed");
        let card = sync.onnx_card(runtime).expect("the ONNX Runtime");
        let started = std::time::Instant::now();
        let mut model = sync.load_parakeet(card).expect("load Parakeet");
        let loaded = started.elapsed();
        let words = parakeet_transcribe(&mut model, std::path::Path::new(&track)).expect("recognition");
        eprintln!("on {card:?}: loaded in {:.1} s, heard in {:.1} s", loaded.as_secs_f64(), (started.elapsed() - loaded).as_secs_f64());
        let heard: Vec<&str> = words.iter().map(|(_, word)| word.as_str()).collect();
        eprintln!("heard {} words: {}", words.len(), heard.join(" "));
        assert!(!words.is_empty(), "nothing was recognised");
    }
}


#[cfg(all(test, windows))]
mod directml_live {
    use super::*;
    use std::time::Instant;

    /// Where the DirectML.dll this process loaded came from.
    fn loaded_directml() -> Option<PathBuf> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
            fn GetModuleFileNameW(module: *mut std::ffi::c_void, file: *mut u16, size: u32) -> u32;
        }
        let name: Vec<u16> = "DirectML.dll".encode_utf16().chain(Some(0)).collect();
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        let mut path = vec![0u16; 1024];
        let length = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
        Some(PathBuf::from(String::from_utf16_lossy(&path[..length])))
    }

    fn env_path(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
    }

    /// The DirectML build installed the way the studio installs it and bound,
    /// once for the process. `YUE_TEST_DML_ADAPTER` pins a DXGI adapter - the
    /// integrated Radeon beside an NVIDIA card - instead of the fastest card.
    fn directml() -> Option<OnnxCard> {
        static BOUND: std::sync::Once = std::sync::Once::new();
        BOUND.call_once(|| {
            let sync = LyricsSync::new(&env_path("YUE_DATA_ROOT"));
            let parts: Vec<&'static Asset> = DIRECTML_ASSETS.iter().filter_map(|id| asset(id)).collect();
            tokio::runtime::Runtime::new().unwrap().block_on(sync.downloader().install_all("test", &parts)).expect("install DirectML");
            assert!(sync.has_directml_libraries(), "the DirectML build is incomplete after installing it");
            if let Ok(adapter) = std::env::var("YUE_TEST_DML_ADAPTER") {
                DIRECTML_ADAPTER.set(adapter.parse().expect("a DXGI adapter index")).unwrap();
            }
            let runtime = sync.downloader().runtime_dir("onnx-dml");
            point_ort_at(&runtime.join("onnxruntime.dll"), Some(OnnxCard::DirectMl));
            assert_eq!(loaded_directml().as_deref(), Some(runtime.join("DirectML.dll").as_path()), "another DirectML.dll serves the process");
        });
        Some(OnnxCard::DirectMl)
    }

    /// Beat This! on the card beside the processor: the time and the tempo.
    #[test]
    #[ignore = "downloads DirectML; needs YUE_DATA_ROOT, YUE_TEST_TRACK and YUE_TEST_BEAT_THIS"]
    fn beat_this_runs_through_directml() {
        let card = directml();
        let mono = crate::audio_facts::decode(&env_path("YUE_TEST_TRACK")).expect("decode the track");
        let beat = env_path("YUE_TEST_BEAT_THIS");
        let measure = |card| {
            let started = Instant::now();
            let mut measurer = crate::audio_facts::Measurer::load(&beat, card).expect("load Beat This");
            let facts = measurer.measure_mono(&mono).expect("measure");
            (facts.bpm, measurer.on_gpu, started.elapsed().as_secs_f64())
        };
        let (card_bpm, on_gpu, card_time) = measure(card);
        assert!(on_gpu, "DirectML did not take Beat This");
        let (processor_bpm, _, processor_time) = measure(None);
        eprintln!("tempo: card {card_bpm} BPM in {card_time:.1} s, processor {processor_bpm} BPM in {processor_time:.1} s");
        assert_eq!(card_bpm, processor_bpm, "the card hears another tempo");
    }

    /// Parakeet with its encoder on the card, the joint on the card and on the
    /// processor, beside the whole of it on the processor: how much of the
    /// song's own lyrics each hears, in order, from `YUE_TEST_LYRICS`.
    #[test]
    #[ignore = "downloads DirectML; needs YUE_DATA_ROOT, YUE_TEST_TRACK, YUE_TEST_LYRICS and YUE_TEST_PARAKEET"]
    fn parakeet_runs_through_directml() {
        let card = directml();
        let track = env_path("YUE_TEST_TRACK");
        let parakeet = env_path("YUE_TEST_PARAKEET");
        let lyrics = std::fs::read_to_string(env_path("YUE_TEST_LYRICS")).expect("the lyrics");
        let sung = words_of(lyrics.lines().filter(|line| !line.trim_start().starts_with('[')).flat_map(str::split_whitespace));
        let hear = |encoder: Option<OnnxCard>, joint: Option<OnnxCard>| {
            let started = Instant::now();
            let mut model = parakeet_rs::ParakeetTDT::from_pretrained_with_joint_config(&parakeet, parakeet_config(encoder), parakeet_config(joint)).expect("load Parakeet");
            let words = parakeet_transcribe(&mut model, &track).expect("recognition");
            let heard = words_of(words.iter().map(|(_, word)| word.as_str()));
            (in_order(&sung, &heard), heard.join(" "), started.elapsed().as_secs_f64())
        };
        let (processor, text, took) = hear(None, None);
        eprintln!("Parakeet on the processor: {:.0}% of the lyrics in order, {took:.1} s\n  {text}", processor * 100.0);
        for (label, joint) in [("joint on the card", card), ("joint on the processor", None)] {
            let (heard, text, took) = hear(card, joint);
            eprintln!("Parakeet encoder on the card, {label}: {:.0}% of the lyrics in order, {took:.1} s\n  {text}", heard * 100.0);
            assert!(heard >= processor - 0.05, "the card hears the lyrics worse than the processor");
        }
    }

    /// Words without punctuation, in lower case.
    fn words_of<'a>(words: impl Iterator<Item = &'a str>) -> Vec<String> {
        words
            .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|word| !word.is_empty())
            .collect()
    }

    /// The share of `reference` that `heard` has in the same order: their
    /// longest common subsequence over the reference's length.
    fn in_order(reference: &[String], heard: &[String]) -> f64 {
        let mut row = vec![0usize; heard.len() + 1];
        for word in reference {
            let mut diagonal = 0;
            for (index, other) in heard.iter().enumerate() {
                let above = row[index + 1];
                row[index + 1] = if word == other { diagonal + 1 } else { row[index + 1].max(row[index]) };
                diagonal = above;
            }
        }
        row[heard.len()] as f64 / reference.len().max(1) as f64
    }
}
