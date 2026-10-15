//! Periscrawl core: crawl engine, persistence, settings and analysis.
//!
//! This crate is frontend-agnostic. It must never depend on ratatui, crossterm,
//! egui or any other UI toolkit. The CLI, GUI and TUI crates are thin layers on
//! top of it.

pub mod audit;
pub mod content_audit;
pub mod crawler;
pub mod db;
pub mod diff;
pub mod export;
pub mod helpers;
pub mod loganalyzer;
pub mod logging;
pub mod models;
pub mod paths;
pub mod report;
pub mod server;
pub mod settings;

pub use crawler::{CrawlEngine, CrawlMessage, PageData};
pub use models::AppSettings;
