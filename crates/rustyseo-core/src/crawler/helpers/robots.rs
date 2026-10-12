use std::error::Error;
use std::time::Duration;

use url::Url;

pub async fn extract_robots_blocked_urls(url: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;

    let robots_txt = response.text().await?;
    let mut blocked_urls = Vec::new();

    for line in robots_txt.lines() {
        let trimmed_line = line.trim();

        // Handle different formats of Disallow directives
        if trimmed_line.to_lowercase().starts_with("disallow:") {
            // Extract the path after "Disallow:"
            let parts: Vec<&str> = trimmed_line.splitn(2, ':').collect();
            if parts.len() == 2 {
                let path = parts[1].trim();

                // Only add meaningful disallow paths
                if !path.is_empty() && path != "/" {
                    // Prepend the domain if path is relative
                    let full_url = if path.starts_with('/') {
                        format!("{}{}", url.trim_end_matches("/robots.txt"), path)
                    } else {
                        path.to_string()
                    };
                    blocked_urls.push(full_url);
                } else if path == "/" {
                    // Root disallow - everything is blocked
                    blocked_urls.push("All pages blocked (Disallow: /)".to_string());
                }
            }
        }
    }

    Ok(blocked_urls)
}

// ---------------------------------------------------------------------------
// Enforcement (RFC 9309-style matching)
// ---------------------------------------------------------------------------

/// User-agent token used when evaluating robots.txt groups. Crawl requests
/// themselves keep the rotated browser User-Agent; this token only decides
/// which group applies to us.
const ROBOTS_UA_TOKEN: &str = "rustyseo";

/// Longest Crawl-delay we are willing to honor (a site asking for hours
/// between requests is effectively asking us not to crawl).
const MAX_CRAWL_DELAY_SECS: f64 = 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct RobotsRule {
    pub allow: bool,
    pub pattern: String,
}

/// Parsed robots.txt for our user-agent, ready for enforcement during a crawl.
#[derive(Debug, Clone)]
pub struct RobotsRules {
    rules: Vec<RobotsRule>,
    pub crawl_delay: Option<Duration>,
    pub sitemaps: Vec<String>,
    fetched: bool,
}

impl RobotsRules {
    /// No restrictions; also the state when robots.txt could not be fetched
    /// (crawl proceeds but nothing is blocked).
    pub fn allow_all() -> Self {
        Self {
            rules: Vec::new(),
            crawl_delay: None,
            sitemaps: Vec::new(),
            fetched: false,
        }
    }

    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Parse robots.txt. Rules from a group matching our token take precedence
    /// over the `*` group; directives from unrelated groups are ignored.
    pub fn parse(text: &str, user_agent_token: &str) -> Self {
        let token = user_agent_token.to_ascii_lowercase();
        let mut current_agents: Vec<String> = Vec::new();
        let mut specific: Vec<RobotsRule> = Vec::new();
        let mut wildcard: Vec<RobotsRule> = Vec::new();
        let mut specific_delay: Option<f64> = None;
        let mut wildcard_delay: Option<f64> = None;
        let mut sitemaps = Vec::new();

        for raw_line in text.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            let in_specific = current_agents.iter().any(|agent| agent == &token);
            let in_wildcard = current_agents.iter().any(|agent| agent == "*");
            let targets_us = in_specific || in_wildcard;

            match key.as_str() {
                "user-agent" => current_agents.push(value.to_ascii_lowercase()),
                "allow" | "disallow" if targets_us => {
                    // `Disallow:` with an empty value means "allow everything".
                    if key == "disallow" && value.is_empty() {
                        continue;
                    }
                    let rule = RobotsRule {
                        allow: key == "allow",
                        pattern: value.to_string(),
                    };
                    if in_specific {
                        specific.push(rule);
                    } else {
                        wildcard.push(rule);
                    }
                }
                "crawl-delay" if targets_us => {
                    if let Ok(seconds) = value.parse::<f64>() {
                        if in_specific {
                            specific_delay = Some(seconds);
                        } else {
                            wildcard_delay = Some(seconds);
                        }
                    }
                }
                "sitemap" if !value.is_empty() => sitemaps.push(value.to_string()),
                _ => {}
            }
        }

        // A group specifically for our agent replaces the `*` group; otherwise
        // the wildcard group applies. Missing `*` rules fall back to allow-all.
        let (rules, delay) = if !specific.is_empty() || specific_delay.is_some() {
            (specific, specific_delay.or(wildcard_delay))
        } else {
            (wildcard, wildcard_delay)
        };
        let crawl_delay = delay
            .filter(|seconds| *seconds > 0.0)
            .map(|seconds| Duration::from_secs_f64(seconds.min(MAX_CRAWL_DELAY_SECS)));

        Self {
            rules,
            crawl_delay,
            sitemaps,
            fetched: true,
        }
    }

    /// May this URL be fetched? Matches the path (+query) against the rules;
    /// the most specific match wins, with `Allow` winning exact ties.
    pub fn is_allowed(&self, url: &str) -> bool {
        if !self.fetched {
            return true;
        }
        let target = match Url::parse(url) {
            Ok(parsed) => {
                let mut path = parsed.path().to_string();
                if let Some(query) = parsed.query() {
                    path.push('?');
                    path.push_str(query);
                }
                path
            }
            Err(_) => url.to_string(),
        };
        if target.is_empty() {
            return true;
        }

        let mut best: Option<(&RobotsRule, usize)> = None;
        for rule in &self.rules {
            let Some(specificity) = match_pattern(&rule.pattern, &target) else {
                continue;
            };
            let replaces = match best {
                None => true,
                Some((best_rule, best_specificity)) => {
                    specificity > best_specificity
                        || (specificity == best_specificity && rule.allow && !best_rule.allow)
                }
            };
            if replaces {
                best = Some((rule, specificity));
            }
        }

        best.map_or(true, |(rule, _)| rule.allow)
    }
}

