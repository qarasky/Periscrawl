//! Session export: turns crawl results into spreadsheet/CSV/JSON deliverables.
//!
//! The sheet builders work from typed [`PageData`](crate::crawler::PageData), so
//! every frontend (CLI, GUI, TUI) produces identical exports. Asset sheets
//! (Images, CSS, JavaScript, Files) list each asset once with usage counts —
//! per-page references live in the page detail views of the apps.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use rust_xlsxwriter::{Format, Workbook};

use crate::crawler::PageData;
use crate::helpers::issues::IssueAnalyzer;

/// One exportable table. Column order in `header` matches every row.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    pub fn new(name: impl Into<String>, header: &[&str]) -> Self {
        Self {
            name: name.into(),
            header: header.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }
}

/// Where default exports are written (`.../rustyseo/exports`).
pub fn exports_dir() -> Result<PathBuf> {
    let dir = ProjectDirs::from("", "", "rustyseo")
        .context("could not determine project directories")?
        .data_dir()
        .join("exports");
    std::fs::create_dir_all(&dir).context("could not create exports directory")?;
    Ok(dir)
}

/// All report sheets for a crawl: data tabs first, issue audit last.
pub fn build_report_sheets(pages: &[PageData], base_domain: Option<&str>) -> Vec<Sheet> {
    let mut sheets = session_sheets(pages, base_domain);
    sheets.push(issues_sheet(pages, base_domain));
    sheets
}

/// Aggregate of one asset (image/css/js/file) across the whole crawl.
struct AssetAgg {
    pages_using: usize,
    /// Images: references missing alt text. CSS/JS: external resource count. Files: unused.
    extra: usize,
    /// Images: formatted size. CSS/JS: page total size. Files: unused.
    info: String,
    sample_pages: Vec<String>,
}

impl AssetAgg {
    fn new(info: String) -> Self {
        Self {
            pages_using: 0,
            extra: 0,
            info,
            sample_pages: Vec::new(),
        }
    }

    fn add_page(&mut self, page_url: &str) {
        self.pages_using += 1;
        if self.sample_pages.len() < 3 {
            self.sample_pages.push(page_url.to_string());
        }
    }
}

