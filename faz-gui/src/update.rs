use anyhow::anyhow;
use config::proxy::{apply_to_command, detect_system_proxy};
use config::wezterm_version;
use serde::*;
use std::cmp::Ordering as CmpOrdering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Release {
    pub url: String,
    pub body: String,
    pub html_url: String,
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    pub size: usize,
    pub url: String,
    pub browser_download_url: String,
}

/// Metadata written alongside a staged (pre-downloaded) update.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StagedUpdateInfo {
    pub tag: String,
    pub body: String,
    pub staged_at: u64,
    pub verified: bool,
    /// Path to the extracted Faz.app inside the staged_update directory.
    pub app_path: String,
}

const STAGED_DIR_NAME: &str = "staged_update";
const STAGED_LOCK_NAME: &str = "staged_update.lock";
const STAGED_META_NAME: &str = "metadata.json";
/// Staged updates older than this are considered expired.
const STAGED_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

fn staged_dir() -> PathBuf {
    config::DATA_DIR.join(STAGED_DIR_NAME)
}

fn staged_meta_path() -> PathBuf {
    staged_dir().join(STAGED_META_NAME)
}

/// Returns info about a staged update if one is present, verified, and not expired.
pub fn staged_update_available() -> Option<StagedUpdateInfo> {
    let meta_path = staged_meta_path();
    let content = std::fs::read_to_string(&meta_path).ok()?;
    let info: StagedUpdateInfo = match serde_json::from_str(&content) {
        Ok(info) => info,
        Err(e) => {
            log::warn!("staged update metadata is corrupted: {}", e);
            return None;
        }
    };
    if !info.verified {
        return None;
    }
    // Check the extracted app still exists.
    let app = PathBuf::from(&info.app_path);
    if !app.exists() {
        return None;
    }
    // A staged update whose version is not newer than what we're running is
    // stale: the update was already applied but the post-update marker
    // cleanup never ran. Drop it so the menu stops offering a pointless
    // "Restart to Update", and so update_checker's startup cleanup removes
    // the directory.
    if !is_newer(&info.tag, wezterm_version()) {
        log::info!(
            "staged update {} is not newer than current {}, removing",
            info.tag,
            wezterm_version()
        );
        cleanup_staged_update();
        return None;
    }
    // Check expiry.
    let now = now_unix_secs();
    if now.saturating_sub(info.staged_at) > STAGED_MAX_AGE_SECS {
        log::info!("staged update {} expired, removing", info.tag);
        cleanup_staged_update();
        return None;
    }
    Some(info)
}

/// Remove the staged_update directory entirely.
///
/// Acquires the staging lock before removing so this call cannot race with a
/// concurrent `download_and_stage_update`. If another process holds the lock
/// the cleanup is skipped silently: that process owns the directory and will
/// clean it up itself.
pub fn cleanup_staged_update() {
    let dir = staged_dir();
    if dir.exists() {
        let _lock = match StagedUpdateLock::try_acquire() {
            Ok(lock) => lock,
            Err(_) => return,
        };
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("cleaned up staged update directory"),
            Err(e) => log::warn!("failed to clean staged update directory: {}", e),
        }
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// RAII guard around an exclusive `flock` on the staging lock file.
///
/// Two Faz processes started at once would otherwise race on
/// `staged_update/`: one tears it down with `remove_dir_all` while the other
/// is mid-extract. Holding `flock(LOCK_EX | LOCK_NB)` for the duration of
/// `download_and_stage_update` serializes them. The kernel automatically
/// releases the lock when the file handle drops or the process exits, so the
/// lock survives crashes without a stale-PID cleanup dance.
struct StagedUpdateLock {
    _file: std::fs::File,
}

impl StagedUpdateLock {
    fn try_acquire() -> anyhow::Result<Self> {
        let dir = config::DATA_DIR.clone();
        config::create_user_owned_dirs(&dir)
            .map_err(|e| anyhow!("failed to create data dir for staging lock: {}", e))?;
        let path = dir.join(STAGED_LOCK_NAME);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow!("failed to open staging lock {}: {}", path.display(), e))?;
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: fd is owned by `file` which outlives this call.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if matches!(err.raw_os_error(), Some(libc::EWOULDBLOCK)) {
                anyhow::bail!("another Faz process is already staging an update");
            }
            return Err(anyhow!(
                "failed to acquire staging lock {}: {}",
                path.display(),
                err
            ));
        }
        Ok(Self { _file: file })
    }
}

