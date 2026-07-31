// Programmatic cleanup of common Whisper artefacts. Port of the Swift
// `TranscriptionPostProcessor` — same passes, same guardrail:
//
// * Known hallucinations — closing-credit phrases Whisper emits on silence.
// * Repetition loops — the same word/phrase repeated many times.
// * Bracketed non-speech notes — `[Music]`, `(applause)`.
// * Leading punctuation noise — `". hello"` → `"hello"`.
// * Whitespace normalisation.
//
// The processor never returns an empty string for non-trivial input that
// wasn't purely a known artefact — if filtering would empty the string, the
// original text is returned untouched.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub struct Options {
    pub strip_known_hallucinations: bool,
    pub collapse_repeats: bool,
    pub strip_bracketed_noise: bool,
    pub normalize_whitespace: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            strip_known_hallucinations: true,
            collapse_repeats: true,
            strip_bracketed_noise: true,
            normalize_whitespace: true,
        }
    }
}

/// Same list as the Swift side: phrases observed in English, Russian and
/// Mandarin transcripts of silent/noisy audio.
const HALLUCINATION_PHRASES: &[&str] = &[
    // English
    "subtitles by the amara.org community",
    "subtitles by amara.org",
    "subtitles by amaraorg",
    "amara.org",
    "thank you for watching",
    "thanks for watching",
    "please subscribe",
    "subscribe to the channel",
    "subscribe to my channel",
    "don't forget to subscribe",
    "english (auto-generated)",
    "(silence)",
    // Russian
    "подписывайтесь на канал",
    "спасибо за просмотр",
    "продолжение следует",
    "корректор:",
    "субтитры:",
    "субтитры подготовил",
    "редактор субтитров",
    "проверьте видео",
    // Mandarin
    "字幕来自 amara.org",
    "感谢观看",
    "请订阅",
    // Generic noise
    "you you you",
    "thank you. thank you.",
];

pub fn clean(input: &str) -> String {
    clean_with(input, Options::default())
}

pub fn clean_with(input: &str, options: Options) -> String {
    let original = input;
    let mut text = input.to_string();

    if options.strip_bracketed_noise {
        text = strip_bracketed_noise(&text);
    }
    if options.strip_known_hallucinations {
        text = strip_known_hallucinations(&text);
    }
    if options.collapse_repeats {
        text = collapse_repeats(&text);
    }
    if options.normalize_whitespace {
        text = normalize_whitespace(&text);
    }
    text = strip_leading_trailing_noise(&text);

    // Guardrail: if cleaning emptied a non-trivial input, the original is
    // probably more useful than nothing — don't silently swallow it.
    let trimmed_original = original.trim();
    if text.is_empty() && trimmed_original.chars().count() > 12 {
        return trimmed_original.to_string();
    }
    text
}

fn strip_known_hallucinations(input: &str) -> String {
    let kept: Vec<&str> = input
        .split('\n')
        .filter(|line| {
            let trimmed = line.trim().to_lowercase();
            if trimmed.is_empty() {
                return true;
            }
            for phrase in HALLUCINATION_PHRASES {
                if trimmed == *phrase {
                    return false;
                }
                // Whole-phrase boundary match where the phrase dominates the
                // line (at most 4 extra characters around it).
                if trimmed.chars().count() <= phrase.chars().count() + 4
                    && contains_with_boundaries(&trimmed, phrase)
                {
                    return false;
                }
            }
            true
        })
        .collect();
    kept.join("\n")
}

/// `haystack.contains(needle)` with word-ish boundaries: the characters
/// immediately before and after the match must not be alphanumeric.
fn contains_with_boundaries(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(needle) {
        let start = search_from + rel;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .map_or(true, |c| !c.is_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .map_or(true, |c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        // Advance past this occurrence and keep looking.
        search_from = start + needle.len().max(1);
        if search_from >= haystack.len() {
            break;
        }
    }
    false
}

/// Collapse the same word repeated 4+ times in a row to one occurrence, and
/// the same 2-word phrase repeated 3+ times to one occurrence. Token-based
/// (the Swift version uses `\b\w+` regexes with backreferences, which the
/// `regex` crate doesn't support); only "pure word" tokens are collapsed so
/// punctuation-separated repeats like "yes, yes, yes" survive, matching the
/// Swift behaviour.
fn collapse_repeats(input: &str) -> String {
    let tokens: Vec<&str> = input.split(' ').collect();
    let is_word = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');
    let eq = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();

    // Pass 1: single word repeated 4+ times.
    let mut out: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if is_word(t) {
            let mut j = i + 1;
            while j < tokens.len() && is_word(tokens[j]) && eq(tokens[j], t) {
                j += 1;
            }
            if j - i >= 4 {
                out.push(t);
                i = j;
                continue;
            }
        }
        out.push(t);
        i = 1 + i;
    }

    // Pass 2: 2-word phrase repeated 3+ times.
    let tokens = out;
    let mut out: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if i + 1 < tokens.len() && is_word(tokens[i]) && is_word(tokens[i + 1]) {
            let mut j = i + 2;
            let mut reps = 1;
            while j + 1 < tokens.len()
                && is_word(tokens[j])
                && is_word(tokens[j + 1])
                && eq(tokens[j], tokens[i])
                && eq(tokens[j + 1], tokens[i + 1])
            {
                reps += 1;
                j += 2;
            }
            if reps >= 3 {
                out.push(tokens[i]);
                out.push(tokens[i + 1]);
                i = j;
                continue;
            }
        }
        out.push(tokens[i]);
        i += 1;
    }

    out.join(" ")
}

