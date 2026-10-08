//! The CUDA libraries the music engine is linked against.
//!
//! The engine loads one of two CUDA backends at run time, `cuda13\ggml-cuda.dll`
//! or `cuda12\ggml-cuda.dll`, whichever the card and its driver run (see
//! `hardware::CudaBuild`). Each imports its own cuBLAS, `cublas64_13.dll` or
//! `cublas64_12.dll`, which imports `cublasLt64_1x.dll`. The loader resolves
//! them from the executable's folder, so they go beside `yue-server.exe`, and
//! the two majors never clash by name. A missing cuBLAS fails the backend
//! load, and the engine stops on it.
//!
//! The two libraries are 512 MB unpacked, which is four times the rest of the
//! installer, so they are not shipped - they are fetched from NVIDIA's own
//! redistributable archive the first time the engine is asked to start, the
//! same way the models are. The build machine has them on PATH through the CUDA
//! Toolkit, which is exactly why this was invisible until someone without the
//! Toolkit ran the release.

use std::path::Path;
#[cfg(all(test, windows))]
use std::path::PathBuf;

use anyhow::{bail, Result};
#[cfg(all(test, windows))]
use anyhow::Context;

use crate::downloads::{Asset, Downloader};
#[cfg(windows)]
use crate::downloads::AssetKind;
use crate::hardware::CudaBuild;

/// The libraries of each build, by file name, exactly as its CUDA backend
/// imports them. The CUDA major is part of the name, so a rebuild on another
/// major is a change here, and the dependency check below catches it.
#[cfg(windows)]
pub const CUDA13_LIBRARIES: [&str; 2] = ["cublas64_13.dll", "cublasLt64_13.dll"];
#[cfg(windows)]
pub const CUDA12_LIBRARIES: [&str; 2] = ["cublas64_12.dll", "cublasLt64_12.dll"];
/// The SONAMEs the Linux engine's `libggml-cuda.so` loads from the system
/// CUDA toolkit: no major-less `libcublas.so` is resolved at run time.
///
/// Referenced by the Linux bundle test (the Linux supervisor resolves these
/// through the system loader and downloads nothing); the future `ldd`-based
/// bundle check will use them too.
#[cfg(not(windows))]
#[allow(dead_code)]
pub const CUDA13_LIBRARIES: [&str; 2] = ["libcublas.so.13", "libcublasLt.so.13"];
/// The SONAMEs of the Linux CUDA 12 backend (Maxwell–Volta, older drivers).
/// See `CUDA13_LIBRARIES` above for why this allows dead code.
#[cfg(not(windows))]
#[allow(dead_code)]
pub const CUDA12_LIBRARIES: [&str; 2] = ["libcublas.so.12", "libcublasLt.so.12"];

/// The Visual C++ runtime the engine and ggml are compiled against, by the
/// names in their import tables. Windows-only: a Linux engine links the
/// system libstdc++/libgomp instead.
#[cfg(windows)]
pub const VC_RUNTIME_LIBRARIES: [&str; 4] =
    ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "vcomp140.dll"];
#[cfg(not(windows))]
pub const VC_RUNTIME_LIBRARIES: [&str; 0] = [];


#[cfg(all(test, windows))]
pub const ASSETS: &[Asset] = &[CUBLAS13, CUBLAS12];

#[cfg(windows)]
const CUBLAS13: Asset = Asset {
    id: "engine-cuda-cublas",
    label: "NVIDIA cuBLAS 13.5",
    kind: AssetKind::Runtime,
    // NVIDIA's redistributable archive, the same one every CUDA application
    // ships from. The size and the digest are published in
    // redist/redistrib_13.3.0.json; the size below was confirmed against a
    // live HEAD request, which also confirmed range support.
    url: "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-13.5.1.27-archive.zip",
    relative_path: "cuda-cublas.zip",
    bytes: 391_055_517,
    // No sub-directory: the libraries go straight in beside yue-server.exe,
    // which is where the Windows loader looks first and what NVIDIA's own
    // deployment guide recommends.
    unzip_into: None,
    marker: "cublasLt64_13.dll",
    pick: &CUDA13_LIBRARIES,
    keep: &[],
    vram_gb: None,
    note: "The linear algebra the engine's CUDA backend is linked against. Without it the engine cannot start at all.",
};

