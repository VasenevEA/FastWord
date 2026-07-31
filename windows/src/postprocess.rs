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
                    && contains_with_word_boundaries(&trimmed, phrase)
                {
                    return false;
                }
            }
            true
        })
        .collect();
    kept.join("\n")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `haystack.contains(needle)` with regex-`\b` semantics at both ends (the
/// Swift original wraps the escaped phrase in `\b…\b`): a boundary exists
/// where word-char-ness flips, with "outside the string" counting as
/// non-word. Notably, a phrase that *ends* in punctuation (`"субтитры:"`)
/// can never boundary-match before a space — exactly like `\b` — so such
/// phrases are only dropped on exact line equality.
fn contains_with_word_boundaries(haystack: &str, needle: &str) -> bool {
    let first_w = needle.chars().next().map_or(false, is_word_char);
    let last_w = needle.chars().next_back().map_or(false, is_word_char);
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let prev_w = haystack[..start].chars().next_back().map_or(false, is_word_char);
        let next_w = haystack[end..].chars().next().map_or(false, is_word_char);
        if prev_w != first_w && next_w != last_w {
            return true;
        }
        // Advance one char and keep looking.
        from = start
            + haystack[start..]
                .chars()
                .next()
                .map_or(1, |c| c.len_utf8());
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// Collapse the same word repeated 4+ times in a row to one occurrence, and
/// the same 2-word phrase repeated 3+ times to one occurrence. Token-based
/// (the Swift version uses `\b\w+` regexes with backreferences, which the
/// `regex` crate doesn't support). Like the Swift `\b(\w+)(\s+\1\b){3,}`,
/// the *final* repetition may carry trailing punctuation ("so so so so." →
/// "so."), while punctuation between repeats ("yes, yes, yes") blocks the
/// collapse.
fn collapse_repeats(input: &str) -> String {
    /// Split a token into (word core, trailing punctuation). None if the
    /// token isn't "word + optional trailing non-word chars".
    fn core_trail(t: &str) -> Option<(&str, &str)> {
        let core_end = t
            .char_indices()
            .take_while(|(_, c)| is_word_char(*c))
            .last()
            .map(|(i, c)| i + c.len_utf8())?;
        let (core, trail) = t.split_at(core_end);
        if trail.chars().all(|c| !is_word_char(c)) {
            Some((core, trail))
        } else {
            None
        }
    }
    let eq = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();

    // Pass 1: single word repeated 4+ times. A run continues while each
    // repeat is punctuation-free; a repeat with trailing punctuation ends
    // the run (and its punctuation survives the collapse).
    let tokens: Vec<&str> = input.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if let Some((core, _)) = core_trail(tokens[i]) {
            let mut j = i;
            while core_trail(tokens[j]).unwrap().1.is_empty() && j + 1 < tokens.len() {
                match core_trail(tokens[j + 1]) {
                    Some((c2, _)) if eq(c2, core) => j += 1,
                    _ => break,
                }
            }
            if j - i + 1 >= 4 {
                let (_, last_trail) = core_trail(tokens[j]).unwrap();
                out.push(format!("{core}{last_trail}"));
                i = j + 1;
                continue;
            }
        }
        out.push(tokens[i].to_string());
        i += 1;
    }

    // Pass 2: 2-word phrase repeated 3+ times; only the very last token of
    // the run may carry trailing punctuation.
    let tokens = out;
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if i + 1 < tokens.len() {
            if let (Some((ca, ta)), Some((cb, _))) =
                (core_trail(&tokens[i]), core_trail(&tokens[i + 1]))
            {
                if ta.is_empty() {
                    // j = index of the first token of the last matched pair.
                    let mut j = i;
                    while core_trail(&tokens[j + 1]).unwrap().1.is_empty() && j + 3 < tokens.len()
                    {
                        match (core_trail(&tokens[j + 2]), core_trail(&tokens[j + 3])) {
                            (Some((c1, t1)), Some((c2, _)))
                                if t1.is_empty() && eq(c1, ca) && eq(c2, cb) =>
                            {
                                j += 2
                            }
                            _ => break,
                        }
                    }
                    if (j - i) / 2 + 1 >= 3 {
                        let (_, last_trail) = core_trail(&tokens[j + 1]).unwrap();
                        out.push(ca.to_string());
                        out.push(format!("{cb}{last_trail}"));
                        i = j + 2;
                        continue;
                    }
                }
            }
        }
        out.push(tokens[i].clone());
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
    fn collapses_repeats_with_trailing_punctuation() {
        // Whisper's loops usually end with a period — the Swift regex's \b
        // lets the final repeat carry punctuation, and so do we.
        assert_eq!(clean("so so so so."), "so.");
        assert_eq!(clean("thank you thank you thank you."), "thank you.");
    }

    #[test]
    fn keeps_swift_parity_for_punctuated_phrase_boundaries() {
        // "субтитры:" ends in ':' — Swift's \b can never match there, so a
        // line *containing* it (vs. exactly equal to it) must survive.
        assert_eq!(clean("субтитры: ив"), "субтитры: ив");
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
