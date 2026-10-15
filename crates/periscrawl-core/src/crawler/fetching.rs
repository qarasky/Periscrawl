use crate::crawler::helpers::html_parser::{PageData, extract_page_elements};
use headless_chrome::Browser;
use rand::Rng;
use reqwest::Client;
use scraper::Html;
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use url::Url;

/// Conditional-request validators captured from a previous crawl.
#[derive(Debug, Clone, Default)]
pub struct FetchValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Outcome of fetching one URL.
pub enum FetchOutcome {
    Fetched(PageData),
    /// Server answered 304: reuse the stored page from the baseline session.
    NotModified,
}

/// PageSpeed context for one fetch, borrowed from the engine. When the crawl
/// is stored (session present), PSI calls run in the background under the
/// shared permit pool so they never block crawl slots; one-shot `--no-db`
/// crawls keep them inline so the results stay on the page records.
pub struct PsiRef<'a> {
    pub config: &'a crate::models::PageSpeedConfig,
    pub sampler: &'a crate::crawler::helpers::psi::PsiSampler,
    pub semaphore: &'a Arc<tokio::sync::Semaphore>,
    pub client: &'a Client,
    pub session_id: Option<i64>,
}

/// Extract ETag / Last-Modified validators from a stored page's headers.
pub fn validators_from(page: &PageData) -> Option<FetchValidators> {
    let mut validators = FetchValidators::default();
    for header in &page.headers {
        if let Some((name, value)) = header.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "etag" => validators.etag = Some(value.trim().to_string()),
                "last-modified" => validators.last_modified = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    if validators.etag.is_none() && validators.last_modified.is_none() {
        None
    } else {
        Some(validators)
    }
}

/// Internal result of a plain (non-JS) fetch.
enum StandardFetch {
    Page(PageData, String),
    NotModified,
}

pub async fn fetch_and_process(
    client: &Client,
    browser: &Option<Arc<Browser>>,
    user_agents: &[String],
    psi: Option<&PsiRef<'_>>,
    enable_javascript: bool,
    js_fallback: bool,
    validators: Option<&FetchValidators>,
    url: &str,
    base_url: &Url,
    referer: Option<String>,
) -> Result<FetchOutcome, String> {
    let mut page_data = if enable_javascript {
        if let Some(browser) = browser {
            fetch_js(url, base_url, browser.clone()).await?
        } else {
            match fetch_standard(client, user_agents, url, base_url, referer, None).await? {
                StandardFetch::Page(data, _) => data,
                StandardFetch::NotModified => return Ok(FetchOutcome::NotModified),
            }
        }
    } else {
        match fetch_standard(client, user_agents, url, base_url, referer, validators).await? {
            StandardFetch::NotModified => return Ok(FetchOutcome::NotModified),
            StandardFetch::Page(standard, html) => {
                if js_fallback && should_render_js(&standard, &html) {
                    if let Some(browser) = browser {
                        tracing::info!(
                            "[JS-FALLBACK] Rendering {} (client-side framework markers or sparse content)",
                            standard.url
                        );
                        match fetch_js(url, base_url, browser.clone()).await {
                            Ok(mut rendered) => {
                                merge_rendered(&mut rendered, &standard);
                                rendered
                            }
                            Err(err) => {
                                tracing::warn!("[JS-FALLBACK] Render failed for {}: {}", url, err);
                                standard
                            }
                        }
                    } else {
                        tracing::warn!(
                            "[JS-FALLBACK] {} looks client-rendered but no browser is available",
                            standard.url
                        );
                        standard
                    }
                } else {
                    standard
                }
            }
        }
    };

    if let Some(psi) = psi {
        if psi.config.status && !psi.config.api_key.is_empty() {
            psi.sampler.add_nav_targets(&page_data.nav_link_targets);
            let is_html = page_data.content_type.to_lowercase().contains("text/html");
            let is_ok = page_data.status.starts_with('2');
            let sampled = psi.sampler.should_sample(&page_data.url);
            if is_html && is_ok && sampled {
                match psi.session_id {
                    // Stored crawl: run PSI in the background (bounded permit
                    // pool) so slow API calls never hold a crawl slot. The
                    // worker writes into the stored page once done.
                    Some(session_id) => {
                        let client = psi.client.clone();
                        let permit_pool = psi.semaphore.clone();
                        let api_key = psi.config.api_key.clone();
                        let page_url = page_data.url.clone();
                        tracing::info!("[CWV] Queued PageSpeed for {}", page_url);
                        tokio::spawn(async move {
                            let _permit = permit_pool.acquire_owned().await.ok();
                            let desktop =
                                match fetch_pagespeed_data(&client, &page_url, "desktop", &api_key)
                                    .await
                                {
                                    Ok(cwv) => Some(cwv),
                                    Err(err) => {
                                        tracing::error!(
                                            "[CWV] Desktop failed for {}: {}",
                                            page_url,
                                            err
                                        );
                                        None
                                    }
                                };
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            let mobile =
                                match fetch_pagespeed_data(&client, &page_url, "mobile", &api_key)
                                    .await
                                {
                                    Ok(cwv) => Some(cwv),
                                    Err(err) => {
                                        tracing::error!(
                                            "[CWV] Mobile failed for {}: {}",
                                            page_url,
                                            err
                                        );
                                        None
                                    }
                                };
                            if desktop.is_some() || mobile.is_some() {
                                if let Err(err) = crate::db::update_page_cwv(
                                    session_id,
                                    &page_url,
                                    desktop.as_ref(),
                                    mobile.as_ref(),
                                ) {
                                    tracing::warn!(
                                        "[CWV] Could not store results for {}: {}",
                                        page_url,
                                        err
                                    );
                                }
                            }
                        });
                    }
                    // No stored session (`--no-db`): keep the results inline
                    // so they stay on the page records of this run.
                    None => {
                        tracing::info!("[CWV] Fetching PageSpeed for {}", url);
                        match fetch_pagespeed_data(psi.client, url, "desktop", &psi.config.api_key)
                            .await
                        {
                            Ok(cwv) => page_data.cwv_desktop = Some(cwv),
                            Err(e) => tracing::error!("[CWV] Desktop failed for {}: {}", url, e),
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        match fetch_pagespeed_data(psi.client, url, "mobile", &psi.config.api_key)
                            .await
                        {
                            Ok(cwv) => page_data.cwv_mobile = Some(cwv),
                            Err(e) => tracing::error!("[CWV] Mobile failed for {}: {}", url, e),
                        }
                    }
                }
            }
        }
    }

    Ok(FetchOutcome::Fetched(page_data))
}

async fn fetch_standard(
    client: &Client,
    user_agents: &[String],
    url: &str,
    base_url: &Url,
    referer: Option<String>,
    validators: Option<&FetchValidators>,
) -> Result<StandardFetch, String> {
    let mut current_url = url.to_string();
    let mut redirect_chain = Vec::new();
    let mut hops = 0;
    let max_hops = 10;

    // Retry configuration
    let max_retries = 4;
    let mut retry_count = 0;
    let mut backoff = Duration::from_secs(2);

    let response = loop {
        let user_agent = pick_random_user_agent(user_agents);
        let mut request = client
            .get(&current_url)
            .header("User-Agent", user_agent)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.5")
            .header("Sec-Fetch-Dest", "document")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "same-origin")
            .header("DNT", "1")
            .header("Upgrade-Insecure-Requests", "1");

        if let Some(ref_url) = referer.as_ref() {
            request = request.header("Referer", ref_url);
        }
        if let Some(validators) = validators {
            if let Some(etag) = &validators.etag {
                request = request.header("If-None-Match", etag);
            }
            if let Some(last_modified) = &validators.last_modified {
                request = request.header("If-Modified-Since", last_modified);
            }
        }

        match request.send().await {
            Ok(res) => {
                let status = res.status();

                if status.is_redirection() && hops < max_hops {
                    if let Some(location) = res.headers().get("location") {
                        if let Ok(location_str) = location.to_str() {
                            // Resolve redirect URL against the CURRENT URL, not the base_url
                            let current_url_parsed = Url::parse(&current_url).map_err(|e| {
                                format!("Failed to parse current URL {}: {}", current_url, e)
                            })?;

                            let next_url = match Url::parse(location_str) {
                                Ok(u) => u.to_string(),
                                Err(_) => match current_url_parsed.join(location_str) {
                                    Ok(u) => u.to_string(),
                                    Err(_) => current_url.clone(),
                                },
                            };

                            redirect_chain.push(crate::models::RedirectHop {
                                url: next_url.clone(),
                                status: status.as_u16(),
                            });

                            current_url = next_url;
                            hops += 1;
                            retry_count = 0;
                            backoff = Duration::from_secs(2);
                            continue;
                        }
                    }
                }

                // Handle Rate Limiting (429) and Server Errors (5xx)
                if status.as_u16() == 429 || status.is_server_error() {
                    // ... (keep existing retry logic)
                    if retry_count < max_retries {
                        retry_count += 1;
                        tracing::warn!(
                            "[RETRY] {} - {} (Attempt {}/{}). Waiting {:?}...",
                            url,
                            status,
                            retry_count,
                            max_retries,
                            backoff
                        );
                        sleep(backoff).await;
                        backoff *= 2;
                        continue;
                    } else {
                        tracing::error!(
                            "[FAIL] {} - Max retries exceeded for status {}",
                            url,
                            status
                        );
                        return Err(format!("Max retries exceeded for status {}", status));
                    }
                }

                break res;
            }
            Err(e) => {
                // ... (keep existing retry logic)
                if retry_count < max_retries {
                    retry_count += 1;
                    tracing::warn!(
                        "[RETRY] {} - Network Error: {} (Attempt {}/{}). Waiting {:?}...",
                        url,
                        e,
                        retry_count,
                        max_retries,
                        backoff
                    );
                    sleep(backoff).await;
                    backoff *= 2;
                    continue;
                } else {
                    return Err(format!("Request failed after retries: {}", e));
                }
            }
        }
    };

    let status_code = response.status();
    tracing::info!("[FETCH] {} -> STATUS: {}", url, status_code);

    if status_code.as_u16() == 304 {
        tracing::info!(
            "[FETCH] {} -> 304 Not Modified (reusing stored content)",
            url
        );
        return Ok(StandardFetch::NotModified);
    }

    let headers_list: Vec<String> = response
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("[invalid]")))
        .collect();

    let status = format!(
        "{} {}",
        status_code.as_u16(),
        status_code.canonical_reason().unwrap_or("")
    );

    let content_type_header = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let html_content = response
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;

    // Only HTML bodies are parsed. Files (docx/pdf/images…) become minimal
    // records with identity/status/size, so content analyzers never run on
    // binary payloads.
    let is_html = content_type_header
        .as_deref()
        .map(|content_type| content_type.to_lowercase().contains("html"))
        .unwrap_or(true);
    let mut page_data = if is_html {
        let document = Html::parse_document(&html_content);
        extract_page_elements(&document)
    } else {
        crate::crawler::helpers::html_parser::PageData::empty()
    };
    // "Size (bytes)" is the decoded response body size, not a text-length proxy.
    page_data.size = html_content.len();
    // Normalize the final (post-redirect) URL so it matches the form other pages'
    // anchor_links/outlinks use when referencing this page - otherwise redirected
    // pages never match url_to_status lookups (e.g. everything looks "unvisited"
    // on a site that trailing-slash- or https-redirects every internal link).
    page_data.url = crate::crawler::url_normalizer::normalize_url(&current_url)
        .unwrap_or_else(|| current_url.clone());
    page_data.requested_url =
        crate::crawler::url_normalizer::normalize_url(url).unwrap_or_else(|| url.to_string());
    page_data.status = status;
    page_data.headers = headers_list;
    page_data.redirect_chain = redirect_chain;

    if let Some(ct) = content_type_header {
        page_data.content_type = ct;
    }

    // Parse the final URL to use as base for resolving relative links
    let final_url = Url::parse(&current_url)
        .map_err(|e| format!("Failed to parse final URL {}: {}", current_url, e))?;

    // Store all original links as outlinks before filtering
    page_data.outlinks = page_data
        .anchor_links
        .iter()
        .map(|link| {
            // Resolve against final_url (the page we are on)
            if let Ok(abs_url) = final_url.join(&link.href) {
                crate::crawler::helpers::html_parser::AnchorLink {
                    href: abs_url.to_string(),
                    text: link.text.clone(),
                    rel: link.rel.clone(),
                }
            } else {
                link.clone()
            }
        })
        .collect();

    // Resolve header/nav/footer links against the final URL and normalize them
    // the same way crawled page URLs are stored, so the PageSpeed sampler can
    // match them exactly.
    let mut seen_nav = std::collections::HashSet::new();
    page_data.nav_link_targets = page_data
        .nav_link_targets
        .iter()
        .filter_map(|href| final_url.join(href).ok())
        .map(|abs| {
            crate::crawler::url_normalizer::normalize_url(abs.as_str())
                .unwrap_or_else(|| abs.to_string())
        })
        .filter(|url| seen_nav.insert(url.clone()))
        .collect();

    // Filter and normalize links to stay on same domain
    let mut seen_urls = std::collections::HashSet::new();
    page_data.anchor_links = page_data
        .anchor_links
        .into_iter()
        .filter_map(|link| {
            // Resolve against final_url (the page we are on)
            if let Ok(mut abs_url) = final_url.join(&link.href) {
                // Check domain against base_url (crawl scope) using loose comparison
                if crate::crawler::url_normalizer::is_same_domain(
                    abs_url.host_str(),
                    base_url.host_str(),
                ) {
                    // Start by clearing fragment
                    abs_url.set_fragment(None);

                    // Normalize the URL string using the centralized normalizer
                    if let Some(normalized_url) =
                        crate::crawler::url_normalizer::normalize_url(abs_url.as_str())
                    {
                        // Check if we should crawl this URL type
                        if crate::crawler::url_normalizer::should_crawl_url(&normalized_url) {
                            // Deduplicate within this page
                            if seen_urls.insert(normalized_url.clone()) {
                                return Some(crate::crawler::helpers::html_parser::AnchorLink {
                                    href: normalized_url,
                                    text: link.text,
                                    rel: link.rel,
                                });
                            }
                        }
                    }
                }
            }
            None
        })
        .collect();

    tracing::info!(
        "[LINKS] Found {} unique same-domain links on {}",
        page_data.anchor_links.len(),
        url
    );

    Ok(StandardFetch::Page(page_data, html_content))
}

