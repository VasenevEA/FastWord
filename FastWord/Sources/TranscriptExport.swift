import Foundation

/// One timed piece of a file transcript, exactly as the sidecar reports it
/// in the `transcribe_file` reply (seconds from the start of the file).
struct TranscriptSegment: Codable, Hashable {
    let start: Double
    let end: Double
    let text: String
}

/// A finished "Transcribe File" job: the segments plus the metadata the
/// export formats and the result header need.
struct FileTranscript {
    let sourceName: String
    let sourceDuration: TimeInterval
    let modelName: String
    /// The language the job was run with ("" = auto-detect).
    let languageCode: String
    let createdAt: Date
    let processingTime: TimeInterval
    let segments: [TranscriptSegment]

    /// Paragraphed text without timestamps — what goes into history and
    /// what the plain-text export writes.
    var plainText: String { TranscriptExport.render(self, as: .txt) }

    /// Source file name without its extension, used for suggested export names.
    var baseName: String {
        let stem = (sourceName as NSString).deletingPathExtension
        return stem.isEmpty ? sourceName : stem
    }
}

enum TranscriptExportFormat: String, CaseIterable, Identifiable {
    case txt
    case srt
    case vtt
    case markdown

    var id: String { rawValue }

    var fileExtension: String {
        switch self {
        case .txt: return "txt"
        case .srt: return "srt"
        case .vtt: return "vtt"
        case .markdown: return "md"
        }
    }

    var displayName: String {
        switch self {
        case .txt: return NSLocalizedString("Plain Text (.txt)", comment: "")
        case .srt: return NSLocalizedString("SubRip Subtitles (.srt)", comment: "")
        case .vtt: return NSLocalizedString("WebVTT Subtitles (.vtt)", comment: "")
        case .markdown: return NSLocalizedString("Markdown (.md)", comment: "")
        }
    }
}

enum TranscriptExport {
    /// A pause at least this long between two segments starts a new
    /// paragraph. Whisper's segment boundaries alone are too frequent to be
    /// paragraphs; a real pause in speech is a much better signal.
    static let paragraphGap: Double = 2.0
    /// A subtitle cue shows at most two lines of this many columns (a
    /// Chinese, Japanese or Korean character takes two)…
    static let maxLineChars = 42
    /// …for at most this long, about what subtitle guidelines allow. Engine
    /// segments run longer (Whisper's to ~180 characters, GigaAM's to 20 s),
    /// so SRT/VTT split them into several cues.
    static let maxCueSeconds = 7.0
    /// A cue or a line breaks best after one of these.
    private static let sentenceMarks = ".!?…。！？"
    private static let clauseMarks = ",;:，、；："

    /// One subtitle cue: whole milliseconds, one or two lines of text.
    struct SubtitleCue: Equatable {
        let start: Int
        let end: Int
        let lines: [String]
    }

    static func render(_ transcript: FileTranscript, as format: TranscriptExportFormat) -> String {
        switch format {
        case .txt: return renderText(transcript.segments)
        case .srt: return renderSRT(transcript.segments)
        case .vtt: return renderVTT(transcript.segments)
        case .markdown: return renderMarkdown(transcript)
        }
    }

    // MARK: - Formats

    static func renderText(_ segments: [TranscriptSegment]) -> String {
        paragraphs(segments)
            .map { joinSegments($0.map(\.text)) }
            .joined(separator: "\n\n")
    }

    static func renderSRT(_ segments: [TranscriptSegment]) -> String {
        let cues = subtitleCues(segments).enumerated().map { index, cue in
            let range = "\(subtitleTimestamp(milliseconds: cue.start, separator: ",")) --> \(subtitleTimestamp(milliseconds: cue.end, separator: ","))"
            return "\(index + 1)\n\(range)\n\(cue.lines.joined(separator: "\n"))"
        }
        return cues.isEmpty ? "" : cues.joined(separator: "\n\n") + "\n"
    }

