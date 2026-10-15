//! PageSpeed Insights sampling.
//!
//! CWV collection costs two PSI API calls per page (desktop + mobile) and each
//! call takes 10–90 s, so a full-site crawl can't afford per-page collection.
//! The sampler picks a representative subset instead:
//!
//! 1. **Navigation pages** — the unique links found in the *seed page's*
//!    `<header>`, `<nav>` and `<footer>` are always sampled (the site's key
//!    pages). Deeper pages' menus are per-section link lists; trusting every
//!    page's menu explodes the sample set (one real site: 469 unique "nav"
//!    URLs where the homepage has 16), so only the first page to report
//!    navigation seeds the set.
//! 2. **One per template** — URLs with depth ≥ 2 group by their parent path
//!    with the last segment treated as a variable slug (`/news/foo` and
//!    `/news/bar` share the group `/news/*`); the first page crawled in each
//!    group is the representative.
//!
//! Depth-1 paths (`/`, `/about`) are their own single-page groups. A hard
//! budget ([`DEFAULT_SAMPLE_BUDGET`]) caps the total regardless, and resumed
//! crawls seed the sampler from already-crawled pages so the budget is spent
//! across the whole session rather than once per run.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use url::Url;

/// Upper bound on sampled pages per session (each is two PSI API calls).
pub const DEFAULT_SAMPLE_BUDGET: usize = 64;

/// How many PageSpeed requests may be in flight at once. PSI calls take tens
/// of seconds; a small pool stays inside API rate limits without blocking
/// crawl throughput (PSI runs in the background).
pub const DEFAULT_CONCURRENCY: usize = 3;

/// Per-crawl-run sampling state. Shared across concurrent fetches; each URL
/// and template group is sampled at most once.
pub struct PsiSampler {
    /// Normalized destinations of links found in the seed page's
    /// header/nav/footer.
    nav_targets: Mutex<HashSet<String>>,
    /// Whether a page has already seeded the navigation set.
    nav_seeded: AtomicBool,
    /// Nav targets already sampled.
    nav_sampled: Mutex<HashSet<String>>,
    /// Template groups already sampled.
    group_sampled: Mutex<HashSet<String>>,
    /// Total PSI samples handed out (across resumed runs of a session).
    sampled_count: AtomicUsize,
    max_samples: usize,
}

