//! Application state and background jobs (crawls, session loads, diffs, exports).
//!
//! Crawls are owned by a shared [`CrawlService`]: the GUI hosts the sync
//! server and drives crawls through it, so `periscrawl serve`/`status`/`watch`/
//! `stop` and other GUI instances all see the same live crawl.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;
use tokio::sync::broadcast;

use periscrawl_core::db::{self, SessionRow};
use periscrawl_core::diff::{self, DiffEntry, DiffStats};
use periscrawl_core::export::{self, Sheet};
use periscrawl_core::helpers::issues::{IssueAnalyzer, IssueFinding};
use periscrawl_core::models::{AppSettings, PageSummary};
use periscrawl_core::server::{CrawlOptions, CrawlService};
use periscrawl_core::PageData;

pub struct CrawlForm {
    pub url: String,
    pub name: String,
    pub max_pages: usize,
    pub concurrency: usize,
    pub javascript: bool,
}

pub struct LivePage {
    pub status: String,
    pub url: String,
    pub links: usize,
    pub failed: bool,
    pub not_modified: bool,
    pub js_rendered: bool,
}

pub struct LiveCrawl {
    pub url: String,
    pub session_id: Option<i64>,
    pub started: Instant,
    pub pages: usize,
    pub failed: usize,
    pub queued: usize,
    pub processing: usize,
    pub robots_blocked: usize,
    pub js_renders: usize,
    pub not_modified: usize,
    pub stop_requested: bool,
    pub recent: Vec<LivePage>,
}

impl LiveCrawl {
    fn new(url: String, session_id: Option<i64>) -> Self {
        Self {
            url,
            session_id,
            started: Instant::now(),
            pages: 0,
            failed: 0,
            queued: 0,
            processing: 0,
            robots_blocked: 0,
            js_renders: 0,
            not_modified: 0,
            stop_requested: false,
            recent: Vec::new(),
        }
    }
}

pub struct LoadedSession {
    pub session_id: i64,
    pub pages: Vec<PageData>,
    pub summaries: Vec<PageSummary>,
    pub sheets: Vec<Sheet>,
    pub findings: Vec<IssueFinding>,
    pub page_count: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ExportKind {
    Xlsx,
    CsvZip,
    Json,
    Pdf,
}

/// Which view the central panel shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CentralTab {
    Sheet(usize),
    Diff,
}

/// The right-hand inspector contents.
#[derive(Clone)]
pub enum DetailView {
    Page { index: usize },
    Asset { kind: AssetKind, url: String },
    Issue { name: String },
    DiffEntry { index: usize },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Images,
    Css,
    JavaScript,
    Files,
}

impl AssetKind {
    pub fn label(self) -> &'static str {
        match self {
            AssetKind::Images => "Image",
            AssetKind::Css => "CSS file",
            AssetKind::JavaScript => "JavaScript file",
            AssetKind::Files => "File",
        }
    }

    /// URLs of this asset kind that a page references.
    pub fn urls_for<'a>(self, page: &'a PageData) -> Vec<&'a str> {
        match self {
            AssetKind::Images => page.images.iter().map(|i| i.src.as_str()).collect(),
            AssetKind::Css => page
                .css
                .as_ref()
                .map(|c| c.css_urls.iter().map(|u| u.as_str()).collect())
                .unwrap_or_default(),
            AssetKind::JavaScript => page
                .javascript
                .as_ref()
                .map(|j| j.js_urls.iter().map(|u| u.as_str()).collect())
                .unwrap_or_default(),
            AssetKind::Files => page
                .anchor_links
                .iter()
                .filter(|l| export::is_file_url(&l.href))
                .map(|l| l.href.as_str())
                .collect(),
        }
    }
}

