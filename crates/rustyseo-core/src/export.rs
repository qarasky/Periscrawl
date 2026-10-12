//! Session export: turns crawl results into spreadsheet/CSV/JSON deliverables.
//!
//! The sheet builders work from typed [`PageData`](crate::crawler::PageData), so
//! every frontend (CLI now, GUI/TUI later) produces identical exports.

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
    let mut images = Sheet::new("Images", &["Page", "Image URL", "Alt Text", "Size"]);
    let mut css = Sheet::new("CSS", &["Page", "CSS URL", "Page CSS Total", "External Count"]);
    let mut js = Sheet::new("JavaScript", &["Page", "JS URL", "Page JS Total", "External Count"]);
    let mut files = Sheet::new("Files", &["Page", "File URL", "Anchor Text"]);

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
                files.push(vec![page.url.clone(), link.href.clone(), link.text.clone()]);
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
            images.push(vec![
                page.url.clone(),
                image.src.clone(),
                image.alt.clone(),
                image.size_formatted.clone(),
            ]);
        }

        if let Some(css_info) = &page.css {
            for url in &css_info.css_urls {
                css.push(vec![
                    page.url.clone(),
                    url.clone(),
                    css_info.total_size_formatted.clone(),
                    css_info.external_css_count.to_string(),
                ]);
            }
        }

        if let Some(js_info) = &page.javascript {
            for url in &js_info.js_urls {
                js.push(vec![
                    page.url.clone(),
                    url.clone(),
                    js_info.total_size_formatted.clone(),
                    js_info.external_js_count.to_string(),
                ]);
            }
        }
    }

    vec![overview, internal, external, redirects, images, css, js, files]
}

/// The issue audit as a sheet: issue, count, sample URLs.
pub fn issues_sheet(pages: &[PageData], base_domain: Option<&str>) -> Sheet {
    let summaries: Vec<_> = pages
        .iter()
        .map(|p| p.summarize(base_domain))
        .collect();

    let mut sheet = Sheet::new("Issues", &["Issue", "Count", "Sample URLs"]);
    for handler in IssueAnalyzer::get_handlers() {
        let (count, urls) = (handler.process)(&summaries);
        if count > 0 {
            let sample: Vec<String> = urls.iter().take(10).cloned().collect();
            sheet.push(vec![
                handler.name.trim().to_string(),
                count.to_string(),
                sample.join(" | "),
            ]);
        }
    }
    sheet
}

/// Extensions that mark a link as a downloadable file (not an HTML page).
const FILE_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "csv", "txt", "rtf", "zip", "rar", "7z",
    "gz", "tar", "mp4", "mp3", "wav", "avi", "mov",
];

fn file_extension(url: &str) -> Option<&str> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last_segment = without_query.rsplit('/').next()?;
    let ext = last_segment.rsplit('.').next()?;
    if ext == last_segment {
        return None;
    }
    let ext_lower = ext.to_ascii_lowercase();
    FILE_EXTENSIONS
        .iter()
        .find(|known| **known == ext_lower)
        .copied()
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
        let name = format!(
            "{}.csv",
            sheet.name.to_lowercase().replace(' ', "_")
        );
        zip.start_file(name, options)?;
        zip.write_all(sheet_to_csv(sheet).as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

/// JSON report: session metadata plus every sheet.
pub fn report_json(
    session: &crate::db::SessionRow,
    sheets: &[Sheet],
) -> Result<serde_json::Value> {
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
