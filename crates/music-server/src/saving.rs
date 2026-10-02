//! "Save as" for everything the window hands the user: a song, its stems, a
//! MIDI file, a lyric sheet, a score, a video.
//!
//! The window asks for a place first; the desktop shell shows Windows' own
//! Save dialog, starting in the folder chosen last. The service then writes
//! the file itself - one it serves, read from its own address, or bytes the
//! window made - and tells the window how far it has got, so the files panel
//! can show each one with its progress and a way to it in Explorer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::{api_error, mcp, ApiError};

/// Shows the Save dialog: the suggested file name and the folder to start in;
/// the chosen path, or None when the user cancels.
pub type SaveDialog = Box<dyn Fn(&str, Option<PathBuf>) -> Option<PathBuf> + Send + Sync>;

static DIALOG: OnceLock<SaveDialog> = OnceLock::new();

/// Set by the desktop shell, which owns the windows a dialog belongs to.
pub fn set_save_dialog(dialog: SaveDialog) {
    let _ = DIALOG.set(dialog);
}

#[derive(Default)]
struct Saving {
    /// Places chosen and not yet written, by the id handed to the window.
    chosen: HashMap<String, PathBuf>,
    /// The folder of the last file saved, where the next dialog starts.
    last_folder: Option<PathBuf>,
    /// Files written: the only ones the window may ask Explorer to show.
    written: Vec<PathBuf>,
}

fn saving() -> &'static Mutex<Saving> {
    static SAVING: OnceLock<Mutex<Saving>> = OnceLock::new();
    SAVING.get_or_init(|| Mutex::new(Saving::default()))
}

#[derive(Debug, Deserialize)]
pub struct Destination {
    name: String,
}

/// Asks where to save `name`. Cancelling is an answer, not an error.
pub async fn choose(Json(request): Json<Destination>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let name: String = request.name.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let name = name.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "a file name is needed".into()));
    }
    let start = saving().lock().expect("saving").last_folder.clone();
    let asked = name.clone();
    let chosen = tokio::task::spawn_blocking(move || DIALOG.get().map(|dialog| dialog(&asked, start)))
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("the Save dialog failed: {error}")))?;
    let Some(chosen) = chosen else {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "this studio has no window to show a Save dialog in".into()));
    };
    let Some(path) = chosen else {
        return Ok(Json(json!({ "cancelled": true })));
    };
    let id = uuid::Uuid::now_v7().to_string();
    let mut state = saving().lock().expect("saving");
    state.last_folder = path.parent().map(PathBuf::from);
    state.chosen.insert(id.clone(), path.clone());
    Ok(Json(json!({ "id": id, "path": path.display().to_string(), "name": path.file_name().map(|name| name.to_string_lossy().to_string()) })))
}

fn tell(id: &str, fields: Value) {
    let mut event = json!({ "id": id });
    if let (Some(event), Value::Object(fields)) = (event.as_object_mut(), fields) {
        event.extend(fields);
    }
    mcp::tell_windows(json!({ "saving": event }));
}

/// What the window sends to be written: an address of this service, or the
/// bytes themselves.
#[derive(Debug, Deserialize)]
struct Served {
    url: String,
}

/// A path on this service from what the window holds: `/v1/...`, or the same
/// with this machine's address in front.
fn own_path(url: &str) -> Option<String> {
    if url.starts_with('/') && !url.starts_with("//") {
        return Some(url.to_string());
    }
    let parsed = reqwest::Url::parse(url).ok()?;
    let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    (local && parsed.port() == Some(crate::listen_port())).then(|| match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    })
}

/// Writes the file chosen as `id`: from the address in a JSON body, or from
/// the body's bytes.
pub async fn write(Path(id): Path<String>, headers: HeaderMap, body: Body) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let path = saving()
        .lock()
        .expect("saving")
        .chosen
        .remove(&id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "no place was chosen for this file".into()))?;
    let json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    tell(&id, json!({ "path": path.display().to_string(), "state": "saving", "written": 0 }));
    let result = if json {
        let bytes = axum::body::to_bytes(body, 64 * 1024)
            .await
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("read the request: {error}")))?;
        let served: Served = serde_json::from_slice(&bytes).map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("the request is not an address: {error}")))?;
        let Some(own) = own_path(&served.url) else {
            return Err(api_error(StatusCode::BAD_REQUEST, format!("{} is not a file of this studio", served.url)));
        };
        let response = crate::net::client()
            .get(format!("http://127.0.0.1:{}{own}", crate::listen_port()))
            .send()
            .await
            .and_then(|response| response.error_for_status());
        match response {
            Ok(response) => {
                let total = response.content_length();
                copy(&id, &path, response.bytes_stream().map(|chunk| chunk.map_err(std::io::Error::other)), total).await
            }
            Err(error) => Err(format!("the studio could not read {own}: {error}")),
        }
    } else {
        let total = headers
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok());
        copy(&id, &path, body.into_data_stream().map(|chunk| chunk.map_err(std::io::Error::other)), total).await
    };
    match result {
        Ok(written) => {
            {
                let mut state = saving().lock().expect("saving");
                state.written.retain(|file| file != &path);
                state.written.push(path.clone());
                let excess = state.written.len().saturating_sub(100);
                state.written.drain(..excess);
            }
            tell(&id, json!({ "state": "done", "written": written, "total": written }));
            Ok(Json(json!({ "path": path.display().to_string(), "written": written })))
        }
        Err(error) => {
            tell(&id, json!({ "state": "error", "error": error }));
            Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, error))
        }
    }
}

