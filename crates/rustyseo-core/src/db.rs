use crate::crawler::PageData;
use directories::ProjectDirs;
use rusqlite::{Connection, Result as SqliteResult, params};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

/// Legacy rows (written before sessions existed) live under this session id.
const LEGACY_SESSION_ID: i64 = 0;

/// One crawl run. Every crawl gets a row so results are addressable and
/// comparable across runs.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: i64,
    pub name: Option<String>,
    pub start_url: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub pages_fetched: usize,
    pub pages_failed: usize,
    pub max_pages: Option<usize>,
}

/// Lightweight page row for listing (avoids deserializing the full PageData).
#[derive(Debug, Clone, Serialize)]
pub struct SessionPageRow {
    pub page_id: i64,
    pub url: String,
    pub status: String,
    pub title: String,
    pub title_len: usize,
    pub description_len: usize,
    pub indexability: String,
    pub word_count: usize,
    pub size: usize,
}

fn get_bookmarks_path() -> PathBuf {
    ProjectDirs::from("", "", "rustyseo")
        .expect("Could not determine project directories")
        .data_dir()
        .join("bookmarks.json")
}

pub fn get_db_path() -> PathBuf {
    ProjectDirs::from("", "", "rustyseo")
        .expect("Could not determine project directories")
        .data_dir()
        .join("rustyseo.db")
}

pub fn init_db() {
    // Ensure the data directory exists before writing anything into it. This
    // matters for headless use (fresh machines, CI, isolated HOME dirs) where
    // no UI has created app directories yet.
    let bookmarks_path = get_bookmarks_path();
    if let Some(parent) = bookmarks_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create app data directory");
    }
    if !bookmarks_path.exists() {
        fs::write(&bookmarks_path, "[]").expect("Failed to create bookmarks.json");
    }

    let db_path = get_db_path();
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create app data directory");
    }
    let conn = Connection::open(&db_path).expect("Failed to open database");
    migrate(&conn).expect("Failed to migrate database");

    // Legacy table kept for backward compatibility with external tooling.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS crawl_sessions (
            id INTEGER PRIMARY KEY,
            start_time TEXT NOT NULL,
            end_time TEXT,
            total_pages INTEGER DEFAULT 0
        )",
        (),
    )
    .expect("Failed to create crawl_sessions table");
}