#[cfg(windows)]
const CUBLAS12: Asset = Asset {
    id: "engine-cuda12-cublas",    label: "NVIDIA cuBLAS 12.9",
    kind: AssetKind::Runtime,
    // The cuBLAS of the CUDA 12.9 toolkit the CUDA 12 backend is built with.
    // Size from redist/redistrib_12.9.1.json, confirmed by a HEAD request.
    url: "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.1.4-archive.zip",
    relative_path: "cuda12-cublas.zip",
    bytes: 549_755_186,
    unzip_into: None,
    marker: "cublasLt64_12.dll",
    pick: &CUDA12_LIBRARIES,
    keep: &[],
    vram_gb: None,
    note: "The linear algebra of the engine's CUDA 12 backend, for cards CUDA 13 dropped and older drivers.",
};

/// The cuBLAS a CUDA build loads. Windows-only: on Linux the engine uses the
/// system CUDA toolkit and nothing is downloaded (see `missing`).
#[cfg(windows)]
pub fn cublas_asset(build: CudaBuild) -> &'static Asset {
    match build {
        CudaBuild::Cuda13 => &CUBLAS13,
        CudaBuild::Cuda12 => &CUBLAS12,
    }
}

/// The engine's downloadable runtime.
pub struct EngineRuntime {
    downloader: Downloader,
}

impl EngineRuntime {
    /// Takes the directory `yue-server.exe` lives in: the libraries belong
    /// beside the binary that imports them, not in a folder of their own.
    pub fn new(bundle_root: &Path) -> Self {
        Self { downloader: Downloader::new(bundle_root.to_path_buf()) }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// Where the libraries live once installed - beside the engine.
    /// Windows-only for now: the Linux `ldd` bundle check will use it too.
    #[cfg(all(test, windows))]
    pub fn library_dir(&self) -> PathBuf {
        self.downloader.root().to_path_buf()
    }

    /// Whether the engine will find every library it needs. cuBLAS counts only
    /// when the engine will compute on CUDA, and only the one of that build:
    /// the bundled engine loads its backends at run time, so on an AMD or
    /// Intel card, or with Vulkan chosen, no CUDA backend is ever loaded.
    ///
    /// The Visual C++ runtime counts always: a machine that already has cuBLAS
    /// but no redistributable has nothing to download and still cannot start
    /// the engine.
    pub fn is_ready(&self, cuda: Option<CudaBuild>) -> bool {
        self.missing(cuda).is_empty() && self.vc_runtime_missing().is_empty()
    }

    /// The Visual C++ runtime ships inside the engine bundle, app-local as
    /// Microsoft permits, so the studio never installs anything into the
    /// system. A machine that has it on its search path is fine either way.
    pub fn vc_runtime_missing(&self) -> Vec<&'static str> {
        VC_RUNTIME_LIBRARIES
            .iter()
            .copied()
            .filter(|library| !self.downloader.root().join(library).is_file() && !is_on_the_search_path(library))
            .collect()
    }

    /// What is still missing, so a caller can report the size before starting.
    ///
    /// A machine that already has the libraries on its search path - a CUDA
    /// Toolkit installation - downloads nothing: the engine inherits that path
    /// and finds them there.
    ///
    /// On Linux nothing is ever downloaded: the engine loads cuBLAS from the
    /// system CUDA toolkit (`LD_LIBRARY_PATH`/`ldconfig`), and a machine
    /// without it simply fails the CUDA backend load, which Auto treats like
    /// any other device failure and moves on to Vulkan or the processor.
    pub fn missing(&self, cuda: Option<CudaBuild>) -> Vec<&'static Asset> {
        #[cfg(not(windows))]
        {
            let _ = cuda;
            return Vec::new();
        }
        #[cfg(windows)]
        {
            let Some(build) = cuda else { return Vec::new() };
            let asset = cublas_asset(build);
            if self.downloader.is_installed(asset) || asset.pick.iter().all(|library| is_on_the_search_path(library)) {
                return Vec::new();
            }
            vec![asset]
        }
    }

    /// Fetches whatever is missing and waits for it.
    pub async fn install_missing(&self, cuda: Option<CudaBuild>) -> Result<()> {
        let absent = self.vc_runtime_missing();
        if !absent.is_empty() {
            bail!("the engine bundle is incomplete: {} missing beside yue-server.exe; reinstall the studio", absent.join(", "));
        }
        self.downloader.install_all("engine", &self.missing(cuda)).await
    }
}