    static func renderVTT(_ segments: [TranscriptSegment]) -> String {
        var out = "WEBVTT\n"
        for cue in subtitleCues(segments) {
            out += "\n\(subtitleTimestamp(milliseconds: cue.start, separator: ".")) --> \(subtitleTimestamp(milliseconds: cue.end, separator: "."))\n"
            out += cue.lines.map(escapeVTT).joined(separator: "\n") + "\n"
        }
        return out
    }

    static func renderMarkdown(_ transcript: FileTranscript) -> String {
        // Paragraph stamps use one width for the whole document: a two-hour
        // recording reads better as 0:05:12 … 1:47:03 than as a mix of
        // 05:12 and 1:47:03.
        let lastEnd = transcript.segments.last?.end ?? 0
        let withHours = max(transcript.sourceDuration, lastEnd) >= 3600

        let date = DateFormatter.localizedString(from: transcript.createdAt, dateStyle: .medium, timeStyle: .short)
        let meta = [clock(transcript.sourceDuration, withHours: transcript.sourceDuration >= 3600),
                    transcript.modelName,
                    date]
            .filter { !$0.isEmpty }
            .joined(separator: " · ")

        var out = "# \(singleLine(transcript.sourceName))\n\n*\(meta)*\n"
        for para in paragraphs(transcript.segments) {
            guard let first = para.first else { continue }
            let text = joinSegments(para.map(\.text))
            out += "\n**[\(clock(first.start, withHours: withHours))]** \(text)\n"
        }
        return out
    }

    // MARK: - Timestamps

    /// Whole milliseconds, rounded once up front so the fields below can
    /// never disagree (the classic 59.9996 s → "00:00:59,1000" bug).
    static func milliseconds(_ seconds: Double) -> Int {
        guard seconds.isFinite, seconds > 0 else { return 0 }
        return Int((seconds * 1000).rounded())
    }

    /// "HH:MM:SS,mmm" (SRT, separator ",") or "HH:MM:SS.mmm" (VTT, ".").
    /// Hours keep growing past 99 rather than wrapping.
    static func subtitleTimestamp(_ seconds: Double, separator: Character) -> String {
        subtitleTimestamp(milliseconds: milliseconds(seconds), separator: separator)
    }

    static func subtitleTimestamp(milliseconds total: Int, separator: Character) -> String {
        let ms = total % 1000
        let s = (total / 1000) % 60
        let m = (total / 60_000) % 60
        let h = total / 3_600_000
        return String(format: "%02ld:%02ld:%02ld%@%03ld", h, m, s, String(separator), ms)
    }

    /// Reader-facing clock: "mm:ss", or "h:mm:ss" when `withHours`. Seconds
    /// are truncated — a paragraph that starts at 12.9 s starts "at 0:12".
    static func clock(_ seconds: Double, withHours: Bool) -> String {
        let total = seconds.isFinite && seconds > 0 ? Int(seconds.rounded(.down)) : 0
        let s = total % 60
        let m = (total / 60) % 60
        let h = total / 3600
        if withHours || h > 0 {
            return String(format: "%ld:%02ld:%02ld", h, m, s)
        }
        return String(format: "%02ld:%02ld", m, s)
    }

    // MARK: - Helpers

    /// Groups consecutive segments into paragraphs, breaking at pauses of
    /// `paragraphGap` seconds or more.
    static func paragraphs(_ segments: [TranscriptSegment]) -> [[TranscriptSegment]] {
        var out: [[TranscriptSegment]] = []
        var current: [TranscriptSegment] = []
        var previousEnd: Double?
        for seg in segments {
            if let prev = previousEnd, seg.start - prev >= paragraphGap, !current.isEmpty {
                out.append(current)
                current = []
            }
            current.append(seg)
            previousEnd = seg.end
        }
        if !current.isEmpty { out.append(current) }
        return out
    }

