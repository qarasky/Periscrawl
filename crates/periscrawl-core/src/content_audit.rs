//! Content and keyword analysis over a session's stored pages.
//!
//! Per-page keywords are stored as "(count) term" strings by the crawler's
//! extractor; this module parses them back into term/count/position and
//! produces site-wide keyword overviews, content-size statistics and target
//! keyword checks.

use std::collections::HashMap;

use serde::Serialize;

use crate::crawler::PageData;

/// One keyword as stored on a page, parsed apart.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedKeyword {
    pub term: String,
    pub count: usize,
    /// Position in the page's keyword list (0 = most prominent).
    pub position: usize,
}

/// Parse "(3) term" style entries; entries without a count get count 1.
pub fn parse_page_keywords(page: &PageData) -> Vec<ParsedKeyword> {
    page.keywords
        .iter()
        .enumerate()
        .filter_map(|(position, raw)| {
            let raw = raw.trim();
            let (count, term) = if let Some(rest) = raw.strip_prefix('(') {
                match rest.split_once(')') {
                    Some((count, term)) => (
                        count.trim().parse::<usize>().unwrap_or(1),
                        term.trim().to_string(),
                    ),
                    None => (1, raw.to_string()),
                }
            } else {
                (1, raw.to_string())
            };
            if term.is_empty() {
                return None;
            }
            Some(ParsedKeyword {
                term,
                count,
                position,
            })
        })
        .collect()
}

/// Site-wide aggregate for one keyword term.
#[derive(Debug, Clone, Serialize)]
pub struct KeywordOverview {
    pub keyword: String,
    pub pages: usize,
    pub total_uses: usize,
    pub avg_position: f64,
    pub in_titles: usize,
    pub in_h1s: usize,
    pub in_descriptions: usize,
}

fn contains_term(haystack: &str, term: &str) -> bool {
    !term.is_empty() && haystack.to_lowercase().contains(&term.to_lowercase())
}

/// Aggregate keyword terms across pages, most widespread first.
pub fn keyword_overview(pages: &[PageData], limit: usize) -> Vec<KeywordOverview> {
    struct Agg {
        pages: usize,
        total_uses: usize,
        position_sum: usize,
        in_titles: usize,
        in_h1s: usize,
        in_descriptions: usize,
    }
    let mut stats: HashMap<String, Agg> = HashMap::new();

    for page in pages {
        for keyword in parse_page_keywords(page) {
            let entry = stats.entry(keyword.term.clone()).or_insert(Agg {
                pages: 0,
                total_uses: 0,
                position_sum: 0,
                in_titles: 0,
                in_h1s: 0,
                in_descriptions: 0,
            });
            entry.pages += 1;
            entry.total_uses += keyword.count;
            entry.position_sum += keyword.position;
            if contains_term(&page.title, &keyword.term) {
                entry.in_titles += 1;
            }
            if contains_term(&page.h1, &keyword.term) {
                entry.in_h1s += 1;
            }
            if contains_term(&page.description, &keyword.term) {
                entry.in_descriptions += 1;
            }
        }
    }

    let mut overview: Vec<KeywordOverview> = stats
        .into_iter()
        .map(|(keyword, agg)| KeywordOverview {
            keyword,
            pages: agg.pages,
            total_uses: agg.total_uses,
            avg_position: if agg.pages == 0 {
                0.0
            } else {
                agg.position_sum as f64 / agg.pages as f64
            },
            in_titles: agg.in_titles,
            in_h1s: agg.in_h1s,
            in_descriptions: agg.in_descriptions,
        })
        .collect();
    overview.sort_by(|a, b| {
        b.pages
            .cmp(&a.pages)
            .then(b.total_uses.cmp(&a.total_uses))
            .then(a.keyword.cmp(&b.keyword))
    });
    overview.truncate(limit);
    overview
}