/// Heuristic: does this page look like it needs client-side rendering?
///
/// True when the raw HTML carries deliberate client-framework markers
/// (Next.js, Nuxt, Vue, React, Angular) AND the extracted content is either
/// thin or the framework's mount point sits empty (a classic SPA shell).
pub fn should_render_js(page: &PageData, html: &str) -> bool {
    let lower = html.to_lowercase();
    let framework_marker = lower.contains("__next_data__")
        || lower.contains("/_next/")
        || lower.contains("__nuxt__")
        || lower.contains("/_nuxt/")
        || lower.contains("data-reactroot")
        || lower.contains("data-server-rendered")
        || lower.contains("data-v-")
        || lower.contains("<app-root");
    if !framework_marker {
        return false;
    }

    let words = page.word_count.unwrap_or(0);
    let thin_content = words < 60;
    let empty_mount = lower.contains("id=\"__next\"></div>")
        || lower.contains("id=\"__nuxt\"></div>")
        || lower.contains("id=\"root\"></div>")
        || lower.contains("id=\"app\"></div>");

    thin_content || empty_mount
}

/// Keep the HTTP-fetch identity (status, headers, redirects, size) while
/// swapping in the rendered DOM's content fields.
fn merge_rendered(rendered: &mut PageData, standard: &PageData) {
    rendered.id = standard.id;
    rendered.url = standard.url.clone();
    rendered.requested_url = standard.requested_url.clone();
    rendered.status = standard.status.clone();
    rendered.headers = standard.headers.clone();
    rendered.redirect_chain = standard.redirect_chain.clone();
    rendered.size = standard.size;
    rendered.content_type = standard.content_type.clone();
    rendered.js_rendered = true;
}

