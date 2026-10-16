//! Persistent state for the AI chat overlay.
//!
//! Stores UI state like the last selected model in `~/.config/faz/ai_chat_state.json`.
//! Load once at overlay start; save when the user switches models.
//!
//! # Why two model-list caches?
//!
//! Faz has two on-disk caches of the assistant's `/models` response that
//! intentionally do *not* share a file:
//!
//! | Surface | Path | TTL | Purpose |
//! |---|---|---|---|
//! | `faz ai` TUI | `~/.cache/kaku/assistant_models.json` | 30 min, base_url-keyed | Driving the dropdown picker; refreshed every TUI session so a freshly-added model shows up. |
//! | Cmd+L overlay (here) | `~/.config/faz/ai_chat_state.json` `cached_models` | persistent, no expiry | Avoid a "loading models…" flash on the very next overlay open; merged with `chat_model` and the curated `chat_model_choices` list. |
//!
//! Merging them would force one TTL policy on both surfaces and add a
//! cross-binary file lock. The caches are tiny (a list of model IDs) and the
//! divergence is intentional. If you change one, document why the other is
//! still the way it is.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct StateFile {
    version: u32,
    /// Last model selected by the user via Shift+Tab.
    last_model: Option<String>,
    /// Cached model list from the last successful /models fetch.
    #[serde(default)]
    cached_models: Vec<String>,
}

/// Load the last selected model from disk. Returns None on any error (non-fatal).
pub fn load_last_model() -> Option<String> {
    try_load().ok().flatten().and_then(|f| f.last_model)
}

/// Load the cached model list from the previous session.
pub fn load_cached_models() -> Vec<String> {
    try_load()
        .ok()
        .flatten()
        .map(|f| f.cached_models)
        .unwrap_or_default()
}

fn try_load() -> Result<Option<StateFile>> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let file: StateFile =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(file))
}

fn load_or_default() -> StateFile {
    try_load()
        .unwrap_or_else(|e| {
            log::warn!("Could not load AI chat state: {e}");
            None
        })
        .unwrap_or_default()
}

/// Save the last selected model to disk atomically.
pub fn save_last_model(model: &str) -> Result<()> {
    let path = state_path()?;
    let mut file = load_or_default();
    file.version = 1;
    file.last_model = Some(model.to_string());
    write_state(&path, &file)
}

/// Persist the fetched model list so the next session starts without a loading delay.
pub fn save_cached_models(models: &[String]) -> Result<()> {
    let path = state_path()?;
    let mut file = load_or_default();
    file.version = 1;
    file.cached_models = models.to_vec();
    write_state(&path, &file)
}

fn write_state(path: &std::path::PathBuf, file: &StateFile) -> Result<()> {
    let json = serde_json::to_string_pretty(file).context("serialize state")?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn state_path() -> Result<PathBuf> {
    let user_config_path = config::user_config_path();
    let config_dir = user_config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid user config path"))?;
    Ok(config_dir.join("ai_chat_state.json"))
}