fn sorted_asset_rows(agg: HashMap<String, AssetAgg>) -> Vec<(String, AssetAgg)> {
    let mut rows: Vec<(String, AssetAgg)> = agg.into_iter().collect();
    rows.sort_by(|a, b| {
        b.1.pages_using
            .cmp(&a.1.pages_using)
            .then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// Data sheets derived purely from page records.
pub fn session_sheets(pages: &[PageData], base_domain: Option<&str>) -> Vec<Sheet> {
    let mut overview = Sheet::new(
        "Overview",
        &[
            "ID", "URL", "Status", "Title", "Title Len", "Description", "Desc Len", "H1",
            "H1 Len", "Words", "Size (bytes)", "Indexability", "Language", "Mobile",
            "Internal Links", "External Links", "Images", "Images Missing Alt", "Schema Tags",
            "Canonical Target", "Noindex Header", "Mixed Content", "CWV Desktop", "CWV Mobile",
        ],
    );
    let mut internal = Sheet::new("Internal Links", &["Page", "Target", "Anchor Text", "Rel"]);
    let mut external = Sheet::new("External Links", &["Page", "Target", "Anchor Text", "Rel"]);
    let mut redirects = Sheet::new(
        "Redirects",
        &["Page", "Hop", "Redirect URL", "Hop Status", "Final Status"],
    );

    let mut images_agg: HashMap<String, AssetAgg> = HashMap::new();
    let mut css_agg: HashMap<String, AssetAgg> = HashMap::new();
    let mut js_agg: HashMap<String, AssetAgg> = HashMap::new();
    let mut files_agg: HashMap<String, AssetAgg> = HashMap::new();

    for page in pages {
        let summary = page.summarize(base_domain);
        overview.push(vec![
            summary.id.to_string(),
            summary.url.clone(),
            summary.status.clone(),
            summary.title.clone(),
            summary.title_len.to_string(),
            summary.description.clone(),
            summary.description_len.to_string(),
            summary.h1.clone(),
            summary.h1_len.to_string(),
            summary.word_count.to_string(),
            summary.size.to_string(),
            summary.indexability.clone(),
            summary.language.clone(),
            summary.mobile.to_string(),
            summary.internal_link_count.to_string(),
            summary.external_link_count.to_string(),
            summary.images_count.to_string(),
            summary.images_missing_alt.to_string(),
            summary.schema_count.to_string(),
            summary.canonical_target.clone().unwrap_or_default(),
            summary.has_noindex_header.to_string(),
            summary.has_mixed_content.to_string(),
            summary
                .cwv_performance_desktop
                .map(|v| format!("{v:.0}"))
                .unwrap_or_default(),
            summary
                .cwv_performance_mobile
                .map(|v| format!("{v:.0}"))
                .unwrap_or_default(),
        ]);

        for link in &page.anchor_links {
            let is_external = link.href.starts_with("http")
                && base_domain.is_some_and(|d| !link.href.contains(d));
            let row = vec![
                page.url.clone(),
                link.href.clone(),
                link.text.clone(),
                link.rel.clone(),
            ];
            if is_external {
                external.push(row);
            } else {
                internal.push(row);
            }

            if file_extension(&link.href).is_some() {
                let entry = files_agg
                    .entry(link.href.clone())
                    .or_insert_with(|| AssetAgg::new(String::new()));
                entry.add_page(&page.url);
            }
        }

        for (index, hop) in page.redirect_chain.iter().enumerate() {
            redirects.push(vec![
                page.url.clone(),
                (index + 1).to_string(),
                hop.url.clone(),
                hop.status.to_string(),
                page.status.clone(),
            ]);
        }

        for image in &page.images {
            let entry = images_agg
                .entry(image.src.clone())
                .or_insert_with(|| AssetAgg::new(image.size_formatted.clone()));
            entry.add_page(&page.url);
            if image.alt.trim().is_empty() {
                entry.extra += 1;
            }
        }

        if let Some(css_info) = &page.css {
            for url in &css_info.css_urls {
                let entry = css_agg
                    .entry(url.clone())
                    .or_insert_with(|| AssetAgg::new(css_info.total_size_formatted.clone()));
                entry.add_page(&page.url);
                entry.extra = css_info.external_css_count;
            }
        }

        if let Some(js_info) = &page.javascript {
            for url in &js_info.js_urls {
                let entry = js_agg
                    .entry(url.clone())
                    .or_insert_with(|| AssetAgg::new(js_info.total_size_formatted.clone()));
                entry.add_page(&page.url);
                entry.extra = js_info.external_js_count;
            }
        }
    }

    let mut images = Sheet::new(
        "Images",
        &[
            "Image URL",
            "File Type",
            "Pages Using",
            "Missing Alt",
            "Size",
            "Sample Pages",
        ],
    );
    for (url, agg) in sorted_asset_rows(images_agg) {
        images.push(vec![
            url.clone(),
            url_extension(&url),
            agg.pages_using.to_string(),
            agg.extra.to_string(),
            agg.info,
            agg.sample_pages.join("; "),
        ]);
    }

    let mut css = Sheet::new("CSS", &["CSS URL", "File Type", "Pages Using", "Sample Pages"]);
    for (url, agg) in sorted_asset_rows(css_agg) {
        css.push(vec![
            url.clone(),
            url_extension(&url),
            agg.pages_using.to_string(),
            agg.sample_pages.join("; "),
        ]);
    }

    let mut js = Sheet::new(
        "JavaScript",
        &["JavaScript URL", "File Type", "Pages Using", "Sample Pages"],
    );
    for (url, agg) in sorted_asset_rows(js_agg) {
        js.push(vec![
            url.clone(),
            url_extension(&url),
            agg.pages_using.to_string(),
            agg.sample_pages.join("; "),
        ]);
    }

    let mut files = Sheet::new(
        "Files",
        &["File URL", "File Type", "Pages Using", "Sample Pages"],
    );
    for (url, agg) in sorted_asset_rows(files_agg) {
        files.push(vec![
            url.clone(),
            url_extension(&url),
            agg.pages_using.to_string(),
            agg.sample_pages.join("; "),
        ]);
    }

    let mut sheets = vec![
        overview,
        cwv_sheet(pages),
        content_sheet(pages),
        keywords_sheet(pages),
        internal,
        external,
        redirects,
        images,
        css,
        js,
        files,
    ];
    sheets.shrink_to_fit();
    sheets
}

/// Core Web Vitals / PageSpeed Insights, one row per page that has data.
pub fn cwv_sheet(pages: &[PageData]) -> Sheet {
    let mut sheet = Sheet::new(
        "CWV",
        &[
            "URL",
            "Desk Score",
            "Mob Score",
            "Desk LCP",
            "Desk CLS",
            "Desk TBT",
            "Desk FCP",
            "Desk SI",
            "Mob LCP",
            "Mob CLS",
            "Mob TBT",
            "Mob FCP",
            "Mob SI",
        ],
    );
    for page in pages {
        let desktop = page.cwv_desktop.as_ref();
        let mobile = page.cwv_mobile.as_ref();
        if desktop.is_none() && mobile.is_none() {
            continue;
        }
        let value = |data: Option<&crate::crawler::helpers::html_parser::CwvData>,
                     pick: fn(
            &crate::crawler::helpers::html_parser::CwvData,
        ) -> &str|
         -> String {
            data.map(|d| pick(d).to_string()).unwrap_or_default()
        };
        sheet.push(vec![
            page.url.clone(),
            value(desktop, |d| &d.performance_score),
            value(mobile, |d| &d.performance_score),
            value(desktop, |d| &d.lcp),
            value(desktop, |d| &d.cls),
            value(desktop, |d| &d.tbt),
            value(desktop, |d| &d.fcp),
            value(desktop, |d| &d.speed_index),
            value(mobile, |d| &d.lcp),
            value(mobile, |d| &d.cls),
            value(mobile, |d| &d.tbt),
            value(mobile, |d| &d.fcp),
            value(mobile, |d| &d.speed_index),
        ]);
    }
    sheet
}

/// Content-focused per-page metrics.
pub fn content_sheet(pages: &[PageData]) -> Sheet {
    let mut sheet = Sheet::new(
        "Content",
        &[
            "URL",
            "Title",
            "Title Len",
            "Description",
            "Desc Len",
            "H1",
            "H1s",
            "H2s",
            "H3s",
            "Words",
            "Size (bytes)",
            "Keywords",
        ],
    );
    for page in pages {
        let summary = page.summarize(None);
        let keywords: Vec<String> = page.keywords.iter().take(5).cloned().collect();
        sheet.push(vec![
            page.url.clone(),
            page.title.clone(),
            page.title_len.to_string(),
            page.description.clone(),
            page.description_len.to_string(),
            page.h1.clone(),
            summary.h1_count.to_string(),
            summary.h2_count.to_string(),
            summary.h3_count.to_string(),
            summary.word_count.to_string(),
            page.size.to_string(),
            keywords.join(", "),
        ]);
    }
    sheet
}

/// Site-wide keyword overview as a sheet (top 200 terms by spread).
pub fn keywords_sheet(pages: &[PageData]) -> Sheet {
    let stats = crate::content_audit::keyword_overview(pages, 200);
    let mut sheet = Sheet::new(
        "Keywords",
        &[
            "Keyword",
            "Pages",
            "Total Uses",
            "Avg Position",
            "In Titles",
            "In H1s",
            "In Descriptions",
        ],
    );
    for stat in stats {
        sheet.push(vec![
            stat.keyword,
            stat.pages.to_string(),
            stat.total_uses.to_string(),
            format!("{:.1}", stat.avg_position),
            stat.in_titles.to_string(),
            stat.in_h1s.to_string(),
            stat.in_descriptions.to_string(),
        ]);
    }
    sheet
}

/// The issue audit as a sheet: severity, issue, count, sample URLs.
pub fn issues_sheet(pages: &[PageData], base_domain: Option<&str>) -> Sheet {
    let findings = IssueAnalyzer::analyze_session(pages, base_domain);

    let mut sheet = Sheet::new("Issues", &["Severity", "Issue", "Count", "Sample URLs"]);
    for finding in findings {
        let sample: Vec<String> = finding.urls.iter().take(10).cloned().collect();
        sheet.push(vec![
            finding.severity.as_str().to_string(),
            finding.name,
            finding.count.to_string(),
            sample.join(" | "),
        ]);
    }
    sheet
}

/// Extensions that mark a link as a downloadable file (not an HTML page).
const FILE_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "csv", "txt", "rtf", "zip", "rar", "7z",
    "gz", "tar", "mp4", "mp3", "wav", "avi", "mov",
];

fn url_extension(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last_segment = without_query.rsplit('/').next().unwrap_or("");
    match last_segment.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => {
            ext.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn file_extension(url: &str) -> Option<&str> {
    let ext = url_extension(url);
    FILE_EXTENSIONS
        .iter()
        .find(|known| **known == ext)
        .copied()
}

/// True when a URL points at a downloadable file (by extension).
pub fn is_file_url(url: &str) -> bool {
    file_extension(url).is_some()
}

// ---------------------------------------------------------------------------
// Writers
// ---------------------------------------------------------------------------

/// Excel sheet names: no `: \ / ? * [ ]`, max 31 chars.
fn sanitize_sheet_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ':' | '\\' | '/' | '?' | '*' | '[' | ']'))
        .take(31)
        .collect()
}

pub fn write_xlsx(sheets: &[Sheet], path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();

    for sheet in sheets {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(sanitize_sheet_name(&sheet.name))?;

        for (col, title) in sheet.header.iter().enumerate() {
            worksheet.write_string_with_format(0, col as u16, title, &header_format)?;
        }
        for (row_index, row) in sheet.rows.iter().enumerate() {
            for (col, value) in row.iter().enumerate() {
                worksheet.write_string((row_index + 1) as u32, col as u16, value)?;
            }
        }
    }

    workbook.save(path)?;
    Ok(())
}

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn sheet_to_csv(sheet: &Sheet) -> String {
    let mut out = String::new();
    out.push_str(
        &sheet
            .header
            .iter()
            .map(|h| csv_escape(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in &sheet.rows {
        out.push_str(
            &row.iter()
                .map(|v| csv_escape(v))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

pub fn write_csv(sheet: &Sheet, path: &Path) -> Result<()> {
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(sheet_to_csv(sheet).as_bytes())?;
    Ok(())
}

/// One CSV per sheet, bundled into a zip.
pub fn write_csv_zip(sheets: &[Sheet], path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    for sheet in sheets {
        let name = format!("{}.csv", sheet.name.to_lowercase().replace(' ', "_"));
        zip.start_file(name, options)?;
        zip.write_all(sheet_to_csv(sheet).as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

/// JSON report: session metadata plus every sheet.
pub fn report_json(session: &crate::db::SessionRow, sheets: &[Sheet]) -> Result<serde_json::Value> {
    let sheet_values: Vec<serde_json::Value> = sheets
        .iter()
        .map(|sheet| {
            serde_json::json!({
                "name": sheet.name,
                "header": sheet.header,
                "rows": sheet.rows,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "session": session,
        "sheets": sheet_values,
    }))
}