fn curl_get_release_json(url: &str, proxy: &Option<String>) -> anyhow::Result<Release> {
    use std::process::Command;

    let mut cmd = Command::new("/usr/bin/curl");
    cmd.arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--connect-timeout")
        .arg("15")
        .arg("--user-agent")
        .arg(format!("kaku/{}", wezterm_version()))
        .arg(url);
    apply_to_command(&mut cmd, proxy);

    let out = cmd.output().map_err(|e| anyhow!("curl failed: {}", e))?;
    if !out.status.success() {
        anyhow::bail!(
            "curl request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("failed to parse release JSON: {}", e))
}

#[allow(unused)]
pub fn get_nightly_release_info() -> anyhow::Result<Release> {
    let proxy = detect_system_proxy();
    curl_get_release_json(
        "https://api.github.com/repos/tw93/Faz/releases/tags/nightly",
        &proxy,
    )
}

fn is_newer(latest: &str, current: &str) -> bool {
    let latest = latest.trim_start_matches('v');
    let current = current.trim_start_matches('v');

    // If latest is a WezTerm-style date version (e.g. 20240203-...) and current is SemVer (e.g. 0.1.0),
    // treat the date version as older/different system.
    if latest.starts_with("20") && latest.contains('-') && !current.starts_with("20") {
        return false;
    }

    match compare_versions(latest, current) {
        Some(CmpOrdering::Greater) => true,
        Some(_) => false,
        None => latest != current,
    }
}

fn compare_versions(left: &str, right: &str) -> Option<CmpOrdering> {
    let left = parse_version_numbers(left)?;
    let right = parse_version_numbers(right)?;
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        let l = left.get(idx).copied().unwrap_or(0);
        let r = right.get(idx).copied().unwrap_or(0);
        match l.cmp(&r) {
            CmpOrdering::Equal => {}
            non_eq => return Some(non_eq),
        }
    }
    Some(CmpOrdering::Equal)
}

fn parse_version_numbers(version: &str) -> Option<Vec<u64>> {
    let cleaned = version.trim().trim_start_matches(['v', 'V']);
    let mut out = Vec::new();
    for part in cleaned.split('.') {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let value = digits.parse::<u64>().ok()?;
        out.push(value);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

// ---------------------------------------------------------------------------

/// Initialize the notification system and surface a just-completed update.
///
/// This deliberately does NOT poll for new releases: Faz never downloads or
/// stages an update on its own. Updating is a manual action (`faz update` or
/// the Check for Updates menu item).
pub fn start_update_checker() {
    static CHECKER_STARTED: AtomicBool = AtomicBool::new(false);
    if let Ok(false) =
        CHECKER_STARTED.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
    {
        // Initialize the notification system up front so macOS shows the
        // permission dialog on first launch, rather than lazily when a
        // notification fires. This runs just after the first paint (see
        // paint_impl) so the init cost stays off the first-frame path.
        wezterm_toast_notification::macos_initialize();

        // Register callback so a notification click asks for confirmation
        // before doing anything destructive. Applying an update closes every
        // window and stops running tasks, so a stray click must not trigger it
        // silently. Menu "Restart to Update" and staged "Check for Updates"
        // share the same confirm path. The confirmed action picks the staged
        // fast-path or the terminal-tab flow inside apply_update_now().
        wezterm_toast_notification::set_update_click_callback(|| {
            crate::frontend::confirm_and_apply_update();
        });

        // Check if we just completed an update and show notification
        check_update_completed();
    }
}

fn check_update_completed() {
    let marker_file = config::DATA_DIR.join("update_completed");
    if !marker_file.exists() {
        return;
    }

    // Check if marker file is recent (within last 5 minutes)
    // This prevents showing stale notifications from old failed updates
    let is_recent = marker_file
        .metadata()
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|e| e.as_secs() < 300).unwrap_or(false))
        .unwrap_or(false);

    if is_recent {
        if let Ok(version) = std::fs::read_to_string(&marker_file) {
            let version = version.trim();
            if !version.is_empty() {
                log::info!("update_completed: showing notification for {}", version);
                wezterm_toast_notification::persistent_toast_notification(
                    "Updated",
                    &format!("Successfully updated to {}.", version),
                );
            }
        }
    } else {
        log::info!("update_completed: skipping stale marker file");
    }

    // Always remove the marker file, and also clean up the staged_update dir
    // since the update succeeded.
    let _ = std::fs::remove_file(&marker_file);
    cleanup_staged_update();
}

// ---------------------------------------------------------------------------
// Helper script for restart-to-update (duplicated from kaku/src/update.rs
// because faz-gui does not depend on the kaku crate).
// ---------------------------------------------------------------------------

/// Resolve the installed Faz.app path, matching the logic in the CLI crate.
pub fn resolve_target_app_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FAZ_UPDATE_TARGET_APP") {
        let app = PathBuf::from(path);
        if app.ends_with("Faz.app") {
            return Ok(app);
        }
        anyhow::bail!("FAZ_UPDATE_TARGET_APP must point to Faz.app");
    }

    let exe = std::env::current_exe().map_err(|e| anyhow!("resolve current executable: {}", e))?;
    for ancestor in exe.ancestors() {
        if ancestor
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("Faz.app"))
            .unwrap_or(false)
        {
            return Ok(ancestor.to_path_buf());
        }
    }

    let default_app = PathBuf::from("/Applications/Faz.app");
    if default_app.exists() {
        return Ok(default_app);
    }

    anyhow::bail!("cannot locate installed Faz.app")
}

