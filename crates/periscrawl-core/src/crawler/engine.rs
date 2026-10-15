use headless_chrome::{Browser, LaunchOptions};
use reqwest::Client;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::time::Duration;
use url::Url;

use crate::crawler::fetching::{FetchOutcome, fetch_and_process, validators_from};
use crate::crawler::helpers::html_parser::PageData;
use crate::crawler::helpers::rate_limiter::AdaptiveRateLimiter;
use crate::crawler::helpers::robots::{RobotsRules, fetch_robots};
use crate::crawler::helpers::user_agents::user_agents;
use crate::db;
use crate::models::AppSettings;
use crate::settings::utils::create::add_recent_entry;

#[derive(Clone)]
pub struct CrawlEngine {
    client: Client,
    visited: Arc<Mutex<HashSet<String>>>,
    finished_urls: Arc<Mutex<HashSet<String>>>,
    success_count: Arc<AtomicUsize>,
    max_pages: usize,
    user_agents: Vec<String>,
    concurrency_limit: usize,
    semaphore: Arc<Semaphore>,
    stats: crate::crawler::stats::CrawlStats,
    enable_javascript: bool,
    browser: Option<Arc<Browser>>,
    pagespeed_config: Option<crate::models::PageSpeedConfig>,
    /// Per-run PageSpeed sampling state (nav pages + one per template).
    psi_sampler: Arc<crate::crawler::helpers::psi::PsiSampler>,
    /// Permit pool bounding concurrent background PSI requests.
    psi_semaphore: Arc<Semaphore>,
    /// Dedicated client for PSI: the API can take up to a minute, so it gets
    /// a much longer timeout than crawl requests.
    psi_client: Client,
    enable_db_persist: bool,
    pending_pages: Arc<Mutex<Vec<PageData>>>,
    db_batch_size: usize,
    /// When set, persisted pages are scoped to this crawl session.
    session_id: Option<i64>,
    /// Respect robots.txt disallow rules while following links.
    respect_robots: bool,
    /// Honor robots.txt Crawl-delay directives.
    respect_crawl_delay: bool,
    /// Do not follow rel="nofollow" links.
    respect_nofollow: bool,
    /// Do not follow links discovered on noindex pages.
    respect_noindex: bool,
    /// Render pages with headless Chrome when the raw HTML looks client-rendered.
    js_fallback: bool,
    /// Pages already crawled by a previous (interrupted) run of this session.
    resume_pages: Option<Arc<Vec<PageData>>>,
    /// Highest page id already stored for this session (0 for fresh crawls).
    id_base: usize,
    /// Baseline pages from a previous session for conditional requests
    /// (incremental re-crawls). Keyed by normalized URL.
    incremental: Option<Arc<HashMap<String, PageData>>>,
    /// Cooperative stop flag: when set, the crawl drains and ends early.
    stop_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Shared adaptive pacing for outgoing requests (429/5xx aware).
    rate_limiter: Arc<AdaptiveRateLimiter>,
}

impl std::fmt::Debug for CrawlEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrawlEngine")
            .field("client", &self.client)
            .field("visited", &self.visited)
            .field("success_count", &self.success_count)
            .field("max_pages", &self.max_pages)
            .field("user_agents", &self.user_agents)
            .field("concurrency_limit", &self.concurrency_limit)
            .field("semaphore", &self.semaphore)
            .field("stats", &self.stats)
            .field("enable_javascript", &self.enable_javascript)
            .field("browser", &self.browser.is_some())
            .field("pagespeed", &self.pagespeed_config.is_some())
            .finish()
    }
}

