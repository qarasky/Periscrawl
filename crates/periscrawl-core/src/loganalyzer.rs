//! Nginx/Apache access-log analysis.
//!
//! Parses Common/Combined Log Format lines and reports bot vs human traffic,
//! status and file-type breakdowns, crawl-budget views and per-day totals.
//! Works on large files: lines are streamed, aggregates are bounded by the
//! requested top-N for detail lists.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::DateTime;
use regex::Regex;
use serde::Serialize;

static LOG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^(?P<ip>\S+) \S+ \S+ \[(?P<ts>[^\]]+)\] "(?P<method>[A-Z]+) (?P<path>\S+)(?: [^"]*)?" (?P<status>\d{3}) (?P<bytes>\S+)(?: "(?P<referer>[^"]*)" "(?P<ua>[^"]*)")?"#,
    )
    .expect("log regex")
});

/// Substring → canonical bot name. First match wins.
const BOT_PATTERNS: &[(&str, &str)] = &[
    ("googlebot-image", "Googlebot-Image"),
    ("googlebot-news", "Googlebot-News"),
    ("googlebot", "Googlebot"),
    ("google-inspectiontool", "Google InspectionTool"),
    ("storebot-google", "Storebot-Google"),
    ("apis-google", "APIs-Google"),
    ("adsbot-google", "AdsBot-Google"),
    ("mediapartners-google", "Mediapartners-Google"),
    ("bingbot", "Bingbot"),
    ("bingpreview", "BingPreview"),
    ("yandexbot", "YandexBot"),
    ("yandeximages", "YandexImages"),
    ("duckduckbot", "DuckDuckBot"),
    ("baiduspider", "Baiduspider"),
    ("applebot", "Applebot"),
    ("ahrefsbot", "AhrefsBot"),
    ("semrushbot", "SemrushBot"),
    ("mj12bot", "MJ12Bot"),
    ("dotbot", "DotBot"),
    ("seznam", "SeznamBot"),
    ("petalbot", "PetalBot"),
    ("gptbot", "GPTBot"),
    ("chatgpt-user", "ChatGPT-User"),
    ("oai-searchbot", "OAI-SearchBot"),
    ("claudebot", "ClaudeBot"),
    ("claude-web", "Claude-Web"),
    ("anthropic-ai", "Anthropic"),
    ("perplexitybot", "PerplexityBot"),
    ("ccbot", "CCBot"),
    ("bytespider", "Bytespider"),
    ("amazonbot", "Amazonbot"),
    ("facebookexternalhit", "FacebookBot"),
    ("meta-externalagent", "meta-externalagent"),
    ("twitterbot", "Twitterbot"),
    ("linkedinbot", "LinkedInBot"),
    ("telegrambot", "TelegramBot"),
    ("whatsapp", "WhatsApp"),
    ("discordbot", "Discordbot"),
    ("slackbot", "Slackbot"),
    ("uptimerobot", "UptimeRobot"),
    ("pingdom", "Pingdom"),
    ("statuscake", "StatusCake"),
    ("monitis", "Monitis"),
    ("dataprovider", "DataForSeo"),
    ("screaming frog", "Screaming Frog"),
    ("serpstatbot", "SerpstatBot"),
    ("sitemap", "SitemapFetcher"),
    ("python-requests", "python-requests"),
    ("python-urllib", "python-urllib"),
    ("go-http-client", "go-http-client"),
    ("curl/", "curl"),
    ("wget/", "wget"),
];

fn classify_bot(user_agent: &str) -> Option<&'static str> {
    let lower = user_agent.to_ascii_lowercase();
    for (needle, name) in BOT_PATTERNS {
        if lower.contains(needle) {
            return Some(name);
        }
    }
    if lower.contains("bot") || lower.contains("spider") || lower.contains("crawler") {
        return Some("Other bot");
    }
    None
}