async fn fetch_js(url: &str, base_url: &Url, browser: Arc<Browser>) -> Result<PageData, String> {
    tracing::debug!("[JS-FETCH] Navigating to {}", url);

    let url_str = url.to_string();

    let (html_content, status) = tokio::task::spawn_blocking(move || {
        let tab = browser
            .new_tab()
            .map_err(|e| format!("Tab creation failed: {}", e))?;
        tab.navigate_to(&url_str)
            .map_err(|e| format!("Navigation failed: {}", e))?;
        tab.wait_until_navigated()
            .map_err(|e| format!("Wait failed: {}", e))?;
        let content = tab
            .get_content()
            .map_err(|e| format!("Content fetch failed: {}", e))?;
        Ok::<(String, String), String>((content, "200 OK (JS)".to_string()))
    })
    .await
    .map_err(|e| e.to_string())??;

    let document = Html::parse_document(&html_content);
    let mut page_data = extract_page_elements(&document);
    page_data.size = html_content.len();
    page_data.url = url.to_string();
    page_data.requested_url = url.to_string();
    page_data.status = status;
    page_data.headers = vec!["Requested-Mode: JavaScript".to_string()];

    // Parse the current page URL to use as base for resolving relative links
    let current_page_url =
        Url::parse(url).map_err(|e| format!("Failed to parse current URL {}: {}", url, e))?;

    // Store all original links as outlinks before filtering
    page_data.outlinks = page_data
        .anchor_links
        .iter()
        .map(|link| {
            if let Ok(abs_url) = current_page_url.join(&link.href) {
                crate::crawler::helpers::html_parser::AnchorLink {
                    href: abs_url.to_string(),
                    text: link.text.clone(),
                    rel: link.rel.clone(),
                }
            } else {
                link.clone()
            }
        })
        .collect();

    // Resolve header/nav/footer links the same way as crawled page URLs.
    let mut seen_nav = std::collections::HashSet::new();
    page_data.nav_link_targets = page_data
        .nav_link_targets
        .iter()
        .filter_map(|href| current_page_url.join(href).ok())
        .map(|abs| {
            crate::crawler::url_normalizer::normalize_url(abs.as_str())
                .unwrap_or_else(|| abs.to_string())
        })
        .filter(|url| seen_nav.insert(url.clone()))
        .collect();

    // Filter and normalize links...
    let mut seen_urls = std::collections::HashSet::new();
    page_data.anchor_links = page_data
        .anchor_links
        .into_iter()
        .filter_map(|link| {
            if let Ok(mut abs_url) = current_page_url.join(&link.href) {
                if crate::crawler::url_normalizer::is_same_domain(
                    abs_url.host_str(),
                    base_url.host_str(),
                ) {
                    abs_url.set_fragment(None);

                    // Normalize using centralized logic
                    if let Some(normalized_url) =
                        crate::crawler::url_normalizer::normalize_url(abs_url.as_str())
                    {
                        if crate::crawler::url_normalizer::should_crawl_url(&normalized_url) {
                            if seen_urls.insert(normalized_url.clone()) {
                                return Some(crate::crawler::helpers::html_parser::AnchorLink {
                                    href: normalized_url,
                                    text: link.text,
                                    rel: link.rel,
                                });
                            }
                        }
                    }
                }
            }
            None
        })
        .collect();

    tracing::info!(
        "[JS-LINKS] Found {} unique same-domain links on {}",
        page_data.anchor_links.len(),
        url
    );

    Ok(page_data)
}

