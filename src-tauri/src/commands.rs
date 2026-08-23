use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::CommandEvent;

use crate::engine;
use crate::errors::{self, FailureKind};
use crate::pluck::{self, DonePayload, Throttle};
use crate::{PluckJob, PluckState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    kind: String, // "video" | "playlist"
    title: String,
    thumbnail: Option<String>,
    duration: Option<f64>,
    heights: Vec<u32>,
    entry_count: Option<u64>,
    entries: Vec<String>,
    source: String, // yt-dlp extractor, e.g. "Youtube", "Twitter"
}

fn extractor_of(v: &Value) -> String {
    v.get("extractor_key")
        .and_then(Value::as_str)
        .or_else(|| v.get("extractor").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn last_thumbnail(v: &Value) -> Option<String> {
    v.get("thumbnails")?
        .as_array()?
        .last()?
        .get("url")?
        .as_str()
        .map(String::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieEntry {
    name: String,
    path: String,
}

/// Normalize a user-entered cookie profile name into a safe file stem:
/// lowercased, restricted to [a-z0-9._-], path separators and traversal
/// sequences stripped, capped at 64 chars. Rejects names that would be empty.
fn sanitize_cookie_name(raw: &str) -> Result<String, String> {
    let cleaned: String = raw
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['_', '.', '-']);
    if trimmed.is_empty() {
        return Err("cookie name must contain at least one letter or digit".into());
    }
    Ok(trimmed.chars().take(64).collect())
}

/// Detect which platform a URL belongs to for cookie file selection.
fn detect_platform(url: &str) -> Option<&'static str> {
    let lower = url.to_lowercase();
    if lower.contains("twitter.com") || lower.contains("x.com") {
        Some("twitter")
    } else if lower.contains("youtube.com") || lower.contains("youtu.be") {
        Some("youtube")
    } else if lower.contains("tiktok.com") {
        Some("tiktok")
    } else if lower.contains("vk.com") || lower.contains("vk.ru") || lower.contains("vkvideo.ru") {
        Some("vk")
    } else {
        None
    }
}

/// Extra name tokens that identify a cookie profile for a platform, so a
/// profile saved as "vk_video" or "vkontakte" still serves vk.com URLs.
/// `name.contains(alias)` is the test: VK profile names survive the
/// "VK Video" → "vk_video" sanitizer and the "vkontakte" legacy name.
/// Twitter aliases cover profiles saved as either domain (x.com links map
/// to the same "twitter" platform as twitter.com links).
fn platform_aliases(platform: Option<&str>) -> &'static [&'static str] {
    match platform {
        Some("vk") => &["vk", "vkontakte"],
        Some("twitter") => &["x.com", "twitter"],
        _ => &[],
    }
}

/// Pick which stored cookies.txt applies to a URL, if any.
///
/// Every saved profile matches when its name appears in the URL (case-
/// insensitive), so a profile named "insta" serves instagram.com links. The
/// longest matching name wins because it is the most specific (an "instagram"
/// profile beats a shorter "insta"). The original platform names still match
/// through [`detect_platform`] so x.com → "twitter" and youtu.be → "youtube"
/// keep working for existing users, and vk.com URLs accept any profile whose
/// name contains "vk" or "vkontakte" (e.g. "vk", "vk_video", "vkontakte").
fn settings_cookie_file(dir: &std::path::Path, url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    let aliases = platform_aliases(detect_platform(&lower));
    let mut candidates: Vec<(usize, String, std::path::PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = stem.to_lowercase();
            let matches_url = !name.is_empty() && lower.contains(&name);
            let matches_alias = aliases.iter().any(|alias| name.contains(*alias));
            if matches_url || matches_alias {
                candidates.push((name.len(), name, path));
            }
        }
    }
    if let Some(platform) = detect_platform(&lower) {
        let path = dir.join(format!("{platform}.txt"));
        if path.is_file() {
            candidates.push((platform.len(), platform.to_string(), path));
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, _, path)| path.to_string_lossy().to_string())
}

/// Resolve the path to a stored cookies.txt for a URL if one exists.
fn cookies_file_for_url(app: &AppHandle, url: &str) -> Option<String> {
    settings_cookie_file(&pluck::app_cookies_dir(app), url)
}

/// Import a cookies.txt file under a user-chosen name (e.g. "insta").
#[tauri::command]
pub fn import_cookie(app: AppHandle, name: String, source_path: String) -> Result<String, String> {
    let name = sanitize_cookie_name(&name)?;
    let dest = pluck::app_cookies_dir(&app).join(format!("{name}.txt"));
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::copy(&source_path, &dest).map_err(|e| format!("failed to copy cookies: {e}"))?;
    Ok(name)
}

