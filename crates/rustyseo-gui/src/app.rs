//! Application state and background jobs (crawls, session loads, diffs, exports).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Instant;

use rustyseo_core::crawler::stats::CrawlStats;
use rustyseo_core::crawler::{CrawlEngine, CrawlMessage};
use rustyseo_core::db::{self, SessionRow};
use rustyseo_core::diff::{self, DiffEntry, DiffStats};
use rustyseo_core::export::{self, Sheet};
use rustyseo_core::helpers::issues::IssueAnalyzer;
use rustyseo_core::models::{AppSettings, PageSummary};
use rustyseo_core::PageData;

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
}

pub enum CrawlEvent {
    Ready { stats: CrawlStats },
    Page {
        status: String,
        url: String,
        links: usize,
        failed: bool,
    },
    Progress {
        scanned: usize,
        queued: usize,
        processing: usize,
    },
    Finished {
        pages: usize,
        failed: usize,
    },
}

pub struct LiveCrawl {
    pub url: String,
    pub max_pages: usize,
    pub session_id: Option<i64>,
    pub rx: Receiver<CrawlEvent>,
    pub stats: Option<CrawlStats>,
    pub started: Instant,
    pub pages: usize,
    pub failed: usize,
    pub scanned: usize,
    pub queued: usize,
    pub processing: usize,
    pub recent: Vec<LivePage>,
    pub done: bool,
}

