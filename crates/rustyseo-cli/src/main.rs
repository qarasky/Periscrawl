//! RustySEO CLI — scriptable, agent-friendly access to the crawl engine.
//!
//! Exit codes:
//!   0  success
//!   1  fatal error (bad args, engine/session failure)
//!   2  crawl produced no pages / no sitemap entries
//!   3  config key not found
//!   6  regressions or sitemap findings detected (`crawl --compare`,
//!      `diff --fail-on-regressions`, `sitemap`)
//!   7  `watch` timed out
//!
//! With `--json-events`, crawl emits newline-delimited JSON on stdout:
//!   {"event":"page", ...} and a final {"event":"crawl.end", ...} record.
//! Every crawl is recorded as a session; query results later with
//! `rustyseo sessions` and `rustyseo session <id> ...`. List-style commands
//! support `--fields` to project the JSON objects down to the keys you need.
//!
//! Live sync: when a sync server is running (`rustyseo serve`, or the GUI),
//! `crawl` delegates to it by default so every frontend can watch the same
//! crawl — `rustyseo watch` streams its events, `rustyseo stop` stops it.

mod mcp;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::json;

use rustyseo_core::crawler::{CrawlEngine, CrawlMessage};
use rustyseo_core::db;
use rustyseo_core::diff::{self, DiffEntry, DiffKind};
use rustyseo_core::helpers::issues::{IssueAnalyzer, severity_for};
use rustyseo_core::models::{AppSettings, PageSummary};
use rustyseo_core::server::SyncClient;

