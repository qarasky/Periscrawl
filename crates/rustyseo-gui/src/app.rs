//! Application state and background jobs (crawls, session loads, exports).

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Instant;

use rustyseo_core::crawler::stats::CrawlStats;
use rustyseo_core::crawler::{CrawlEngine, CrawlMessage};
use rustyseo_core::db::{self, SessionRow};
use rustyseo_core::export::{self, Sheet};
use rustyseo_core::models::AppSettings;

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
    pub sheets: Vec<Sheet>,
    pub page_count: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ExportKind {
    Xlsx,
    CsvZip,
    Json,
}

pub struct RustySeoApp {
    pub runtime: tokio::runtime::Runtime,
    pub sessions: Vec<SessionRow>,
    pub selected_session: Option<i64>,
    pub sheets: Vec<Sheet>,
    pub active_tab: usize,
    pub filter: String,
    pub loading_session: bool,
    pub load_rx: Option<Receiver<LoadedSession>>,
    pub status: String,
    pub status_tx: Sender<String>,
    status_rx: Receiver<String>,
    pub form: CrawlForm,
    pub crawl: Option<LiveCrawl>,
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

        let mut app = Self {
            runtime,
            sessions: Vec::new(),
            selected_session: None,
            sheets: Vec::new(),
            active_tab: 0,
            filter: String::new(),
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
        };
        app.refresh_sessions();
        app
    }

    pub fn refresh_sessions(&mut self) {
        match db::list_sessions(200) {
            Ok(sessions) => self.sessions = sessions,
            Err(err) => self.status = format!("Could not list sessions: {err}"),
        }
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
        self.filter.clear();
        self.active_tab = 0;

        let (tx, rx) = channel();
        self.load_rx = Some(rx);
        std::thread::spawn(move || {
            let pages = db::load_session_pages(id).unwrap_or_default();
            let base_domain = db::get_session(id).ok().flatten().and_then(|session| {
                url::Url::parse(&session.start_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
            });
            let sheets = export::build_report_sheets(&pages, base_domain.as_deref());
            let _ = tx.send(LoadedSession {
                session_id: id,
                sheets,
                page_count: pages.len(),
            });
        });
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
                    self.sheets = loaded.sheets;
                    self.active_tab = 0;
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
        if self.crawl.is_some() || self.loading_session {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        crate::ui::draw(self, ui);
    }
}
