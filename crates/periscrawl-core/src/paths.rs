//! Platform data directories, with a one-time migration from the project's
//! pre-rename (`rustyseo`) location so existing sessions, settings, exports
//! and logs survive the rebrand.

use std::path::PathBuf;
use std::sync::Once;

use directories::ProjectDirs;

static MIGRATE: Once = Once::new();
const APP_DIR_NAME: &str = "periscrawl";
const LEGACY_DIR_NAME: &str = "rustyseo";
const LEGACY_DB_FILE: &str = "rustyseo.db";

/// Project directories under the current name. The first call migrates data
/// written under the previous name when a legacy directory exists and the
/// new one does not.
pub fn project_dirs() -> ProjectDirs {
    MIGRATE.call_once(migrate_legacy_data_dir);
    ProjectDirs::from("", "", APP_DIR_NAME).expect("Could not determine project directories")
}

/// Platform data directory (the parent of the SQLite database).
pub fn data_dir() -> PathBuf {
    project_dirs().data_dir().to_path_buf()
}

fn migrate_legacy_data_dir() {
    let new_dir = match ProjectDirs::from("", "", APP_DIR_NAME) {
        Some(dirs) => dirs.data_dir().to_path_buf(),
        None => return,
    };
    if new_dir.exists() {
        return;
    }
    let legacy_dir = match ProjectDirs::from("", "", LEGACY_DIR_NAME) {
        Some(dirs) => dirs.data_dir().to_path_buf(),
        None => return,
    };
    if !legacy_dir.exists() {
        return;
    }
    if std::fs::rename(&legacy_dir, &new_dir).is_err() {
        return;
    }

    // The database file carries the old name too.
    let legacy_db = new_dir.join(LEGACY_DB_FILE);
    let new_db = new_dir.join("periscrawl.db");
    if legacy_db.exists() && !new_db.exists() {
        let _ = std::fs::rename(&legacy_db, &new_db);
    }
    tracing::info!(
        "Migrated data directory {} -> {}",
        legacy_dir.display(),
        new_dir.display()
    );
}
