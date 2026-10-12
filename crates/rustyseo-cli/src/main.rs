//! RustySEO CLI — scriptable, agent-friendly access to the crawl engine.
//!
//! Exit codes:
//!   0  success
//!   1  fatal error (bad args, engine failure)
//!   2  crawl produced no pages
//!   3  config key not found
//!
//! With `--json-events`, crawl emits newline-delimited JSON on stdout:
//!   {"event":"page", ...} and a final {"event":"crawl.end", ...} record.

use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use serde_json::json;

use rustyseo_core::crawler::CrawlMessage;
use rustyseo_core::crawler::CrawlEngine;
use rustyseo_core::models::AppSettings;

#[derive(Parser)]
#[command(name = "rustyseo", version, about = "Crawl and audit websites from the terminal.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a website (depth-first over internal links).
    Crawl {
        /// Start URL, e.g. https://example.com
        url: String,

        /// Maximum number of pages to fetch.
        #[arg(long)]
        max_pages: Option<usize>,

        /// Concurrent requests.
        #[arg(long)]
        concurrency: Option<usize>,

        /// Render pages with headless Chrome (needs Chrome/Chromium installed).
        #[arg(long)]
        js: bool,

        /// Do not persist pages to the local SQLite database.
        #[arg(long)]
        no_db: bool,

        /// Emit newline-delimited JSON events instead of human-readable lines.
        #[arg(long)]
        json_events: bool,
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
            no_db,
            json_events,
        } => crawl(&url, max_pages, concurrency, js, no_db, json_events).await,
        Command::Config { action } => config(action),
        Command::Doctor { json } => doctor(json),
    }
}

async fn crawl(
    url: &str,
    max_pages: Option<usize>,
    concurrency: Option<usize>,
    js: bool,
    no_db: bool,
    json_events: bool,
) -> anyhow::Result<ExitCode> {
    // Make sure the app directories/files exist before touching settings or DB.
    rustyseo_core::settings::utils::create::create_settings_file().await;
    rustyseo_core::settings::utils::create::create_recent_crawls_file().await;
    rustyseo_core::db::init_db();

    let settings = AppSettings::load();

    let mut engine = CrawlEngine::new()
        .await
        .with_max_pages(max_pages.unwrap_or(settings.crawler.max_pages))
        .with_concurrency(concurrency.unwrap_or(settings.crawler.concurrency))
        .with_javascript(js);

    if !no_db {
        engine = engine.with_db_persistence(None);
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<CrawlMessage>(1000);
    let task_engine = engine.clone();
    let task_url = url.to_string();
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
                            "js": js,
                        })
                    );
                } else {
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

    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    if json_events {
        println!(
            "{}",
            json!({
                "event": "crawl.end",
                "url": url,
                "pages": pages,
                "failed": failed,
                "elapsed_ms": elapsed_ms,
                "db": !no_db,
            })
        );
    } else {
        println!(
            "\nCrawled {pages} pages ({failed} non-2xx/3xx) in {:.1}s",
            elapsed.as_secs_f64()
        );
    }

    if pages == 0 {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

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

fn doctor(json_output: bool) -> anyhow::Result<ExitCode> {
    let settings_path = AppSettings::path();
    let settings_ok = settings_path.exists();
    let settings_parse_error = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).err())
        .map(|err| err.to_string());

    rustyseo_core::db::init_db();
    let db_path = rustyseo_core::db::get_db_path();
    let db_ok = db_path.exists();
    let page_count = if db_ok {
        rustyseo_core::db::get_page_count()
    } else {
        0
    };

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
        println!(
            "  database  : {}  ({page_count} pages)",
            db_path.display()
        );
        match &chrome {
            Some(path) => println!("  chrome    : {}", path.display()),
            None => println!("  chrome    : not found (only needed for --js crawls)"),
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
