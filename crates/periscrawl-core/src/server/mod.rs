//! Live sync: a small crawl service plus a loopback JSON socket so the CLI,
//! GUI and (later) MCP can start, watch and stop the same crawl.
//!
//! Protocol: newline-delimited JSON over TCP on 127.0.0.1. Clients
//! authenticate with the token from `sync.token` (mode 0600) and then send
//! `{"id":n,"cmd":"...","params":{...}}`; responses are
//! `{"id":n,"ok":true,"result":{...}}`. `subscribe` turns the connection into
//! an event stream (pages, progress, crawl end) until the crawl finishes.

mod client;

pub use client::SyncClient;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use crate::crawler::stats::CrawlStats;
use crate::crawler::{CrawlEngine, CrawlMessage};
use crate::db;
use crate::models::AppSettings;

const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Options for starting a crawl through the service.
#[derive(Debug, Clone, Deserialize)]
pub struct CrawlOptions {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub max_pages: Option<usize>,
    #[serde(default)]
    pub js: bool,
}

/// Live counters for the crawl currently running (if any).
pub struct RunningState {
    pub session_id: i64,
    pub url: String,
    pub started: Instant,
    pub stop: Arc<AtomicBool>,
    pub stats: Mutex<Option<CrawlStats>>,
    pub pages: AtomicUsize,
    pub failed: AtomicUsize,
    pub scanned: AtomicUsize,
    pub queued: AtomicUsize,
    pub processing: AtomicUsize,
}

/// Live counters for one running crawl.
#[derive(Serialize, Clone)]
pub struct CrawlStatus {
    pub session_id: i64,
    pub url: String,
    pub pages: usize,
    pub failed: usize,
    pub scanned: usize,
    pub queued: usize,
    pub processing: usize,
    pub robots_blocked: usize,
    pub js_renders: usize,
    pub not_modified: usize,
    pub elapsed_ms: u64,
    pub stop_requested: bool,
}

#[derive(Serialize)]
pub struct StatusSnapshot {
    pub running: bool,
    /// Every crawl running right now (crawls run in parallel).
    pub crawls: Vec<CrawlStatus>,
    // Legacy top-level fields mirror the first running crawl so simple
    // clients keep working while the list is the source of truth.
    pub url: Option<String>,
    pub session_id: Option<i64>,
    pub pages: usize,
    pub failed: usize,
    pub scanned: usize,
    pub queued: usize,
    pub processing: usize,
    pub robots_blocked: usize,
    pub js_renders: usize,
    pub not_modified: usize,
    pub elapsed_ms: u64,
    pub stop_requested: bool,
}

/// Owns crawl runs and broadcasts their events. Shared by the GUI (which
/// hosts it in-process) and by `periscrawl serve`.
#[derive(Clone)]
pub struct CrawlService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    running: Mutex<HashMap<i64, Arc<RunningState>>>,
    events: broadcast::Sender<Value>,
}

impl Default for CrawlService {
    fn default() -> Self {
        Self::new()
    }
}

impl CrawlService {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(ServiceInner {
                running: Mutex::new(HashMap::new()),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.inner.events.subscribe()
    }

    pub fn is_running(&self) -> bool {
        self.inner
            .running
            .lock()
            .map(|running| !running.is_empty())
            .unwrap_or(false)
    }

    /// Session ids of the crawls running right now (oldest first).
    pub fn running_session_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .inner
            .running
            .lock()
            .map(|running| running.keys().copied().collect())
            .unwrap_or_default();
        ids.sort_unstable();
        ids
    }

    fn running_states(&self) -> Vec<Arc<RunningState>> {
        let mut states: Vec<Arc<RunningState>> = self
            .inner
            .running
            .lock()
            .map(|running| running.values().cloned().collect())
            .unwrap_or_default();
        states.sort_by_key(|state| state.session_id);
        states
    }