/// Into a `.part` beside the target, renamed when whole: a cancelled or
/// broken save never leaves half a file under the name the user chose.
async fn copy<S>(id: &str, path: &std::path::Path, mut chunks: S, total: Option<u64>) -> Result<u64, String>
where
    S: futures_util::Stream<Item = std::io::Result<axum::body::Bytes>> + Unpin,
{
    let part = path.with_extension(format!("{}.part", path.extension().map(|extension| extension.to_string_lossy()).unwrap_or_default()));
    let mut file = tokio::fs::File::create(&part).await.map_err(|error| format!("{} cannot be written: {error}", path.display()))?;
    let mut written = 0u64;
    let mut told = std::time::Instant::now();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(format!("the file stopped arriving: {error}"));
            }
        };
        if let Err(error) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!("{} cannot be written: {error}", path.display()));
        }
        written += chunk.len() as u64;
        if told.elapsed() >= std::time::Duration::from_millis(250) {
            told = std::time::Instant::now();
            tell(id, json!({ "written": written, "total": total }));
        }
    }
    file.flush().await.map_err(|error| format!("{} cannot be written: {error}", path.display()))?;
    drop(file);
    if let Err(error) = tokio::fs::rename(&part, path).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("{} cannot be put in place: {error}", path.display()));
    }
    Ok(written)
}

#[derive(Debug, Deserialize)]
pub struct Written {
    path: String,
}

/// A native media player for one file: VLC, mpv and the classics, detached.
///
/// The Linux replacement for the desktop shell-outs: where the Windows build
/// hands playback to its Winamp mode and its Explorer verbs, the fork plays a
/// saved file or a library track in the player's own window. No shell is ever
/// involved - the binary is resolved on PATH (or named whole by
/// `YUE_MEDIA_PLAYER`) and the path goes as one argument, so a file name
/// cannot become a command.
pub fn open_in_player(path: &std::path::Path) -> Result<String, String> {
    // The desktop shell's own default application, via the shell it has.
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.display().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("the default application did not start: {error}"))?;
        return Ok("default application".to_string());
    }
    #[cfg(not(windows))]
    {
        let player = resolve_media_player(|name| on_search_path(name)).ok_or_else(|| {
            "no media player was found (VLC, mpv, mplayer): install one, or name it in YUE_MEDIA_PLAYER".to_string()
        })?;
        std::process::Command::new(&player)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("{player} did not start: {error}"))?;
        return Ok(player);
    }
}

/// Native players in order of preference, before the `xdg-open` fallback.
/// Linux-only: Windows opens the default application (see `open_in_player`).
#[cfg(not(windows))]
const MEDIA_PLAYERS: [&str; 5] = ["vlc", "mpv", "mplayer", "celluloid", "totem"];

/// The player a file opens in: `YUE_MEDIA_PLAYER` first - a bare name looked
/// up the same way, or an absolute path - then the known players in order,
/// then whatever opens audio on this desktop. Linux-only (see above).
#[cfg(not(windows))]
fn resolve_media_player(exists: impl Fn(&str) -> bool) -> Option<String> {
    if let Some(wanted) = std::env::var("YUE_MEDIA_PLAYER").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
        if wanted.contains('/') {
            return PathBuf::from(&wanted).is_file().then_some(wanted);
        }
        if exists(&wanted) {
            return Some(wanted);
        }
        return None;
    }
    #[cfg(not(windows))]
    if let Some(found) = MEDIA_PLAYERS.iter().find(|name| exists(name)) {
        return Some((*found).to_string());
    }
    // No known player: the desktop's default for audio. On Windows the
    // service never reaches here with a player missing (see open_in_player).
    #[cfg(not(windows))]
    if exists("xdg-open") {
        return Some("xdg-open".to_string());
    }
    None
}

/// Whether the loader would find this binary without help: any directory on
/// PATH holding a file of that name. Linux-only: player lookup (see above).
#[cfg(not(windows))]
fn on_search_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|directory| directory.join(name).is_file())
}