/// Schema migrations, tracked with `PRAGMA user_version`.
///
/// v0: pages(id INTEGER PRIMARY KEY, data TEXT)
/// v1: sessions table; pages(session_id, page_id, url, status, data) so that
///     every crawl's results are addressable per session and old crawls are
///     never overwritten by new ones.
fn migrate(conn: &Connection) -> SqliteResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= 1 {
        return Ok(());
    }

    let legacy_pages = table_exists(conn, "pages") && !column_exists(conn, "pages", "session_id");
    if legacy_pages {
        conn.execute_batch(&format!(
            "BEGIN;
             CREATE TABLE pages_v1 (
                 session_id INTEGER NOT NULL DEFAULT {LEGACY_SESSION_ID},
                 page_id INTEGER NOT NULL,
                 url TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT '',
                 fetched_at TEXT,
                 data TEXT NOT NULL,
                 PRIMARY KEY (session_id, page_id)
             );
             INSERT INTO pages_v1 (session_id, page_id, url, status, data)
                 SELECT {LEGACY_SESSION_ID}, id, '', '', data FROM pages;
             DROP TABLE pages;
             ALTER TABLE pages_v1 RENAME TO pages;
             COMMIT;"
        ))?;
    } else {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS pages (
                 session_id INTEGER NOT NULL DEFAULT {LEGACY_SESSION_ID},
                 page_id INTEGER NOT NULL,
                 url TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT '',
                 fetched_at TEXT,
                 data TEXT NOT NULL,
                 PRIMARY KEY (session_id, page_id)
             );"
        ))?;
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT,
             start_url TEXT NOT NULL,
             started_at TEXT NOT NULL,
             finished_at TEXT,
             status TEXT NOT NULL DEFAULT 'running',
             pages_fetched INTEGER NOT NULL DEFAULT 0,
             pages_failed INTEGER NOT NULL DEFAULT 0,
             max_pages INTEGER
         );",
    )?;

    conn.execute("PRAGMA user_version = 1", [])?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("PRAGMA table_info({table})");
    conn.prepare(&sql)
        .and_then(|mut stmt| {
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            Ok(names.iter().any(|name| name == column))
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Bookmarks (JSON file, unchanged)
// ---------------------------------------------------------------------------

pub fn load_bookmarks() -> Vec<String> {
    let bookmarks_path = get_bookmarks_path();
    if let Ok(content) = fs::read_to_string(bookmarks_path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        vec![]
    }
}

pub fn add_bookmark(url: &str) {
    let bookmarks_path = get_bookmarks_path();
    let mut bookmarks = load_bookmarks();
    if !bookmarks.contains(&url.to_string()) {
        bookmarks.push(url.to_string());
        if let Ok(json) = serde_json::to_string(&bookmarks) {
            let _ = fs::write(&bookmarks_path, json);
        }
    }
}

pub fn remove_bookmark(url: &str) {
    let bookmarks_path = get_bookmarks_path();
    let mut bookmarks = load_bookmarks();
    if let Some(pos) = bookmarks.iter().position(|r| r == url) {
        bookmarks.remove(pos);
        if let Ok(json) = serde_json::to_string(&bookmarks) {
            let _ = fs::write(&bookmarks_path, json);
        }
    }
}

// ---------------------------------------------------------------------------
// Connection helpers
// ---------------------------------------------------------------------------

pub fn get_connection() -> SqliteResult<Connection> {
    Connection::open(get_db_path())
}

fn serialize_page(page_data: &PageData) -> SqliteResult<String> {
    serde_json::to_string(page_data).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Create a new crawl session in `running` state.
pub fn create_session(
    name: Option<&str>,
    start_url: &str,
    max_pages: Option<usize>,
) -> SqliteResult<i64> {
    let conn = Connection::open(get_db_path())?;
    let started_at = chrono::Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sessions (name, start_url, started_at, status, max_pages)
         VALUES (?1, ?2, ?3, 'running', ?4)",
        params![name, start_url, started_at, max_pages.map(|m| m as i64)],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mark a session finished and record the final counts.
pub fn finish_session(
    id: i64,
    status: &str,
    pages_fetched: usize,
    pages_failed: usize,
) -> SqliteResult<()> {
    let conn = Connection::open(get_db_path())?;
    conn.execute(
        "UPDATE sessions
         SET finished_at = ?1, status = ?2, pages_fetched = ?3, pages_failed = ?4
         WHERE id = ?5",
        params![
            chrono::Local::now().to_rfc3339(),
            status,
            pages_fetched as i64,
            pages_failed as i64,
            id
        ],
    )?;
    Ok(())
}

fn row_to_session(row: &rusqlite::Row<'_>) -> SqliteResult<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        name: row.get(1)?,
        start_url: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status: row.get(5)?,
        pages_fetched: row.get::<_, i64>(6)? as usize,
        pages_failed: row.get::<_, i64>(7)? as usize,
        max_pages: row.get::<_, Option<i64>>(8)?.map(|m| m as usize),
    })
}

const SESSION_COLUMNS: &str = "id, name, start_url, started_at, finished_at, status, pages_fetched, pages_failed, max_pages";

pub fn list_sessions(limit: usize) -> SqliteResult<Vec<SessionRow>> {
    let conn = Connection::open(get_db_path())?;
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions ORDER BY id DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([limit as i64], |row| row_to_session(row))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

pub fn get_session(id: i64) -> SqliteResult<Option<SessionRow>> {
    let conn = Connection::open(get_db_path())?;
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([id], |row| row_to_session(row))?;
    Ok(rows.next().and_then(|r| r.ok()))
}

/// Count of pages stored for a session.
pub fn session_page_count(session_id: i64) -> SqliteResult<usize> {
    let conn = Connection::open(get_db_path())?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pages WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Load every full PageData record for a session (for issue analysis etc.).
pub fn load_session_pages(session_id: i64) -> SqliteResult<Vec<PageData>> {
    let conn = Connection::open(get_db_path())?;
    let mut stmt = conn.prepare("SELECT data FROM pages WHERE session_id = ?1 ORDER BY page_id")?;
    let rows = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|data| serde_json::from_str(&data).ok())
        .collect();
    Ok(rows)
}

/// Lightweight rows for display / JSON output.
pub fn session_page_rows(session_id: i64, limit: Option<usize>) -> SqliteResult<Vec<SessionPageRow>> {
    let conn = Connection::open(get_db_path())?;
    let sql = "SELECT page_id, url, status,
                      COALESCE(json_extract(data, '$.title'), ''),
                      COALESCE(json_extract(data, '$.title_len'), 0),
                      COALESCE(json_extract(data, '$.description_len'), 0),
                      COALESCE(json_extract(data, '$.indexability'), ''),
                      COALESCE(json_extract(data, '$.word_count'), 0),
                      COALESCE(json_extract(data, '$.size'), 0)
               FROM pages WHERE session_id = ?1 ORDER BY page_id LIMIT ?2";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![session_id, limit.map(|l| l as i64).unwrap_or(-1)], |row| {
            Ok(SessionPageRow {
                page_id: row.get(0)?,
                url: row.get(1)?,
                status: row.get(2)?,
                // Titles come from HTML and may contain newlines/extra spacing.
                title: row
                    .get::<_, String>(3)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                title_len: row.get::<_, i64>(4)? as usize,
                description_len: row.get::<_, i64>(5)? as usize,
                indexability: row.get(6)?,
                word_count: row.get::<_, i64>(7)? as usize,
                size: row.get::<_, i64>(8)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Page persistence
// ---------------------------------------------------------------------------

/// Save a page, either scoped to a crawl session or as legacy (session 0).
pub fn save_page_for_session(
    conn: &Connection,
    session_id: Option<i64>,
    page_data: &PageData,
) -> SqliteResult<()> {
    let session_id = session_id.unwrap_or(LEGACY_SESSION_ID);
    let data = serialize_page(page_data)?;
    conn.execute(
        "INSERT OR REPLACE INTO pages (session_id, page_id, url, status, fetched_at, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            page_data.id as i64,
            page_data.url,
            page_data.status,
            chrono::Local::now().to_rfc3339(),
            data
        ],
    )?;
    Ok(())
}

/// Legacy save: pages land in session 0. Kept for the TUI until it adopts sessions.
pub fn save_page_data_with_conn(conn: &Connection, page_data: &PageData) -> SqliteResult<()> {
    save_page_for_session(conn, None, page_data)
}

pub fn save_page_data(page_data: &PageData) -> SqliteResult<()> {
    let conn = Connection::open(get_db_path())?;
    save_page_data_with_conn(&conn, page_data)
}

/// Legacy loader: reads from session 0 only.
pub fn load_page_data(id: usize) -> Option<PageData> {
    let conn = Connection::open(get_db_path()).ok()?;
    let data: String = conn
        .query_row(
            "SELECT data FROM pages WHERE session_id = ?1 AND page_id = ?2",
            params![LEGACY_SESSION_ID, id as i64],
            |row| row.get(0),
        )
        .ok()?;
    serde_json::from_str(&data).ok()
}

/// Legacy loader: all session-0 pages.
pub fn load_all_page_data() -> Vec<PageData> {
    let conn = Connection::open(get_db_path()).unwrap();
    let mut stmt = conn
        .prepare("SELECT data FROM pages WHERE session_id = ?1 ORDER BY page_id")
        .unwrap();
    let page_iter = stmt
        .query_map([LEGACY_SESSION_ID], |row| {
            let data: String = row.get(0)?;
            let page_data: PageData = serde_json::from_str(&data).unwrap();
            Ok(page_data)
        })
        .unwrap();
    page_iter.filter_map(|p| p.ok()).collect()
}

pub fn clear_page_data() -> SqliteResult<()> {
    let conn = Connection::open(get_db_path())?;
    conn.execute("DELETE FROM pages", ())?;
    Ok(())
}

pub fn get_page_count() -> usize {
    let conn = Connection::open(get_db_path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pages", (), |row| row.get(0))
        .unwrap_or(0);
    count as usize
}

pub fn get_pages_for_js(conn: &Connection, js_url: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT url FROM pages WHERE data LIKE ?1")
        .unwrap();
    let rows = stmt
        .query_map([format!("%{}%", js_url)], |row| row.get(0))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_pages_for_css(conn: &Connection, css_url: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT url FROM pages WHERE data LIKE ?1")
        .unwrap();
    let rows = stmt
        .query_map([format!("%{}%", css_url)], |row| row.get(0))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}
