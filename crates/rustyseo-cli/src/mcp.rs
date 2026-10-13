//! Minimal MCP (Model Context Protocol) server over stdio.
//!
//! Exposes crawl control (through the sync server when one is running) plus
//! session queries to MCP clients such as Claude Code. stdout carries only
//! JSON-RPC messages; diagnostics go to stderr.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use rustyseo_core::db;
use rustyseo_core::export;
use rustyseo_core::helpers::issues::{IssueAnalyzer, severity_for};
use rustyseo_core::models::PageSummary;
use rustyseo_core::report;
use rustyseo_core::server::SyncClient;

pub async fn serve() -> anyhow::Result<()> {
    db::init_db();
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // Notifications (no id) are fire-and-forget.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "rustyseo", "version": env!("CARGO_PKG_VERSION") },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(&name, &arguments).await {
                    Ok(text) => Ok(json!({
                        "content": [ { "type": "text", "text": text } ],
                        "isError": false,
                    })),
                    Err(message) => Ok(json!({
                        "content": [ { "type": "text", "text": message } ],
                        "isError": true,
                    })),
                }
            }
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            other => Err(format!("method not found: {other}")),
        };

        let message = match response {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": error } })
            }
        };
        stdout.write_all(message.to_string().as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "crawl_start",
            "description": "Start a crawl of a website. Requires a running RustySEO sync server (the desktop GUI, or `rustyseo serve`). Returns the session id; read results later with session_issues / session_pages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Start URL, e.g. https://example.com" },
                    "max_pages": { "type": "integer", "description": "Page budget (defaults to the settings value)" },
                    "name": { "type": "string", "description": "Optional label for the session" },
                    "js": { "type": "boolean", "description": "Render every page with headless Chrome (default false; the smart JS fallback applies from settings)" }
                },
                "required": ["url"]
            }
        },
        {
            "name": "crawl_status",
            "description": "Live status of the crawl currently running on the sync server (if any): pages, failures, queue depth, elapsed time.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "crawl_stop",
            "description": "Ask the running crawl to stop (in-flight requests drain first).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "sessions_list",
            "description": "List recorded crawl sessions, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "description": "Default 20" } }
            }
        },
        {
            "name": "session_pages",
            "description": "List pages of a session (id, url, status, title, words, indexability).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "integer" },
                    "limit": { "type": "integer" },
                    "status": { "type": "string", "description": "Optional case-insensitive status filter, e.g. '404'" }
                },
                "required": ["session_id"]
            }
        },
        {
            "name": "session_issues",
            "description": "Run the SEO issue analyzers over a session. Returns per-issue severity, count and sample URLs. Filter with severity: critical | warning | notice.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "integer" },
                    "severity": { "type": "string" },
                    "max_urls": { "type": "integer", "description": "Sample URLs per issue (default 10)" }
                },
                "required": ["session_id"]
            }
        },
        {
            "name": "session_export",
            "description": "Export a session. format=json returns the report inline; format=pdf, xlsx or csv writes a file (out_path or the exports dir) and returns its path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "integer" },
                    "format": { "type": "string", "description": "json | xlsx | csv (default json)" },
                    "tab": { "type": "string", "description": "Single sheet when format=csv" },
                    "out_path": { "type": "string" }
                },
                "required": ["session_id"]
            }
        }
    ])
}