async fn fetch_pagespeed_data(
    client: &Client,
    url: &str,
    strategy: &str,
    api_key: &str,
) -> Result<crate::crawler::helpers::html_parser::CwvData, String> {
    let mut api_url =
        Url::parse("https://www.googleapis.com/pagespeedonline/v5/runPagespeed").unwrap();
    api_url
        .query_pairs_mut()
        .append_pair("url", url)
        .append_pair("key", api_key)
        .append_pair("strategy", strategy)
        .append_pair("category", "performance");

    let response = client
        .get(api_url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("API error: {}", response.status()));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("JSON parse failed: {}", e))?;

    let lighthouse = json.get("lighthouseResult").ok_or("No lighthouseResult")?;
    let categories = lighthouse.get("categories").ok_or("No categories")?;
    let performance = categories
        .get("performance")
        .ok_or("No performance category")?;
    let score = performance
        .get("score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        * 100.0;
    let audits = lighthouse.get("audits").ok_or("No audits")?;

    let get_display_value = |audit_name: &str| {
        audits
            .get(audit_name)
            .and_then(|a| a.get("displayValue"))
            .and_then(|v| v.as_str())
            .unwrap_or("N/A")
            .to_string()
    };

    Ok(crate::crawler::helpers::html_parser::CwvData {
        fcp: get_display_value("first-contentful-paint"),
        lcp: get_display_value("largest-contentful-paint"),
        cls: get_display_value("cumulative-layout-shift"),
        tbt: get_display_value("total-blocking-time"),
        speed_index: get_display_value("speed-index"),
        performance_score: format!("{:.0}", score),
    })
}

