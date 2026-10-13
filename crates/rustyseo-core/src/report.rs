//! Client-facing PDF reports for crawl sessions.
//!
//! Builds a clean HTML report (summary, issues by severity, broken pages) and
//! renders it to PDF via printpdf's HTML layout engine — automatic page
//! breaks, embedded fonts and page numbers included.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use printpdf::{GeneratePdfOptions, PdfDocument, PdfSaveOptions};

use crate::crawler::PageData;
use crate::db::SessionRow;
use crate::helpers::issues::{IssueAnalyzer, IssueFinding, IssueSeverity};

pub struct ReportOptions {
    /// Broken pages listed (default 60).
    pub top_errors: usize,
    /// Issues listed (default 40).
    pub top_issues: usize,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            top_errors: 60,
            top_issues: 40,
        }
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render the audit report as HTML (also used as the PDF intermediate).
pub fn build_session_report_html(
    session: &SessionRow,
    pages: &[PageData],
    base_domain: Option<&str>,
    options: &ReportOptions,
) -> String {
    // Issue findings from the shared session analyzer (consistent gating,
    // includes the whole-crawl link-graph issues).
    let mut findings: Vec<IssueFinding> = IssueAnalyzer::analyze_session(pages, base_domain);
    findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.count.cmp(&a.count)));
    let (mut critical, mut warning, mut notice, mut total) = (0usize, 0usize, 0usize, 0usize);
    for finding in &findings {
        total += finding.count;
        match finding.severity {
            IssueSeverity::Critical => critical += finding.count,
            IssueSeverity::Warning => warning += finding.count,
            IssueSeverity::Notice => notice += finding.count,
        }
    }

    // Broken pages (4xx/5xx), worst first by status.
    let mut broken: Vec<(&str, &str)> = pages
        .iter()
        .filter(|page| {
            page.status
                .get(..1)
                .and_then(|first| first.parse::<u8>().ok())
                .is_some_and(|first| first >= 4)
        })
        .map(|page| (page.status.as_str(), page.url.as_str()))
        .collect();
    broken.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));

    let started = chrono::DateTime::parse_from_rfc3339(&session.started_at)
        .map(|ts| ts.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| session.started_at.clone());
    let generated = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let severity_color = |severity: IssueSeverity| match severity {
        IssueSeverity::Critical => "#c0392b",
        IssueSeverity::Warning => "#d35400",
        IssueSeverity::Notice => "#7f8c8d",
    };

    let mut html = String::with_capacity(16_384);
    html.push_str("<!DOCTYPE html><html><head><style>\n");
    html.push_str(
        "body { font-size: 12px; color: #1c1e26; }\
         h1 { font-size: 22px; margin-bottom: 2px; }\
         h2 { font-size: 15px; margin-top: 18px; margin-bottom: 6px; }\
         .meta { color: #5c6370; font-size: 11px; margin-bottom: 12px; }\
         table { border-collapse: collapse; width: 100%; margin-bottom: 4px; }\
         th { background: #f2f4f8; text-align: left; font-size: 11px; }\
         th, td { border: 1px solid #d5d9e2; padding: 4px 6px; font-size: 11px; }\
         .num { text-align: right; }\
         .sev { font-weight: bold; }\
         .url { font-size: 10px; color: #34495e; }\
         .tiles td { border: none; padding: 2px 10px 2px 0; }\
         .tile-label { color: #5c6370; font-size: 10px; }\
         .tile-value { font-size: 18px; font-weight: bold; }\
         .footer { margin-top: 14px; color: #99a0ad; font-size: 9px; }\n",
    );
    html.push_str("</style></head><body>\n");

    html.push_str("<h1>SEO Audit Report</h1>\n");
    html.push_str(&format!(
        "<div class=\"meta\">{} &nbsp;•&nbsp; session #{} &nbsp;•&nbsp; crawled {} &nbsp;•&nbsp; {} pages fetched ({} failed)</div>\n",
        esc(&session.start_url),
        session.id,
        esc(&started),
        session.pages_fetched,
        session.pages_failed
    ));

    html.push_str("<table class=\"tiles\"><tr>");
    for (label, value, color) in [
        ("Pages", pages.len(), "#1c1e26"),
        ("Stored issues", total, "#1c1e26"),
        ("Critical", critical, "#c0392b"),
        ("Warning", warning, "#d35400"),
        ("Notice", notice, "#7f8c8d"),
        ("Broken pages", broken.len(), "#c0392b"),
    ] {
        html.push_str(&format!(
            "<td><div class=\"tile-label\">{}</div><div class=\"tile-value\" style=\"color:{}\">{}</div></td>",
            label, color, value
        ));
    }
    html.push_str("</tr></table>\n");

    html.push_str("<h2>Issues by severity</h2>\n");
    if findings.is_empty() {
        html.push_str("<div>No issues detected.</div>\n");
    } else {
        html.push_str("<table><tr><th>Severity</th><th>Issue</th><th class=\"num\">Count</th></tr>\n");
        for finding in findings.iter().take(options.top_issues) {
            html.push_str(&format!(
                "<tr><td class=\"sev\" style=\"color:{}\">{}</td><td>{}</td><td class=\"num\">{}</td></tr>\n",
                severity_color(finding.severity),
                finding.severity.as_str(),
                esc(&finding.name),
                finding.count
            ));
        }
        html.push_str("</table>\n");
        if findings.len() > options.top_issues {
            html.push_str(&format!(
                "<div class=\"meta\">… and {} more issue types (see the full export for everything).</div>\n",
                findings.len() - options.top_issues
            ));
        }
    }

    html.push_str("<h2>Broken pages (4xx / 5xx)</h2>\n");
    if broken.is_empty() {
        html.push_str("<div>No broken pages found. Nice.</div>\n");
    } else {
        html.push_str("<table><tr><th>Status</th><th>URL</th></tr>\n");
        for (status, url) in broken.iter().take(options.top_errors) {
            html.push_str(&format!(
                "<tr><td class=\"num\">{}</td><td class=\"url\">{}</td></tr>\n",
                esc(status),
                esc(url)
            ));
        }
        html.push_str("</table>\n");
        if broken.len() > options.top_errors {
            html.push_str(&format!(
                "<div class=\"meta\">… and {} more broken pages (see the Excel/CSV export for the full list).</div>\n",
                broken.len() - options.top_errors
            ));
        }
    }

    html.push_str(&format!(
        "<div class=\"footer\">Generated by RustySEO on {} — data from the session's stored pages; verify fixes with a re-crawl and compare.</div>\n",
        esc(&generated)
    ));
    html.push_str("</body></html>\n");
    html
}