impl Default for PsiSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl PsiSampler {
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_SAMPLE_BUDGET)
    }

    pub fn with_budget(max_samples: usize) -> Self {
        Self {
            nav_targets: Mutex::new(HashSet::new()),
            nav_seeded: AtomicBool::new(false),
            nav_sampled: Mutex::new(HashSet::new()),
            group_sampled: Mutex::new(HashSet::new()),
            sampled_count: AtomicUsize::new(0),
            max_samples,
        }
    }

    /// Merge navigation links from the seed page. Only the first page to
    /// report navigation seeds the set — per-page menus would otherwise grow
    /// the "nav" set to hundreds of URLs.
    pub fn add_nav_targets(&self, targets: &[String]) {
        if targets.is_empty() {
            return;
        }
        if self.nav_seeded.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut nav = self.nav_targets.lock().unwrap();
        for target in targets {
            nav.insert(target.clone());
        }
    }

    /// Whether this URL should get PSI calls: navigation pages always (once),
    /// otherwise the first page seen for its template group — capped by the
    /// sample budget.
    pub fn should_sample(&self, url: &str) -> bool {
        let eligible = if self.nav_targets.lock().unwrap().contains(url) {
            self.nav_sampled.lock().unwrap().insert(url.to_string())
        } else {
            self.group_sampled
                .lock()
                .unwrap()
                .insert(template_group_key(url))
        };
        if !eligible {
            return false;
        }
        let taken = self.sampled_count.fetch_add(1, Ordering::SeqCst);
        if taken >= self.max_samples {
            // Over budget: leave the counter reflecting handed-out samples.
            self.sampled_count.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Seed from pages stored by earlier runs of the same session (resume /
    /// incremental): their navigation defines the nav set and pages that
    /// already have CWV results are marked sampled so the revamped run does
    /// not spend budget re-testing them.
    pub fn seed_from_pages(&self, pages: &[crate::crawler::PageData]) {
        for page in pages {
            self.add_nav_targets(&page.nav_link_targets);
            if page.cwv_desktop.is_some() || page.cwv_mobile.is_some() {
                self.nav_sampled.lock().unwrap().insert(page.url.clone());
                self.group_sampled
                    .lock()
                    .unwrap()
                    .insert(template_group_key(&page.url));
                self.sampled_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Number of samples handed out so far.
    pub fn sampled_count(&self) -> usize {
        self.sampled_count.load(Ordering::SeqCst)
    }
}

/// Template group key for a normalized URL. `/news/some-slug` → `/news/*`;
/// depth-1 paths (and the root) are their own groups. Query strings and
/// fragments do not affect grouping.
pub fn template_group_key(url: &str) -> String {
    let path = match Url::parse(url) {
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => {
            let without_scheme = url
                .split_once("://")
                .map(|(_, rest)| rest)
                .unwrap_or(url);
            match without_scheme.split_once('/') {
                Some((_, rest)) => format!("/{}", rest.split(['?', '#']).next().unwrap_or("")),
                None => "/".to_string(),
            }
        }
    };

    let trimmed = path.trim_end_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    if segments.len() <= 1 {
        // Root or a single-segment page: its own group.
        return format!("/{}", segments.first().copied().unwrap_or(""));
    }

    // Replace the variable last segment with a wildcard.
    let parent = segments[..segments.len() - 1].join("/");
    format!("/{parent}/*")
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SAMPLE_BUDGET, PsiSampler, template_group_key};

    #[test]
    fn group_keys_collapse_slug_segments() {
        assert_eq!(template_group_key("https://site.kz/news/foo-bar"), "/news/*");
        assert_eq!(template_group_key("https://site.kz/news/other"), "/news/*");
        assert_eq!(
            template_group_key("https://site.kz/news/2024/deep-item/"),
            "/news/2024/*"
        );
        // Depth-1 pages are their own groups.
        assert_eq!(template_group_key("https://site.kz/"), "/");
        assert_eq!(template_group_key("https://site.kz/about"), "/about");
        assert_eq!(template_group_key("https://site.kz/about/"), "/about");
        // Query strings don't create new groups.
        assert_eq!(template_group_key("https://site.kz/news?page=2"), "/news");
        assert_eq!(
            template_group_key("https://site.kz/news/item?utm=x"),
            "/news/*"
        );
    }

    #[test]
    fn sampler_dedupes_nav_targets_and_groups() {
        let sampler = PsiSampler::new();
        sampler.add_nav_targets(&[
            "https://site.kz/about/team".to_string(),
            "https://site.kz/".to_string(),
            "https://site.kz/news/nav-item".to_string(),
        ]);

        // Nav pages are sampled once, repeatedly requested or not.
        assert!(sampler.should_sample("https://site.kz/about/team"));
        assert!(!sampler.should_sample("https://site.kz/about/team"));
        assert!(sampler.should_sample("https://site.kz/"));

        // First page of a template group is sampled, the rest are not.
        assert!(sampler.should_sample("https://site.kz/news/first"));
        assert!(!sampler.should_sample("https://site.kz/news/second"));

        // A nav page is sampled even when its group was already taken.
        assert!(sampler.should_sample("https://site.kz/news/nav-item"));
        // ...but still only once.
        assert!(!sampler.should_sample("https://site.kz/news/nav-item"));
    }

    #[test]
    fn only_the_first_page_seeds_the_nav_set() {
        let sampler = PsiSampler::new();
        sampler.add_nav_targets(&["https://site.kz/home-nav".to_string()]);
        // A later page's menu (possibly hundreds of context links) is ignored.
        sampler.add_nav_targets(&["https://site.kz/deep-menu-item".to_string()]);

        assert!(sampler.should_sample("https://site.kz/home-nav"));
        // The deep menu item is not nav; it falls back to group sampling.
        assert!(sampler.should_sample("https://site.kz/deep-menu-item"));
    }

    #[test]
    fn budget_caps_total_samples() {
        let sampler = PsiSampler::with_budget(3);
        assert!(sampler.should_sample("https://site.kz/a"));
        assert!(sampler.should_sample("https://site.kz/b"));
        assert!(sampler.should_sample("https://site.kz/c"));
        assert!(!sampler.should_sample("https://site.kz/d"));
        assert_eq!(sampler.sampled_count(), 3);
    }

    #[test]
    fn default_budget_is_sane() {
        assert!(DEFAULT_SAMPLE_BUDGET >= 16 && DEFAULT_SAMPLE_BUDGET <= 256);
    }
}
