//! Self-healing yt-dlp engine.
//!
//! The bundled sidecar binary is frozen at whatever version CI shipped. When
//! a site breaks yt-dlp extractors, the fix is usually "run the latest
//! release" — so on a stale-engine failure we download the current official
//! build into `<appData>/bin/` and prefer it for every later spawn.
//!
//! We download to appData rather than running `yt-dlp -U` in place because
//! installs are per-machine (Program Files on Windows, /usr/bin via .deb on
//! Linux) and not writable by the app.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;

/// Set once an engine update succeeded this session, so repeated failures
/// don't re-download the same binary over and over.
static UPDATED_THIS_SESSION: AtomicBool = AtomicBool::new(false);

pub fn updated_this_session() -> bool {
    UPDATED_THIS_SESSION.load(Ordering::SeqCst)
}

fn engine_file_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// Path of a previously-downloaded engine, if one exists.
pub fn updated_engine_path(app: &AppHandle) -> Option<PathBuf> {
    let path = app.path().app_data_dir().ok()?.join("bin").join(engine_file_name());
    path.is_file().then_some(path)
}

/// Directory of bundled yt-dlp plugins (ChromeCookieUnlock et al.), when the
/// packaged resources are present.
pub fn plugins_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().resource_dir().ok()?.join("yt-dlp-plugins");
    dir.is_dir().then_some(dir)
}

/// Spawn command for yt-dlp: the self-updated copy when present, otherwise
/// the bundled sidecar — always with the bundled plugin directory loaded so
/// ChromeCookieUnlock can patch around locked Chromium cookie databases
/// (yt-dlp #7271). Drop-in replacement for `.sidecar("yt-dlp")`.
pub fn ytdlp_command(
    app: &AppHandle,
) -> Result<tauri_plugin_shell::process::Command, String> {
    let mut cmd = match updated_engine_path(app) {
        Some(path) => app.shell().command(path),
        None => app.shell().sidecar("yt-dlp").map_err(|e| e.to_string())?,
    };
    if let Some(dir) = plugins_dir(app) {
        cmd = cmd.args([
            "--plugin-dirs".to_string(),
            dir.to_string_lossy().into_owned(),
        ]);
    }
    Ok(cmd)
}

/// Official release asset matching this platform. yt-dlp publishes
/// standalone single-file binaries that run without Python installed.
fn asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

/// Download the latest official yt-dlp into appData and return its path.
/// Safe to call repeatedly: a successful call is remembered, and callers
/// gate on failure kind anyway.
pub async fn update_engine(app: &AppHandle) -> Result<PathBuf, String> {
    let url = format!(
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/{}",
        asset_name()
    );
    let bytes = crate::extractors::client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("engine download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("engine download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("engine download failed: {e}"))?;

    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?
        .join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("engine dir: {e}"))?;

    let dest = dir.join(engine_file_name());
    let tmp = dir.join(format!("{}.download", engine_file_name()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("engine write: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }

    // Windows rename fails onto an existing file; clear the old one first.
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&tmp, &dest).map_err(|e| format!("engine install: {e}"))?;

    UPDATED_THIS_SESSION.store(true, Ordering::SeqCst);
    Ok(dest)
}
