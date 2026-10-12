use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::models::AppSettings;

/// Statistics tracker for crawl operations
#[derive(Debug, Clone)]
pub struct CrawlStats {
    pub pages_crawled: Arc<AtomicUsize>,
    pub pages_failed: Arc<AtomicUsize>,
    pub links_discovered: Arc<AtomicUsize>,
    pub links_filtered: Arc<AtomicUsize>,
    pub links_duplicate: Arc<AtomicUsize>,
    pub robots_blocked: Arc<AtomicUsize>,
    pub js_renders: Arc<AtomicUsize>,
}

impl CrawlStats {
    pub fn new() -> Self {
        Self {
            pages_crawled: Arc::new(AtomicUsize::new(0)),
            pages_failed: Arc::new(AtomicUsize::new(0)),
            links_discovered: Arc::new(AtomicUsize::new(0)),
            links_filtered: Arc::new(AtomicUsize::new(0)),
            links_duplicate: Arc::new(AtomicUsize::new(0)),
            robots_blocked: Arc::new(AtomicUsize::new(0)),
            js_renders: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn increment_crawled(&self) {
        self.pages_crawled.fetch_add(1, Ordering::SeqCst);
    }

    pub fn increment_failed(&self) {
        self.pages_failed.fetch_add(1, Ordering::SeqCst);
    }

    pub fn add_discovered(&self, count: usize) {
        self.links_discovered.fetch_add(count, Ordering::SeqCst);
    }

    pub fn add_filtered(&self, count: usize) {
        self.links_filtered.fetch_add(count, Ordering::SeqCst);
    }

    pub fn add_duplicate(&self, count: usize) {
        self.links_duplicate.fetch_add(count, Ordering::SeqCst);
    }

    pub fn increment_robots_blocked(&self) {
        self.robots_blocked.fetch_add(1, Ordering::SeqCst);
    }

    pub fn robots_blocked(&self) -> usize {
        self.robots_blocked.load(Ordering::SeqCst)
    }

    pub fn increment_js_render(&self) {
        self.js_renders.fetch_add(1, Ordering::SeqCst);
    }

    pub fn js_renders(&self) -> usize {
        self.js_renders.load(Ordering::SeqCst)
    }

    pub fn get_summary(&self) -> String {
        format!(
            "Crawled: {}, Failed: {}, Links Found: {}, Filtered: {}, Duplicates: {}, Robots Blocked: {}, JS Renders: {}, Batch Size Intertion: {}",
            self.pages_crawled.load(Ordering::SeqCst),
            self.pages_failed.load(Ordering::SeqCst),
            self.links_discovered.load(Ordering::SeqCst),
            self.links_filtered.load(Ordering::SeqCst),
            self.links_duplicate.load(Ordering::SeqCst),
            self.robots_blocked.load(Ordering::SeqCst),
            self.js_renders.load(Ordering::SeqCst),
            AppSettings::load().crawler.batch_size
        )
    }
}

impl Default for CrawlStats {
    fn default() -> Self {
        Self::new()
    }
}