impl CrawlEngine {
    pub async fn new() -> Self {
        let user_agents_vec = match user_agents() {
            Ok(agents) if !agents.is_empty() => agents,
            _ => vec!["Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string()],
        };

        let default_ua = user_agents_vec[0].clone();
        let concurrency_limit = 10; // Default limit
        let db_batch_size = AppSettings::load().crawler.batch_size;

        Self {
            client: Client::builder()
                .user_agent(default_ua)
                .timeout(Duration::from_secs(15))
                .pool_max_idle_per_host(concurrency_limit)
                .tcp_keepalive(Some(Duration::from_secs(60)))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            visited: Arc::new(Mutex::new(HashSet::new())),
            finished_urls: Arc::new(Mutex::new(HashSet::new())),
            success_count: Arc::new(AtomicUsize::new(0)),
            max_pages: 500, // Safety limit
            user_agents: user_agents_vec,
            concurrency_limit,
            semaphore: Arc::new(Semaphore::new(concurrency_limit)),
            stats: crate::crawler::stats::CrawlStats::new(),
            enable_javascript: false,
            browser: None,
            pagespeed_config: None,
            psi_sampler: Arc::new(crate::crawler::helpers::psi::PsiSampler::new()),
            psi_semaphore: Arc::new(Semaphore::new(
                crate::crawler::helpers::psi::DEFAULT_CONCURRENCY,
            )),
            psi_client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            enable_db_persist: false,
            pending_pages: Arc::new(Mutex::new(Vec::with_capacity(db_batch_size))),
            db_batch_size,
            session_id: None,
            respect_robots: true,
            respect_crawl_delay: false,
            respect_nofollow: false,
            respect_noindex: false,
            js_fallback: false,
            resume_pages: None,
            id_base: 0,
            incremental: None,
            stop_flag: None,
            rate_limiter: Arc::new(AdaptiveRateLimiter::default_polite()),
        }
    }

    // This is a bit of a hack to ensure that the crawler doesn't get too far ahead of the UI.

    pub fn with_pagespeed(mut self, config: Option<crate::models::PageSpeedConfig>) -> Self {
        self.pagespeed_config = config;
        self
    }

    pub fn with_javascript(mut self, enable: bool) -> Self {
        self.enable_javascript = enable;
        if enable {
            let options = LaunchOptions {
                headless: true,
                ..Default::default()
            };
            match Browser::new(options) {
                Ok(b) => self.browser = Some(Arc::new(b)),
                Err(e) => tracing::error!("Failed to launch browser: {}", e),
            }
        }
        self
    }

    /// Sets the maximum number of pages to crawl
    pub fn with_max_pages(mut self, max: usize) -> Self {
        self.max_pages = max;
        self
    }

    /// `max_pages == 0` means unlimited: keep crawling until the frontier is
    /// exhausted or the crawl is stopped.
    fn budget_reached(&self, spawned: usize) -> bool {
        self.max_pages != 0 && spawned >= self.max_pages
    }

    /// Human-readable page budget for logs (`0` → `unlimited`).
    fn budget_label(&self) -> String {
        if self.max_pages == 0 {
            "unlimited".to_string()
        } else {
            self.max_pages.to_string()
        }
    }

    fn stop_requested(&self) -> bool {
        self.stop_flag
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    /// PageSpeed context for one fetch (None when PSI is off for this run).
    fn psi_ref(&self) -> Option<crate::crawler::fetching::PsiRef<'_>> {
        let config = self.pagespeed_config.as_ref()?;
        if !config.status || config.api_key.is_empty() {
            return None;
        }
        Some(crate::crawler::fetching::PsiRef {
            config,
            sampler: &self.psi_sampler,
            semaphore: &self.psi_semaphore,
            client: &self.psi_client,
            session_id: self.session_id,
        })
    }

    /// Wait until background PageSpeed work for this engine has finished.
    /// One-shot CLI crawls call this so their output includes the CWV data;
    /// the server/GUI does not call it, so sessions complete the moment pages
    /// are done and CWV results land in the database in the background.
    pub async fn wait_for_psi_drain(&self) {
        let active = self
            .pagespeed_config
            .as_ref()
            .is_some_and(|config| config.status && !config.api_key.is_empty());
        if !active {
            return;
        }
        let permits = crate::crawler::helpers::psi::DEFAULT_CONCURRENCY as u32;
        let semaphore = self.psi_semaphore.clone();
        if tokio::time::timeout(
            Duration::from_secs(20 * 60),
            semaphore.acquire_many_owned(permits),
        )
        .await
        .is_err()
        {
            tracing::warn!("[CWV] Timed out waiting for PageSpeed results");
        }
    }

    /// Sets the concurrency limit
    pub fn with_concurrency(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self.semaphore = Arc::new(Semaphore::new(limit));
        self
    }

    /// Enables DB persistence with configurable batch size
    pub fn with_db_persistence(mut self, batch_size: Option<usize>) -> Self {
        if db::get_connection().is_ok() {
            self.enable_db_persist = true;
            self.db_batch_size = batch_size.unwrap_or(self.db_batch_size);

            tracing::info!(
                "DB persistence enabled with batch size {}",
                self.db_batch_size
            );
        } else {
            tracing::warn!("Failed to open DB connection, persistence disabled");
        }
        self
    }

    /// Associate persisted pages with a crawl session (see `db::create_session`).
    /// Without this, pages are written to the legacy session-0 bucket.
    pub fn with_session(mut self, session_id: i64) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Respect robots.txt disallow rules while following links (default: on).
    pub fn with_robots(mut self, respect: bool) -> Self {
        self.respect_robots = respect;
        self
    }

    /// Honor robots.txt Crawl-delay directives (default: off).
    pub fn with_crawl_delay(mut self, respect: bool) -> Self {
        self.respect_crawl_delay = respect;
        self
    }

    /// Do not follow links marked rel="nofollow".
    pub fn with_nofollow(mut self, respect: bool) -> Self {
        self.respect_nofollow = respect;
        self
    }

    /// Do not follow links found on noindex pages (meta robots / X-Robots-Tag).
    pub fn with_noindex(mut self, respect: bool) -> Self {
        self.respect_noindex = respect;
        self
    }

    /// Render pages with headless Chrome when the raw HTML looks
    /// client-rendered (framework markers + sparse content). Launches the
    /// browser on demand.
    pub fn with_js_fallback(mut self, enable: bool) -> Self {
        self.js_fallback = enable;
        if enable && self.browser.is_none() {
            let options = LaunchOptions {
                headless: true,
                ..Default::default()
            };
            match Browser::new(options) {
                Ok(browser) => self.browser = Some(Arc::new(browser)),
                Err(err) => {
                    tracing::error!("Failed to launch browser for JS fallback: {}", err)
                }
            }
        }
        self
    }

    /// Continue a previous run of the same session: the stored pages count as
    /// already crawled and their links seed the crawl frontier.
    pub fn with_resume_pages(mut self, pages: Vec<PageData>) -> Self {
        // Continue page ids after the highest stored id so resumed pages can
        // never overwrite already-persisted rows of the same session.
        self.id_base = pages.iter().map(|page| page.id).max().unwrap_or(0);
        // Same session: earlier runs already spent PSI samples — don't repeat
        // them on resume.
        self.psi_sampler.seed_from_pages(&pages);
        self.resume_pages = Some(Arc::new(pages));
        self
    }

    /// Re-crawl incrementally: pages from a previous session provide
    /// ETag / Last-Modified validators, and 304 responses reuse that stored
    /// content instead of re-downloading it.
    pub fn with_incremental_baseline(mut self, pages: Vec<PageData>) -> Self {
        // Incremental re-crawls reuse the same session's samples too.
        self.psi_sampler.seed_from_pages(&pages);
        let mut baseline: HashMap<String, PageData> = HashMap::with_capacity(pages.len());
        for page in pages {
            if !page.requested_url.is_empty() {
                let requested =
                    crate::crawler::url_normalizer::normalize_url(&page.requested_url)
                        .unwrap_or_else(|| page.requested_url.clone());
                baseline.insert(requested, page.clone());
            }
            let key =
                crate::crawler::url_normalizer::normalize_url(&page.url).unwrap_or_else(|| page.url.clone());
            baseline.insert(key, page);
        }
        self.incremental = Some(Arc::new(baseline));
        self
    }

    /// Cooperative cancellation: set the flag to make the crawl stop after the
    /// in-flight requests drain. Callers are responsible for marking the
    /// session status (`stopped`) afterwards.
    pub fn with_stop_flag(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.stop_flag = Some(flag);
        self
    }

    /// Shared crawl statistics (same counters across clones).
    pub fn stats(&self) -> crate::crawler::stats::CrawlStats {
        self.stats.clone()
    }

    /// Flush pending pages to DB
    async fn flush_pending_pages(&self) {
        if !self.enable_db_persist {
            return;
        }
        let mut pending = self.pending_pages.lock().await;
        if pending.is_empty() {
            return;
        }
        let pages_to_save: Vec<PageData> = pending.drain(..).collect();
        if let Ok(conn) = db::get_connection() {
            let mut saved = 0;
            for page_data in pages_to_save {
                if db::save_page_for_session(&conn, self.session_id, &page_data).is_ok() {
                    saved += 1;
                }
            }
            if saved > 0 {
                tracing::debug!("Flushed {} pages to DB", saved);
            }
        }
    }

    /// Add page to pending buffer, flush if batch full
    async fn add_pending_page(&self, page_data: PageData) {
        if !self.enable_db_persist {
            return;
        }
        let mut pending = self.pending_pages.lock().await;
        pending.push(page_data);
        if pending.len() >= self.db_batch_size {
            let pages_to_save: Vec<PageData> = pending.drain(..).collect();
            drop(pending);
            if let Ok(conn) = db::get_connection() {
                for page_data in pages_to_save {
                    let _ = db::save_page_for_session(&conn, self.session_id, &page_data);
                }
                tracing::debug!("Batch flush: {} pages saved to DB", self.db_batch_size);
            }
        }
    }

    /// Primary entry point for crawling.
    /// Returns a vector of PageData for backward compatibility.

    pub async fn crawl(&self, start_url: &str, headless: bool) -> Vec<PageData> {
        // SET THE RECENTLY CRAWLED INTO THE LIST OF RECENTLY CRAWLED URLS
        add_recent_entry(start_url.to_string()).await;
        // Use a smaller fixed buffer instead of max_pages to prevent memory issues
        let (tx, mut rx) = mpsc::channel(1000);
        let start_url = start_url.to_string();
        let engine = self.clone();

        // Start the crawl in a background task
        tokio::spawn(async move {
            engine.crawl_concurrently(&start_url, tx).await;
        });

        let mut results = Vec::new();
        while let Some(data) = rx.recv().await {
            match data {
                crate::crawler::CrawlMessage::Page(p) => {
                    if headless {
                        println!(
                            "[{}] {} - {} links found",
                            p.status,
                            p.url,
                            p.anchor_links.len()
                        );
                    }
                    results.push(p);
                }
                crate::crawler::CrawlMessage::Progress { .. } => {} // ignore progress for CLI
            }
        }

        results
    }

    /// concurrent crawler logic using JoinSet for task management
    pub async fn crawl_concurrently(
        &self,
        start_url: &str,
        tx: mpsc::Sender<crate::crawler::CrawlMessage>,
    ) {
        let base_url = match Url::parse(start_url) {
            Ok(url) => url,
            Err(_) => return,
        };

        // robots.txt enforcement: fetched once per crawl for the seed host;
        // every dequeued URL (seed, discovered links, sitemaps) is checked.
        let robots = if self.respect_robots {
            fetch_robots(&self.client, &base_url).await
        } else {
            RobotsRules::allow_all()
        };
        let crawl_delay = if self.respect_crawl_delay {
            robots.crawl_delay
        } else {
            None
        };
        if robots.has_rules() || crawl_delay.is_some() {
            tracing::info!(
                "[ROBOTS] Enforcement active: {} rule(s), crawl-delay {:?}",
                robots.rule_count(),
                crawl_delay
            );
        }

        //  new queue module for breadth-first crawling
        // Allow queue to hold more items than max_pages to account for the frontier
        // Factor of 10 allows building a backlog without dropping links too early
        let queue_capacity = std::cmp::max(self.max_pages * 10, 200_000);
        let mut queue = crate::crawler::queue::CrawlQueue::with_max_size(Some(queue_capacity));

        if let Some(resume) = &self.resume_pages {
            // Resume mode: stored pages count as crawled; their links seed the
            // frontier so the crawl continues where the previous run stopped.
            let mut visited = self.visited.lock().await;
            let mut finished = self.finished_urls.lock().await;
            let mut seeded = 0usize;
            for page in resume.iter() {
                visited.insert(page.url.clone());
                if !page.requested_url.is_empty() {
                    visited.insert(page.requested_url.clone());
                }
                finished.insert(page.url.clone());
                for link in &page.anchor_links {
                    if !visited.contains(&link.href)
                        && queue.push(link.href.clone(), Some(page.url.clone()))
                    {
                        visited.insert(link.href.clone());
                        seeded += 1;
                    }
                }
            }
            tracing::info!(
                "[RESUME] {} stored pages preloaded, {} frontier links queued",
                resume.len(),
                seeded
            );
        } else if let Some(normalized_start) =
            crate::crawler::url_normalizer::normalize_url(start_url)
        {
            queue.push(normalized_start.clone(), None);
            // Mark start URL as seen immediately to prevent re-queuing
            self.visited.lock().await.insert(normalized_start);
        } else {
            tracing::error!("Invalid start URL: {}", start_url);
            return;
        }

        // Try to discover URLs from sitemap.xml and robots.txt
        tracing::info!("[DISCOVERY] Attempting to discover URLs from sitemap and robots.txt...");
        let sitemap_urls =
            crate::crawler::sitemap::discover_additional_urls(start_url, &self.client).await;
        if !sitemap_urls.is_empty() {
            tracing::info!(
                "[DISCOVERY] Found {} URLs from sitemaps, adding to queue",
                sitemap_urls.len()
            );
            let mut added = 0;
            {
                let mut visited = self.visited.lock().await;
                for url in sitemap_urls {
                    // Only add URLs from the same domain
                    if let Ok(parsed) = Url::parse(&url) {
                        if crate::crawler::url_normalizer::is_same_domain(
                            parsed.host_str(),
                            base_url.host_str(),
                        ) {
                            if let Some(normalized_url) =
                                crate::crawler::url_normalizer::normalize_url(&url)
                            {
                                if crate::crawler::url_normalizer::should_crawl_url(&normalized_url)
                                {
                                    if !visited.contains(&normalized_url) {
                                        if queue.push(normalized_url.clone(), None) {
                                            visited.insert(normalized_url);
                                            added += 1;
                                        } else {
                                            tracing::warn!(
                                                "[QUEUE] Queue at capacity, skipping sitemap URL: {}",
                                                url
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            tracing::info!("[DISCOVERY] Added {} sitemap URLs to queue", added);
        }

        let mut join_set = tokio::task::JoinSet::new();
        // Counts spawned fetch tasks, not completed pages: gating on this makes
        // `max_pages` an exact budget instead of overshooting by however many
        // requests happened to be in flight when the previous limit was hit.
        let mut spawned: usize = 0;

        while !queue.is_empty() || !join_set.is_empty() {
            // Fill up our concurrency quota
            while !queue.is_empty() && join_set.len() < self.concurrency_limit {
                if self.budget_reached(spawned) || self.stop_requested() {
                    break;
                }

                let (normalized_url, referer) = queue.pop().unwrap();

                // robots.txt enforcement happens at dequeue time so every URL
                // source (seed, links, sitemaps) is covered by one check.
                if !robots.is_allowed(&normalized_url) {
                    self.stats.increment_robots_blocked();
                    tracing::debug!("[ROBOTS] Skipping disallowed URL: {}", normalized_url);
                    continue;
                }
                spawned += 1;

                // URLs in queue are already normalized, filtered, and marked "seen" (visited)
                // We proceed directly to processing

                let engine = self.clone();
                let base_url_clone = base_url.clone();
                join_set.spawn(async move {
                    let _permit = engine
                        .semaphore
                        .acquire()
                        .await
                        .map_err(|e| e.to_string())?;
                    engine.rate_limiter.acquire().await;
                    if let Some(delay) = crawl_delay {
                        tokio::time::sleep(delay).await;
                    }
                    let validators = engine
                        .incremental
                        .as_ref()
                        .and_then(|baseline| baseline.get(&normalized_url))
                        .and_then(validators_from);
                    let psi_ref = engine.psi_ref();
                    let result = match fetch_and_process(
                        &engine.client,
                        &engine.browser,
                        &engine.user_agents,
                        psi_ref.as_ref(),
                        engine.enable_javascript,
                        engine.js_fallback,
                        validators.as_ref(),
                        &normalized_url,
                        &base_url_clone,
                        referer,
                    )
                    .await
                    {
                        Ok(FetchOutcome::Fetched(data)) => Ok(data),
                        Ok(FetchOutcome::NotModified) => {
                            match engine
                                .incremental
                                .as_ref()
                                .and_then(|baseline| baseline.get(&normalized_url))
                            {
                                Some(stored) => {
                                    let mut data = stored.clone();
                                    data.not_modified = true;
                                    Ok(data)
                                }
                                None => Err(
                                    "304 Not Modified without a stored baseline page".to_string()
                                ),
                            }
                        }
                        Err(err) => Err(err),
                    };
                    match result.as_ref() {
                        Ok(data)
                            if data.status.starts_with("429") || data.status.starts_with('5') =>
                        {
                            engine.rate_limiter.on_throttled(None);
                        }
                        Ok(_) => engine.rate_limiter.on_success(),
                        Err(_) => {}
                    }
                    result
                });
            }

            // Wait for at least one task to complete. The short tick lets a
            // stop request abort in-flight requests *immediately* instead of
            // waiting for the slowest request (or a PSI call) to finish.
            let next = tokio::select! {
                result = join_set.join_next() => result,
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    if self.stop_requested() {
                        let aborted = join_set.len();
                        if aborted > 0 {
                            tracing::warn!(
                                "[STOP] Aborting {} in-flight request(s) immediately",
                                aborted
                            );
                        }
                        join_set.abort_all();
                        queue.clear();
                    }
                    continue;
                }
            };
            if let Some(res) = next {
                match res {
                    Ok(Ok(mut data)) => {
                        // Redirects mean the URL we queued (data.requested_url) and the
                        // URL we actually landed on (data.url) can differ. `visited` only
                        // ever recorded the pre-redirect form, so a page discovered and
                        // linked to by its final URL elsewhere would look "unseen" and get
                        // queued + fetched again, producing a duplicate row for the same
                        // page. Guard against that here: dedupe on the final URL, and mark
                        // it visited so later link discovery won't re-queue it either.
                        let is_duplicate_result = {
                            let mut finished = self.finished_urls.lock().await;
                            !finished.insert(data.url.clone())
                        };
                        if is_duplicate_result {
                            tracing::debug!(
                                "[DEDUP] Dropping duplicate result for {} (requested as {})",
                                data.url,
                                data.requested_url
                            );
                            continue;
                        }
                        if data.url != data.requested_url {
                            self.visited.lock().await.insert(data.url.clone());
                        }
                        if data.js_rendered {
                            self.stats.increment_js_render();
                        }
                        if data.not_modified {
                            self.stats.increment_not_modified();
                        }

                        // Extract new links BEFORE sending result to ensure consistency
                        let current_url = data.url.clone();
                        let current_success = self.success_count.load(Ordering::SeqCst);
                        let links_found = data.anchor_links.len();

                        self.stats.add_discovered(links_found);

                        // Only add new links if we haven't reached max successful crawls
                        // (or when the budget is unlimited).
                        if self.max_pages == 0 || current_success < self.max_pages {
                            let mut new_links = Vec::new();
                            let mut filtered_count = 0;
                            let mut duplicate_count = 0;
                            let normalization_failed = 0;

                            // Optionally do not follow links found on noindex pages.
                            let skip_links = self.respect_noindex && page_is_noindex(&data);
                            if skip_links {
                                tracing::debug!(
                                    "[NOINDEX] Not following links from noindex page: {}",
                                    current_url
                                );
                                filtered_count += data.anchor_links.len();
                            }

                            let mut added_count = 0;
                            if !skip_links {
                                let mut visited = self.visited.lock().await; // Lock once

                                for link in &data.anchor_links {
                                    // Respect rel="nofollow" when configured to.
                                    if self.respect_nofollow
                                        && link.rel.to_lowercase().contains("nofollow")
                                    {
                                        filtered_count += 1;
                                        continue;
                                    }
                                    // Links are already normalized and filtered by fetch_and_process

                                    if visited.contains(&link.href) {
                                        duplicate_count += 1;
                                        tracing::debug!("[LINK] Duplicate (Seen): {}", link.href);
                                    } else {
                                        // Attempt to add to queue FIRST
                                        if queue.push(link.href.clone(), Some(current_url.clone()))
                                        {
                                            // Successfully queued -> Mark as visited/seen
                                            visited.insert(link.href.clone());
                                            new_links.push(link.href.clone()); // for logging/stats
                                            added_count += 1;
                                            tracing::debug!("[LINK] NEW -> Queue: {}", link.href);
                                        } else {
                                            // Queue full
                                            tracing::warn!(
                                                "Queue full, dropping link: {}",
                                                link.href
                                            );
                                            // Stop processing links if queue is full?
                                            // Or continue counting duplicates?
                                            // Assuming push returns false means full, we can break.
                                            break;
                                        }
                                    }
                                }
                            } // Release lock immediately

                            self.stats.add_filtered(filtered_count);
                            self.stats.add_duplicate(duplicate_count);

                            // We already added them to queue in the loop

                            // Warn if we couldn't add all links due to capacity
                            if added_count < new_links.len() {
                                tracing::warn!(
                                    "[QUEUE] Queue at capacity! Added {}/{} new links. Queue size: {}/{}",
                                    added_count,
                                    new_links.len(),
                                    queue.len(),
                                    queue
                                        .remaining_capacity()
                                        .map(|r| r + queue.len())
                                        .unwrap_or(queue.len())
                                );
                            }

                            tracing::info!(
                                "[QUEUE] Page: {} | Found: {} links | New: {} | Added: {} | Filtered: {} | Duplicates: {} | Failed Norm: {} | Queue: {} | Success: {}/{}",
                                current_url,
                                links_found,
                                new_links.len(),
                                added_count,
                                filtered_count,
                                duplicate_count,
                                normalization_failed,
                                queue.len(),
                                current_success + 1,
                                self.budget_label()
                            );

                            // Print summary stats every 50 pages
                            if (current_success + 1) % 50 == 0 {
                                tracing::warn!("[STATS] {}", self.stats.get_summary());
                            }

                            // Warning if no new links found
                            if new_links.is_empty() && links_found > 0 {
                                tracing::warn!(
                                    "[WARNING] Page {} had {} links but ALL were duplicates/filtered. Queue size: {}",
                                    current_url,
                                    links_found,
                                    queue.len()
                                );
                            }
                        }

                        // Send result back
                        // Assign a stable, unique sequence id before this page is persisted -
                        // without it every page defaults to id 0 and each DB write via
                        // INSERT OR REPLACE silently clobbers the previous page's row.
                        data.id =
                            self.id_base + self.success_count.fetch_add(1, Ordering::SeqCst) + 1;
                        self.stats.increment_crawled();
                        let _ = tx
                            .send(crate::crawler::CrawlMessage::Page(data.clone()))
                            .await;

                        // Persist to DB if enabled (batched)
                        if self.enable_db_persist {
                            self.add_pending_page(data).await;
                        }

                        // Send progress update
                        let scanned = self.success_count.load(Ordering::SeqCst) as usize;
                        let queued = queue.len();
                        let processing = join_set.len();
                        let _ = tx
                            .send(crate::crawler::CrawlMessage::Progress {
                                scanned,
                                queued,
                                processing,
                            })
                            .await;
                    }
                    Ok(Err(e)) => {
                        self.stats.increment_failed();
                        tracing::error!("Crawl Error: {}", e);
                    }
                    Err(e) => {
                        if e.is_cancelled() {
                            tracing::debug!("[STOP] Fetch task cancelled by stop request");
                        } else {
                            self.stats.increment_failed();
                            tracing::error!("Task Panic/Error: {}", e);
                        }
                    }
                }
            }

            // Reached the page budget (or asked to stop): stop spawning new
            // fetches. On stop, the wait loop above has already aborted any
            // in-flight tasks — nothing left to drain.
            if self.budget_reached(spawned) || self.stop_requested() {
                queue.clear();
            }

            // Diagnostic: Check if queue is empty but we haven't hit max pages
            if queue.is_empty() && join_set.is_empty() {
                let current = self.success_count.load(Ordering::SeqCst);
                if self.max_pages != 0 && current < self.max_pages {
                    tracing::warn!(
                        "[QUEUE EXHAUSTED] Crawl stopped at {} pages (target: {}). No more links to discover. {}",
                        current,
                        self.max_pages,
                        self.stats.get_summary()
                    );
                }
            }
        }

        // Final stats
        tracing::warn!("[CRAWL COMPLETE] {}", self.stats.get_summary());

        // Flush any remaining pending pages to DB
        if self.enable_db_persist {
            self.flush_pending_pages().await;
        }
    }
}

/// True when a page asks not to be indexed (meta robots or X-Robots-Tag header).
fn page_is_noindex(data: &PageData) -> bool {
    if data.indexability.to_lowercase().contains("noindex") {
        return true;
    }
    data.headers.iter().any(|h| {
        let lower = h.to_lowercase();
        lower.starts_with("x-robots-tag") && lower.contains("noindex")
    })
}