pub struct PeriscrawlApp {
    pub runtime: tokio::runtime::Runtime,
    pub service: CrawlService,
    events: broadcast::Receiver<Value>,
    pub sync_port: Arc<Mutex<Option<u16>>>,
    pub sessions: Vec<SessionRow>,
    pub selected_session: Option<i64>,
    pub pages: Vec<PageData>,
    pub summaries: Vec<PageSummary>,
    /// Issue findings from the shared session analyzer (used by the issue
    /// detail pane; the Issues tab table comes from the sheets).
    pub findings: Vec<IssueFinding>,
    page_lookup: HashMap<String, usize>,
    pub sheets: Vec<Sheet>,
    pub active_tab: CentralTab,
    pub active_sort: Option<(usize, bool)>,
    pub filter: String,
    pub detail: Option<DetailView>,
    pub loading_session: bool,
    pub load_rx: Option<Receiver<LoadedSession>>,
    pub status: String,
    pub status_tx: Sender<String>,
    status_rx: Receiver<String>,
    pub form: CrawlForm,
    pub crawl: Option<LiveCrawl>,
    pub diff_with: Option<i64>,
    pub diff_entries: Vec<DiffEntry>,
    pub diff_stats: Option<DiffStats>,
    pub diff_loading: bool,
    pub diff_rx: Option<Receiver<(Vec<DiffEntry>, DiffStats)>>,
    pub settings_open: bool,
    pub settings_draft: AppSettings,
    pub log_path: std::path::PathBuf,
    /// Width of the details pane, driven by its left-edge drag grip.
    pub detail_width: f32,
    /// Last rendered rect of the details pane (for the grip overlay).
    pub detail_rect: Option<egui::Rect>,
}