/// Whether the loader would already find this library without help.
///
/// The engine inherits this process's PATH, so the directories searched there
/// are the directories searched here. A machine with the CUDA Toolkit
/// installed has cuBLAS on PATH already, and asking it to download half a
/// gigabyte of the same thing would be rude.
fn is_on_the_search_path(library: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|directory| directory.join(library).is_file())
}

/// The DLLs a Windows binary names in its import table.
///
/// NVIDIA's deployment guide says to read this with `dumpbin /IMPORTS` and
/// redistribute exactly what it lists, because the binary compatibility
/// version is part of the file name - `cublas64_13.dll`, not `cublas.dll`.
/// Reading it here means the release can check itself instead of trusting
/// that whoever built it remembered.
///
/// Windows-only: the Linux bundle check will read ELF dependencies instead.
#[cfg(all(test, windows))]
pub fn imported_libraries(binary: &Path) -> Result<Vec<String>> {
    let data = std::fs::read(binary)?;
    let at = |offset: usize| -> Result<u32> {
        let bytes = data.get(offset..offset + 4).context("truncated PE header")?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    };
    let short = |offset: usize| -> Result<u16> {
        let bytes = data.get(offset..offset + 2).context("truncated PE header")?;
        Ok(u16::from_le_bytes(bytes.try_into().expect("two bytes")))
    };

    let pe = at(0x3c)? as usize;
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        bail!("{} is not a PE binary", binary.display());
    }
    let sections = short(pe + 6)? as usize;
    let optional_size = short(pe + 20)? as usize;
    let optional = pe + 24;
    // 0x20b is PE32+, whose data directories start 16 bytes further in.
    let directories = optional + if short(optional)? == 0x20b { 112 } else { 96 };
    let import_rva = at(directories + 8)?;
    if import_rva == 0 {
        return Ok(Vec::new());
    }

    let headers: Vec<(u32, u32, u32)> = (0..sections)
        .map(|index| {
            let base = optional + optional_size + index * 40;
            Ok((at(base + 12)?, at(base + 16)?, at(base + 20)?))
        })
        .collect::<Result<_>>()?;
    let offset_of = |rva: u32| -> Option<usize> {
        headers
            .iter()
            .find(|(virtual_address, size, _)| rva >= *virtual_address && rva < virtual_address + size)
            .map(|(virtual_address, _, raw)| (raw + (rva - virtual_address)) as usize)
    };

    let mut names = Vec::new();
    let mut entry = offset_of(import_rva).context("import table points outside the file")?;
    loop {
        let descriptor = data.get(entry..entry + 20).context("truncated import table")?;
        if descriptor.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_rva = u32::from_le_bytes(descriptor[12..16].try_into().expect("four bytes"));
        let start = offset_of(name_rva).context("an import name points outside the file")?;
        let end = data[start..].iter().position(|byte| *byte == 0).context("unterminated import name")? + start;
        names.push(String::from_utf8_lossy(&data[start..end]).into_owned());
        entry += 20;
    }
    Ok(names)
}

/// What a binary needs that is neither beside it nor supplied by Windows.
///
/// Removed with the staged-bundle check above: the Linux fork stages no
/// Windows bundle, and the `ldd` step in build-yue-runtime.sh covers the
/// Linux one. The import-table reader stays for the notepad test below.

#[cfg(test)]
mod tests {
    use super::*;

    /// The file names are the contract with the engine binary: they are what
    /// the import table asks the loader for, and what `pick` takes out of the
    /// archive. A typo here is a download that finishes and changes nothing.
    #[test]
    #[cfg(windows)]
    fn every_imported_library_is_picked_out_of_the_archive() {
        for (build, libraries, major) in
            [(CudaBuild::Cuda13, CUDA13_LIBRARIES, "13"), (CudaBuild::Cuda12, CUDA12_LIBRARIES, "12")]
        {
            let asset = cublas_asset(build);
            for library in libraries {
                assert!(library.contains(major), "{library} does not name CUDA {major}");
                assert!(asset.pick.contains(&library), "{library} is imported by {build:?} but never taken out of its archive");
            }
            assert!(asset.url.contains(&format!("-{major}.")), "{} is not a CUDA {major} cuBLAS", asset.url);
        }
    }