/// A saved file, opened in the native media player. Only files this service
/// wrote (see `write`) may be opened: a path from the request could name
/// anything on the machine.
pub async fn play(Json(file): Json<Written>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let path = PathBuf::from(&file.path);
    if !saving().lock().expect("saving").written.contains(&path) {
        return Err(api_error(StatusCode::NOT_FOUND, format!("{} is not a file saved here", path.display())));
    }
    match open_in_player(&path) {
        Ok(player) => Ok(Json(json!({ "player": player, "path": path.display().to_string() }))),
        Err(error) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, error)),
    }
}

/// The file manager, open on a file saved here, with the file selected.
pub async fn reveal(Json(file): Json<Written>) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let path = PathBuf::from(&file.path);
    if !saving().lock().expect("saving").written.contains(&path) {
        return Err(api_error(StatusCode::NOT_FOUND, format!("{} is not a file saved here", path.display())));
    }
    #[cfg(windows)]
    let mut opener = std::process::Command::new("explorer.exe");
    // No Explorer on Linux: the desktop's own opener (same as the
    // open-data-directory and dataset-reveal routes use).
    #[cfg(not(windows))]
    let mut opener = std::process::Command::new("xdg-open");
    // Explorer reads /select,"path" as one switch; quoting the whole of it
    // breaks the switch.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::raw_arg(&mut opener, format!("/select,\"{}\"", path.display()));
    #[cfg(not(windows))]
    opener.arg(path.parent().unwrap_or(&path));
    opener
        .spawn()
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("the file manager did not open: {error}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_studios_own_files_are_read() {
        let port = crate::listen_port();
        assert_eq!(own_path("/v1/library/songs/a/midi/file").as_deref(), Some("/v1/library/songs/a/midi/file"));
        assert_eq!(own_path(&format!("http://127.0.0.1:{port}/media/a.mp3?v=2")).as_deref(), Some("/media/a.mp3?v=2"));
        assert_eq!(own_path("https://example.com/a.mp3"), None);
        assert_eq!(own_path(&format!("http://127.0.0.1:{}/a", port.wrapping_add(1))), None);
        assert_eq!(own_path("//example.com/a"), None);
    }

    /// `YUE_MEDIA_PLAYER` held aside: it is process-global and no other test
    /// reads it, but a developer export must not steer these answers. The
    /// player tests below share it, so they take the lock in turn.
    /// Linux-only, like the player lookup itself.
    #[cfg(not(windows))]
    static PLAYER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[cfg(not(windows))]
    struct QuietEnv(Option<std::ffi::OsString>, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    #[cfg(not(windows))]
    impl QuietEnv {
        fn hold() -> Self {
            let guard = PLAYER_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let held = std::env::var_os("YUE_MEDIA_PLAYER");
            unsafe { std::env::remove_var("YUE_MEDIA_PLAYER") };
            Self(held, guard)
        }
    }
    #[cfg(not(windows))]
    impl Drop for QuietEnv {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                unsafe { std::env::set_var("YUE_MEDIA_PLAYER", value) };
            }
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn the_player_is_vlc_first_then_mpv_then_mplayer() {
        let _held = QuietEnv::hold();
        let present = |names: Vec<&'static str>| move |name: &str| names.iter().any(|known| *known == name);
        assert_eq!(resolve_media_player(present(vec!["vlc", "mpv", "mplayer"])).as_deref(), Some("vlc"));
        assert_eq!(resolve_media_player(present(vec!["mpv", "mplayer"])).as_deref(), Some("mpv"));
        assert_eq!(resolve_media_player(present(vec!["mplayer"])).as_deref(), Some("mplayer"));
        assert_eq!(resolve_media_player(present(vec!["celluloid"])).as_deref(), Some("celluloid"));
    }

    #[test]
    #[cfg(not(windows))]
    fn without_a_player_the_desktop_default_opens_audio() {
        let _held = QuietEnv::hold();
        assert_eq!(resolve_media_player(|name| name == "xdg-open").as_deref(), Some("xdg-open"));
        assert_eq!(resolve_media_player(|_| false), None);
    }

    #[test]
    #[cfg(not(windows))]
    fn the_player_override_names_a_binary_or_a_whole_path() {
        let _held = QuietEnv::hold();
        unsafe { std::env::set_var("YUE_MEDIA_PLAYER", "mpv") };
        assert_eq!(resolve_media_player(|name| name == "mpv").as_deref(), Some("mpv"));
        assert_eq!(resolve_media_player(|_| false), None);
        let missing = std::env::temp_dir().join(format!("no-player-{}", uuid::Uuid::now_v7()));
        unsafe { std::env::set_var("YUE_MEDIA_PLAYER", missing.to_string_lossy().to_string()) };
        assert_eq!(resolve_media_player(|_| true), None);
        let binary = std::env::current_exe().expect("a test binary");
        unsafe { std::env::set_var("YUE_MEDIA_PLAYER", binary.to_string_lossy().to_string()) };
        assert_eq!(resolve_media_player(|_| false).as_deref(), Some(binary.to_string_lossy().as_ref()));
    }
}
