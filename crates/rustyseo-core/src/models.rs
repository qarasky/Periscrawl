
use serde::{Deserialize, Serialize};

/// Payload sent back from the background robots/sitemaps fetch task.
pub struct RobotsResult {
    pub disallowed_urls: Vec<String>,
    pub raw_content: String,
    pub sitemap_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppSettings {
    pub crawler: CrawlerConfig,
    pub ui: UiConfig,
    pub system: SystemConfig,
    pub connectors: ConnectorsConfig,
    pub provider: LLMprovider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrawlerConfig {
    pub max_pages: usize,
    pub concurrency: usize,
    pub user_agent: String,
    pub stay_on_domain: bool,
    pub follow_redirects: bool,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub enable_javascript: bool,
    #[serde(default)]
    pub max_memory_pages: usize,
    #[serde(default)]
    pub extractor: bool,
    #[serde(default)]
    pub extractor_text: String,
    #[serde(default)]
    pub extractor_type: String,
    #[serde(default)]
    pub batch_size: usize,
    #[serde(default)]
    pub check_external_links: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiConfig {
    pub theme: String,
    pub show_logs_on_start: bool,
    pub sidebar_width_percentage: u16,
    pub refresh_rate_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemConfig {
    pub database_path: String,
    pub log_level: String,
    pub export_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorsConfig {
    pub pagespeed: PageSpeedConfig,
    pub search_console: SearchConsoleConfig,
    pub gemini: GeminiConfig,
    pub openai: OpenAiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageSpeedConfig {
    pub api_key: String,
    pub status: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchConsoleConfig {
    pub token: String,
    pub project_id: String,
    pub project_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiConfig {
    pub api_key: String,
    pub model: String,
    pub status: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LLMprovider {
    pub llm: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            crawler: CrawlerConfig {
                max_pages: 300,
                concurrency: 10,
                user_agent: "RustySEO/0.1.0".to_string(),
                stay_on_domain: true,
                follow_redirects: true,
                timeout_seconds: 15,
                enable_javascript: false,
                max_memory_pages: 1000,
                extractor: false,
                extractor_type: "".to_string(),
                extractor_text: "".to_string(),
                batch_size: 50,
                check_external_links: false,
            },
            ui: UiConfig {
                theme: "Oceanic".to_string(),
                show_logs_on_start: false,
                sidebar_width_percentage: 33,
                refresh_rate_ms: 100,
            },
            system: SystemConfig {
                database_path: "./rustyseo.db".to_string(),
                log_level: "info".to_string(),
                export_format: "csv".to_string(),
            },

            provider: LLMprovider {
                llm: "Not selected".to_string(),
            },
            connectors: ConnectorsConfig {
                pagespeed: PageSpeedConfig {
                    api_key: "".to_string(),
                    status: false,
                },
                search_console: SearchConsoleConfig {
                    token: "".to_string(),
                    project_id: "".to_string(),
                    project_name: "".to_string(),
                },
                gemini: GeminiConfig {
                    api_key: "".to_string(),
                    model: "gemini-pro".to_string(),
                    status: false,
                },
                openai: OpenAiConfig {
                    api_key: "".to_string(),
                    model: "gpt-4-turbo".to_string(),
                },
            },
        }
    }
}

impl AppSettings {
    pub fn path() -> std::path::PathBuf {
        let project_dirs = directories::ProjectDirs::from("", "", "rustyseo").unwrap();
        project_dirs.data_dir().join("cli-settings.toml")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            toml::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatLog {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct InternalLink {
    pub id: usize,
    pub source: String,
    pub destination: String,
    pub anchor: String,
    pub rel: String,
}

#[derive(Debug, Clone)]
pub struct ExternalLink {
    pub id: usize,
    pub source: String,
    pub destination: String,
    pub anchor: String,
    pub rel: String,
}

/// A single on-page SEO factor as shown in the "View SEO Score" action.
#[derive(Debug, Clone)]
pub struct SeoScoreFactor {
    pub label: String,
    pub passed: bool,
    pub detail: String,
}

/// Composite on-page SEO score for a single crawled page, shown by the
/// Actions Menu "View SEO Score" action.
#[derive(Debug, Clone)]
pub struct SeoScoreBreakdown {
    pub url: String,
    pub score: u32,
    pub factors: Vec<SeoScoreFactor>,
}

/// A link found on a specific page, shown by the Actions Menu "Extract Links" action.
#[derive(Debug, Clone)]
pub struct PageLinkEntry {
    pub destination: String,
    pub anchor: String,
    pub rel: String,
    pub is_internal: bool,
}

#[derive(Debug, Clone)]
pub struct CssUrl {
    pub id: usize,
    pub url: String,
    pub page_count: usize, // Number of pages that reference this CSS URL
}

#[derive(Debug, Clone)]
pub struct JsUrl {
    pub id: usize,
    pub url: String,
    pub script_type: String,
    pub is_async: bool,
    pub is_defer: bool,
    pub page_count: usize,
}

/// Entry for the Custom Search extraction results table
#[derive(Debug, Clone)]
pub struct ExtractionTableEntry {
    pub id: usize,
    pub url: String,
    pub element: String,
    pub snippet: String,
}

/// Entry for the Images results table
#[derive(Debug, Clone)]
pub struct ImageTableEntry {
    pub id: usize,
    pub url: String,
    pub alt: String,
    pub status: String,
    pub size: String,
    pub page_count: usize,
}

/// Entry for the Files results table
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: usize,
    pub url: String,
    pub filetype: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedirectHop {
    pub url: String,
    pub status: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedirectEntry {
    pub id: usize,
    pub initial_url: String,
    pub status_code: u16,
    pub chain: Vec<RedirectHop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RobotsEntry {
    pub id: usize,
    pub url: String,
    pub blocked_urls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PageSummary {
    pub id: usize,
    pub url: String,
    pub title: String,
    pub title_len: usize,
    pub h1: String,
    pub description: String,
    pub description_len: usize,
    pub status: String,
    pub h1_len: usize,
    pub h1_count: usize,
    pub h2_count: usize,
    pub h3_count: usize,
    pub h4_count: usize,
    pub h5_count: usize,
    pub h6_count: usize,
    pub has_schema: bool,
    pub schema_count: usize,
    pub size: usize,
    pub word_count: usize,
    pub internal_link_count: usize,
    pub external_link_count: usize,
    pub images_count: usize,
    pub images_missing_alt: usize,
    pub is_canonical: bool,
    pub has_png_jpg: bool,
    pub mobile: bool,
    pub indexability: String,
    pub language: String,
    pub cwv_performance_desktop: Option<f64>,
    pub cwv_performance_mobile: Option<f64>,
    pub has_generic_anchors: bool,
    /// True if an `X-Robots-Tag` response header contains `noindex` - a page can be
    /// blocked from indexing this way even when its meta robots tag looks fine.
    pub has_noindex_header: bool,
    /// Normalized href of the page's `rel="canonical"` tag, if present and it
    /// points somewhere other than the page's own URL.
    pub canonical_target: Option<String>,
    /// Number of `rel="canonical"` tags found (should be 0 or 1; >1 is a bug).
    pub canonical_count: usize,
    /// True if an HTTPS page loads an image, stylesheet, or script over plain HTTP.
    pub has_mixed_content: bool,
}