fn pick_random_user_agent(user_agents: &[String]) -> String {
    if user_agents.is_empty() {
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)".to_string()
    } else {
        user_agents[rand::rng().random_range(0..user_agents.len())].clone()
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::{should_render_js, validators_from};
    use crate::crawler::helpers::html_parser::{PageData, extract_page_elements};

    fn page_with_words(words: usize) -> PageData {
        let html = format!("<html><body>{}</body></html>", "word ".repeat(words));
        let document = scraper::Html::parse_document(&html);
        extract_page_elements(&document)
    }

    #[test]
    fn framework_marker_with_thin_content_triggers() {
        let page = page_with_words(10);
        let html = r#"<html><body><div id="__next"></div><script>__NEXT_DATA__</script></body></html>"#;
        assert!(should_render_js(&page, html));
    }

    #[test]
    fn framework_marker_with_rich_content_skips() {
        // A server-rendered Next.js page still carries the markers; rendering
        // it again would be wasted work.
        let page = page_with_words(500);
        let html = r#"<html><body><div id="__next">lots of server-rendered text</div><script>__NEXT_DATA__</script></body></html>"#;
        assert!(!should_render_js(&page, html));
    }

    #[test]
    fn plain_thin_page_without_framework_skips() {
        let page = page_with_words(5);
        let html = "<html><body><p>Short but static page.</p></body></html>";
        assert!(!should_render_js(&page, html));
    }

    #[test]
    fn empty_mount_without_framework_marker_skips() {
        let page = page_with_words(2);
        let html = r#"<html><body><div id="root"></div><script src="/main.js"></script></body></html>"#;
        // No framework marker by name: an empty React mount alone is not
        // enough to justify a render.
        assert!(!should_render_js(&page, html));
        let vue_page = page_with_words(2);
        let vue_html = r#"<html><body><div id="app"></div><script src="/app.js"></script></body></html>"#;
        assert!(!should_render_js(&vue_page, vue_html));
    }

    #[test]
    fn vue_marker_with_empty_mount_triggers() {
        let page = page_with_words(2);
        let html =
            r#"<html><body><div id="app" data-server-rendered="true"><div id="app"></div></div><script data-v-1234abcd></script></body></html>"#;
        assert!(should_render_js(&page, html));
    }

    #[test]
    fn validators_extracted_from_stored_headers() {
        let mut page = page_with_words(1);
        page.headers = vec![
            "Content-Type: text/html".to_string(),
            "ETag: \"abc123\"".to_string(),
            "Last-Modified: Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
        ];
        let validators = validators_from(&page).expect("validators");
        assert_eq!(validators.etag.as_deref(), Some("\"abc123\""));
        assert_eq!(
            validators.last_modified.as_deref(),
            Some("Wed, 21 Oct 2026 07:28:00 GMT")
        );

        let mut bare = page_with_words(1);
        bare.headers = vec!["Content-Type: text/html".to_string()];
        assert!(validators_from(&bare).is_none());
    }
}