#[derive(Debug, Clone, Serialize)]
pub struct PageRow {
    pub url: String,
    pub words: usize,
    pub title: String,
    pub description_len: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub label: String,
    pub pages: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrimaryUsage {
    pub url: String,
    pub keyword: String,
    /// Keyword occurrences as a share of the page's words.
    pub density_percent: f64,
    pub in_title: bool,
    pub in_h1: bool,
    pub in_description: bool,
    pub in_url: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetCheck {
    pub keyword: String,
    pub pages_found: usize,
    pub in_titles: usize,
    pub in_h1s: usize,
    pub in_descriptions: usize,
    pub in_urls: usize,
    pub sample_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordStats {
    pub total: usize,
    pub min: usize,
    pub max: usize,
    pub avg: f64,
    pub median: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentReport {
    pub session_id: i64,
    pub pages_analyzed: usize,
    pub words: WordStats,
    pub buckets: Vec<Bucket>,
    pub thin_pages: Vec<PageRow>,
    pub longest_pages: Vec<PageRow>,
    pub unique_keywords: usize,
    pub pages_without_keywords: usize,
    pub top_keywords: Vec<KeywordOverview>,
    pub primary_usage: Vec<PrimaryUsage>,
    pub targets: Vec<TargetCheck>,
}

/// Build the full content/keyword report. `targets` are optional keywords to
/// check across the crawl (comma-split by the CLI).
pub fn build_content_report(
    session_id: i64,
    pages: &[PageData],
    top: usize,
    targets: &[String],
) -> ContentReport {
    let top = top.max(1);
    let mut word_counts: Vec<usize> = pages
        .iter()
        .map(|page| page.word_count.unwrap_or(0))
        .collect();

    let total: usize = word_counts.iter().sum();
    let min = word_counts.iter().copied().min().unwrap_or(0);
    let max = word_counts.iter().copied().max().unwrap_or(0);
    let avg = if word_counts.is_empty() {
        0.0
    } else {
        total as f64 / word_counts.len() as f64
    };
    word_counts.sort_unstable();
    let median = if word_counts.is_empty() {
        0
    } else {
        word_counts[word_counts.len() / 2]
    };

    let bucket_defs: [(&str, usize, usize); 6] = [
        ("< 100 words", 0, 100),
        ("100 – 300", 100, 300),
        ("300 – 600", 300, 600),
        ("600 – 1,000", 600, 1000),
        ("1,000 – 2,000", 1000, 2000),
        ("2,000+", 2000, usize::MAX),
    ];
    let buckets: Vec<Bucket> = bucket_defs
        .iter()
        .map(|(label, low, high)| Bucket {
            label: label.to_string(),
            pages: pages
                .iter()
                .filter(|page| {
                    let words = page.word_count.unwrap_or(0);
                    words >= *low && words < *high
                })
                .count(),
        })
        .collect();

    let row_of = |page: &PageData| PageRow {
        url: page.url.clone(),
        words: page.word_count.unwrap_or(0),
        title: page.title.clone(),
        description_len: page.description_len,
    };

    let mut thin_source: Vec<&PageData> = pages
        .iter()
        .filter(|page| page.word_count.unwrap_or(0) < 300)
        .collect();
    thin_source.sort_by_key(|page| page.word_count.unwrap_or(0));
    let thin_pages: Vec<PageRow> = thin_source.iter().take(top).map(|page| row_of(page)).collect();

    let mut longest_source: Vec<&PageData> = pages.iter().collect();
    longest_source.sort_by_key(|page| std::cmp::Reverse(page.word_count.unwrap_or(0)));
    let longest_pages: Vec<PageRow> = longest_source
        .iter()
        .take(top)
        .map(|page| row_of(page))
        .collect();

    let parsed_per_page: Vec<Vec<ParsedKeyword>> =
        pages.iter().map(|page| parse_page_keywords(page)).collect();
    let pages_without_keywords = parsed_per_page.iter().filter(|list| list.is_empty()).count();
    let mut unique: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for list in &parsed_per_page {
        for keyword in list {
            unique.insert(keyword.term.as_str());
        }
    }

    let top_keywords = keyword_overview(pages, top);

    // Usage check for the primary keyword of the largest content pages.
    let mut primary_usage: Vec<PrimaryUsage> = Vec::new();
    for page in longest_source.iter().take(top) {
        let Some(primary) = parse_page_keywords(page).into_iter().next() else {
            continue;
        };
        let words = page.word_count.unwrap_or(0).max(1);
        primary_usage.push(PrimaryUsage {
            url: page.url.clone(),
            keyword: primary.term.clone(),
            density_percent: primary.count as f64 * 100.0 / words as f64,
            in_title: contains_term(&page.title, &primary.term),
            in_h1: contains_term(&page.h1, &primary.term),
            in_description: contains_term(&page.description, &primary.term),
            in_url: contains_term(&page.url, &primary.term),
        });
    }

    let targets: Vec<TargetCheck> = targets
        .iter()
        .filter(|target| !target.trim().is_empty())
        .map(|target| {
            let target = target.trim();
            let mut check = TargetCheck {
                keyword: target.to_string(),
                pages_found: 0,
                in_titles: 0,
                in_h1s: 0,
                in_descriptions: 0,
                in_urls: 0,
                sample_urls: Vec::new(),
            };
            for page in pages {
                let in_title = contains_term(&page.title, target);
                let in_h1 = contains_term(&page.h1, target);
                let in_description = contains_term(&page.description, target);
                let in_url = contains_term(&page.url, target);
                let in_content = page
                    .keywords
                    .iter()
                    .any(|keyword| contains_term(keyword, target));
                if !(in_title || in_h1 || in_description || in_url || in_content) {
                    continue;
                }
                check.pages_found += 1;
                if in_title {
                    check.in_titles += 1;
                }
                if in_h1 {
                    check.in_h1s += 1;
                }
                if in_description {
                    check.in_descriptions += 1;
                }
                if in_url {
                    check.in_urls += 1;
                }
                if check.sample_urls.len() < 10 {
                    check.sample_urls.push(page.url.clone());
                }
            }
            check
        })
        .collect();

    ContentReport {
        session_id,
        pages_analyzed: pages.len(),
        words: WordStats {
            total,
            min,
            max,
            avg,
            median,
        },
        buckets,
        thin_pages,
        longest_pages,
        unique_keywords: unique.len(),
        pages_without_keywords,
        top_keywords,
        primary_usage,
        targets,
    }
}

#[cfg(test)]
mod tests {
    use super::{contains_term, parse_page_keywords};
    use crate::crawler::helpers::html_parser::{PageData, extract_page_elements};

    fn page_with(title: &str, body: &str) -> PageData {
        let html = format!("<html><head><title>{title}</title></head><body>{body}</body></html>");
        let document = scraper::Html::parse_document(&html);
        extract_page_elements(&document)
    }

    #[test]
    fn parses_counted_keyword_entries() {
        let mut page = page_with("t", "hello world");
        page.keywords = vec!["(12) hello".to_string(), "world".to_string()];
        let parsed = parse_page_keywords(&page);
        assert_eq!(parsed[0].term, "hello");
        assert_eq!(parsed[0].count, 12);
        assert_eq!(parsed[0].position, 0);
        assert_eq!(parsed[1].term, "world");
        assert_eq!(parsed[1].count, 1);
        assert_eq!(parsed[1].position, 1);
    }

    #[test]
    fn term_matching_is_case_insensitive() {
        assert!(contains_term("Modernization of Heat", "heat"));
        assert!(!contains_term("Modernization", "zzz"));
    }
}