    /// A runtime asset that unpacks nowhere would download and vanish.
    #[test]
    #[cfg(windows)]
    fn the_libraries_land_in_one_directory_the_engine_can_be_pointed_at() {
        for asset in ASSETS {
            // Straight into the bundle: a sub-directory would put them
            // somewhere the loader does not look.
            assert_eq!(asset.unzip_into, None);
            assert_eq!(asset.kind, AssetKind::Runtime);
            assert!(asset.bytes > 0, "{} has no size, so nothing can report progress", asset.id);
        }
    }

    /// The release that shipped without cuBLAS passed every test there was,
    /// because no test ever looked at what the engine binary asks the loader
    /// for. On Windows this checked the staged bundle the same way; the Linux
    /// fork stages no Windows bundle (see build-yue-runtime.sh and the `ldd`
    /// step below), so the check was removed with it.
    ///
    /// Linux counterpart: stage the runtime, then read what the loader needs:
    /// `ldd yue-server` and `ldd libggml-cuda.so` must show no missing
    /// libraries. That runs where the engine is built, not in this suite.
    #[test]
    #[cfg(all(test, windows))]
    fn imports_are_read_out_of_a_real_binary() {
        let system = Path::new("C:/Windows/System32/notepad.exe");
        if !system.is_file() {
            return;
        }
        let imports = imported_libraries(system).expect("notepad has an import table");
        assert!(!imports.is_empty(), "no imports were read at all");
        assert!(imports.iter().all(|name| name.to_ascii_lowercase().ends_with(".dll")));
    }

    /// The libraries count as installed only once they are beside the engine.
    ///
    /// This asserts on the downloader rather than on `is_ready`, because a
    /// machine with the CUDA Toolkit is ready without downloading anything -
    /// which is the point of the PATH check, and would make the test lie about
    /// what it proved.
    #[test]
    #[cfg(windows)]
    fn the_libraries_count_as_installed_only_beside_the_engine() {
        let root = std::env::temp_dir().join(format!("engine-runtime-{}", uuid::Uuid::now_v7()));
        let runtime = EngineRuntime::new(&root);
        let asset = cublas_asset(CudaBuild::Cuda13);
        assert!(!runtime.downloader().is_installed(asset));
        assert_eq!(runtime.library_dir(), root);
        // Off CUDA the engine never loads cuBLAS, so nothing is missing.
        assert!(runtime.missing(None).is_empty());

        std::fs::create_dir_all(runtime.library_dir()).unwrap();
        for library in CUDA13_LIBRARIES {
            std::fs::write(runtime.library_dir().join(library), b"x").unwrap();
        }
        assert!(runtime.downloader().is_installed(asset));
        // The other build asks for its own cuBLAS, unless PATH has it.
        if !CUDA12_LIBRARIES.iter().all(|library| is_on_the_search_path(library)) {
            assert_eq!(runtime.missing(Some(CudaBuild::Cuda12)).len(), 1);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// On Linux the engine uses the system CUDA toolkit: nothing is ever
    /// downloaded, there is no VC runtime to check, and the SONAMEs name
    /// their CUDA major the way the loader resolves them.
    #[test]
    #[cfg(not(windows))]
    fn linux_needs_no_downloaded_cuda_or_vc_runtime() {
        for (libraries, major) in [(CUDA13_LIBRARIES, "13"), (CUDA12_LIBRARIES, "12")] {
            for library in libraries {
                assert!(library.contains(major), "{library} does not name CUDA {major}");
                assert!(library.ends_with(&format!(".so.{major}")), "{library} is not a versioned SONAME");
            }
        }
        assert!(VC_RUNTIME_LIBRARIES.is_empty());
        let root = std::env::temp_dir().join(format!("engine-runtime-linux-{}", uuid::Uuid::now_v7()));
        let runtime = EngineRuntime::new(&root);
        assert!(runtime.missing(None).is_empty());
        assert!(runtime.missing(Some(CudaBuild::Cuda13)).is_empty());
        assert!(runtime.missing(Some(CudaBuild::Cuda12)).is_empty());
        assert!(runtime.vc_runtime_missing().is_empty());
        assert!(runtime.is_ready(Some(CudaBuild::Cuda13)));
        std::fs::remove_dir_all(&root).ok();
    }
}