#[derive(Parser)]
#[command(name = "rustyseo", version, about = "Crawl and audit websites from the terminal.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a website (breadth-first over internal links) and record a session.
    Crawl {
        /// Start URL, e.g. https://example.com
        url: String,

        /// Maximum number of pages to fetch.
        #[arg(long)]
        max_pages: Option<usize>,

        /// Concurrent requests.
        #[arg(long)]
        concurrency: Option<usize>,

        /// Render every page with headless Chrome (needs Chrome/Chromium).
        #[arg(long)]
        js: bool,

        /// Render only pages that look client-side (framework markers + sparse
        /// content) instead of every page.
        #[arg(long)]
        js_fallback: bool,

        /// Continue an earlier session: its stored pages count as done and
        /// their links seed the frontier. Requires the database.
        #[arg(long, value_name = "SESSION")]
        resume: Option<i64>,

        /// Incremental re-crawl: use this session's stored pages (ETag /
        /// Last-Modified headers) so unchanged pages answer 304 and are
        /// reused instead of re-downloaded. Requires the database.
        #[arg(long, value_name = "SESSION", conflicts_with = "resume")]
        since: Option<i64>,

        /// After crawling, diff against this baseline session and exit 6 when
        /// regressions are found (removed pages, new 4xx/5xx, newly noindexed).
        #[arg(long, value_name = "SESSION")]
        compare: Option<i64>,

        /// Do not persist pages to the local SQLite database or create a session.
        #[arg(long)]
        no_db: bool,

        /// Only print the final summary (no per-page lines).
        #[arg(long)]
        quiet: bool,

        /// Run the crawl in this process even when a sync server is running.
        /// By default crawls delegate to the running server so the GUI and
        /// other terminals can watch them live.
        #[arg(long)]
        local: bool,

        /// Emit newline-delimited JSON events instead of human-readable lines.
        #[arg(long)]
        json_events: bool,

        /// Human-friendly label for this crawl session.
        #[arg(long)]
        name: Option<String>,
    },

    /// List recorded crawl sessions (newest first).
    Sessions {
        /// Maximum number of sessions to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Comma-separated fields to keep in JSON output, e.g. `id,status`.
        #[arg(long)]
        fields: Option<String>,

        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Inspect one crawl session.
    Session {
        /// Session id (see `rustyseo sessions`).
        id: i64,

        #[command(subcommand)]
        action: SessionAction,
    },

    /// Compare two sessions (baseline first, current second).
    Diff {
        /// Baseline (earlier) session id.
        baseline: i64,

        /// Current (later) session id.
        current: i64,

        /// Exit 6 when any regression is present (removed page, new 4xx/5xx,
        /// newly noindexed) — for scheduled audits.
        #[arg(long)]
        fail_on_regressions: bool,

        /// Comma-separated fields to keep per diff entry, e.g. `url,kind`.
        #[arg(long)]
        fields: Option<String>,

        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Export a session as xlsx, CSV (single sheet or zip) or JSON.
    Export {
        /// Session id (see `rustyseo sessions`).
        session: i64,

        /// Output format.
        #[arg(long, value_enum, default_value_t = ExportFormat::Xlsx)]
        format: ExportFormat,

        /// Output path. Use `-` to print JSON to stdout. Defaults to the
        /// exports directory (~/…/rustyseo/exports).
        #[arg(long)]
        out: Option<String>,

        /// Export a single sheet as CSV, e.g. --tab "Internal Links".
        #[arg(long)]
        tab: Option<String>,
    },

    /// Inspect or edit the settings file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Check the local environment (settings, database, headless Chrome).
    Doctor {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Show the state of the running sync server (and its active crawl).
    Status {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Stream live crawl events from the sync server (GUI or `rustyseo serve`).
    Watch {
        /// Print raw NDJSON events (same shape as `crawl --json-events`).
        #[arg(long)]
        json: bool,

        /// Give up after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// Ask the crawl running on the sync server to stop.
    Stop,

    /// Run a headless sync server that other commands and the GUI attach to.
    Serve,

    /// Run an MCP (Model Context Protocol) server over stdio for AI agents.
    Mcp,

    /// Validate a site's sitemap: check every listed URL's status.
    Sitemap {
        /// Start URL, e.g. https://example.com
        url: String,

        /// Concurrent checks.
        #[arg(long, default_value_t = 8)]
        concurrency: usize,

        /// Cap the number of entries checked.
        #[arg(long)]
        max_urls: Option<usize>,

        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Analyze an nginx/apache access log: bots vs humans, statuses, file
    /// types, top paths and crawl waste.
    Logs {
        /// Path to the access log file.
        file: String,

        /// Rows kept in each detail list.
        #[arg(long, default_value_t = 20)]
        top: usize,

        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Session summary (status, counts, timings).
    Show {
        #[arg(long)]
        json: bool,
    },
    /// List pages crawled in the session.
    Pages {
        /// Maximum number of pages to show.
        #[arg(long)]
        limit: Option<usize>,

        /// Comma-separated fields to keep in JSON output.
        #[arg(long)]
        fields: Option<String>,

        #[arg(long)]
        json: bool,
    },
    /// Run the SEO issue analyzers over the session's pages.
    Issues {
        /// Maximum example URLs to include per issue.
        #[arg(long, default_value_t = 5)]
        max_urls: usize,

        /// Comma-separated fields to keep in JSON output.
        #[arg(long)]
        fields: Option<String>,

        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum ExportFormat {
    Json,
    Xlsx,
    Csv,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the settings file path.
    Path,
    /// Print the full settings file.
    Show,
    /// Read one key, e.g. `crawler.max_pages`.
    Get { key: String },
    /// Write one key, e.g. `rustyseo config set crawler.max_pages 500`.
    Set { key: String, value: String },
    /// Open the settings file in the OS editor.
    Edit,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to stderr so `--json-events` stdout stays machine-parseable.
    // Set RUST_LOG=info|debug for diagnostics.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Crawl {
            url,
            max_pages,
            concurrency,
            js,
            js_fallback,
            resume,
            since,
            compare,
            no_db,
            quiet,
            local,
            json_events,
            name,
        } => {
            crawl(CrawlArgs {
                url,
                max_pages,
                concurrency,
                js,
                js_fallback,
                resume,
                since,
                compare,
                no_db,
                quiet,
                local,
                json_events,
                name,
            })
            .await
        }
        Command::Sessions {
            limit,
            fields,
            json,
        } => sessions(limit, fields.as_deref(), json),
        Command::Session { id, action } => session_command(id, action),
        Command::Diff {
            baseline,
            current,
            fail_on_regressions,
            fields,
            json,
        } => diff_command(baseline, current, fail_on_regressions, fields.as_deref(), json),
        Command::Export {
            session,
            format,
            out,
            tab,
        } => export(session, format, out, tab),
        Command::Config { action } => config(action),
        Command::Doctor { json } => doctor(json),
        Command::Status { json } => status_command(json).await,
        Command::Watch { json, timeout } => watch_command(json, timeout).await,
        Command::Stop => stop_command().await,
        Command::Serve => serve_command().await,
        Command::Mcp => {
            mcp::serve().await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Sitemap {
            url,
            concurrency,
            max_urls,
            json,
        } => sitemap_command(&url, concurrency, max_urls, json).await,
        Command::Logs { file, top, json } => logs_command(&file, top, json),
    }
}

// ---------------------------------------------------------------------------
// Log analyzer
// ---------------------------------------------------------------------------

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

/// Thousands separators for human-readable counts.
fn commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn short_timestamp(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| raw.to_string())
}

fn logs_command(file: &str, top: usize, json_output: bool) -> anyhow::Result<ExitCode> {
    let report = rustyseo_core::loganalyzer::analyze_file(file, top)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Log report: {}", report.file);
        println!(
            "  lines {} (parsed {} • malformed {})",
            commas(report.lines),
            commas(report.parsed),
            commas(report.malformed)
        );
        if let (Some(first), Some(last)) = (&report.first_timestamp, &report.last_timestamp) {
            println!(
                "  range {} → {}",
                short_timestamp(first),
                short_timestamp(last)
            );
        }
        println!(
            "  requests: {} humans • {} bots ({:.0}%) • {} unique IPs • {}",
            commas(report.human_requests),
            commas(report.bot_requests),
            percent(report.bot_requests, report.parsed),
            commas(report.unique_ips),
            format_bytes(report.total_bytes)
        );

        println!("\nStatus:");
        for (code, count) in &report.status {
            println!(
                "  {code}: {:>9} ({:.1}%)",
                count,
                percent(*count, report.parsed)
            );
        }
        println!(
            "  classes: 2xx {:.0}% • 3xx {:.0}% • 4xx {:.0}% • 5xx {:.0}%",
            percent(report.status_2xx, report.parsed),
            percent(report.status_3xx, report.parsed),
            percent(report.status_4xx, report.parsed),
            percent(report.status_5xx, report.parsed),
        );

        println!("\nTop bots:");
        if report.top_bots.is_empty() {
            println!("  (no bots detected)");
        }
        for bot in &report.top_bots {
            println!(
                "  {:<24} {:>9} reqs • {:>7} urls • {:>6} 4xx • {:>6} 5xx • {}",
                bot.bot,
                bot.requests,
                bot.unique_urls,
                bot.client_errors,
                bot.server_errors,
                format_bytes(bot.bytes)
            );
        }

        println!("\nFile types:");
        for entry in &report.file_types {
            println!(
                "  {:<12} {:>9} reqs • {}",
                entry.name,
                entry.requests,
                format_bytes(entry.bytes)
            );
        }

        println!("\nTop paths:");
        for entry in &report.top_paths {
            println!(
                "  {:>9} reqs • {:>8} • {}",
                entry.requests,
                format_bytes(entry.bytes),
                entry.path
            );
        }

        println!("\nTop error paths:");
        if report.error_paths.is_empty() {
            println!("  (none)");
        }
        for entry in &report.error_paths {
            println!(
                "  {:>6} 4xx • {:>6} 5xx • {:>9} reqs • {}",
                entry.client_errors, entry.server_errors, entry.requests, entry.path
            );
        }

        println!("\nTop IPs:");
        for entry in &report.top_ips {
            println!(
                "  {:<28} {:>9} reqs • {}",
                entry.name,
                entry.requests,
                format_bytes(entry.bytes)
            );
        }

        println!("\nDaily:");
        for day in &report.daily {
            println!(
                "  {}  {:>9} reqs • bots {:.0}% • 4xx {} • 5xx {}",
                day.date,
                day.requests,
                percent(day.bots, day.requests),
                day.client_errors,
                day.server_errors
            );
        }
    }

    if report.parsed == 0 {
        eprintln!("no parseable log lines found in {file}");
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

async fn sitemap_command(
    url: &str,
    concurrency: usize,
    max_urls: Option<usize>,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let report = rustyseo_core::audit::validate_sitemap(url, concurrency, max_urls)
        .await
        .map_err(anyhow::Error::msg)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Sitemap check for {}", report.start_url);
        println!("  entries checked : {}", report.entries);
        println!("  ok              : {}", report.ok);
        println!("  redirects       : {}", report.redirects.len());
        println!("  dead (4xx/5xx)  : {}", report.dead.len());
        println!("  network errors  : {}", report.errors.len());
        if let Some(cap) = report.truncated_at {
            println!("  (capped at {cap} entries)");
        }

        if !report.dead.is_empty() {
            println!("\nDead entries (first 50):");
            for entry in report.dead.iter().take(50) {
                let suffix = if entry.hops > 0 {
                    format!(" (via {} redirect(s))", entry.hops)
                } else {
                    String::new()
                };
                println!(
                    "  [{}] {}{}",
                    entry
                        .final_status
                        .map(|status| status.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    entry.url,
                    suffix
                );
            }
            if report.dead.len() > 50 {
                println!("  … and {} more", report.dead.len() - 50);
            }
        }

        if !report.redirects.is_empty() {
            println!("\nRedirects (first 10):");
            for entry in report.redirects.iter().take(10) {
                println!(
                    "  [{} → {}] {}{}",
                    entry.status.unwrap_or(0),
                    entry.final_status.unwrap_or(0),
                    entry.url,
                    entry
                        .location
                        .as_ref()
                        .map(|location| format!(" → {location}"))
                        .unwrap_or_default()
                );
            }
        }

        if !report.errors.is_empty() {
            println!("\nNetwork errors (first 10):");
            for entry in report.errors.iter().take(10) {
                println!(
                    "  {} — {}",
                    entry.url,
                    entry.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }

    if report.entries == 0 {
        eprintln!("no sitemap entries found for {url}");
        return Ok(ExitCode::from(2));
    }
    if report.problem_count() > 0 {
        return Ok(ExitCode::from(6));
    }
    Ok(ExitCode::SUCCESS)
}

struct CrawlArgs {
    url: String,
    max_pages: Option<usize>,
    concurrency: Option<usize>,
    js: bool,
    js_fallback: bool,
    resume: Option<i64>,
    since: Option<i64>,
    compare: Option<i64>,
    no_db: bool,
    quiet: bool,
    local: bool,
    json_events: bool,
    name: Option<String>,
}

async fn crawl(args: CrawlArgs) -> anyhow::Result<ExitCode> {
    // Live sync: when a sync server is running (GUI or `rustyseo serve`) and
    // this crawl can be expressed as a service request, delegate to it so
    // every frontend sees the same crawl in real time.
    if !args.local
        && args.resume.is_none()
        && args.since.is_none()
        && rustyseo_core::server::server_ping().await.is_some()
    {
        return crawl_via_server(args).await;
    }

    let CrawlArgs {
        url,
        max_pages,
        concurrency,
        js,
        js_fallback,
        resume,
        since,
        compare,
        no_db,
        quiet,
        local: _,
        json_events,
        name,
    } = args;

    if resume.is_some() && no_db {
        eprintln!("error: --resume requires the database (remove --no-db)");
        return Ok(ExitCode::FAILURE);
    }
    if since.is_some() && no_db {
        eprintln!("error: --since requires the database (remove --no-db)");
        return Ok(ExitCode::FAILURE);
    }
    if compare.is_some() && no_db {
        eprintln!("error: --compare requires the database (remove --no-db)");
        return Ok(ExitCode::FAILURE);
    }

    // Make sure the app directories/files exist before touching settings or DB.
    rustyseo_core::settings::utils::create::create_settings_file().await;
    rustyseo_core::settings::utils::create::create_recent_crawls_file().await;
    db::init_db();

    let settings = AppSettings::load();
    let effective_max_pages = max_pages.unwrap_or(settings.crawler.max_pages);
    let use_js_fallback = (js_fallback || settings.crawler.js_fallback) && !js;

    let mut engine = CrawlEngine::new()
        .await
        .with_max_pages(effective_max_pages)
        .with_concurrency(concurrency.unwrap_or(settings.crawler.concurrency))
        .with_robots(settings.crawler.respect_robots)
        .with_crawl_delay(settings.crawler.respect_crawl_delay)
        .with_nofollow(settings.crawler.respect_nofollow)
        .with_noindex(settings.crawler.respect_noindex);

    if js {
        engine = engine.with_javascript(true);
    }
    if use_js_fallback {
        engine = engine.with_js_fallback(true);
    }

    // Incremental re-crawl: a previous session's pages provide ETag /
    // Last-Modified validators so unchanged pages answer 304 and reuse their
    // stored content instead of being re-downloaded.
    if let Some(baseline_id) = since {
        let Some(baseline_session) = db::get_session(baseline_id)? else {
            eprintln!("session {baseline_id} not found");
            return Ok(ExitCode::FAILURE);
        };
        let baseline_pages = db::load_session_pages(baseline_id)?;
        tracing::info!(
            "[INCREMENTAL] baseline session {} — {} stored pages ({})",
            baseline_id,
            baseline_pages.len(),
            baseline_session.start_url
        );
        engine = engine.with_incremental_baseline(baseline_pages);
    }

    // Session handling: fresh session, resumed session, or none.
    let (session_id, base_fetched, base_failed) = if let Some(resume_id) = resume {
        let Some(session) = db::get_session(resume_id)? else {
            eprintln!("session {resume_id} not found");
            return Ok(ExitCode::FAILURE);
        };
        let stored_pages = db::load_session_pages(resume_id)?;
        engine = engine.with_db_persistence(None);
        engine = engine.with_session(resume_id).with_resume_pages(stored_pages);
        (Some(resume_id), session.pages_fetched, session.pages_failed)
    } else if no_db {
        (None, 0, 0)
    } else {
        engine = engine.with_db_persistence(None);
        let sid = db::create_session(name.as_deref(), &url, Some(effective_max_pages))?;
        (Some(sid), 0, 0)
    };

    if let Some(sid) = session_id {
        engine = engine.with_session(sid);
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<CrawlMessage>(1000);
    let task_engine = engine.clone();
    let task_url = url.clone();
    let started = Instant::now();

    tokio::spawn(async move {
        task_engine.crawl_concurrently(&task_url, tx).await;
    });

    let mut pages: usize = 0;
    let mut failed: usize = 0;

    while let Some(message) = rx.recv().await {
        match message {
            CrawlMessage::Page(page) => {
                pages += 1;
                let code = page.status.get(..3).and_then(|c| c.parse::<u16>().ok());
                if code.is_none_or(|c| c >= 400) {
                    failed += 1;
                }
                let links = page.anchor_links.len();
                if json_events {
                    println!(
                        "{}",
                        json!({
                            "event": "page",
                            "id": page.id,
                            "status": page.status,
                            "url": page.url,
                            "links": links,
                            "js_rendered": page.js_rendered,
                        })
                    );
                } else if !quiet {
                    println!("[{}] {} - {} links found", page.status, page.url, links);
                }
            }
            CrawlMessage::Progress {
                scanned,
                queued,
                processing,
            } => {
                if json_events {
                    println!(
                        "{}",
                        json!({
                            "event": "progress",
                            "scanned": scanned,
                            "queued": queued,
                            "processing": processing,
                        })
                    );
                }
            }
        }
    }

    if let Some(sid) = session_id {
        db::finish_session(
            sid,
            "completed",
            base_fetched + pages,
            base_failed + failed,
        )?;
    }

    let stats = engine.stats();
    let robots_blocked = stats.robots_blocked();
    let js_renders = stats.js_renders();
    let not_modified = stats.not_modified();

    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    if json_events {
        println!(
            "{}",
            json!({
                "event": "crawl.end",
                "url": url,
                "session_id": session_id,
                "pages": pages,
                "failed": failed,
                "robots_blocked": robots_blocked,
                "js_renders": js_renders,
                "not_modified": not_modified,
                "elapsed_ms": elapsed_ms,
                "db": !no_db,
            })
        );
    } else {
        let session_note = session_id
            .map(|sid| format!(" (session {sid})"))
            .unwrap_or_default();
        let robots_note = if robots_blocked > 0 {
            format!(", {robots_blocked} skipped by robots.txt")
        } else {
            String::new()
        };
        let js_note = if js_renders > 0 {
            format!(", {js_renders} rendered with headless Chrome")
        } else {
            String::new()
        };
        let cached_note = if not_modified > 0 {
            format!(", {not_modified} unchanged (304, reused stored content)")
        } else {
            String::new()
        };
        println!(
            "\nCrawled {pages} pages ({failed} non-2xx/3xx) in {:.1}s{session_note}{robots_note}{js_note}{cached_note}",
            elapsed.as_secs_f64()
        );
    }

    // Regression check against a baseline session, for scheduled audits.
    let mut regressions = 0usize;
    if let (Some(baseline_id), Some(current_id)) = (compare, session_id) {
        regressions = compare_against(baseline_id, current_id, json_events)?;
    }

    if pages == 0 {
        return Ok(ExitCode::from(2));
    }
    if regressions > 0 {
        return Ok(ExitCode::from(6));
    }
    Ok(ExitCode::SUCCESS)
}

/// Diff `current` against `baseline`, print the result and return the number
/// of regressions (removed pages, new 4xx/5xx, newly noindexed).
fn compare_against(
    baseline_id: i64,
    current_id: i64,
    json_events: bool,
) -> anyhow::Result<usize> {
    let baseline_session = db::get_session(baseline_id)?
        .ok_or_else(|| anyhow::anyhow!("baseline session {baseline_id} not found"))?;
    let current_session = db::get_session(current_id)?
        .ok_or_else(|| anyhow::anyhow!("session {current_id} not found"))?;

    let baseline_summaries = session_summaries(baseline_id, &baseline_session.start_url)?;
    let current_summaries = session_summaries(current_id, &current_session.start_url)?;
    let (entries, diff_stats) = diff::diff_sessions(&baseline_summaries, &current_summaries);
    let regression_entries: Vec<&DiffEntry> =
        entries.iter().filter(|entry| is_regression(entry)).collect();
    let regressions = regression_entries.len();

    if json_events {
        println!(
            "{}",
            json!({
                "event": "diff",
                "baseline": baseline_id,
                "added": diff_stats.added,
                "removed": diff_stats.removed,
                "changed": diff_stats.changed,
                "unchanged": diff_stats.unchanged,
                "regressions": regressions,
            })
        );
    } else {
        println!(
            "Diff vs session {baseline_id}: +{} added, −{} removed, ~{} changed, {} unchanged — {} regression(s)",
            diff_stats.added,
            diff_stats.removed,
            diff_stats.changed,
            diff_stats.unchanged,
            regressions
        );
        for entry in regression_entries.iter().take(20) {
            println!("  [{}] {}", diff_kind_label(entry.kind), entry.url);
        }
    }

    Ok(regressions)
}

/// Delegate a crawl to the running sync server and stream its events, so the
/// output matches a local crawl while the GUI (or another terminal) sees the
/// same crawl live.
async fn crawl_via_server(args: CrawlArgs) -> anyhow::Result<ExitCode> {
    let CrawlArgs {
        url,
        max_pages,
        js,
        compare,
        quiet,
        json_events,
        name,
        ..
    } = args;

    let mut client = SyncClient::connect().await.map_err(anyhow::Error::msg)?;
    if !quiet && !json_events {
        println!("Delegating to the running sync server (use --local to crawl in this process).");
    }

    let result = client
        .request(
            "crawl.start",
            json!({ "url": url, "name": name, "max_pages": max_pages, "js": js }),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let session_id = result.get("session_id").and_then(serde_json::Value::as_i64);

    client
        .request("subscribe", json!({}))
        .await
        .map_err(anyhow::Error::msg)?;

    let mut pages = 0usize;
    let mut failed = 0usize;
    while let Some(event) = client.next_event().await.map_err(anyhow::Error::msg)? {
        let event_name = event
            .get("event")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        match event_name.as_str() {
            "page" => {
                pages += 1;
                let status = event["status"].as_str().unwrap_or("");
                let code = status.get(..3).and_then(|c| c.parse::<u16>().ok());
                if code.is_none_or(|c| c >= 400) {
                    failed += 1;
                }
                if json_events {
                    println!("{event}");
                } else if !quiet {
                    let suffix = match (
                        event["js_rendered"].as_bool(),
                        event["not_modified"].as_bool(),
                    ) {
                        (Some(true), _) => " [js]",
                        (_, Some(true)) => " [304]",
                        _ => "",
                    };
                    println!(
                        "[{}] {} - {} links found{}",
                        status,
                        event["url"].as_str().unwrap_or(""),
                        event["links"],
                        suffix
                    );
                }
            }
            "progress" => {
                if json_events {
                    println!("{event}");
                }
            }
            "crawl.end" => {
                pages = event["pages"].as_u64().unwrap_or(pages as u64) as usize;
                failed = event["failed"].as_u64().unwrap_or(failed as u64) as usize;
                if json_events {
                    println!("{event}");
                }
                break;
            }
            _ => {}
        }
    }

    if !json_events {
        let session_note = session_id
            .map(|sid| format!(" (session {sid})"))
            .unwrap_or_default();
        println!("\nCrawled {pages} pages ({failed} non-2xx/3xx){session_note}");
    }

    let mut regressions = 0usize;
    if let (Some(baseline_id), Some(current_id)) = (compare, session_id) {
        regressions = compare_against(baseline_id, current_id, json_events)?;
    }

    if pages == 0 {
        return Ok(ExitCode::from(2));
    }
    if regressions > 0 {
        return Ok(ExitCode::from(6));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Live sync commands
// ---------------------------------------------------------------------------

async fn status_command(json_output: bool) -> anyhow::Result<ExitCode> {
    match SyncClient::connect().await {
        Ok(mut client) => {
            let status = client
                .request("status", json!({}))
                .await
                .map_err(anyhow::Error::msg)?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "server": true, "status": status }))?
                );
            } else if status["running"].as_bool() == Some(true) {
                println!(
                    "Crawling {} — {} pages, {} failed, queued {}, processing {} • {:.0}s{}",
                    status["url"].as_str().unwrap_or(""),
                    status["pages"].as_u64().unwrap_or(0),
                    status["failed"].as_u64().unwrap_or(0),
                    status["queued"].as_u64().unwrap_or(0),
                    status["processing"].as_u64().unwrap_or(0),
                    status["elapsed_ms"].as_u64().unwrap_or(0) as f64 / 1000.0,
                    if status["stop_requested"].as_bool() == Some(true) {
                        " (stopping…)"
                    } else {
                        ""
                    }
                );
                if let Some(sid) = status["session_id"].as_i64() {
                    println!("  session: {sid}");
                }
            } else {
                println!("Sync server is running; no crawl is active.");
            }
        }
        Err(_) => {
            if json_output {
                println!("{}", json!({ "server": false }));
            } else {
                println!(
                    "No RustySEO sync server is running.\nStart one with `rustyseo serve`, or open the GUI."
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn watch_command(json_output: bool, timeout_secs: Option<u64>) -> anyhow::Result<ExitCode> {
    let mut client = match SyncClient::connect().await {
        Ok(client) => client,
        Err(err) => {
            eprintln!(
                "error: {err}\nStart a sync server (`rustyseo serve`) or open the GUI first."
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    client
        .request("subscribe", json!({}))
        .await
        .map_err(anyhow::Error::msg)?;

    let deadline = timeout_secs.map(|secs| Instant::now() + Duration::from_secs(secs));
    loop {
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                eprintln!("watch timed out");
                return Ok(ExitCode::from(7));
            }
        }
        let event = match tokio::time::timeout(Duration::from_secs(1), client.next_event()).await {
            Ok(Ok(Some(event))) => event,
            Ok(Ok(None)) => break,
            Ok(Err(err)) => {
                eprintln!("error: {err}");
                return Ok(ExitCode::FAILURE);
            }
            Err(_) => continue, // tick: re-check the deadline
        };

        let event_name = event
            .get("event")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if json_output {
            println!("{event}");
        } else {
            match event_name {
                "page" => {
                    let suffix = match (
                        event["js_rendered"].as_bool(),
                        event["not_modified"].as_bool(),
                    ) {
                        (Some(true), _) => " [js]",
                        (_, Some(true)) => " [304]",
                        _ => "",
                    };
                    println!(
                        "[{}] {} - {} links found{}",
                        event["status"].as_str().unwrap_or(""),
                        event["url"].as_str().unwrap_or(""),
                        event["links"],
                        suffix
                    );
                }
                "crawl.end" => {
                    println!(
                        "\nCrawl finished: {} pages, {} failed (session {}, {}s)",
                        event["pages"].as_u64().unwrap_or(0),
                        event["failed"].as_u64().unwrap_or(0),
                        event["session_id"].as_i64().unwrap_or(-1),
                        event["elapsed_ms"].as_u64().unwrap_or(0) / 1000
                    );
                }
                _ => {}
            }
        }
        if event_name == "crawl.end" {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn stop_command() -> anyhow::Result<ExitCode> {
    let mut client = match SyncClient::connect().await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("error: {err}");
            return Ok(ExitCode::FAILURE);
        }
    };
    let result = client
        .request("crawl.stop", json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    if result["stopped"].as_bool() == Some(true) {
        println!("Stop requested; the crawl will end after in-flight requests finish.");
    } else {
        println!("No crawl is running.");
    }
    Ok(ExitCode::SUCCESS)
}

async fn serve_command() -> anyhow::Result<ExitCode> {
    db::init_db();
    rustyseo_core::settings::utils::create::create_settings_file().await;

    let service = rustyseo_core::server::CrawlService::new();
    match rustyseo_core::server::start_sync_server(service).await {
        Ok(info) => {
            println!(
                "RustySEO sync server listening on 127.0.0.1:{} (token: {})",
                info.port,
                rustyseo_core::server::token_file().display()
            );
            println!(
                "`rustyseo crawl <url>` delegates here, `rustyseo watch` streams events, `rustyseo stop` stops."
            );
        }
        Err(err) => {
            eprintln!("error: {err}");
            return Ok(ExitCode::FAILURE);
        }
    }

    tokio::signal::ctrl_c().await.ok();
    println!("\nSync server shutting down.");
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

fn diff_kind_label(kind: DiffKind) -> &'static str {
    match kind {
        DiffKind::Added => "added",
        DiffKind::Removed => "removed",
        DiffKind::Changed => "changed",
    }
}

fn is_regression(entry: &DiffEntry) -> bool {
    match entry.kind {
        DiffKind::Removed => true,
        DiffKind::Added => false,
        DiffKind::Changed => entry.changes.iter().any(|change| {
            (change.field == "Status"
                && (change.new.starts_with('4') || change.new.starts_with('5')))
                || (change.field == "Indexability" && change.new.to_lowercase().contains("noindex"))
        }),
    }
}

fn session_summaries(session_id: i64, start_url: &str) -> anyhow::Result<Vec<PageSummary>> {
    let pages = db::load_session_pages(session_id)?;
    let base_domain = url::Url::parse(start_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()));
    Ok(pages
        .iter()
        .map(|page| page.summarize(base_domain.as_deref()))
        .collect())
}

fn diff_command(
    baseline: i64,
    current: i64,
    fail_on_regressions: bool,
    fields_raw: Option<&str>,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    db::init_db();
    let Some(baseline_session) = db::get_session(baseline)? else {
        eprintln!("session {baseline} not found");
        return Ok(ExitCode::FAILURE);
    };
    let Some(current_session) = db::get_session(current)? else {
        eprintln!("session {current} not found");
        return Ok(ExitCode::FAILURE);
    };

    let baseline_summaries = session_summaries(baseline, &baseline_session.start_url)?;
    let current_summaries = session_summaries(current, &current_session.start_url)?;
    let (entries, stats) = diff::diff_sessions(&baseline_summaries, &current_summaries);
    let regressions = entries.iter().filter(|entry| is_regression(entry)).count();

    if json_output {
        let fields = parse_fields(fields_raw);
        let entries_json: Vec<serde_json::Value> = entries
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "baseline": { "id": baseline, "start_url": baseline_session.start_url },
                "current": { "id": current, "start_url": current_session.start_url },
                "stats": stats,
                "regressions": regressions,
                "entries": project_array(&entries_json, &fields),
            }))?
        );
    } else {
        println!(
            "Diff #{baseline} → #{current} ({})",
            current_session.start_url
        );
        println!(
            "  +{} added   −{} removed   ~{} changed   {} unchanged   {} regression(s)",
            stats.added, stats.removed, stats.changed, stats.unchanged, regressions
        );
        for entry in entries.iter().take(500) {
            let changes = entry
                .changes
                .iter()
                .map(|change| format!("{}: {} → {}", change.field, change.old, change.new))
                .collect::<Vec<_>>()
                .join("; ");
            if changes.is_empty() {
                println!("  [{}] {}", diff_kind_label(entry.kind), entry.url);
            } else {
                println!(
                    "  [{}] {} — {}",
                    diff_kind_label(entry.kind),
                    entry.url,
                    changes
                );
            }
        }
        if entries.len() > 500 {
            println!("  … and {} more", entries.len() - 500);
        }
    }

    if fail_on_regressions && regressions > 0 {
        return Ok(ExitCode::from(6));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

fn format_timestamp(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| raw.to_string())
}

fn parse_fields(raw: Option<&str>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(|field| field.trim().to_string())
            .filter(|field| !field.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn project_object(value: &serde_json::Value, fields: &[String]) -> serde_json::Value {
    if fields.is_empty() {
        return value.clone();
    }
    let mut map = serde_json::Map::new();
    if let Some(object) = value.as_object() {
        for field in fields {
            if let Some(field_value) = object.get(field) {
                map.insert(field.clone(), field_value.clone());
            }
        }
    }
    serde_json::Value::Object(map)
}

fn project_array(values: &[serde_json::Value], fields: &[String]) -> Vec<serde_json::Value> {
    values
        .iter()
        .map(|value| project_object(value, fields))
        .collect()
}

fn sessions(limit: usize, fields_raw: Option<&str>, json_output: bool) -> anyhow::Result<ExitCode> {
    db::init_db();
    let rows = db::list_sessions(limit)?;

    if json_output {
        let fields = parse_fields(fields_raw);
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?;
        println!("{}", serde_json::to_string_pretty(&project_array(&values, &fields))?);
        return Ok(ExitCode::SUCCESS);
    }

    if rows.is_empty() {
        println!("No sessions recorded yet. Run `rustyseo crawl <url>` first.");
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{:<4} {:<18} {:<10} {:>5} {:>6}  {:<16}  {}",
        "ID", "NAME", "STATUS", "PAGES", "FAILED", "STARTED", "URL"
    );
    for row in rows {
        println!(
            "{:<4} {:<18} {:<10} {:>5} {:>6}  {:<16}  {}",
            row.id,
            row.name.as_deref().unwrap_or("-"),
            row.status,
            row.pages_fetched,
            row.pages_failed,
            format_timestamp(&row.started_at),
            row.start_url,
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn session_command(id: i64, action: SessionAction) -> anyhow::Result<ExitCode> {
    db::init_db();
    let Some(session) = db::get_session(id)? else {
        eprintln!("session {id} not found");
        return Ok(ExitCode::FAILURE);
    };

    match action {
        SessionAction::Show { json: json_output } => {
            let page_count = db::session_page_count(id)?;
            if json_output {
                let mut value = serde_json::to_value(&session)?;
                value["stored_pages"] = json!(page_count);
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("Session {} — {}", session.id, session.start_url);
                if let Some(name) = &session.name {
                    println!("  name      : {name}");
                }
                println!("  status    : {}", session.status);
                println!("  started   : {}", format_timestamp(&session.started_at));
                if let Some(finished) = &session.finished_at {
                    println!("  finished  : {}", format_timestamp(finished));
                }
                println!(
                    "  pages     : {} fetched, {} failed (max_pages={})",
                    session.pages_fetched,
                    session.pages_failed,
                    session
                        .max_pages
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "-".into()),
                );
                println!("  stored    : {page_count} page records");
            }
        }
        SessionAction::Pages {
            limit,
            fields,
            json: json_output,
        } => {
            let rows = db::session_page_rows(id, limit)?;
            if json_output {
                let fields = parse_fields(fields.as_deref());
                let values: Vec<serde_json::Value> = rows
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<_, _>>()?;
                println!("{}", serde_json::to_string_pretty(&project_array(&values, &fields))?);
            } else {
                println!(
                    "{:<4} {:<10} {:>5}  {:<40}  {}",
                    "ID", "STATUS", "WORDS", "TITLE", "URL"
                );
                for row in rows {
                    let title: String = row.title.chars().take(40).collect();
                    println!(
                        "{:<4} {:<10} {:>5}  {:<40}  {}",
                        row.page_id, row.status, row.word_count, title, row.url
                    );
                }
            }
        }
        SessionAction::Issues {
            max_urls,
            fields,
            json: json_output,
        } => {
            let pages = db::load_session_pages(id)?;
            let base_domain = url::Url::parse(&session.start_url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()));
            let summaries: Vec<_> = pages
                .iter()
                .map(|p| p.summarize(base_domain.as_deref()))
                .collect();

            let mut findings = Vec::new();
            let mut total = 0usize;
            for handler in IssueAnalyzer::get_handlers() {
                let (count, urls) = (handler.process)(&summaries);
                total += count;
                if count > 0 {
                    findings.push(json!({
                        "issue": handler.name.trim(),
                        "severity": severity_for(handler.name).as_str(),
                        "count": count,
                        "urls": urls.iter().take(max_urls).collect::<Vec<_>>(),
                    }));
                }
            }

            if json_output {
                let fields = parse_fields(fields.as_deref());
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "session_id": id,
                        "pages_analyzed": summaries.len(),
                        "total_findings": total,
                        "issues": project_array(&findings, &fields),
                    }))?
                );
            } else {
                println!(
                    "Session {id}: {} issues found across {} pages\n",
                    total,
                    summaries.len()
                );
                for finding in &findings {
                    let name = finding["issue"].as_str().unwrap_or("?");
                    let severity = finding["severity"].as_str().unwrap_or("notice");
                    let count = finding["count"].as_u64().unwrap_or(0);
                    println!("  [{severity}] {name}: {count}");
                    if let Some(urls) = finding["urls"].as_array() {
                        for url in urls.iter().take(2) {
                            println!("      {}", url.as_str().unwrap_or(""));
                        }
                    }
                }
                if findings.is_empty() {
                    println!("  No issues detected.");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn export(
    session_id: i64,
    format: ExportFormat,
    out: Option<String>,
    tab: Option<String>,
) -> anyhow::Result<ExitCode> {
    if tab.is_some() && !matches!(format, ExportFormat::Csv) {
        eprintln!("error: --tab only applies to --format csv");
        return Ok(ExitCode::FAILURE);
    }
    if out.as_deref() == Some("-") && !matches!(format, ExportFormat::Json) {
        eprintln!("error: --out - (stdout) only supported for --format json");
        return Ok(ExitCode::FAILURE);
    }

    db::init_db();
    let Some(session) = db::get_session(session_id)? else {
        eprintln!("session {session_id} not found");
        return Ok(ExitCode::FAILURE);
    };
    let pages = db::load_session_pages(session_id)?;
    if pages.is_empty() {
        eprintln!("session {session_id} has no stored pages to export");
        return Ok(ExitCode::FAILURE);
    }

    let base_domain = url::Url::parse(&session.start_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()));
    let sheets = rustyseo_core::export::build_report_sheets(&pages, base_domain.as_deref());

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let target = |ext: &str| -> anyhow::Result<std::path::PathBuf> {
        match &out {
            Some(path) => Ok(std::path::PathBuf::from(path)),
            None => Ok(rustyseo_core::export::exports_dir()?
                .join(format!("rustyseo_session{session_id}_{timestamp}.{ext}"))),
        }
    };

    match format {
        ExportFormat::Json => {
            let value = rustyseo_core::export::report_json(&session, &sheets)?;
            let pretty = serde_json::to_string_pretty(&value)?;
            if out.as_deref() == Some("-") {
                println!("{pretty}");
            } else {
                let path = target("json")?;
                std::fs::write(&path, pretty)?;
                println!("{}", path.display());
            }
        }
        ExportFormat::Xlsx => {
            let path = target("xlsx")?;
            rustyseo_core::export::write_xlsx(&sheets, &path)?;
            println!("{}", path.display());
        }
        ExportFormat::Csv => {
            if let Some(tab) = tab {
                let sheet = sheets
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(&tab))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "sheet not found: {tab} (available: {})",
                            sheets
                                .iter()
                                .map(|s| s.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                let path = target("csv")?;
                rustyseo_core::export::write_csv(sheet, &path)?;
                println!("{}", path.display());
            } else {
                let path = target("zip")?;
                rustyseo_core::export::write_csv_zip(&sheets, &path)?;
                println!("{}", path.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn config(action: ConfigAction) -> anyhow::Result<ExitCode> {
    let path = AppSettings::path();
    if !path.exists() {
        // Sync creation of defaults: reuse the core model and write the toml.
        std::fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
        let defaults = toml::to_string_pretty(&AppSettings::default())?;
        std::fs::write(&path, defaults)?;
    }

    match action {
        ConfigAction::Path => {
            println!("{}", path.display());
        }
        ConfigAction::Show => {
            print!("{}", std::fs::read_to_string(&path)?);
        }
        ConfigAction::Edit => {
            rustyseo_core::settings::utils::open::edit_file();
        }
        ConfigAction::Get { key } => {
            let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
            let mut cursor = &doc;
            for part in key.split('.') {
                match cursor.get(part) {
                    Some(value) => cursor = value,
                    None => {
                        eprintln!("config key not found: {key}");
                        return Ok(ExitCode::from(3));
                    }
                }
            }
            println!("{cursor}");
        }
        ConfigAction::Set { key, value } => {
            let raw = std::fs::read_to_string(&path)?;
            let mut doc: toml::Value = toml::from_str(&raw)?;
            // Parse "500" / "true" / "hello" as a TOML value, falling back to string.
            let parsed = toml::from_str::<toml::Value>(&format!("v = {value}"))
                .map(|v| v.get("v").cloned().unwrap_or(toml::Value::String(value.clone())))
                .unwrap_or(toml::Value::String(value.clone()));

            let mut cursor = &mut doc;
            let parts: Vec<&str> = key.split('.').collect();
            for (index, part) in parts.iter().enumerate() {
                if index == parts.len() - 1 {
                    cursor
                        .as_table_mut()
                        .ok_or_else(|| anyhow::anyhow!("{key} is not a table path"))?
                        .insert((*part).to_string(), parsed.clone());
                } else {
                    cursor = cursor
                        .as_table_mut()
                        .ok_or_else(|| anyhow::anyhow!("{key} is not a table path"))?
                        .entry((*part).to_string())
                        .or_insert(toml::Value::Table(toml::value::Table::new()));
                }
            }
            std::fs::write(&path, toml::to_string_pretty(&doc)?)?;
            println!("{key} = {parsed}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

fn doctor(json_output: bool) -> anyhow::Result<ExitCode> {
    let settings_path = AppSettings::path();
    let settings_ok = settings_path.exists();
    let settings_parse_error = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).err())
        .map(|err| err.to_string());

    db::init_db();
    let db_path = db::get_db_path();
    let db_ok = db_path.exists();
    let page_count = if db_ok { db::get_page_count() } else { 0 };
    let session_count = db::list_sessions(1).map(|s| s.len()).unwrap_or(0);

    let chrome = find_chrome();
    let chrome_ok = chrome.is_some();

    if json_output {
        println!(
            "{}",
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "settings": {
                    "path": settings_path.display().to_string(),
                    "exists": settings_ok,
                    "parse_error": settings_parse_error,
                },
                "database": {
                    "path": db_path.display().to_string(),
                    "exists": db_ok,
                    "pages": page_count,
                    "has_sessions": session_count > 0,
                },
                "chrome": {
                    "found": chrome_ok,
                    "path": chrome.map(|p| p.display().to_string()),
                },
            })
        );
    } else {
        println!("rustyseo {} — environment check\n", env!("CARGO_PKG_VERSION"));
        println!(
            "  settings  : {}  ({})",
            settings_path.display(),
            if settings_ok { "ok" } else { "missing — created on first crawl" }
        );
        if let Some(err) = &settings_parse_error {
            println!("              parse error: {err}");
        }
        println!("  database  : {}  ({page_count} pages)", db_path.display());
        match &chrome {
            Some(path) => println!("  chrome    : {}", path.display()),
            None => println!("  chrome    : not found (only needed for --js / --js-fallback crawls)"),
        }
    }

    let healthy = settings_parse_error.is_none() && db_ok;
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn find_chrome() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        candidates.push("/Applications/Chromium.app/Contents/MacOS/Chromium".into());
        candidates.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
    } else if cfg!(target_os = "windows") {
        for base in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ] {
            candidates.push(base.into());
        }
    } else {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ] {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        candidates.push(path.into());
                    }
                }
            }
        }
    }
    candidates.into_iter().find(|p| p.exists())
}
