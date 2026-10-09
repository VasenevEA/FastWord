import AppKit
import Foundation

/// The Speakers picker of the Transcribe File window.
enum SpeakerMode: Hashable {
    case off
    /// Speaker detection guesses how many people there are.
    case auto
    /// The user knows how many people speak — the most reliable setting.
    case exact(Int)

    static let exactCounts = 2...6

    var isOn: Bool { self != .off }

    var displayName: String {
        switch self {
        case .off: return NSLocalizedString("speakers.off", comment: "")
        case .auto: return NSLocalizedString("speakers.auto", comment: "")
        case .exact(let count): return count.formatted()
        }
    }
}

/// Names and merges the user applied to the detected speakers. Segments keep
/// their raw speaker ids; the view, Copy, exports and History all read them
/// through this layer, so undo and reset only swap it out.
struct SpeakerLabels: Hashable {
    /// Custom names by speaker id; a speaker without one is "Speaker N".
    var names: [Int: String] = [:]
    /// Speakers merged into another one: id → the id it was merged into.
    var mergedInto: [Int: Int] = [:]

    /// Longer names would crowd the transcript and every subtitle cue.
    static let maxNameLength = 60

    var isEmpty: Bool { names.isEmpty && mergedInto.isEmpty }

    /// The id a speaker shows up as once merges are applied. Ids are not
    /// renumbered: merging Speaker 3 into Speaker 1 leaves a Speaker 2.
    func effective(_ id: Int) -> Int {
        var current = id
        var seen: Set<Int> = [id]
        while let next = mergedInto[current], seen.insert(next).inserted {
            current = next
        }
        return current
    }

    func name(of id: Int) -> String {
        let id = effective(id)
        return names[id] ?? Self.defaultName(id)
    }

    /// "Speaker 1" for id 0: ids count from the first voice heard.
    static func defaultName(_ id: Int) -> String {
        String(format: NSLocalizedString("Speaker %ld", comment: ""), id + 1)
    }

    /// An empty name — or the default one typed out — goes back to "Speaker N".
    mutating func rename(_ id: Int, to name: String) {
        let id = effective(id)
        let cleaned = Self.clean(name)
        names[id] = cleaned.isEmpty || cleaned == Self.defaultName(id) ? nil : cleaned
    }

    /// `id`'s lines go to `target`, under `target`'s name and color.
    mutating func merge(_ id: Int, into target: Int) {
        let from = effective(id)
        let to = effective(target)
        guard from != to else { return }
        mergedInto[from] = to
    }

    /// One line, no runs of spaces, at most `maxNameLength` characters: a name
    /// is printed inline in the text, as a subtitle prefix and as a WebVTT
    /// voice, where a line break would end the cue.
    static func clean(_ name: String) -> String {
        let words = name.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        return String(words.joined(separator: " ").prefix(maxNameLength))
    }
}

/// One speaker as the result view lists them, after merges.
struct SpeakerSummary: Identifiable, Equatable {
    let id: Int
    let name: String
    let seconds: Double
    /// Share of all the talk time that has a speaker.
    let share: Double
    /// One of their longer lines, to tell who it is.
    let sample: String?
}

extension FileTranscript {
    /// The job ran speaker detection and it produced labels.
    var hasSpeakers: Bool { segments.contains { $0.speaker != nil } }

    /// A segment's speaker once merges are applied.
    func speaker(of segment: TranscriptSegment) -> Int? {
        segment.speaker.map(labels.effective)
    }

    var speakerCount: Int { Set(segments.compactMap(speaker(of:))).count }

    /// Names show up in the text and the exports only with two or more
    /// speakers. With one, everything reads exactly as without speaker
    /// detection.
    var showsSpeakers: Bool { speakerCount >= 2 }

    /// Speakers by id, with their talk time. Counted from the segments
    /// rather than taken from the engine, so the numbers always match the
    /// text shown (cleanup may have dropped a segment or two).
    var speakers: [SpeakerSummary] {
        var seconds: [Int: Double] = [:]
        var samples: [Int: String] = [:]
        for seg in segments {
            guard let id = speaker(of: seg) else { continue }
            seconds[id, default: 0] += max(0, seg.end - seg.start)
            // The first line long enough to recognize someone by; failing
            // that, their longest one.
            let current = samples[id]
            if current.map({ $0.count < Self.sampleLength && seg.text.count > $0.count }) ?? true {
                samples[id] = seg.text
            }
        }
        let total = seconds.values.reduce(0, +)
        return seconds.keys.sorted().map { id in
            SpeakerSummary(
                id: id,
                name: labels.name(of: id),
                seconds: seconds[id] ?? 0,
                share: total > 0 ? (seconds[id] ?? 0) / total : 0,
                sample: samples[id].map { $0.count > 120 ? String($0.prefix(120)) + "…" : $0 })
        }
    }

    private static let sampleLength = 40
}

/// Why speaker detection failed, as the result view words it. The sidecar
/// reports it in English after its own "Speaker detection failed: "
/// ("…: the speaker detector crashed (signal 10)."); that is put in the
/// user's language here, and the tooltip keeps the original for details.
enum SpeakerWarning {
    static func reason(_ warning: String) -> String {
        let text = warning.trimmingCharacters(in: .whitespacesAndNewlines)
        let prefix = "Speaker detection failed: "
        // Not the sidecar's: FileTranscriber's own, already localized.
        guard text.hasPrefix(prefix) else { return text }
        let reason = text.dropFirst(prefix.count)
        if reason.contains("speaker model") || reason.contains("embedding model") {
            return NSLocalizedString("speakers.warning.models", comment: "")
        }
        if reason.hasPrefix("speaker detection timed out") {
            return NSLocalizedString("speakers.warning.timeout", comment: "")
        }
        if reason.hasPrefix("could not start the speaker detector") {
            return NSLocalizedString("speakers.warning.start", comment: "")
        }
        // Crashed, failed, stopped responding, or an error inside it.
        return NSLocalizedString("speakers.warning.crashed", comment: "")
    }
}

/// Eight system colors, repeating after eight. The color only helps tell
/// speakers apart at a glance; the name carries the meaning.
enum SpeakerPalette {
    static let colors: [NSColor] = [
        .systemBlue, .systemOrange, .systemGreen, .systemPurple,
        .systemPink, .systemTeal, .systemBrown, .systemIndigo,
    ]

    static func color(for id: Int) -> NSColor {
        colors[abs(id) % colors.count]
    }
}