    fn snapshot(running: &Arc<RunningState>) -> CrawlStatus {
        let stats = running.stats.lock().ok().and_then(|s| s.clone());
        CrawlStatus {
            session_id: running.session_id,
            url: running.url.clone(),
            pages: running.pages.load(Ordering::SeqCst),
            failed: running.failed.load(Ordering::SeqCst),
            scanned: running.scanned.load(Ordering::SeqCst),
            queued: running.queued.load(Ordering::SeqCst),
            processing: running.processing.load(Ordering::SeqCst),
            robots_blocked: stats.as_ref().map(|s| s.robots_blocked()).unwrap_or(0),
            js_renders: stats.as_ref().map(|s| s.js_renders()).unwrap_or(0),
            not_modified: stats.as_ref().map(|s| s.not_modified()).unwrap_or(0),
            elapsed_ms: running.started.elapsed().as_millis() as u64,
            stop_requested: running.stop.load(Ordering::SeqCst),
        }
    }

    pub fn status(&self) -> StatusSnapshot {
        let crawls: Vec<CrawlStatus> = self.running_states().iter().map(Self::snapshot).collect();
        let first = crawls.first();
        StatusSnapshot {
            running: !crawls.is_empty(),
            url: first.map(|c| c.url.clone()),
            session_id: first.map(|c| c.session_id),
            pages: first.map(|c| c.pages).unwrap_or(0),
            failed: first.map(|c| c.failed).unwrap_or(0),
            scanned: first.map(|c| c.scanned).unwrap_or(0),
            queued: first.map(|c| c.queued).unwrap_or(0),
            processing: first.map(|c| c.processing).unwrap_or(0),
            robots_blocked: first.map(|c| c.robots_blocked).unwrap_or(0),
            js_renders: first.map(|c| c.js_renders).unwrap_or(0),
            not_modified: first.map(|c| c.not_modified).unwrap_or(0),
            elapsed_ms: first.map(|c| c.elapsed_ms).unwrap_or(0),
            stop_requested: first.map(|c| c.stop_requested).unwrap_or(false),
            crawls,
        }
    }

    /// Ask every running crawl to stop. Returns how many were asked.
    /// Stopping aborts in-flight requests immediately.
    pub fn stop(&self) -> usize {
        let states = self.running_states();
        for running in &states {
            running.stop.store(true, Ordering::SeqCst);
        }
        states.len()
    }

