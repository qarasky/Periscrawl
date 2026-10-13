//! PageSpeed Insights sampling.
//!
//! CWV collection costs two PSI API calls per page (desktop + mobile) and each
//! call takes 10–30 s, so a full-site crawl can't afford per-page collection.
//! The sampler picks a representative subset instead:
//!
//! 1. **Navigation pages** — every unique URL linked from a page's
//!    `<header>`, `<nav>` or `<footer>` is always sampled (the site's key
//!    pages).
//! 2. **One per template** — URLs with depth ≥ 2 group by their parent path
//!    with the last segment treated as a variable slug (`/news/foo` and
//!    `/news/bar` share the group `/news/*`); the first page crawled in each
//!    group is the representative.
//!
//! Depth-1 paths (`/`, `/about`) are their own single-page groups, so they
//! are sampled too. Matching happens on normalized URLs, the same form the
//! crawler stores pages under.

use std::collections::HashSet;
use std::sync::Mutex;

use url::Url;

/// Per-crawl-run sampling state. Shared across concurrent fetches; each URL
/// and template group is sampled at most once.
#[derive(Default)]
pub struct PsiSampler {
    /// Normalized destinations of links found in header/nav/footer.
    nav_targets: Mutex<HashSet<String>>,
    /// Nav targets already sampled (a nav page is sampled once even when the
    /// same URL appears in the nav of every page).
    nav_sampled: Mutex<HashSet<String>>,
    /// Template groups already sampled.
    group_sampled: Mutex<HashSet<String>>,
}

impl PsiSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge the navigation links discovered on a fetched page.
    pub fn add_nav_targets(&self, targets: &[String]) {
        if targets.is_empty() {
            return;
        }
        let mut nav = self.nav_targets.lock().unwrap();
        for target in targets {
            nav.insert(target.clone());
        }
    }

    /// Whether this URL should get PSI calls: navigation pages always (once),
    /// otherwise the first page seen for its template group.
    pub fn should_sample(&self, url: &str) -> bool {
        if self.nav_targets.lock().unwrap().contains(url) {
            return self.nav_sampled.lock().unwrap().insert(url.to_string());
        }
        self.group_sampled.lock().unwrap().insert(template_group_key(url))
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
    use super::{PsiSampler, template_group_key};

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
        assert_eq!(
            template_group_key("https://site.kz/news?page=2"),
            "/news"
        );
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
        ]);

        // Nav pages are sampled once, repeatedly requested or not.
        assert!(sampler.should_sample("https://site.kz/about/team"));
        assert!(!sampler.should_sample("https://site.kz/about/team"));
        assert!(sampler.should_sample("https://site.kz/"));

        // First page of a template group is sampled, the rest are not.
        assert!(sampler.should_sample("https://site.kz/news/first"));
        assert!(!sampler.should_sample("https://site.kz/news/second"));

        // A nav page is sampled even when its group was already taken.
        sampler.add_nav_targets(&["https://site.kz/news/later-nav-item".to_string()]);
        assert!(sampler.should_sample("https://site.kz/news/later-nav-item"));
        // ...but still only once.
        assert!(!sampler.should_sample("https://site.kz/news/later-nav-item"));
    }
}