pub struct LoadedSession {
    pub session_id: i64,
    pub pages: Vec<PageData>,
    pub summaries: Vec<PageSummary>,
    pub sheets: Vec<Sheet>,
    pub page_count: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ExportKind {
    Xlsx,
    CsvZip,
    Json,
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

pub struct RustySeoApp {
    pub runtime: tokio::runtime::Runtime,
    pub sessions: Vec<SessionRow>,
    pub selected_session: Option<i64>,
    pub pages: Vec<PageData>,
    pub summaries: Vec<PageSummary>,
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
}

impl RustySeoApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        db::init_db();

        let settings = AppSettings::load();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime");

        let (status_tx, status_rx) = channel();
        let log_path = db::get_db_path().with_file_name("gui.log");

        let mut app = Self {
            runtime,
            sessions: Vec::new(),
            selected_session: None,
            pages: Vec::new(),
            summaries: Vec::new(),
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
        };
        app.refresh_sessions();
        // Testing/automation hook: auto-open a session, e.g.
        // RUSTYSEO_GUI_OPEN_SESSION=2 rustyseo-gui
        if let Ok(id) = std::env::var("RUSTYSEO_GUI_OPEN_SESSION") {
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

    /// Full URL list for an issue (recomputed from the analyzers).
    pub fn issue_urls(&self, name: &str) -> Vec<String> {
        IssueAnalyzer::get_handlers()
            .iter()
            .find(|handler| handler.name.trim() == name)
            .map(|handler| (handler.process)(&self.summaries).1)
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

        let settings = AppSettings::load();
        let max_pages = self.form.max_pages.max(1);
        let concurrency = self.form.concurrency.clamp(1, 64);
        let js = self.form.javascript;
        let name = {
            let trimmed = self.form.name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };

        let session_id = match db::create_session(name.as_deref(), &url, Some(max_pages)) {
            Ok(id) => Some(id),
            Err(err) => {
                self.status = format!("Could not create session: {err}");
                None
            }
        };

        let (event_tx, event_rx) = channel::<CrawlEvent>();
        let task_url = url.clone();

        self.runtime.spawn(async move {
            let mut engine = CrawlEngine::new()
                .await
                .with_max_pages(max_pages)
                .with_concurrency(concurrency)
                .with_robots(settings.crawler.respect_robots)
                .with_crawl_delay(settings.crawler.respect_crawl_delay)
                .with_nofollow(settings.crawler.respect_nofollow)
                .with_noindex(settings.crawler.respect_noindex)
                .with_javascript(js)
                .with_db_persistence(None);
            if let Some(sid) = session_id {
                engine = engine.with_session(sid);
            }

            let stats = engine.stats();
            let _ = event_tx.send(CrawlEvent::Ready { stats });

            let (tx, mut rx) = tokio::sync::mpsc::channel(1000);
            let engine_task = engine.clone();
            let crawl_url = task_url.clone();
            let handle =
                tokio::spawn(async move { engine_task.crawl_concurrently(&crawl_url, tx).await });

            let mut pages = 0usize;
            let mut failed = 0usize;
            while let Some(message) = rx.recv().await {
                match message {
                    CrawlMessage::Page(page) => {
                        pages += 1;
                        let code = page.status.get(..3).and_then(|c| c.parse::<u16>().ok());
                        let is_failed = code.is_none_or(|c| c >= 400);
                        if is_failed {
                            failed += 1;
                        }
                        let _ = event_tx.send(CrawlEvent::Page {
                            status: page.status.clone(),
                            url: page.url.clone(),
                            links: page.anchor_links.len(),
                            failed: is_failed,
                        });
                    }
                    CrawlMessage::Progress {
                        scanned,
                        queued,
                        processing,
                    } => {
                        let _ = event_tx.send(CrawlEvent::Progress {
                            scanned,
                            queued,
                            processing,
                        });
                    }
                }
            }
            let _ = handle.await;
            if let Some(sid) = session_id {
                let _ = db::finish_session(sid, "completed", pages, failed);
            }
            let _ = event_tx.send(CrawlEvent::Finished { pages, failed });
        });

        self.crawl = Some(LiveCrawl {
            url,
            max_pages,
            session_id,
            rx: event_rx,
            stats: None,
            started: Instant::now(),
            pages: 0,
            failed: 0,
            scanned: 0,
            queued: 0,
            processing: 0,
            recent: Vec::new(),
            done: false,
        });
        self.status = "Crawl started".to_string();
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
            let _ = tx.send(LoadedSession {
                session_id: id,
                page_count: pages.len(),
                pages,
                summaries,
                sheets,
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
        match rustyseo_core::settings::utils::write::save_settings(&self.settings_draft) {
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
                        let path = dir.join(format!("rustyseo_session{sid}_{timestamp}.xlsx"));
                        export::write_xlsx(&sheets, &path)?;
                        Ok(path)
                    }
                    ExportKind::CsvZip => {
                        let path = dir.join(format!("rustyseo_session{sid}_{timestamp}.zip"));
                        export::write_csv_zip(&sheets, &path)?;
                        Ok(path)
                    }
                    ExportKind::Json => {
                        let path = dir.join(format!("rustyseo_session{sid}_{timestamp}.json"));
                        let session =
                            session.ok_or_else(|| anyhow::anyhow!("session {sid} not found"))?;
                        let value = export::report_json(&session, &sheets)?;
                        std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
                        Ok(path)
                    }
                }
            })();
            let message = match result {
                Ok(path) => format!("Exported: {}", path.display()),
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

        let mut finished = false;
        if let Some(crawl) = &mut self.crawl {
            loop {
                match crawl.rx.try_recv() {
                    Ok(CrawlEvent::Ready { stats }) => crawl.stats = Some(stats),
                    Ok(CrawlEvent::Page {
                        status,
                        url,
                        links,
                        failed,
                    }) => {
                        crawl.pages += 1;
                        if failed {
                            crawl.failed += 1;
                        }
                        crawl.recent.push(LivePage {
                            status,
                            url,
                            links,
                            failed,
                        });
                        if crawl.recent.len() > 500 {
                            crawl.recent.remove(0);
                        }
                    }
                    Ok(CrawlEvent::Progress {
                        scanned,
                        queued,
                        processing,
                    }) => {
                        crawl.scanned = scanned;
                        crawl.queued = queued;
                        crawl.processing = processing;
                    }
                    Ok(CrawlEvent::Finished { pages, failed }) => {
                        crawl.pages = pages;
                        crawl.failed = failed;
                        crawl.done = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        crawl.done = true;
                        break;
                    }
                }
            }
            finished = crawl.done;
        }

        if finished {
            let session_id = self.crawl.as_ref().and_then(|c| c.session_id);
            let pages = self.crawl.as_ref().map(|c| c.pages).unwrap_or(0);
            let robots = self
                .crawl
                .as_ref()
                .and_then(|c| c.stats.as_ref())
                .map(|s| s.robots_blocked())
                .unwrap_or(0);
            self.crawl = None;
            self.refresh_sessions();
            let robots_note = if robots > 0 {
                format!(", {robots} skipped by robots.txt")
            } else {
                String::new()
            };
            self.status = format!("Crawl finished: {pages} pages{robots_note}");
            if let Some(id) = session_id {
                self.load_session(id);
            }
        }
    }
}

impl eframe::App for RustySeoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        if self.crawl.is_some() || self.loading_session || self.diff_loading {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        crate::ui::draw(self, ui);
    }
}