/// Match a robots.txt path pattern against a URL path (+query). Supports `*`
/// wildcards and a trailing `$` end-anchor. Returns a specificity score
/// (count of literal characters matched) when the pattern matches.
fn match_pattern(pattern: &str, target: &str) -> Option<usize> {
    let mut pat = pattern;
    let anchored_end = pat.ends_with('$');
    if anchored_end {
        pat = &pat[..pat.len() - 1];
    }
    if pat.is_empty() {
        return if anchored_end { None } else { Some(0) };
    }

    let mut position = 0usize;
    let mut specificity = 0usize;
    for (index, segment) in pat.split('*').enumerate() {
        if segment.is_empty() {
            continue;
        }
        if index == 0 {
            if !target[position..].starts_with(segment) {
                return None;
            }
            position += segment.len();
        } else {
            let offset = target[position..].find(segment)?;
            position += offset + segment.len();
        }
        specificity += segment.len();
    }
    if anchored_end && position != target.len() {
        return None;
    }
    Some(specificity.max(1))
}

/// Fetch and parse robots.txt for the host of `base`. Network failures mean
/// "no restrictions" so a broken robots.txt can never stall a crawl.
pub async fn fetch_robots(client: &reqwest::Client, base: &Url) -> RobotsRules {
    let Some(host) = base.host_str() else {
        return RobotsRules::allow_all();
    };
    let mut robots_url = format!("{}://{}", base.scheme(), host);
    if let Some(port) = base.port() {
        robots_url.push_str(&format!(":{port}"));
    }
    robots_url.push_str("/robots.txt");

    match client.get(&robots_url).send().await {
        Ok(response) if response.status().is_success() => match response.text().await {
            Ok(text) => {
                let rules = RobotsRules::parse(&text, ROBOTS_UA_TOKEN);
                tracing::info!(
                    "[ROBOTS] {} rules from {} (crawl-delay: {:?})",
                    rules.rule_count(),
                    robots_url,
                    rules.crawl_delay
                );
                rules
            }
            Err(err) => {
                tracing::warn!("[ROBOTS] Failed reading {}: {}; crawling unrestricted", robots_url, err);
                RobotsRules::allow_all()
            }
        },
        Ok(response) => {
            tracing::info!(
                "[ROBOTS] No robots.txt at {} (status {}); crawling unrestricted",
                robots_url,
                response.status()
            );
            RobotsRules::allow_all()
        }
        Err(err) => {
            tracing::warn!(
                "[ROBOTS] Failed fetching {}: {}; crawling unrestricted",
                robots_url,
                err
            );
            RobotsRules::allow_all()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> RobotsRules {
        RobotsRules::parse(text, "rustyseo")
    }

    #[test]
    fn wildcard_disallow_blocks_prefix() {
        let rules = parse("User-agent: *\nDisallow: /private/");
        assert!(!rules.is_allowed("https://example.com/private/page"));
        assert!(rules.is_allowed("https://example.com/public"));
        assert!(rules.is_allowed("https://example.com/"));
    }

    #[test]
    fn allow_wins_on_longer_match() {
        let rules = parse("User-agent: *\nDisallow: /private/\nAllow: /private/open/");
        assert!(rules.is_allowed("https://example.com/private/open/page"));
        assert!(!rules.is_allowed("https://example.com/private/closed"));
    }

    #[test]
    fn wildcard_and_end_anchor_patterns() {
        let rules = parse("User-agent: *\nDisallow: /*.pdf$");
        assert!(!rules.is_allowed("https://example.com/files/manual.pdf"));
        assert!(rules.is_allowed("https://example.com/files/manual.pdfx"));
    }

    #[test]
    fn specific_group_replaces_wildcard_group() {
        let rules = parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: rustyseo\nDisallow: /admin/",
        );
        assert!(rules.is_allowed("https://example.com/blog"));
        assert!(!rules.is_allowed("https://example.com/admin/x"));
    }

    #[test]
    fn empty_disallow_allows_everything() {
        let rules = parse("User-agent: *\nDisallow:");
        assert!(rules.is_allowed("https://example.com/anything"));
    }

    #[test]
    fn root_disallow_blocks_everything() {
        let rules = parse("User-agent: *\nDisallow: /");
        assert!(!rules.is_allowed("https://example.com/"));
        assert!(!rules.is_allowed("https://example.com/page"));
    }

    #[test]
    fn crawl_delay_parsed_and_capped() {
        let rules = parse("User-agent: *\nCrawl-delay: 0.5");
        assert_eq!(rules.crawl_delay, Some(Duration::from_millis(500)));
        let capped = parse("User-agent: *\nCrawl-delay: 99999");
        assert_eq!(capped.crawl_delay, Some(Duration::from_secs(60)));
    }

    #[test]
    fn unrelated_groups_ignored() {
        let rules = parse("User-agent: googlebot\nDisallow: /nope/\n");
        assert!(rules.is_allowed("https://example.com/nope/page"));
    }
}
