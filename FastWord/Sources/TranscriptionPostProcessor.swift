import Foundation

/// Programmatic cleanup of common Whisper artefacts that survive the model's
/// own no-speech threshold:
///
/// * **Known hallucinations** — Whisper was trained on YouTube transcripts and
///   regularly emits closing-credit phrases ("Subtitles by Amara.org",
///   "Подписывайтесь на канал", "感谢观看") when the audio is silent or noisy.
/// * **Repetition loops** — the same token or short n-gram repeated many
///   times when the audio gets stuck on background noise.
/// * **Bracketed non-speech notes** — `[Music]`, `(applause)`, etc.
/// * **Pure-punctuation outputs** — segments that came out as just `.` or `…`.
/// * **Leading / trailing punctuation noise** — `". hello"` → `"hello"`.
/// * **Whitespace normalisation** — collapse runs of spaces and trim.
///
/// All passes are opt-out via `Options`. The processor never returns an empty
/// string when given non-empty input that wasn't *purely* a known artefact —
/// if filtering would empty the string and the input was non-trivial, the
/// original text is returned untouched so we never accidentally hide real
/// transcripts behind an aggressive filter.
enum TranscriptionPostProcessor {

    struct Options {
        var stripKnownHallucinations: Bool = true
        var collapseRepeats: Bool = true
        var stripBracketedNoise: Bool = true
        var normalizeWhitespace: Bool = true

        static let `default` = Options()
        static let off = Options(
            stripKnownHallucinations: false,
            collapseRepeats: false,
            stripBracketedNoise: false,
            normalizeWhitespace: false
        )
    }

    static func clean(_ input: String, options: Options = .default) -> String {
        let original = input
        var text = input

        if options.stripBracketedNoise {
            text = stripBracketedNoise(text)
        }
        if options.stripKnownHallucinations {
            text = stripKnownHallucinations(text)
        }
        if options.collapseRepeats {
            text = collapseRepeats(text)
        }
        if options.normalizeWhitespace {
            text = normalizeWhitespace(text)
        }
        text = stripLeadingTrailingNoise(text)

        // Guardrail: if cleaning emptied a non-trivial input, the original is
        // probably more useful than nothing — don't silently swallow it.
        let trimmedOriginal = original.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty && trimmedOriginal.count > 12 {
            return trimmedOriginal
        }
        return text
    }

    // MARK: - Passes

    /// Closing-credit / boilerplate phrases Whisper emits on silence or noise,
    /// observed in English, Russian and Mandarin transcripts. Matched
    /// case-insensitively as either the entire output or a phrase that
    /// appears alone on its own line.
    private static let hallucinationPhrases: [String] = [
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
    ]

    private static func stripKnownHallucinations(_ input: String) -> String {
        // Split into lines, drop any line that case-insensitively matches a
        // known phrase, then re-join. Within remaining lines, also strip
        // trailing matches separated by a newline-equivalent boundary.
        let lines = input.split(separator: "\n", omittingEmptySubsequences: false)
        let kept = lines.filter { line in
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if trimmed.isEmpty { return true }
            for phrase in hallucinationPhrases {
                if trimmed == phrase { return false }
                // Whole-phrase boundary match: line *contains* the phrase
                // surrounded by punctuation/whitespace/start/end.
                if let _ = trimmed.range(of: "\\b\(NSRegularExpression.escapedPattern(for: phrase))\\b",
                                         options: [.regularExpression]) {
                    if trimmed.count <= phrase.count + 4 {
                        // Phrase dominates the line; drop it.
                        return false
                    }
                }
            }
            return true
        }
        return kept.joined(separator: "\n")
    }

    /// Detect `\b(\w+)( \1\b){3,}` — the same word repeated 4 or more times
    /// in a row — and collapse to a single occurrence. Also handles short
    /// n-grams of 2-3 words repeated 3+ times.
    private static func collapseRepeats(_ input: String) -> String {
        var text = input

        // Repeat-of-single-word: collapse 4+ to 1.
        if let rx = try? NSRegularExpression(
            pattern: "\\b(\\w+)(\\s+\\1\\b){3,}",
            options: [.caseInsensitive]
        ) {
            let range = NSRange(text.startIndex..., in: text)
            text = rx.stringByReplacingMatches(in: text, range: range, withTemplate: "$1")
        }

        // Repeat-of-2-word-phrase: collapse 3+ to 1.
        if let rx = try? NSRegularExpression(
            pattern: "\\b(\\w+\\s+\\w+)(\\s+\\1){2,}",
            options: [.caseInsensitive]
        ) {
            let range = NSRange(text.startIndex..., in: text)
            text = rx.stringByReplacingMatches(in: text, range: range, withTemplate: "$1")
        }

        return text
    }

    /// Remove `[non-speech]` and `(non-speech)` markers like `[Music]`,
    /// `(applause)`, `[шум]`, that some Whisper builds emit. Conservative —
    /// only strips when contents look short and non-conversational.
    private static func stripBracketedNoise(_ input: String) -> String {
        var text = input
        if let rx = try? NSRegularExpression(
            pattern: "[\\[\\(][^\\[\\]\\(\\)]{1,30}[\\]\\)]",
            options: [.caseInsensitive]
        ) {
            let range = NSRange(text.startIndex..., in: text)
            text = rx.stringByReplacingMatches(in: text, range: range, withTemplate: "")
        }
        return text
    }

    private static func normalizeWhitespace(_ input: String) -> String {
        var text = input
        // Collapse runs of whitespace (including newlines mixed with spaces)
        // into a single space within paragraphs.
        if let rx = try? NSRegularExpression(pattern: "[ \\t]+", options: []) {
            let range = NSRange(text.startIndex..., in: text)
            text = rx.stringByReplacingMatches(in: text, range: range, withTemplate: " ")
        }
        // Collapse multiple newlines to a single one.
        if let rx = try? NSRegularExpression(pattern: "\\n{2,}", options: []) {
            let range = NSRange(text.startIndex..., in: text)
            text = rx.stringByReplacingMatches(in: text, range: range, withTemplate: "\n")
        }
        return text
    }

    private static func stripLeadingTrailingNoise(_ input: String) -> String {
        // Trim leading punctuation + whitespace ("…", ". hello" → "hello")
        // and trailing whitespace. We keep trailing punctuation since it's
        // often legitimate sentence-ending.
        var text = input
        let leadingNoise = CharacterSet.whitespacesAndNewlines
            .union(CharacterSet(charactersIn: ".,;:!?…\"'-—–»«()[]{}"))
        while let first = text.unicodeScalars.first, leadingNoise.contains(first) {
            text.removeFirst()
        }
        text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return text
    }
}