async fn call_tool(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "crawl_start" => {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or("url is required")?;
            let mut client = SyncClient::connect().await.map_err(|err| {
                format!("{err} — start the GUI or `rustyseo serve` first")
            })?;
            let params = json!({
                "url": url,
                "name": args.get("name").cloned().unwrap_or(Value::Null),
                "max_pages": args.get("max_pages").cloned().unwrap_or(Value::Null),
                "js": args.get("js").and_then(Value::as_bool).unwrap_or(false),
            });
            let result = client.request("crawl.start", params).await?;
            serde_json::to_string_pretty(&result).map_err(|err| err.to_string())
        }
        "crawl_status" => match SyncClient::connect().await {
            Ok(mut client) => {
                let status = client.request("status", json!({})).await?;
                serde_json::to_string_pretty(&json!({ "server": true, "status": status }))
                    .map_err(|err| err.to_string())
            }
            Err(_) => Ok(json!({
                "server": false,
                "hint": "start the GUI or `rustyseo serve` to control crawls"
            })
            .to_string()),
        },
        "crawl_stop" => {
            let mut client = SyncClient::connect().await?;
            let result = client.request("crawl.stop", json!({})).await?;
            Ok(result.to_string())
        }
        "sessions_list" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let rows = db::list_sessions(limit).map_err(|err| err.to_string())?;
            serde_json::to_string_pretty(&rows).map_err(|err| err.to_string())
        }
        "session_pages" => {
            let session_id = arg_i64(args, "session_id")?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|limit| limit as usize);
            let mut rows = db::session_page_rows(session_id, limit).map_err(|err| err.to_string())?;
            if let Some(filter) = args.get("status").and_then(Value::as_str) {
                let needle = filter.to_lowercase();
                rows.retain(|row| row.status.to_lowercase().contains(&needle));
            }
            let summary = json!({ "session_id": session_id, "count": rows.len(), "pages": rows });
            serde_json::to_string_pretty(&summary).map_err(|err| err.to_string())
        }
        "session_issues" => {
            let session_id = arg_i64(args, "session_id")?;
            let severity_filter = args
                .get("severity")
                .and_then(Value::as_str)
                .map(|value| value.to_lowercase());
            let max_urls = args.get("max_urls").and_then(Value::as_u64).unwrap_or(10) as usize;

            let session = db::get_session(session_id)
                .map_err(|err| err.to_string())?
                .ok_or("session not found")?;
            let pages = db::load_session_pages(session_id).map_err(|err| err.to_string())?;
            let base_domain = url::Url::parse(&session.start_url)
                .ok()
                .and_then(|url| url.host_str().map(|host| host.to_string()));
            let summaries: Vec<PageSummary> = pages
                .iter()
                .map(|page| page.summarize(base_domain.as_deref()))
                .collect();

            let mut issues = Vec::new();
            let mut total = 0usize;
            for handler in IssueAnalyzer::get_handlers() {
                let severity = severity_for(handler.name).as_str();
                if let Some(filter) = &severity_filter {
                    if severity != filter {
                        continue;
                    }
                }
                let (count, urls) = (handler.process)(&summaries);
                if count == 0 {
                    continue;
                }
                total += count;
                issues.push(json!({
                    "issue": handler.name.trim(),
                    "severity": severity,
                    "count": count,
                    "urls": urls.iter().take(max_urls).collect::<Vec<_>>(),
                }));
            }
            serde_json::to_string_pretty(&json!({
                "session_id": session_id,
                "pages_analyzed": summaries.len(),
                "total_findings": total,
                "issues": issues,
            }))
            .map_err(|err| err.to_string())
        }
        "session_export" => {
            let session_id = arg_i64(args, "session_id")?;
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json")
                .to_lowercase();
            let session = db::get_session(session_id)
                .map_err(|err| err.to_string())?
                .ok_or("session not found")?;
            let pages = db::load_session_pages(session_id).map_err(|err| err.to_string())?;
            if pages.is_empty() {
                return Err("session has no stored pages".to_string());
            }
            let base_domain = url::Url::parse(&session.start_url)
                .ok()
                .and_then(|url| url.host_str().map(|host| host.to_string()));
            let sheets = export::build_report_sheets(&pages, base_domain.as_deref());

            match format.as_str() {
                "json" => {
                    let value = export::report_json(&session, &sheets).map_err(|err| err.to_string())?;
                    serde_json::to_string_pretty(&value).map_err(|err| err.to_string())
                }
                "xlsx" | "csv" | "pdf" => {
                    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
                    let path = match args.get("out_path").and_then(Value::as_str) {
                        Some(path) => std::path::PathBuf::from(path),
                        None => {
                            let ext = match format.as_str() {
                                "xlsx" => "xlsx",
                                "pdf" => "pdf",
                                _ => "zip",
                            };
                            export::exports_dir()
                                .map_err(|err| err.to_string())?
                                .join(format!("rustyseo_session{session_id}_{timestamp}.{ext}"))
                        }
                    };
                    if format == "xlsx" {
                        export::write_xlsx(&sheets, &path).map_err(|err| err.to_string())?;
                    } else if format == "pdf" {
                        report::write_session_pdf(
                            &session,
                            &pages,
                            base_domain.as_deref(),
                            &path,
                            &Default::default(),
                        )
                        .map_err(|err| err.to_string())?;
                    } else if let Some(tab) = args.get("tab").and_then(Value::as_str) {
                        let sheet = sheets
                            .iter()
                            .find(|sheet| sheet.name.eq_ignore_ascii_case(tab))
                            .ok_or_else(|| format!("sheet not found: {tab}"))?;
                        export::write_csv(sheet, &path).map_err(|err| err.to_string())?;
                    } else {
                        export::write_csv_zip(&sheets, &path).map_err(|err| err.to_string())?;
                    }
                    Ok(json!({ "written": path.display().to_string() }).to_string())
                }
                other => Err(format!("unsupported format: {other}")),
            }
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn arg_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{key} is required (integer)"))
}