    /// Joins segment texts into running text. Segments come trimmed, so the
    /// separator is ours to pick: a space between words, but none next to
    /// Chinese or Japanese, which put no spaces between sentences (one after
    /// "。" is simply a typo there).
    static func joinSegments(_ texts: [String]) -> String {
        var out = ""
        for text in texts {
            if let last = out.unicodeScalars.last, let first = text.unicodeScalars.first,
               !isSpaceless(last), !isSpaceless(first) {
                out += " "
            }
            out += text
        }
        return out
    }

    /// Han, kana and CJK punctuation. Not Hangul: Korean spaces its words.
    private static func isSpaceless(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x3000...0x30FF,    // CJK punctuation (。、「」), hiragana, katakana
             0x31F0...0x31FF,    // katakana extensions
             0x3400...0x4DBF,    // ideographs, extension A
             0x4E00...0x9FFF,    // ideographs
             0xF900...0xFAFF,    // compatibility ideographs
             0xFF00...0xFFEF,    // full-width forms (，！？)
             0x20000...0x2FFFF:  // ideographs, extensions B–F
            return true
        default:
            return false
        }
    }

    /// Scripts that don't space their words: Chinese and Japanese, and Thai,
    /// Lao, Myanmar and Khmer — which do space their sentences, so segments
    /// in them still join with a space.
    private static func isUnspaced(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x0E00...0x0E7F,    // Thai
             0x0E80...0x0EFF,    // Lao
             0x1000...0x109F,    // Myanmar
             0x1780...0x17FF:    // Khmer
            return true
        default:
            return isSpaceless(scalar)
        }
    }

    /// East Asian wide and full-width characters: two columns on screen.
    private static func isWide(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x1100...0x115F,    // Hangul leading jamo
             0x2E80...0x303E,    // CJK radicals, symbols and punctuation
             0x3041...0x33FF,    // kana, bopomofo, Hangul jamo, CJK compatibility
             0x3400...0x4DBF,    // ideographs, extension A
             0x4E00...0x9FFF,    // ideographs
             0xA000...0xA4CF,    // Yi
             0xAC00...0xD7A3,    // Hangul syllables
             0xF900...0xFAFF,    // compatibility ideographs
             0xFE30...0xFE4F,    // CJK compatibility forms
             0xFF00...0xFF60,    // full-width forms (，！？)
             0xFFE0...0xFFE6,    // full-width signs
             0x20000...0x3FFFD:  // ideographs, extensions B–H
            return true
        default:
            return false
        }
    }

    /// The cues for `segments`: one per segment, or several where a segment
    /// is too long to read as one. Split between words, with time shared
    /// out by text width — speech runs at a fairly even pace within one
    /// segment.
    static func subtitleCues(_ segments: [TranscriptSegment]) -> [SubtitleCue] {
        let maxCueChars = 2 * maxLineChars
        var cues: [SubtitleCue] = []
        for seg in segments {
            let words = cueWords(singleLine(seg.text))
            guard !words.isEmpty else { continue }
            let (start, end) = cueBounds(seg)
            let length = textLength(words[...])
            let byLength = (length + maxCueChars - 1) / maxCueChars
            let byTime = Int((Double(end - start) / (maxCueSeconds * 1000)).rounded(.up))
            // Nothing to share out in a segment without duration.
            var count = end > start ? min(max(byLength, byTime, 1), words.count) : 1
            var pieces = split(words, into: count)
            // Unspaced text breaks between short words, so a cue whose lines
            // still run over can always make room with one more cue.
            while end > start, count < words.count,
                  pieces.contains(where: { !fits($0[...]) && breaksUnspaced($0[...]) }) {
                count += 1
                pieces = split(words, into: count)
            }
            var cueStart = start
            var done = 0
            for (index, piece) in pieces.enumerated() {
                done += textLength(piece[...]) + (piece[0].spaced ? 1 : 0)
                let cueEnd = index == count - 1 ? end : start + (end - start) * done / length
                cues.append(SubtitleCue(start: cueStart, end: cueEnd, lines: wrap(piece)))
                cueStart = cueEnd
            }
        }
        return cues
    }

    /// A piece of cue text that a cue or a line may start with: a word, or
    /// in a script that doesn't space its words, a dictionary word.
    private struct CueWord {
        let text: String
        /// Whether a space separates it from the word before.
        let spaced: Bool
        let width: Int

        init(_ text: Substring, spaced: Bool) {
            self.text = String(text)
            self.spaced = spaced
            width = TranscriptExport.width(text)
        }
    }

    private static func cueWords(_ text: String) -> [CueWord] {
        // No spaces next to Chinese or Japanese, as between segments.
        let text = joinSegments(text.split(separator: " ").map(String.init))
        var cuts: [String.Index] = []
        if text.unicodeScalars.contains(where: isUnspaced) {
            // The words in order: dictionary words of unspaced text, and runs
            // of other letters and digits, which stay whole ("iPhone15", "3.5").
            // Only unspaced runs go to the word breaker: a long Latin run among
            // them can throw its dictionary lookup off by a character.
            var words: [(range: Range<String.Index>, unspaced: Bool)] = []
            var i = text.startIndex
            while i < text.endIndex {
                let char = text[i]
                guard char.isLetter || char.isNumber else {
                    i = text.index(after: i)
                    continue
                }
                let unspaced = !isSpacedWordChar(char)
                let end = text[i...].firstIndex { isSpacedWordChar($0) == unspaced } ?? text.endIndex
                if unspaced {
                    text.enumerateSubstrings(in: i..<end, options: [.byWords, .substringNotRequired]) { _, word, _, _ in
                        if text[word].contains(where: { $0.isLetter || $0.isNumber }) {
                            words.append((word, true))
                        }
                    }
                } else {
                    words.append((i..<end, false))
                }
                i = end
            }
            for (previous, word) in zip(words, words.dropFirst()) {
                // A word after a space starts a piece of its own anyway.
                guard previous.unspaced || word.unspaced,
                      !text[previous.range.upperBound..<word.range.lowerBound].contains(" ") else { continue }
                // What lies between two words stays with the one before, so no
                // line starts with "，" or "。" — all but an opening bracket or
                // quote, which goes with the next.
                var cut = word.range.lowerBound
                while cut > previous.range.upperBound, opens(text[text.index(before: cut)]) {
                    cut = text.index(before: cut)
                }
                cuts.append(cut)
            }
        }
        var out: [CueWord] = []
        var next = cuts.makeIterator(), cut = next.next()
        for token in text.split(separator: " ") {
            var from = token.startIndex
            while let at = cut, at < token.endIndex {
                if at > from {
                    out.append(CueWord(text[from..<at], spaced: from == token.startIndex && !out.isEmpty))
                    from = at
                }
                cut = next.next()
            }
            out.append(CueWord(text[from..<token.endIndex], spaced: from == token.startIndex && !out.isEmpty))
        }
        return out
    }

    /// A letter or digit of a script that spaces its words.
    private static func isSpacedWordChar(_ char: Character) -> Bool {
        (char.isLetter || char.isNumber) && !char.unicodeScalars.contains(where: isUnspaced)
    }

    private static func opens(_ char: Character) -> Bool {
        guard let scalar = char.unicodeScalars.first else { return false }
        return [.openPunctuation, .initialPunctuation].contains(scalar.properties.generalCategory)
    }

    private static func width<S: StringProtocol>(_ text: S) -> Int {
        text.reduce(0) { $0 + ($1.unicodeScalars.first.map(isWide) == true ? 2 : 1) }
    }

    /// Width of `words` laid out on one line.
    private static func textLength(_ words: ArraySlice<CueWord>) -> Int {
        var length = 0
        for word in words {
            length += word.width + (word.spaced && length > 0 ? 1 : 0)
        }
        return length
    }

    private static func joined(_ words: ArraySlice<CueWord>) -> String {
        var out = ""
        for word in words {
            if word.spaced, !out.isEmpty { out += " " }
            out += word.text
        }
        return out
    }

    /// Whether `words` break anywhere without a space.
    private static func breaksUnspaced(_ words: ArraySlice<CueWord>) -> Bool {
        words.dropFirst().contains { !$0.spaced }
    }

    /// Whether `words` make one cue: two lines' worth of text and, where they
    /// break without spaces, two lines each within `maxLineChars`. Between
    /// spaced words, which break less often, holding every line to the limit
    /// would cost a cue for a line a character or two over.
    private static func fits(_ words: ArraySlice<CueWord>) -> Bool {
        let length = textLength(words)
        guard length <= 2 * maxLineChars else { return false }
        guard length > maxLineChars, breaksUnspaced(words) else { return true }
        return wrap(Array(words)).allSatisfy { width($0) <= maxLineChars }
    }

    /// `words` in `count` runs of about equal length, each short enough for
    /// one cue where the words allow it, breaking after a sentence or a
    /// clause when one ends near the ideal spot.
    private static func split(_ words: [CueWord], into count: Int) -> [[CueWord]] {
        let maxCueChars = 2 * maxLineChars
        var pieces: [[CueWord]] = []
        var from = 0
        for left in stride(from: count, to: 1, by: -1) {
            let rest = textLength(words[from...])
            let target = Double(rest) / Double(left)
            var best = from + 1
            var bestScore = Double.infinity
            // Every later run keeps at least one word.
            for to in (from + 1)...(words.count - left + 1) {
                let length = textLength(words[from..<to])
                var score = abs(Double(length) - target)
                if let mark = words[to - 1].text.last {
                    if sentenceMarks.contains(mark) {
                        score -= target * 0.3
                    } else if clauseMarks.contains(mark) {
                        score -= target * 0.15
                    }
                }
                if !fits(words[from..<to]) || textLength(words[to...]) > (left - 1) * maxCueChars {
                    score += 10_000
                }
                if score < bestScore {
                    bestScore = score
                    best = to
                }
            }
            pieces.append(Array(words[from..<best]))
            from = best
        }
        pieces.append(Array(words[from...]))
        return pieces
    }

    /// A cue's text on one line, or on two of about equal width when one
    /// would run past `maxLineChars`.
    private static func wrap(_ words: [CueWord]) -> [String] {
        guard textLength(words[...]) > maxLineChars, words.count > 1 else { return [joined(words[...])] }
        // The longer line as short as possible. Between unspaced words,
        // which give no other hint of the phrasing, a break after a sentence
        // or a clause is worth a few columns more.
        func score(_ cut: Int) -> Int {
            let longer = max(textLength(words[..<cut]), textLength(words[cut...]))
            guard !words[cut].spaced, longer <= maxLineChars, let mark = words[cut - 1].text.last else { return longer }
            return longer - (sentenceMarks.contains(mark) ? 10 : clauseMarks.contains(mark) ? 6 : 0)
        }
        let cut = (1..<words.count).min { score($0) < score($1) } ?? 1
        return [joined(words[..<cut]), joined(words[cut...])]
    }

    /// Cue bounds in ms, never inverted even if rounding or a sloppy
    /// segment would make the end precede the start.
    private static func cueBounds(_ seg: TranscriptSegment) -> (Int, Int) {
        let start = milliseconds(seg.start)
        return (start, max(start, milliseconds(seg.end)))
    }

    /// Collapses line breaks into spaces: cue lines are laid out by `wrap`,
    /// and a stray blank line would end a subtitle cue (or a Markdown
    /// heading) early.
    private static func singleLine(_ text: String) -> String {
        text.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }

    /// WebVTT cue text is parsed as markup: a bare "<" or "&" would be read
    /// as a tag or entity.
    private static func escapeVTT(_ text: String) -> String {
        text.replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
    }
}