/// Remove a stored cookies.txt by profile name.
#[tauri::command]
pub fn delete_cookie(app: AppHandle, name: String) -> Result<(), String> {
    let name = sanitize_cookie_name(&name)?;
    let path = pluck::app_cookies_dir(&app).join(format!("{name}.txt"));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// List all stored cookie profiles (name + saved file path).
#[tauri::command]
pub fn list_cookies(app: AppHandle) -> Vec<CookieEntry> {
    let dir = pluck::app_cookies_dir(&app);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            out.push(CookieEntry {
                name: name.to_string(),
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cookie_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yt-plucker-cookie-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cookie_name_sanitizes_safely() {
        assert_eq!(sanitize_cookie_name("Insta").unwrap(), "insta");
        assert_eq!(sanitize_cookie_name(" insta.com ").unwrap(), "insta.com");
        assert_eq!(sanitize_cookie_name("../../evil").unwrap(), "evil");
        assert_eq!(sanitize_cookie_name("x/y").unwrap(), "x_y");
        assert!(sanitize_cookie_name("").is_err());
        assert!(sanitize_cookie_name("...").is_err());
        assert!(sanitize_cookie_name("///").is_err());
    }

    #[test]
    fn matching_uses_name_substring_in_url() {
        let dir = test_cookie_dir("substring");
        std::fs::write(dir.join("insta.txt"), "").unwrap();
        assert_eq!(
            settings_cookie_file(&dir, "https://www.instagram.com/reel/abc/").as_deref(),
            Some(dir.join("insta.txt").to_str().unwrap())
        );
        assert_eq!(settings_cookie_file(&dir, "https://example.com/"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn longest_name_wins() {
        let dir = test_cookie_dir("longest");
        std::fs::write(dir.join("insta.txt"), "").unwrap();
        std::fs::write(dir.join("instagram.txt"), "").unwrap();
        assert_eq!(
            settings_cookie_file(&dir, "https://www.instagram.com/reel/abc123/").as_deref(),
            Some(dir.join("instagram.txt").to_str().unwrap())
        );
        // shorter profile still matches when it is the only one available
        std::fs::remove_file(dir.join("instagram.txt")).unwrap();
        assert_eq!(
            settings_cookie_file(&dir, "https://www.instagram.com/reel/abc123/").as_deref(),
            Some(dir.join("insta.txt").to_str().unwrap())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_platform_names_still_match() {
        let dir = test_cookie_dir("legacy");
        std::fs::write(dir.join("twitter.txt"), "").unwrap();
        std::fs::write(dir.join("youtube.txt"), "").unwrap();
        // x.com URLs never contain "twitter"; the legacy mapping supplies it.
        assert_eq!(
            settings_cookie_file(&dir, "https://x.com/somebody/status/123").as_deref(),
            Some(dir.join("twitter.txt").to_str().unwrap())
        );
        // youtu.be short links don't contain "youtube" either.
        assert_eq!(
            settings_cookie_file(&dir, "https://youtu.be/abc123").as_deref(),
            Some(dir.join("youtube.txt").to_str().unwrap())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vk_profiles_match_across_all_vk_hosts() {
        let dir = test_cookie_dir("vk");
        std::fs::write(dir.join("vk.txt"), "").unwrap();
        std::fs::write(dir.join("vk_video.txt"), "").unwrap();
        std::fs::write(dir.join("vkontakte.txt"), "").unwrap();
        for url in [
            "https://vk.com/video-100500_456239017",
            "https://m.vk.ru/video-100500_456239017",
            "https://vkvideo.ru/video-100500_456239017",
        ] {
            assert_eq!(
                settings_cookie_file(&dir, url).as_deref(),
                Some(dir.join("vkontakte.txt").to_str().unwrap()),
                "longest vk profile should win for {url}"
            );
        }
        // a non-vk URL must not be served by "vk"-titled profiles
        assert_eq!(
            settings_cookie_file(&dir, "https://github.com/").as_deref(),
            None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn x_com_profiles_serve_twitter_links() {
        let dir = test_cookie_dir("xcom");
        std::fs::write(dir.join("x.com.txt"), "").unwrap();
        // A profile named after either domain must serve both hosts.
        assert_eq!(
            settings_cookie_file(&dir, "https://twitter.com/user/status/123").as_deref(),
            Some(dir.join("x.com.txt").to_str().unwrap())
        );
        assert_eq!(
            settings_cookie_file(&dir, "https://x.com/user/status/123").as_deref(),
            Some(dir.join("x.com.txt").to_str().unwrap())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vk_profile_sanitized_from_friendly_name() {
        // "VK Video" becomes "vk_video"; a vk.com URL must still find it
        let dir = test_cookie_dir("vk_friendly");
        assert_eq!(sanitize_cookie_name("VK Video").unwrap(), "vk_video");
        std::fs::write(dir.join("vk_video.txt"), "").unwrap();
        assert_eq!(
            settings_cookie_file(&dir, "https://vk.com/video-100500_456239017").as_deref(),
            Some(dir.join("vk_video.txt").to_str().unwrap())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[tauri::command]
pub async fn fetch_metadata(
    app: AppHandle,
    url: String,
    playlist_mode: bool,
    cookies_from_browser: Option<String>,
) -> Result<Metadata, String> {
    let mut args: Vec<String> = vec!["-J".into(), "--no-warnings".into()];
    if playlist_mode {
        // full -J on a large playlist takes minutes; flat is instant
        args.extend(["--flat-playlist".into(), "--yes-playlist".into()]);
    } else {
        args.push("--no-playlist".into());
    }
    // Read login cookies from a browser to get past YouTube's bot check.
    if let Some(b) = cookies_from_browser.as_deref() {
        if !b.is_empty() && b != "none" {
            args.extend(["--cookies-from-browser".into(), b.into()]);
        }
    }
    // Auto-apply stored cookies.txt if the URL matches a profile.
    if let Some(cf) = cookies_file_for_url(&app, &url) {
        args.extend(["--cookies".into(), cf]);
    }
    args.push(url.clone());

    let output = engine::ytdlp_command(&app)?
        .env("PYTHONIOENCODING", "utf-8")
        .args(args)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr
            .lines()
            .find(|l| l.contains("ERROR"))
            .unwrap_or("yt-dlp could not read this URL")
            .to_string();
        // Analyze failures get the same plain-language treatment as download
        // failures; unknown causes keep the raw line since the user is
        // debugging their input.
        let (kind, friendly) = errors::classify(&msg, Some(&url));
        return Err(if kind == FailureKind::Other { msg } else { friendly });
    }

    let v: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("unexpected yt-dlp output: {e}"))?;

    if v.get("_type").and_then(Value::as_str) == Some("playlist") {
        let entries = v
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let titles = entries
            .iter()
            .take(500)
            .map(|e| {
                e.get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string()
            })
            .collect();
        let count = v
            .get("playlist_count")
            .and_then(Value::as_u64)
            .unwrap_or(entries.len() as u64);
        let thumbnail =
            last_thumbnail(&v).or_else(|| entries.first().and_then(last_thumbnail));
        Ok(Metadata {
            kind: "playlist".into(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Playlist")
                .into(),
            thumbnail,
            duration: None,
            heights: vec![],
            entry_count: Some(count),
            entries: titles,
            source: extractor_of(&v),
        })
    } else {
        let mut heights: Vec<u32> = v
            .get("formats")
            .and_then(Value::as_array)
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| f.get("height").and_then(Value::as_u64))
                    .map(|h| h as u32)
                    .collect()
            })
            .unwrap_or_default();
        heights.sort_unstable();
        heights.dedup();
        heights.reverse();
        Ok(Metadata {
            kind: "video".into(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .into(),
            thumbnail: v
                .get("thumbnail")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| last_thumbnail(&v)),
            duration: v.get("duration").and_then(Value::as_f64),
            heights,
            entry_count: None,
            entries: vec![],
            source: extractor_of(&v),
        })
    }
}

#[tauri::command]
pub async fn start_pluck(
    app: AppHandle,
    state: State<'_, PluckState>,
    job_id: u64,
    url: String,
    quality: String,
    dest_dir: String,
    playlist_mode: bool,
    cookies_from_browser: Option<String>,
) -> Result<(), String> {
    let archive = pluck::archive_path(&app, job_id)?;
    let cookies_file = cookies_file_for_url(&app, &url);
    if let Some(cf) = &cookies_file {
        // surface which cookies.txt got applied so a missing match is obvious
        let _ = app.emit(
            "pluck://cookies",
            serde_json::json!({ "jobId": job_id, "file": cf }),
        );
    }
    let args = pluck::build_args(
        &url,
        &quality,
        &dest_dir,
        playlist_mode,
        &archive.to_string_lossy(),
        None,
        &[],
        None,
        cookies_from_browser.as_deref(),
        cookies_file.as_deref(),
    )?;

    let (mut rx, child) = engine::ytdlp_command(&app)?
        .env("PYTHONIOENCODING", "utf-8")
        .args(args.clone())
        .spawn()
        .map_err(|e| e.to_string())?;

    let cancelled = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    state.0.lock().unwrap().insert(
        job_id,
        PluckJob {
            pid: child.pid(),
            cancelled: cancelled.clone(),
            paused: paused.clone(),
        },
    );

    let app_handle = app.clone();
    let archive_task = archive.clone();
    tauri::async_runtime::spawn(async move {
        let mut throttle = Throttle::new(Duration::from_millis(150));
        // One engine self-heal per job: on a stale-engine failure, download
        // the current yt-dlp and rerun once. Pause/cancel never self-heal.
        let mut healed = false;
        'attempts: loop {
            let mut worst: Option<(FailureKind, String)> = None;
            while let Some(event) = rx.recv().await {
                match event {
                    // progress arrives on stderr in quiet mode (--print enables it),
                    // so both streams are parsed identically
                    CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        for line in text.lines() {
                            if let Some(failure) =
                                pluck::handle_line(&app_handle, job_id, line, &mut throttle)
                            {
                                worst = errors::worse(worst, Some(failure));
                            }
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        let was_cancelled = cancelled.load(Ordering::SeqCst);
                        let was_paused = paused.load(Ordering::SeqCst);
                        if was_paused {
                            // Keep the archive so a resume continues where it left off.
                            let _ = app_handle.emit(
                                "pluck://paused",
                                pluck::PausedPayload { job_id },
                            );
                        } else {
                            let ok = payload.code == Some(0) && !was_cancelled;
                            if !ok
                                && !was_cancelled
                                && !healed
                                && matches!(
                                    worst.as_ref().map(|(k, _)| *k),
                                    Some(FailureKind::StaleEngine)
                                )
                            {
                                healed = true;
                                let _ = app_handle.emit(
                                    "pluck://status",
                                    pluck::StatusPayload {
                                        job_id,
                                        message: "Updating downloader…".into(),
                                    },
                                );
                                match engine::update_engine(&app_handle).await {
                                    Ok(_) => {
                                        if let Ok((next_rx, next_child)) =
                                            engine::ytdlp_command(&app_handle)
                                                .and_then(|c| {
                                                    c.env("PYTHONIOENCODING", "utf-8")
                                                        .args(args.clone())
                                                        .spawn()
                                                        .map_err(|e| e.to_string())
                                                })
                                        {
                                            if let Some(j) = app_handle
                                                .state::<PluckState>()
                                                .0
                                                .lock()
                                                .unwrap()
                                                .get_mut(&job_id)
                                            {
                                                j.pid = next_child.pid();
                                            }
                                            rx = next_rx;
                                            continue 'attempts;
                                        }
                                    }
                                    Err(_) => {}
                                }
                            }
                            // a fully-finished pluck no longer needs its resume archive
                            if ok {
                                let _ = std::fs::remove_file(&archive_task);
                            }
                            let done = if ok {
                                DonePayload::ok(job_id, true, false)
                            } else if was_cancelled {
                                DonePayload::cancelled(job_id)
                            } else {
                                DonePayload::failed(job_id, &worst)
                            };
                            let _ = app_handle.emit("pluck://done", done);
                        }
                        break;
                    }
                    _ => {}
                }
            }
            break;
        }
        app_handle
            .state::<PluckState>()
            .0
            .lock()
            .unwrap()
            .remove(&job_id);
    });

    Ok(())
}

#[tauri::command]
pub fn cancel_pluck(state: State<'_, PluckState>, job_id: u64) -> Result<(), String> {
    let jobs = state.0.lock().unwrap();
    let job = jobs.get(&job_id).ok_or("pluck not found")?;
    job.cancelled.store(true, Ordering::SeqCst);
    pluck::kill_tree(job.pid);
    Ok(())
}

#[tauri::command]
pub fn pause_pluck(state: State<'_, PluckState>, job_id: u64) -> Result<(), String> {
    let jobs = state.0.lock().unwrap();
    let job = jobs.get(&job_id).ok_or("pluck not found")?;
    job.paused.store(true, Ordering::SeqCst);
    pluck::kill_tree(job.pid);
    Ok(())
}