    /// Ask one crawl to stop (used by the GUI's per-crawl Stop button).
    pub fn stop_session(&self, session_id: i64) -> bool {
        let state = self
            .inner
            .running
            .lock()
            .ok()
            .and_then(|running| running.get(&session_id).cloned());
        match state {
            Some(running) => {
                running.stop.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Start a crawl and broadcast its events. Returns the session id.
    /// Any number of crawls can run in parallel; the service keeps them in a
    /// map keyed by session id.
    pub async fn start_crawl(&self, options: CrawlOptions) -> Result<i64, String> {
        if url::Url::parse(&options.url).is_err() {
            return Err(format!("invalid URL: {}", options.url));
        }
        let settings = AppSettings::load();
        let max_pages = options.max_pages.unwrap_or(settings.crawler.max_pages);
        let session_id = db::create_session(options.name.as_deref(), &options.url, Some(max_pages))
            .map_err(|err| format!("could not create session: {err}"))?;

        let running = Arc::new(RunningState {
            session_id,
            url: options.url.clone(),
            started: Instant::now(),
            stop: Arc::new(AtomicBool::new(false)),
            stats: Mutex::new(None),
            pages: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            scanned: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            processing: AtomicUsize::new(0),
        });
        {
            let mut active = self
                .inner
                .running
                .lock()
                .map_err(|_| "service lock poisoned".to_string())?;
            active.insert(session_id, running.clone());
        }

        let _ = self.inner.events.send(json!({
            "event": "crawl.start",
            "url": options.url,
            "session_id": session_id,
        }));

        let service = self.clone();
        let running_task = running.clone();
        let url = options.url.clone();
        let js = options.js;

        tokio::spawn(async move {
                let settings = AppSettings::load();
                let max_pages = options.max_pages.unwrap_or(settings.crawler.max_pages);
                let mut engine = CrawlEngine::new()
                    .await
                    .with_max_pages(max_pages)
                    .with_concurrency(settings.crawler.concurrency)
                    .with_robots(settings.crawler.respect_robots)
                    .with_crawl_delay(settings.crawler.respect_crawl_delay)
                    .with_nofollow(settings.crawler.respect_nofollow)
                    .with_noindex(settings.crawler.respect_noindex)
                    .with_js_fallback(settings.crawler.js_fallback)
                    .with_pagespeed(settings.connectors.pagespeed.active())
                    .with_session(session_id)
                    .with_db_persistence(None)
                    .with_stop_flag(running_task.stop.clone());
                if js {
                    engine = engine.with_javascript(true);
                }
                if let Ok(mut stats) = running_task.stats.lock() {
                    *stats = Some(engine.stats());
                }

                let (tx, mut rx) = tokio::sync::mpsc::channel(1000);
                let engine_task = engine.clone();
                let crawl_url = url.clone();
                let handle = tokio::spawn(async move {
                    engine_task.crawl_concurrently(&crawl_url, tx).await;
                });

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
                            running_task.pages.store(pages, Ordering::SeqCst);
                            running_task.failed.store(failed, Ordering::SeqCst);
                            let _ = service.inner.events.send(json!({
                                "event": "page",
                                "session_id": session_id,
                                "id": page.id,
                                "url": page.url,
                                "status": page.status,
                                "links": page.anchor_links.len(),
                                "js_rendered": page.js_rendered,
                                "not_modified": page.not_modified,
                            }));
                        }
                        CrawlMessage::Progress {
                            scanned,
                            queued,
                            processing,
                        } => {
                            running_task.scanned.store(scanned, Ordering::SeqCst);
                            running_task.queued.store(queued, Ordering::SeqCst);
                            running_task.processing.store(processing, Ordering::SeqCst);
                            let _ = service.inner.events.send(json!({
                                "event": "progress",
                                "session_id": session_id,
                                "scanned": scanned,
                                "queued": queued,
                                "processing": processing,
                            }));
                        }
                    }
                }
                let _ = handle.await;

                let stopped = running_task.stop.load(Ordering::SeqCst);
                let status = if stopped { "stopped" } else { "completed" };
                if let Err(err) = db::finish_session(session_id, status, pages, failed) {
                    tracing::warn!("[SYNC] could not finish session {session_id}: {err}");
                }
                let _ = service.inner.events.send(json!({
                    "event": "crawl.end",
                    "session_id": session_id,
                    "url": url,
                    "pages": pages,
                    "failed": failed,
                    "status": status,
                    "elapsed_ms": running_task.started.elapsed().as_millis() as u64,
                }));
                if let Ok(mut active) = service.inner.running.lock() {
                    active.remove(&session_id);
                }
            });

        Ok(session_id)
    }
}

// ---------------------------------------------------------------------------
// Loopback socket server
// ---------------------------------------------------------------------------

pub fn port_file() -> PathBuf {
    db::get_db_path().with_file_name("sync.port")
}

pub fn token_file() -> PathBuf {
    db::get_db_path().with_file_name("sync.token")
}

pub struct SyncServerInfo {
    pub port: u16,
}

/// Is another instance currently serving? Returns its port when a ping with
/// the stored token succeeds.
pub async fn server_ping() -> Option<u16> {
    let port: u16 = std::fs::read_to_string(port_file()).ok()?.trim().parse().ok()?;
    let token = std::fs::read_to_string(token_file()).ok()?.trim().to_string();

    let attempt = async {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let (read_half, mut write_half) = stream.into_split();
        write_half
            .write_all(format!("{{\"token\":\"{token}\"}}\n").as_bytes())
            .await
            .ok()?;
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.ok()?;
        let value: Value = serde_json::from_str(&line).ok()?;
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            Some(port)
        } else {
            None
        }
    };
    tokio::time::timeout(Duration::from_millis(500), attempt)
        .await
        .ok()
        .flatten()
}

/// Bind the loopback listener and serve until the process ends. Refuses to
/// start when another instance is already serving.
pub async fn start_sync_server(service: CrawlService) -> Result<SyncServerInfo, String> {
    if let Some(port) = server_ping().await {
        return Err(format!(
            "another Periscrawl instance is already serving on port {port}"
        ));
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|err| format!("could not bind loopback socket: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| err.to_string())?
        .port();
    let mut token_bytes = [0u8; 16];
    rand::rng().fill(&mut token_bytes);
    let token: String = token_bytes.iter().map(|byte| format!("{byte:02x}")).collect();

    std::fs::write(port_file(), port.to_string())
        .map_err(|err| format!("could not write port file: {err}"))?;
    std::fs::write(token_file(), &token)
        .map_err(|err| format!("could not write token file: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(token_file(), std::fs::Permissions::from_mode(0o600));
    }

    let accept_service = service.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let service = accept_service.clone();
                    let token = token.clone();
                    tokio::spawn(handle_connection(stream, service, token));
                }
                Err(err) => {
                    tracing::warn!("[SYNC] accept failed: {err}");
                }
            }
        }
    });

    Ok(SyncServerInfo { port })
}

