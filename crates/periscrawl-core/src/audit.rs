//! Sitemap validation: check every URL a site's sitemap lists.
//!
//! Discovers sitemaps (robots.txt + common locations, recursive indexes) and
//! issues a HEAD request per entry with redirect-chain awareness, reporting
//! dead entries (4xx/5xx or redirects ending on errors) before they waste a
//! crawl budget or leak into search engines.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Semaphore;
use url::Url;

use crate::crawler::helpers::rate_limiter::AdaptiveRateLimiter;
use crate::crawler::helpers::user_agents::user_agents;

/// Result of checking one sitemap entry.
#[derive(Debug, Clone, Serialize)]
pub struct CheckedUrl {
    pub url: String,
    /// First response status (what the sitemap URL itself returned).
    pub status: Option<u16>,
    /// Status after following redirects (up to 5 hops).
    pub final_status: Option<u16>,
    /// First redirect target, when the entry redirects.
    pub location: Option<String>,
    pub hops: u32,
    pub error: Option<String>,
}

/// Aggregate report for a sitemap check.
#[derive(Debug, Clone, Serialize)]
pub struct SitemapReport {
    pub start_url: String,
    pub entries: usize,
    pub ok: usize,
    pub redirects: Vec<CheckedUrl>,
    pub dead: Vec<CheckedUrl>,
    pub errors: Vec<CheckedUrl>,
    /// Set when `max_urls` cut the check short.
    pub truncated_at: Option<usize>,
}

impl SitemapReport {
    /// Dead entries plus network errors — the actionable problems.
    pub fn problem_count(&self) -> usize {
        self.dead.len() + self.errors.len()
    }
}

/// Validate the sitemap(s) of `start_url`. Entries are checked concurrently
/// (bounded) with adaptive pacing so a big sitemap stays polite.
pub async fn validate_sitemap(
    start_url: &str,
    concurrency: usize,
    max_urls: Option<usize>,
) -> Result<SitemapReport, String> {
    let user_agent = user_agents()
        .ok()
        .and_then(|list| list.into_iter().next())
        .unwrap_or_else(|| "Mozilla/5.0 (compatible; Periscrawl sitemap-check)".to_string());

    let client = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| err.to_string())?;

    let mut entries = crate::crawler::sitemap::discover_additional_urls(start_url, &client).await;
    let truncated_at = match max_urls {
        Some(cap) if entries.len() > cap => {
            entries.truncate(cap);
            Some(cap)
        }
        _ => None,
    };

    if entries.is_empty() {
        return Ok(SitemapReport {
            start_url: start_url.to_string(),
            entries: 0,
            ok: 0,
            redirects: Vec::new(),
            dead: Vec::new(),
            errors: Vec::new(),
            truncated_at,
        });
    }

    let limiter = Arc::new(AdaptiveRateLimiter::new(50, 20, 10_000));
    let semaphore = Arc::new(Semaphore::new(concurrency.clamp(1, 32)));
    let client = Arc::new(client);

    let mut tasks = tokio::task::JoinSet::new();
    let entry_count = entries.len();
    for url in entries {
        let semaphore = Arc::clone(&semaphore);
        let limiter = Arc::clone(&limiter);
        let client = Arc::clone(&client);
        tasks.spawn(async move {
            let permit = semaphore.acquire_owned().await;
            if permit.is_err() {
                return None;
            }
            check_entry(&client, &limiter, &url).await
        });
    }

    let mut results = Vec::with_capacity(entry_count);
    while let Some(joined) = tasks.join_next().await {
        if let Ok(Some(checked)) = joined {
            results.push(checked);
        }
    }

    let mut report = SitemapReport {
        start_url: start_url.to_string(),
        entries: results.len(),
        ok: 0,
        redirects: Vec::new(),
        dead: Vec::new(),
        errors: Vec::new(),
        truncated_at,
    };

    for result in results {
        match (result.status, result.final_status) {
            (_, None) => report.errors.push(result),
            (Some(first), Some(last)) if (300..400).contains(&first) => {
                // A redirect that lands on 4xx/5xx (or loops) is dead weight.
                if (200..300).contains(&last) {
                    report.redirects.push(result);
                } else {
                    report.dead.push(result);
                }
            }
            (_, Some(last)) if (200..300).contains(&last) => report.ok += 1,
            _ => report.dead.push(result),
        }
    }

    report.dead.sort_by(|a, b| a.url.cmp(&b.url));
    report.redirects.sort_by(|a, b| a.url.cmp(&b.url));
    report.errors.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(report)
}

async fn check_entry(
    client: &reqwest::Client,
    limiter: &AdaptiveRateLimiter,
    url: &str,
) -> Option<CheckedUrl> {
    let mut current = url.to_string();
    let mut chain: Vec<u16> = Vec::new();
    let mut location: Option<String> = None;
    let mut use_get = false;

    for hop in 0..=5u32 {
        limiter.acquire().await;
        let request = if use_get {
            client.get(&current)
        } else {
            client.head(&current)
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                return Some(CheckedUrl {
                    url: url.to_string(),
                    status: chain.first().copied(),
                    final_status: chain.last().copied(),
                    location,
                    hops: chain.len().saturating_sub(1) as u32,
                    error: Some(err.to_string()),
                })
            }
        };

        let status = response.status().as_u16();
        // Some servers answer HEAD with 405/501 — retry with GET.
        if matches!(status, 405 | 501) && !use_get {
            use_get = true;
            continue;
        }
        chain.push(status);

        if (300..400).contains(&status) {
            let next = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty());
            if let Some(next) = next {
                let resolved = resolve_location(&current, next);
                if location.is_none() {
                    location = Some(resolved.clone());
                }
                if hop < 5 {
                    current = resolved;
                    continue;
                }
            }
        }
        break;
    }

    Some(CheckedUrl {
        url: url.to_string(),
        status: chain.first().copied(),
        final_status: chain.last().copied(),
        location,
        hops: chain.len().saturating_sub(1) as u32,
        error: None,
    })
}

fn resolve_location(current: &str, location: &str) -> String {
    if let Ok(base) = Url::parse(current) {
        if let Ok(joined) = base.join(location) {
            return joined.to_string();
        }
    }
    location.to_string()
}