pub fn write_update_helper_script(script_path: &Path) -> anyhow::Result<()> {
    // This is the same helper script as kaku/src/update.rs::write_helper_script.
    let script = include_str!("../../scripts/update_helper.sh");
    std::fs::write(script_path, script)
        .map_err(|e| anyhow!("failed to write helper script: {}", e))?;
    let status = std::process::Command::new("/bin/chmod")
        .arg("700")
        .arg(script_path)
        .status()
        .map_err(|e| anyhow!("chmod failed: {}", e))?;
    if !status.success() {
        anyhow::bail!("chmod helper script failed");
    }
    Ok(())
}

pub fn spawn_update_helper(
    script: &Path,
    target_app: &Path,
    new_app: &Path,
    work_dir: &Path,
) -> anyhow::Result<()> {
    use std::process::{Command, Stdio};

    // Validate paths end with Faz.app.
    if !target_app.ends_with("Faz.app") {
        anyhow::bail!(
            "target_app must end with Faz.app: {}",
            target_app.display()
        );
    }
    if !new_app.ends_with("Faz.app") {
        anyhow::bail!("new_app must end with Faz.app: {}", new_app.display());
    }

    Command::new("/usr/bin/nohup")
        .arg("/bin/bash")
        .arg(script)
        .arg(target_app)
        .arg(new_app)
        .arg(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn update helper: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_numeric_comparison() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.2.0", "0.11.0"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(is_newer("v0.1.2", "0.1.1"));
    }

    #[test]
    fn version_0_9_is_newer_than_0_8() {
        assert!(is_newer("V0.9.0", "0.8.0"));
        assert!(is_newer("0.9.0", "0.8.0"));
        assert!(!is_newer("0.8.0", "0.9.0"));
    }

    #[test]
    fn staged_metadata_roundtrip() {
        let info = StagedUpdateInfo {
            tag: "V0.9.0".to_string(),
            body: "some release notes".to_string(),
            staged_at: 1700000000,
            verified: true,
            app_path: "/tmp/test/Faz.app".to_string(),
        };
        let json = serde_json::to_string_pretty(&info).unwrap();
        let parsed: StagedUpdateInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tag, "V0.9.0");
        assert!(parsed.verified);
        assert_eq!(parsed.app_path, "/tmp/test/Faz.app");
    }


}