impl PeriscrawlApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Data app: text in tables should never steal pointer clicks for
        // selection — rows must be clickable all the way through. The detail
        // pane re-enables selection locally where copying matters.
        cc.egui_ctx
            .all_styles_mut(|style| style.interaction.selectable_labels = false);
        db::init_db();

        let settings = AppSettings::load();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime");

        let (status_tx, status_rx) = channel();
        let log_path = db::get_db_path().with_file_name("gui.log");

        let service = CrawlService::new();
        let events = service.subscribe();
        let sync_port: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));
        {
            let service = service.clone();
            let status_tx = status_tx.clone();
            let sync_port = Arc::clone(&sync_port);
            runtime.spawn(async move {
                match periscrawl_core::server::start_sync_server(service).await {
                    Ok(info) => {
                        if let Ok(mut port) = sync_port.lock() {
                            *port = Some(info.port);
                        }
                        let _ = status_tx.send(format!(
                            "Sync server listening on 127.0.0.1:{} — `periscrawl watch` attaches here",
                            info.port
                        ));
                    }
                    Err(err) => {
                        let _ = status_tx.send(format!("Sync inactive: {err}"));
                    }
                }
            });
        }

        let mut app = Self {
            runtime,
            service,
            events,
            sync_port,
            sessions: Vec::new(),
            selected_session: None,
            pages: Vec::new(),
            summaries: Vec::new(),
            findings: Vec::new(),
            page_lookup: HashMap::new(),
            sheets: Vec::new(),
            active_tab: CentralTab::Sheet(0),
            active_sort: None,
            filter: String::new(),
            detail: None,
            loading_session: false,
            load_rx: None,
            status: "Ready. Enter a URL and start a crawl, or pick a session.".to_string(),
            status_tx,
            status_rx,
            form: CrawlForm {
                url: String::new(),
                name: String::new(),
                max_pages: settings.crawler.max_pages,
                concurrency: settings.crawler.concurrency,
                javascript: settings.crawler.enable_javascript,
            },
            crawl: None,
            diff_with: None,
            diff_entries: Vec::new(),
            diff_stats: None,
            diff_loading: false,
            diff_rx: None,
            settings_open: false,
            settings_draft: settings,
            log_path,
            detail_width: 540.0,
            detail_rect: None,
        };
        app.refresh_sessions();
        // Testing/automation hook: auto-open a session, e.g.
        // PERISCRAWL_GUI_OPEN_SESSION=2 periscrawl-gui
        if let Ok(id) = std::env::var("PERISCRAWL_GUI_OPEN_SESSION")
            .or_else(|_| std::env::var("RUSTYSEO_GUI_OPEN_SESSION")) // legacy name
        {
            if let Ok(id) = id.parse::<i64>() {
                app.load_session(id);
            }
        }
        app
    }

    pub fn refresh_sessions(&mut self) {
        match db::list_sessions(200) {
            Ok(sessions) => self.sessions = sessions,
            Err(err) => self.status = format!("Could not list sessions: {err}"),
        }
    }

    /// Resolve a page URL to its index in the loaded session.
    pub fn resolve_page(&self, url: &str) -> Option<usize> {
        self.page_lookup.get(url).copied()
    }

    /// All pages that reference the given asset.
    pub fn asset_users(&self, kind: AssetKind, url: &str) -> Vec<usize> {
        self.pages
            .iter()
            .enumerate()
            .filter(|(_, page)| kind.urls_for(page).iter().any(|u| *u == url))
            .map(|(index, _)| index)
            .collect()
    }

    /// Full URL list for an issue, from the session analyzer run at load.
    pub fn issue_urls(&self, name: &str) -> Vec<String> {
        self.findings
            .iter()
            .find(|finding| finding.name == name)
            .map(|finding| finding.urls.clone())
            .unwrap_or_default()
    }

    pub fn start_crawl(&mut self) {
        if self.crawl.is_some() {
            return;
        }
        let raw = self.form.url.trim();
        if raw.is_empty() {
            self.status = "Enter a URL first".to_string();
            return;
        }
        let url = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        if url::Url::parse(&url).is_err() {
            self.status = format!("Invalid URL: {url}");
            return;
        }

        let name = {
            let trimmed = self.form.name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let options = CrawlOptions {
            url: url.clone(),
            name,
            max_pages: Some(self.form.max_pages),
            js: self.form.javascript,
        };

        match self.runtime.block_on(self.service.start_crawl(options)) {
            Ok(session_id) => {
                self.crawl = Some(LiveCrawl::new(url, Some(session_id)));
                self.status = "Crawl started".to_string();
            }
            Err(err) => {
                self.status = format!("Could not start crawl: {err}");
            }
        }
    }

    pub fn stop_crawl(&mut self) {
        if self.service.stop() {
            self.status = "Stop requested; finishing in-flight requests…".to_string();
        }
    }

    pub fn load_session(&mut self, id: i64) {
        if self.loading_session {
            return;
        }
        self.selected_session = Some(id);
        self.loading_session = true;
        self.sheets.clear();
        self.pages.clear();
        self.summaries.clear();
        self.findings.clear();
        self.page_lookup.clear();
        self.filter.clear();
        self.detail = None;
        self.active_tab = CentralTab::Sheet(0);
        self.active_sort = None;
        self.diff_with = None;
        self.diff_entries.clear();
        self.diff_stats = None;

        let (tx, rx) = channel();
        self.load_rx = Some(rx);
        std::thread::spawn(move || {
            let pages = db::load_session_pages(id).unwrap_or_default();
            let base_domain = db::get_session(id).ok().flatten().and_then(|session| {
                url::Url::parse(&session.start_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
            });
            let summaries: Vec<PageSummary> = pages
                .iter()
                .map(|page| page.summarize(base_domain.as_deref()))
                .collect();
            let sheets = export::build_report_sheets(&pages, base_domain.as_deref());
            let findings = IssueAnalyzer::analyze_session(&pages, base_domain.as_deref());
            let _ = tx.send(LoadedSession {
                session_id: id,
                page_count: pages.len(),
                pages,
                summaries,
                sheets,
                findings,
            });
        });
    }

    pub fn start_diff(&mut self, other: i64) {
        if self.summaries.is_empty() {
            self.status = "Load a session first".to_string();
            return;
        }
        self.diff_loading = true;
        self.diff_entries.clear();
        self.diff_stats = None;

        let (tx, rx) = channel();
        self.diff_rx = Some(rx);
        let new_summaries = self.summaries.clone();
        std::thread::spawn(move || {
            let old_pages = db::load_session_pages(other).unwrap_or_default();
            let base_domain = db::get_session(other).ok().flatten().and_then(|session| {
                url::Url::parse(&session.start_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
            });
            let old_summaries: Vec<PageSummary> = old_pages
                .iter()
                .map(|page| page.summarize(base_domain.as_deref()))
                .collect();
            let (entries, stats) = diff::diff_sessions(&old_summaries, &new_summaries);
            let _ = tx.send((entries, stats));
        });
    }

    pub fn open_settings(&mut self) {
        self.settings_draft = AppSettings::load();
        self.settings_open = true;
    }

    pub fn save_settings_draft(&mut self) {
        match periscrawl_core::settings::utils::write::save_settings(&self.settings_draft) {
            Ok(path) => {
                self.status = format!("Settings saved: {}", path.display());
                self.form.max_pages = self.settings_draft.crawler.max_pages;
                self.form.concurrency = self.settings_draft.crawler.concurrency;
                self.form.javascript = self.settings_draft.crawler.enable_javascript;
            }
            Err(err) => self.status = format!("Could not save settings: {err}"),
        }
    }

    pub fn export_current(&mut self, kind: ExportKind) {
        let Some(sid) = self.selected_session else {
            self.status = "No session selected".to_string();
            return;
        };
        if self.sheets.is_empty() {
            self.status = "Nothing to export yet".to_string();
            return;
        }
        let sheets = self.sheets.clone();
        let session = db::get_session(sid).ok().flatten();
        let tx = self.status_tx.clone();

        std::thread::spawn(move || {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
            let result = (|| -> anyhow::Result<std::path::PathBuf> {
                let dir = export::exports_dir()?;
                match kind {
                    ExportKind::Xlsx => {
                        let path = dir.join(format!("periscrawl_session{sid}_{timestamp}.xlsx"));
                        export::write_xlsx(&sheets, &path)?;
                        Ok(path)
                    }
                    ExportKind::CsvZip => {
                        let path = dir.join(format!("periscrawl_session{sid}_{timestamp}.zip"));
                        export::write_csv_zip(&sheets, &path)?;
                        Ok(path)
                    }
                    ExportKind::Json => {
                        let path = dir.join(format!("periscrawl_session{sid}_{timestamp}.json"));
                        let session =
                            session.ok_or_else(|| anyhow::anyhow!("session {sid} not found"))?;
                        let value = export::report_json(&session, &sheets)?;
                        std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
                        Ok(path)
                    }
                    ExportKind::Pdf => {
                        let path = dir.join(format!("periscrawl_session{sid}_{timestamp}.pdf"));
                        let session =
                            session.ok_or_else(|| anyhow::anyhow!("session {sid} not found"))?;
                        let pages = db::load_session_pages(sid)
                            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
                        let base_domain = url::Url::parse(&session.start_url)
                            .ok()
                            .and_then(|url| url.host_str().map(|host| host.to_string()));
                        periscrawl_core::report::write_session_pdf(
                            &session,
                            &pages,
                            base_domain.as_deref(),
                            &path,
                            &Default::default(),
                        )?;
                        Ok(path)
                    }
                }
            })();
            let message = match result {
                Ok(path) => {
                    // The user asked for the export's folder to pop open.
                    if let Some(parent) = path.parent() {
                        open_in_file_manager(parent);
                    }
                    format!("Exported: {} (folder opened)", path.display())
                }
                Err(err) => format!("Export failed: {err}"),
            };
            let _ = tx.send(message);
        });
        self.status = "Exporting…".to_string();
    }

    /// Drain background channels. Called once per frame before drawing.
    pub fn poll_events(&mut self) {
        while let Ok(message) = self.status_rx.try_recv() {
            self.status = message;
        }

        if let Some(rx) = &self.load_rx {
            match rx.try_recv() {
                Ok(loaded) => {
                    self.loading_session = false;
                    self.load_rx = None;
                    self.page_lookup = loaded
                        .pages
                        .iter()
                        .enumerate()
                        .map(|(index, page)| (page.url.clone(), index))
                        .collect();
                    self.pages = loaded.pages;
                    self.summaries = loaded.summaries;
                    self.sheets = loaded.sheets;
                    self.findings = loaded.findings;
                    self.active_tab = CentralTab::Sheet(0);
                    self.status = format!(
                        "Loaded session {} ({} stored pages)",
                        loaded.session_id, loaded.page_count
                    );
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.loading_session = false;
                    self.load_rx = None;
                    self.status = "Session load failed".to_string();
                }
            }
        }

        if let Some(rx) = &self.diff_rx {
            match rx.try_recv() {
                Ok((entries, stats)) => {
                    self.diff_loading = false;
                    self.diff_rx = None;
                    self.status = format!(
                        "Diff ready: {} added, {} removed, {} changed, {} unchanged",
                        stats.added, stats.removed, stats.changed, stats.unchanged
                    );
                    self.diff_entries = entries;
                    self.diff_stats = Some(stats);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.diff_loading = false;
                    self.diff_rx = None;
                    self.status = "Diff failed".to_string();
                }
            }
        }

        // Live crawl events from the shared service (ours or an external one).
        let mut ended: Option<Value> = None;
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    let name = event
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    match name.as_str() {
                        "crawl.start" => {
                            if self.crawl.is_none() {
                                let url = event["url"].as_str().unwrap_or("").to_string();
                                let session_id = event["session_id"].as_i64();
                                self.crawl = Some(LiveCrawl::new(url, session_id));
                                self.status = "External crawl started — watching live".to_string();
                            }
                        }
                        "page" => {
                            if let Some(crawl) = &mut self.crawl {
                                let status = event["status"].as_str().unwrap_or("").to_string();
                                let code = status.get(..3).and_then(|c| c.parse::<u16>().ok());
                                let failed = code.is_none_or(|c| c >= 400);
                                crawl.recent.push(LivePage {
                                    status,
                                    url: event["url"].as_str().unwrap_or("").to_string(),
                                    links: event["links"].as_u64().unwrap_or(0) as usize,
                                    failed,
                                    not_modified: event["not_modified"].as_bool() == Some(true),
                                    js_rendered: event["js_rendered"].as_bool() == Some(true),
                                });
                                if crawl.recent.len() > 500 {
                                    crawl.recent.remove(0);
                                }
                            }
                        }
                        "crawl.end" => ended = Some(event),
                        _ => {}
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // Keep live counters fresh from the service; treat a stopped service as
        // crawl completion even if the end event was missed.
        if let Some(crawl) = &mut self.crawl {
            let status = self.service.status();
            crawl.pages = status.pages;
            crawl.failed = status.failed;
            crawl.queued = status.queued;
            crawl.processing = status.processing;
            crawl.robots_blocked = status.robots_blocked;
            crawl.js_renders = status.js_renders;
            crawl.not_modified = status.not_modified;
            crawl.stop_requested = status.stop_requested;
            if !status.running && ended.is_none() {
                ended = Some(Value::Null);
            }
        }

        if let Some(event) = ended {
            let session_id = event
                .get("session_id")
                .and_then(Value::as_i64)
                .or_else(|| self.crawl.as_ref().and_then(|c| c.session_id));
            let pages = event
                .get("pages")
                .and_then(Value::as_u64)
                .map(|p| p as usize)
                .or_else(|| self.crawl.as_ref().map(|c| c.pages))
                .unwrap_or(0);
            let outcome = event
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            self.crawl = None;
            self.refresh_sessions();
            self.status = format!("Crawl {outcome}: {pages} pages");
            if let Some(id) = session_id {
                self.load_session(id);
            }
        }
    }
}

impl eframe::App for PeriscrawlApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        if self.crawl.is_some() || self.loading_session || self.diff_loading {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        crate::ui::draw(self, ui);
    }
}

/// Reveal a directory in the OS file manager (Finder / Explorer / xdg).
fn open_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = "xdg-open";

    let _ = std::process::Command::new(command).arg(path).spawn();
}