/// Render and write the PDF report for a session.
pub fn write_session_pdf(
    session: &SessionRow,
    pages: &[PageData],
    base_domain: Option<&str>,
    path: impl AsRef<Path>,
    options: &ReportOptions,
) -> Result<()> {
    let path = path.as_ref();
    let html = build_session_report_html(session, pages, base_domain, options);

    let mut warnings = Vec::new();
    let pdf_options = GeneratePdfOptions {
        margin_top: Some(14.0),
        margin_right: Some(14.0),
        margin_bottom: Some(16.0),
        margin_left: Some(14.0),
        show_page_numbers: Some(true),
        ..Default::default()
    };
    let doc = PdfDocument::from_html(
        &html,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &pdf_options,
        &mut warnings,
    )
    .map_err(|err| anyhow::anyhow!("PDF generation failed: {err}"))?;

    let bytes = doc.save(&PdfSaveOptions::default(), &mut Vec::new());
    if bytes.is_empty() {
        bail!("PDF rendering produced no output");
    }
    std::fs::write(path, bytes)
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_session_report_html;
    use super::ReportOptions;
    use crate::db::SessionRow;

    fn session() -> SessionRow {
        SessionRow {
            id: 7,
            name: Some("demo".to_string()),
            start_url: "https://example.com".to_string(),
            started_at: "2026-10-12T10:00:00+05:00".to_string(),
            finished_at: None,
            status: "completed".to_string(),
            pages_fetched: 0,
            pages_failed: 0,
            max_pages: None,
        }
    }

    #[test]
    fn report_contains_sections_and_escapes() {
        let html = build_session_report_html(&session(), &[], None, &ReportOptions::default());
        assert!(html.contains("SEO Audit Report"));
        assert!(html.contains("Issues by severity"));
        assert!(html.contains("Broken pages"));
        assert!(html.contains("https://example.com"));
    }
}
