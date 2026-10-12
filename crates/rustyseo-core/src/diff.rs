//! Session diffing: compare two crawls of the same site (recurring audits).

use std::collections::HashMap;

use crate::models::PageSummary;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffKind {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FieldChange {
    pub field: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffEntry {
    pub url: String,
    pub kind: DiffKind,
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
}

/// Compare two crawls, keyed by URL. `old` is the earlier session, `new` the
/// later one. Only added/removed/changed pages are returned; `stats` counts
/// everything, including unchanged pages.
pub fn diff_sessions(old: &[PageSummary], new: &[PageSummary]) -> (Vec<DiffEntry>, DiffStats) {
    let old_by_url: HashMap<&str, &PageSummary> =
        old.iter().map(|page| (page.url.as_str(), page)).collect();
    let new_by_url: HashMap<&str, &PageSummary> =
        new.iter().map(|page| (page.url.as_str(), page)).collect();

    let mut entries = Vec::new();
    let mut stats = DiffStats::default();

    for page in old {
        if !new_by_url.contains_key(page.url.as_str()) {
            stats.removed += 1;
            entries.push(DiffEntry {
                url: page.url.clone(),
                kind: DiffKind::Removed,
                changes: Vec::new(),
            });
        }
    }

    for page in new {
        match old_by_url.get(page.url.as_str()) {
            None => {
                stats.added += 1;
                entries.push(DiffEntry {
                    url: page.url.clone(),
                    kind: DiffKind::Added,
                    changes: Vec::new(),
                });
            }
            Some(before) => {
                let changes = compare_pages(before, page);
                if changes.is_empty() {
                    stats.unchanged += 1;
                } else {
                    stats.changed += 1;
                    entries.push(DiffEntry {
                        url: page.url.clone(),
                        kind: DiffKind::Changed,
                        changes,
                    });
                }
            }
        }
    }

    // Removed first, then added, then changed — each alphabetical.
    entries.sort_by(|a, b| {
        let rank = |kind: DiffKind| match kind {
            DiffKind::Removed => 0,
            DiffKind::Added => 1,
            DiffKind::Changed => 2,
        };
        rank(a.kind).cmp(&rank(b.kind)).then(a.url.cmp(&b.url))
    });

    (entries, stats)
}

fn compare_pages(old: &PageSummary, new: &PageSummary) -> Vec<FieldChange> {
    let mut changes = Vec::new();

    let mut push = |field: &str, old_value: String, new_value: String| {
        if old_value != new_value {
            changes.push(FieldChange {
                field: field.to_string(),
                old: old_value,
                new: new_value,
            });
        }
    };

    push("Status", old.status.clone(), new.status.clone());
    push("Title", old.title.clone(), new.title.clone());
    push("Description", old.description.clone(), new.description.clone());
    push("H1", old.h1.clone(), new.h1.clone());
    push(
        "Word count",
        old.word_count.to_string(),
        new.word_count.to_string(),
    );
    push(
        "Indexability",
        old.indexability.clone(),
        new.indexability.clone(),
    );
    push(
        "Canonical",
        old.canonical_target.clone().unwrap_or_default(),
        new.canonical_target.clone().unwrap_or_default(),
    );

    changes
}