/// Broad content kind for a request path (strip query/fragment first).
fn file_kind(path: &str) -> &'static str {
    let without_query = path.split(['?', '#']).next().unwrap_or(path);
    let last_segment = without_query.rsplit('/').next().unwrap_or("");
    let ext = last_segment
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "" | "html" | "htm" | "php" | "asp" | "aspx" | "jsp" => "HTML",
        "css" => "CSS",
        "js" | "mjs" | "map" => "JavaScript",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" => "Image",
        "woff" | "woff2" | "ttf" | "otf" | "eot" => "Font",
        "pdf" => "PDF",
        "xml" => "XML",
        "json" => "JSON",
        "txt" | "csv" => "Text",
        "mp4" | "webm" | "mov" | "avi" => "Video",
        "mp3" | "wav" | "ogg" => "Audio",
        "zip" | "rar" | "gz" | "7z" => "Archive",
        _ => "Other",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CountStat {
    pub name: String,
    pub requests: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotStat {
    pub bot: String,
    pub requests: usize,
    pub bytes: u64,
    pub unique_urls: usize,
    pub client_errors: usize,
    pub server_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathStat {
    pub path: String,
    pub requests: usize,
    pub bytes: u64,
    pub client_errors: usize,
    pub server_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyStat {
    pub date: String,
    pub requests: usize,
    pub bots: usize,
    pub humans: usize,
    pub client_errors: usize,
    pub server_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogReport {
    pub file: String,
    pub lines: usize,
    pub parsed: usize,
    pub malformed: usize,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub unique_ips: usize,
    pub total_bytes: u64,
    pub bot_requests: usize,
    pub human_requests: usize,
    pub status: BTreeMap<String, usize>,
    pub status_2xx: usize,
    pub status_3xx: usize,
    pub status_4xx: usize,
    pub status_5xx: usize,
    pub file_types: Vec<CountStat>,
    pub top_bots: Vec<BotStat>,
    pub top_paths: Vec<PathStat>,
    pub error_paths: Vec<PathStat>,
    pub top_ips: Vec<CountStat>,
    pub daily: Vec<DailyStat>,
}

#[derive(Default)]
struct BotAgg {
    requests: usize,
    bytes: u64,
    urls: HashSet<String>,
    client_errors: usize,
    server_errors: usize,
}

#[derive(Default)]
struct PathAgg {
    requests: usize,
    bytes: u64,
    client_errors: usize,
    server_errors: usize,
}

#[derive(Default)]
struct IpAgg {
    requests: usize,
    bytes: u64,
    bot: Option<String>,
}

#[derive(Default)]
struct DayAgg {
    requests: usize,
    bots: usize,
    humans: usize,
    client_errors: usize,
    server_errors: usize,
}

/// Analyze an access-log file. `top` bounds the detail lists in the report.
pub fn analyze_file(path: impl AsRef<Path>, top: usize) -> Result<LogReport> {
    let path = path.as_ref();
    let top = top.max(1);
    let file = std::fs::File::open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut lines = 0usize;
    let mut parsed = 0usize;
    let mut malformed = 0usize;
    let mut total_bytes = 0u64;
    let mut first_ts: Option<DateTime<chrono::FixedOffset>> = None;
    let mut last_ts: Option<DateTime<chrono::FixedOffset>> = None;
    let mut unique_ips: HashSet<String> = HashSet::new();
    let mut bot_requests = 0usize;
    let mut human_requests = 0usize;

    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    let (mut s2xx, mut s3xx, mut s4xx, mut s5xx) = (0usize, 0usize, 0usize, 0usize);

    let mut file_types: HashMap<&'static str, (usize, u64)> = HashMap::new();
    let mut bots: HashMap<&'static str, BotAgg> = HashMap::new();
    let mut paths: HashMap<String, PathAgg> = HashMap::new();
    let mut ips: HashMap<String, IpAgg> = HashMap::new();
    let mut daily: BTreeMap<String, DayAgg> = BTreeMap::new();

    use std::io::BufRead;
    for line in reader.lines() {
        lines += 1;
        let line = match line {
            Ok(line) => line,
            Err(_) => {
                malformed += 1;
                continue;
            }
        };
        let Some(caps) = LOG_LINE.captures(&line) else {
            malformed += 1;
            continue;
        };
        parsed += 1;

        let ip = caps.name("ip").map(|m| m.as_str()).unwrap_or("-");
        let user_agent = caps.name("ua").map(|m| m.as_str()).unwrap_or("");
        let path_value = caps.name("path").map(|m| m.as_str()).unwrap_or("/");
        let status: u16 = caps
            .name("status")
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let bytes: u64 = caps
            .name("bytes")
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let timestamp = caps
            .name("ts")
            .and_then(|m| DateTime::parse_from_str(m.as_str(), "%d/%b/%Y:%H:%M:%S %z").ok());

        total_bytes += bytes;
        unique_ips.insert(ip.to_string());
        *statuses.entry(status.to_string()).or_insert(0) += 1;
        match status {
            200..=299 => s2xx += 1,
            300..=399 => s3xx += 1,
            400..=499 => s4xx += 1,
            500..=599 => s5xx += 1,
            _ => {}
        }
        if let Some(ts) = timestamp {
            if first_ts.map_or(true, |first| ts < first) {
                first_ts = Some(ts);
            }
            if last_ts.map_or(true, |last| ts > last) {
                last_ts = Some(ts);
            }
            let day = ts.date_naive().format("%Y-%m-%d").to_string();
            let entry = daily.entry(day).or_default();
            entry.requests += 1;
            if status >= 400 && status < 500 {
                entry.client_errors += 1;
            }
            if (500..600).contains(&status) {
                entry.server_errors += 1;
            }
            match classify_bot(user_agent) {
                Some(_) => entry.bots += 1,
                None => entry.humans += 1,
            }
        }

        let kind = file_kind(path_value);
        let type_entry = file_types.entry(kind).or_insert((0, 0));
        type_entry.0 += 1;
        type_entry.1 += bytes;

        match classify_bot(user_agent) {
            Some(bot) => {
                bot_requests += 1;
                let entry = bots.entry(bot).or_default();
                entry.requests += 1;
                entry.bytes += bytes;
                if entry.urls.len() < 100_000 {
                    entry.urls.insert(path_value.to_string());
                }
                if status >= 400 && status < 500 {
                    entry.client_errors += 1;
                }
                if (500..600).contains(&status) {
                    entry.server_errors += 1;
                }
            }
            None => human_requests += 1,
        }

        let path_entry = paths.entry(path_value.to_string()).or_default();
        path_entry.requests += 1;
        path_entry.bytes += bytes;
        if status >= 400 && status < 500 {
            path_entry.client_errors += 1;
        }
        if (500..600).contains(&status) {
            path_entry.server_errors += 1;
        }

        let ip_entry = ips.entry(ip.to_string()).or_default();
        ip_entry.requests += 1;
        ip_entry.bytes += bytes;
        if ip_entry.bot.is_none() {
            ip_entry.bot = classify_bot(user_agent).map(|bot| bot.to_string());
        }
    }

    let mut file_types: Vec<CountStat> = file_types
        .into_iter()
        .map(|(kind, (requests, bytes))| CountStat {
            name: kind.to_string(),
            requests,
            bytes,
        })
        .collect();
    file_types.sort_by(|a, b| b.requests.cmp(&a.requests));

    let mut top_bots: Vec<BotStat> = bots
        .into_iter()
        .map(|(bot, agg)| BotStat {
            bot: bot.to_string(),
            requests: agg.requests,
            bytes: agg.bytes,
            unique_urls: agg.urls.len(),
            client_errors: agg.client_errors,
            server_errors: agg.server_errors,
        })
        .collect();
    top_bots.sort_by(|a, b| b.requests.cmp(&a.requests));
    top_bots.truncate(top);

    let mut all_paths: Vec<(String, PathAgg)> = paths.into_iter().collect();
    all_paths.sort_by(|a, b| b.1.requests.cmp(&a.1.requests));
    let top_paths: Vec<PathStat> = all_paths
        .iter()
        .take(top)
        .map(|(path, agg)| PathStat {
            path: path.clone(),
            requests: agg.requests,
            bytes: agg.bytes,
            client_errors: agg.client_errors,
            server_errors: agg.server_errors,
        })
        .collect();

    let mut error_paths: Vec<PathStat> = all_paths
        .iter()
        .filter(|(_, agg)| agg.client_errors + agg.server_errors > 0)
        .map(|(path, agg)| PathStat {
            path: path.clone(),
            requests: agg.requests,
            bytes: agg.bytes,
            client_errors: agg.client_errors,
            server_errors: agg.server_errors,
        })
        .collect();
    error_paths.sort_by(|a, b| {
        (b.client_errors + b.server_errors).cmp(&(a.client_errors + a.server_errors))
    });
    error_paths.truncate(top);

    let mut top_ips: Vec<CountStat> = ips
        .into_iter()
        .map(|(ip, agg)| CountStat {
            name: match agg.bot {
                Some(bot) => format!("{ip} ({bot})"),
                None => ip,
            },
            requests: agg.requests,
            bytes: agg.bytes,
        })
        .collect();
    top_ips.sort_by(|a, b| b.requests.cmp(&a.requests));
    top_ips.truncate(top);

    let daily: Vec<DailyStat> = daily
        .into_iter()
        .map(|(date, agg)| DailyStat {
            date,
            requests: agg.requests,
            bots: agg.bots,
            humans: agg.humans,
            client_errors: agg.client_errors,
            server_errors: agg.server_errors,
        })
        .collect();

    Ok(LogReport {
        file: path.display().to_string(),
        lines,
        parsed,
        malformed,
        first_timestamp: first_ts.map(|ts| ts.to_rfc3339()),
        last_timestamp: last_ts.map(|ts| ts.to_rfc3339()),
        unique_ips: unique_ips.len(),
        total_bytes,
        bot_requests,
        human_requests,
        status: statuses,
        status_2xx: s2xx,
        status_3xx: s3xx,
        status_4xx: s4xx,
        status_5xx: s5xx,
        file_types,
        top_bots,
        top_paths,
        error_paths,
        top_ips,
        daily,
    })
}

#[cfg(test)]
mod tests {
    use super::{classify_bot, file_kind};

    #[test]
    fn bot_classification_covers_major_ai_and_search_bots() {
        assert_eq!(
            classify_bot("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"),
            Some("Googlebot")
        );
        assert_eq!(
            classify_bot("Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.2; +https://openai.com/gptbot"),
            Some("GPTBot")
        );
        assert_eq!(classify_bot("SomeRandomSpider/0.1"), Some("Other bot"));
        assert_eq!(classify_bot("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15"), None);
    }

    #[test]
    fn file_kind_maps_extensions() {
        assert_eq!(file_kind("/"), "HTML");
        assert_eq!(file_kind("/catalog/product-name"), "HTML");
        assert_eq!(file_kind("/assets/app.js?v=3"), "JavaScript");
        assert_eq!(file_kind("/img/hero.webp"), "Image");
        assert_eq!(file_kind("/docs/report.pdf"), "PDF");
    }
}
