//! Persist `AppSettings` back to the platform config file.

use anyhow::{Context, Result};

use crate::models::AppSettings;

/// Write the given settings to the standard settings path (creating the
/// directory if needed). Returns the path written to.
pub fn save_settings(settings: &AppSettings) -> Result<std::path::PathBuf> {
    let path = AppSettings::path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let toml = toml::to_string_pretty(settings).context("could not serialise settings")?;
    std::fs::write(&path, toml).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}