fn strip_bracketed_noise(input: &str) -> String {
    static RX: OnceLock<Regex> = OnceLock::new();
    let rx = RX.get_or_init(|| Regex::new(r"[\[\(][^\[\]\(\)]{1,30}[\]\)]").unwrap());
    rx.replace_all(input, "").into_owned()
}

fn normalize_whitespace(input: &str) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static NEWLINES: OnceLock<Regex> = OnceLock::new();
    let spaces = SPACES.get_or_init(|| Regex::new(r"[ \t]+").unwrap());
    let newlines = NEWLINES.get_or_init(|| Regex::new(r"\n{2,}").unwrap());
    let text = spaces.replace_all(input, " ");
    newlines.replace_all(&text, "\n").into_owned()
}

fn strip_leading_trailing_noise(input: &str) -> String {
    const LEADING_NOISE: &str = ".,;:!?…\"'-—–»«()[]{}";
    let text = input
        .trim_start_matches(|c: char| c.is_whitespace() || LEADING_NOISE.contains(c));
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_normal_text_through() {
        assert_eq!(clean("Привет, это обычный текст."), "Привет, это обычный текст.");
        assert_eq!(clean("Hello world."), "Hello world.");
    }

    #[test]
    fn strips_short_hallucination_when_alone() {
        assert_eq!(clean("Субтитры:"), "");
        assert_eq!(clean("amara.org"), "");
        assert_eq!(clean("感谢观看"), "");
    }

    #[test]
    fn guardrail_resurrects_long_hallucination_only_input() {
        // Parity with the Swift processor: the "never return empty for
        // non-trivial input" guardrail wins over the hallucination filter
        // when the phrase is the *entire* input and longer than 12 chars.
        assert_eq!(clean("Спасибо за просмотр"), "Спасибо за просмотр");
    }

    #[test]
    fn keeps_hallucination_phrase_inside_real_sentence() {
        // A long line dominated by real content must survive.
        let s = "I told him thanks for watching my bag while I was away";
        assert_eq!(clean(s), s);
    }

    #[test]
    fn drops_hallucination_line_keeps_content_line() {
        let out = clean("Реальный текст диктовки\nСпасибо за просмотр");
        assert_eq!(out, "Реальный текст диктовки");
    }

    #[test]
    fn collapses_word_repeated_four_plus_times() {
        assert_eq!(clean("you you you you you"), "you");
        assert_eq!(clean("так так так так так так"), "так");
    }

    #[test]
    fn keeps_word_repeated_three_times() {
        assert_eq!(clean("да да да"), "да да да");
    }

    #[test]
    fn collapses_two_word_phrase_repeated_three_plus_times() {
        assert_eq!(clean("thank you thank you thank you"), "thank you");
    }

    #[test]
    fn does_not_collapse_punctuated_repeats() {
        assert_eq!(clean("yes, yes, yes, yes, yes."), "yes, yes, yes, yes, yes.");
    }

    #[test]
    fn strips_bracketed_noise() {
        assert_eq!(clean("[Music] Hello there"), "Hello there");
        assert_eq!(clean("Привет (аплодисменты) мир"), "Привет мир");
    }

    #[test]
    fn strips_leading_punctuation_noise() {
        assert_eq!(clean(". hello"), "hello");
        assert_eq!(clean("… привет"), "привет");
    }

    #[test]
    fn keeps_trailing_sentence_punctuation() {
        assert_eq!(clean("Это конец."), "Это конец.");
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(clean("a  b\t c"), "a b c");
        assert_eq!(clean("a\n\n\nb"), "a\nb");
    }

    #[test]
    fn guardrail_returns_original_when_cleaning_empties_long_input() {
        // Bracket-strip alone would empty this, but it's non-trivial input
        // (more than 12 chars), so the original must come back.
        let s = "(это длинная скобка с текстом)";
        assert_eq!(clean(s), s);
    }

    #[test]
    fn short_pure_noise_becomes_empty() {
        assert_eq!(clean("..."), "");
        assert_eq!(clean("[Music]"), "");
        assert_eq!(clean(""), "");
    }
}