async fn handle_connection(stream: TcpStream, service: CrawlService, token: String) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Authentication: first line must carry the token.
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    match serde_json::from_str::<Value>(&line) {
        Ok(value) if value.get("token").and_then(Value::as_str) == Some(token.as_str()) => {}
        _ => {
            let _ = write_half
                .write_all(b"{\"ok\":false,\"error\":\"bad token\"}\n")
                .await;
            return;
        }
    }
    let _ = write_half
        .write_all(b"{\"ok\":true,\"event\":\"welcome\"}\n")
        .await;

    loop {
        line.clear();
        let read = match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let _ = read;
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                let _ = write_half
                    .write_all(b"{\"ok\":false,\"error\":\"invalid json\"}\n")
                    .await;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let cmd = request
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let reply = |id: Value, result: Result<Value, String>| {
            let message = match result {
                Ok(result) => json!({"id": id, "ok": true, "result": result}),
                Err(error) => json!({"id": id, "ok": false, "error": error}),
            };
            let mut bytes = message.to_string();
            bytes.push('\n');
            bytes.into_bytes()
        };

        match cmd.as_str() {
            "ping" => {
                let message = reply(id, Ok(json!({"pong": true})));
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
            "status" => {
                let status = serde_json::to_value(service.status()).unwrap_or(Value::Null);
                let message = reply(id, Ok(status));
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
            "crawl.start" => {
                let result = match serde_json::from_value::<CrawlOptions>(params) {
                    Ok(options) => service
                        .start_crawl(options)
                        .await
                        .map(|session_id| json!({"session_id": session_id})),
                    Err(err) => Err(format!("bad crawl.start params: {err}")),
                };
                let message = reply(id, result);
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
            "crawl.stop" => {
                let result = match params.get("session_id").and_then(Value::as_i64) {
                    Some(session_id) => Ok(json!({
                        "stopped": usize::from(service.stop_session(session_id))
                    })),
                    None => Ok(json!({ "stopped": service.stop() })),
                };
                let message = reply(id, result);
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
            "sessions.list" => {
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(20) as usize;
                let result = match db::list_sessions(limit) {
                    Ok(rows) => serde_json::to_value(rows).map_err(|err| err.to_string()),
                    Err(err) => Err(err.to_string()),
                };
                let message = reply(id, result);
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
            "subscribe" => {
                // Optional filter: follow only one session's events. Without a
                // filter the stream stays open (the client decides when to
                // leave, e.g. after the first crawl.end).
                let filter = params.get("session_id").and_then(Value::as_i64);
                if filter.is_none() && !service.is_running() {
                    let message = reply(id, Err("no crawl is running".to_string()));
                    if write_half.write_all(&message).await.is_err() {
                        return;
                    }
                    continue;
                }
                let message = reply(id, Ok(json!({"subscribed": true})));
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
                let mut events = service.subscribe();
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let event_session = event.get("session_id").and_then(Value::as_i64);
                            let is_end = event.get("event").and_then(Value::as_str)
                                == Some("crawl.end");
                            let mut bytes = event.to_string();
                            bytes.push('\n');
                            if write_half.write_all(bytes.as_bytes()).await.is_err() {
                                return;
                            }
                            if is_end {
                                match filter {
                                    // Follow one session: done when it ends.
                                    Some(session_id) if event_session == Some(session_id) => {
                                        return;
                                    }
                                    Some(_) => {}
                                    // Unfiltered stream: keep going.
                                    None => {}
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!("[SYNC] subscriber lagged {skipped} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
            other => {
                let message = reply(id, Err(format!("unknown command: {other}")));
                if write_half.write_all(&message).await.is_err() {
                    return;
                }
            }
        }
    }
}
