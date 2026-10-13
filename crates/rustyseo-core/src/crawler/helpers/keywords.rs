use crate::crawler::helpers::text::body_text;
use scraper::Html;
use std::collections::HashMap;

pub fn extract_keywords(html: &Html) -> Vec<String> {
    // 1. Visible body text (script/style/noscript/svg/canvas excluded).
    let body_text = body_text(html);

    // 2. Tokenize and normalize
    let words = body_text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 2) // Skip very short words
        .map(|s| s.to_lowercase());

    // 3. Simple blocklist of common English + Russian stop words
    let stop_words = [
        "the", "and", "for", "that", "with", "this", "from", "your", "have", "will", "been",
        "they", "more", "when", "into", "their", "there", "what", "which", "some", "them", "then",
        "just", "than", "were", "well", "only", "about", "could", "also", "would", "very", "every",
        "many", "does", "ever", "most", "even", "such", "than", "here", "there", "where", "when",
        "look", "they", "their", "them", "from", "each", "used", "your", "much", "time", "back",
        "true", "work", "take", "name", "good", "used", "made", "both", "once", "still", "last",
        "long", "find", "down", "come", "than", "away", "find", "come", "than", "down", "once",
        "both",
        // Russian function words (Cyrillic sites otherwise drown in these)
        "и", "в", "во", "не", "что", "он", "на", "я", "с", "со", "как", "а", "то", "все", "она",
        "так", "его", "но", "да", "ты", "к", "у", "же", "вы", "за", "бы", "по", "только", "ее",
        "мне", "было", "вот", "от", "меня", "еще", "нет", "о", "из", "ему", "теперь", "когда",
        "даже", "ну", "вдруг", "ли", "если", "уже", "или", "ни", "быть", "был", "него", "до",
        "вас", "опять", "уж", "вам", "ведь", "там", "потом", "себя", "ничего", "ей", "может",
        "они", "тут", "где", "есть", "надо", "ней", "для", "мы", "тебя", "их", "чем", "была",
        "сам", "чтоб", "без", "будто", "чего", "раз", "тоже", "себе", "под", "будет", "тогда",
        "кто", "этот", "того", "потому", "этого", "какой", "совсем", "ним", "здесь", "этом",
        "один", "почти", "мой", "тем", "чтобы", "нее", "сейчас", "были", "куда", "зачем", "всех",
        "никогда", "можно", "при", "наконец", "два", "об", "другой", "хоть", "после", "над",
        "больше", "тот", "через", "эти", "нас", "про", "всего", "них", "какая", "много", "разве",
        "три", "эту", "моя", "впрочем", "хорошо", "свою", "этой", "перед", "иногда", "лучше",
        "чуть", "том", "нельзя", "такой", "им", "более", "всегда", "конечно", "всю", "между",
        "которые", "который", "которая", "которое", "этих", "этим", "свои", "своих", "вашего",
        "вашей", "год", "года", "году", "лет", "года", "также", "если", "чем", "кто", "эта",
    ];

    // 4. Count frequencies
    let mut counts = HashMap::new();
    for word in words {
        if !stop_words.contains(&word.as_str()) {
            *counts.entry(word).or_insert(0) += 1;
        }
    }

    // 5. Sort by frequency and take top 10
    let mut sorted_counts: Vec<(String, usize)> = counts.into_iter().collect();
    sorted_counts.sort_by(|a, b| b.1.cmp(&a.1));

    sorted_counts
        .into_iter()
        .take(10)
        .map(|(word, count)| format!("({}) {}", count, word))
        .collect()
}
