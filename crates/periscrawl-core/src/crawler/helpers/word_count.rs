use crate::crawler::helpers::text::body_text;
use scraper::Html;

/// Count words in the page's visible text (script/style excluded).
pub fn get_words(html: &Html) -> usize {
    body_text(html).split_whitespace().count()
}
