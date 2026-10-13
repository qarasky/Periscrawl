//! Shared visible-text extraction for content metrics.

use scraper::{Html, Selector};
use std::sync::LazyLock;

static BODY_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("body").unwrap());

/// Visible text of the document body. Skips `script`, `style`, `noscript`,
/// `svg` and `canvas` subtrees entirely — client-framework payloads (Next.js
/// RSC data, JSON state blobs) would otherwise dominate word counts.
pub fn body_text(document: &Html) -> String {
    let mut body_text = String::new();

    if let Some(body) = document.select(&BODY_SELECTOR).next() {
        for node in body.descendants() {
            if let Some(text) = node.value().as_text() {
                let mut is_blacklisted = false;
                let mut parent = node.parent();
                while let Some(ancestor) = parent {
                    if let Some(element) = ancestor.value().as_element() {
                        let name = element.name();
                        if name == "script"
                            || name == "style"
                            || name == "noscript"
                            || name == "svg"
                            || name == "canvas"
                        {
                            is_blacklisted = true;
                            break;
                        }
                    }
                    parent = ancestor.parent();
                }

                if !is_blacklisted {
                    body_text.push_str(text);
                    body_text.push(' ');
                }
            }
        }
    }

    body_text
}

#[cfg(test)]
mod tests {
    use super::body_text;

    #[test]
    fn script_and_style_content_is_excluded() {
        let html = r#"<html><body>
            <p>visible words here</p>
            <script>self.__next_f.push([1,"rsc payload words that should not count"])</script>
            <style>.a { content: "not words"; }</style>
            <noscript>noscript words</noscript>
        </body></html>"#;
        let document = scraper::Html::parse_document(html);
        let text = body_text(&document);
        assert!(text.contains("visible words here"));
        assert!(!text.contains("rsc payload"));
        assert!(!text.contains("not words"));
        assert!(!text.contains("noscript words"));
    }
}
