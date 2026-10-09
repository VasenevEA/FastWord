// Joins a file transcript with speaker diarization.
//
// Diarization says who spoke when; the recognizer says what was said, in
// segments that end at pauses, not where the speaker changes — a quick
// "Yes. And then…" reply often shares a segment with the question before it.
// So speakers are attributed per word, from word timings (whisper's DTW
// token times, GigaAM's CTC token times), and a segment with more than one
// speaker in it is cut between the words where the speaker changes. The
// text itself is never rewritten: every piece is a slice of the segment's
// own text, so punctuation and spacing stay as the recognizer wrote them.

use serde::{Deserialize, Serialize};

use crate::Segment;

/// One word of a segment, located in time and in the segment's text.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    /// Byte offset of the word in the segment text. The text from here to
    /// the next word's offset (its trailing space and punctuation included)
    /// belongs to this word.
    pub at: usize,
}

/// A stretch of time one speaker talks in, from diarization. Turns of
/// different speakers may overlap (crosstalk, a "mm-hmm" over a monologue).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: u32,
}

/// Total talk time of one speaker in the final transcript.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeakerTime {
    pub id: u32,
    pub seconds: f64,
}

/// Gives every segment exactly one speaker, cutting segments where the
/// speaker changes between two of their words, and lists each speaker's
/// talk time. Speaker ids are renumbered 0, 1, … in order of first
/// appearance in the transcript; a diarized speaker no word was attributed
/// to does not appear. Without any turns (diarization heard nobody), the
/// whole transcript is speaker 0. Segments must be sorted by start.
pub fn attribute(segments: Vec<Segment>, turns: &[Turn]) -> (Vec<Segment>, Vec<SpeakerTime>) {
    let timeline = Timeline::new(turns);
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    for seg in segments {
        let words = usable_words(&seg);
        let speakers = timeline.speakers_of(&words);
        split_by_speaker(seg, &words, &speakers, &mut out);
    }

    // Renumber by first appearance and add up talk time.
    let mut ids: Vec<u32> = Vec::new();
    let mut seconds: Vec<f64> = Vec::new();
    for seg in &mut out {
        let raw = seg.speaker.unwrap_or(0);
        let id = match ids.iter().position(|&r| r == raw) {
            Some(i) => i,
            None => {
                ids.push(raw);
                seconds.push(0.0);
                ids.len() - 1
            }
        };
        seconds[id] += seg.end - seg.start;
        seg.speaker = Some(id as u32);
    }
    let speakers = seconds
        .into_iter()
        .enumerate()
        .map(|(id, s)| SpeakerTime {
            id: id as u32,
            seconds: (s * 1000.0).round() / 1000.0,
        })
        .collect();
    (out, speakers)
}

/// The segment's words if they can be trusted to slice its text — in order,
/// on character boundaries, inside the text — else the whole segment as one
/// word. The first word always starts the text, so no text is orphaned.
///
/// Word times are kept inside the segment: its bounds were fitted to the
/// voice in the audio, while a recognizer's last word of a segment tends to
/// run on into the pause (or the next speaker's turn) after it.
fn usable_words(seg: &Segment) -> Vec<Word> {
    let text = &seg.text;
    let ok = !seg.words.is_empty()
        && seg.words.windows(2).all(|w| w[0].at < w[1].at)
        && seg.words.iter().all(|w| {
            w.at < text.len()
                && text.is_char_boundary(w.at)
                && w.start.is_finite()
                && w.end.is_finite()
        });
    if !ok {
        return vec![Word {
            start: seg.start,
            end: seg.end,
            at: 0,
        }];
    }
    let mut words = seg.words.clone();
    words[0].at = 0;
    for w in &mut words {
        w.start = w.start.clamp(seg.start, seg.end);
        w.end = w.end.clamp(w.start, seg.end);
    }
    words
}

/// Cuts `seg` into runs of words with the same speaker and appends them to
/// `out`. A piece starts at its first word (the first piece where the
/// segment does) and ends where the next one starts.
fn split_by_speaker(
    seg: Segment,
    words: &[Word],
    speakers: &[Option<u32>],
    out: &mut Vec<Segment>,
) {
    let mut pieces: Vec<(usize, u32)> = Vec::new(); // (first word, speaker)
    for (i, spk) in speakers.iter().enumerate() {
        let spk = spk.unwrap_or(0);
        if pieces.last().is_none_or(|&(_, s)| s != spk) {
            pieces.push((i, spk));
        }
    }
    // A piece whose text is blank (only possible with odd recognizer
    // output) goes to the speaker before it rather than standing alone.
    let slice = |from: usize, to: usize| {
        let a = words[from].at;
        let b = words.get(to).map_or(seg.text.len(), |w| w.at);
        seg.text[a..b].trim()
    };
    let mut kept: Vec<(usize, u32)> = Vec::with_capacity(pieces.len());
    for (k, &(first, spk)) in pieces.iter().enumerate() {
        let next = pieces.get(k + 1).map_or(words.len(), |p| p.0);
        if slice(first, next).is_empty() && !kept.is_empty() {
            continue;
        }
        if kept.last().is_some_and(|&(_, s)| s == spk) {
            continue;
        }
        kept.push((first, spk));
    }
    if kept.len() <= 1 {
        let spk = kept.first().map_or(0, |p| p.1);
        out.push(Segment {
            speaker: Some(spk),
            words: Vec::new(),
            ..seg
        });
        return;
    }

    let mut start = seg.start;
    for (k, &(first, spk)) in kept.iter().enumerate() {
        let next = kept.get(k + 1).map_or(words.len(), |p| p.0);
        let end = match words.get(next) {
            Some(w) => w.start.clamp(start, seg.end),
            None => seg.end,
        };
        let text = slice(first, next);
        out.push(Segment {
            start,
            end,
            text: text.to_string(),
            speaker: Some(spk),
            words: Vec::new(),
        });
        start = end;
    }
}

/// Diarization turns as a sequence of non-overlapping spans, in order, each
/// with the set of speakers talking in it.
struct Timeline {
    spans: Vec<(f64, f64, Vec<u32>)>,
    turns: Vec<Turn>,
}

impl Timeline {
    fn new(turns: &[Turn]) -> Self {
        let turns: Vec<Turn> = turns
            .iter()
            .copied()
            .filter(|t| t.end > t.start && t.start.is_finite() && t.end.is_finite())
            .collect();
        let mut cuts: Vec<f64> = turns.iter().flat_map(|t| [t.start, t.end]).collect();
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let mut spans = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut who: Vec<u32> = turns
                .iter()
                .filter(|t| t.start <= a && t.end >= b)
                .map(|t| t.speaker)
                .collect();
            who.sort_unstable();
            who.dedup();
            if !who.is_empty() {
                spans.push((a, b, who));
            }
        }
        Self { spans, turns }
    }

    /// The speaker of each word, in order:
    ///
    /// - the speaker talking alone for most of the word;
    /// - for a word entirely inside crosstalk, whichever of the speakers
    ///   talking said the words around it — a short turn nested in someone
    ///   else's phrase (a back-channel the recognizer usually drops anyway)
    ///   does not take that phrase's words — else the one with the longest
    ///   turn there;
    /// - for a word in no turn at all (a pause diarization left out, word
    ///   times a little off), the speaker of the nearest turn.
    ///
    /// `None` only without any turns.
    fn speakers_of(&self, words: &[Word]) -> Vec<Option<u32>> {
        if self.turns.is_empty() {
            return vec![None; words.len()];
        }
        enum Pick {
            Sure(u32),
            Crosstalk(Vec<u32>),
        }
        let picks: Vec<Pick> = words
            .iter()
            .map(|w| {
                let (a, b) = word_span(w);
                let mut alone: Vec<(u32, f64)> = Vec::new();
                let mut shared: Vec<u32> = Vec::new();
                let first = self.spans.partition_point(|(_, e, _)| *e <= a);
                for (s, e, who) in self.spans[first..].iter().take_while(|(s, _, _)| *s < b) {
                    let overlap = b.min(*e) - a.max(*s);
                    if overlap <= 0.0 {
                        continue;
                    }
                    if who.len() == 1 {
                        match alone.iter_mut().find(|(spk, _)| *spk == who[0]) {
                            Some((_, t)) => *t += overlap,
                            None => alone.push((who[0], overlap)),
                        }
                    } else {
                        shared.extend(who);
                    }
                }
                if let Some(&(spk, _)) = alone
                    .iter()
                    .max_by(|x, y| x.1.total_cmp(&y.1).then(y.0.cmp(&x.0)))
                {
                    return Pick::Sure(spk);
                }
                if !shared.is_empty() {
                    shared.sort_unstable();
                    shared.dedup();
                    return Pick::Crosstalk(shared);
                }
                Pick::Sure(self.nearest(a, b))
            })
            .collect();

        let sure = |i: usize| match picks[i] {
            Pick::Sure(s) => Some(s),
            Pick::Crosstalk(_) => None,
        };
        (0..words.len())
            .map(|i| match &picks[i] {
                Pick::Sure(s) => Some(*s),
                Pick::Crosstalk(who) => {
                    let before = (0..i).rev().find_map(sure);
                    let after = (i + 1..words.len()).find_map(sure);
                    let context = [before, after]
                        .into_iter()
                        .flatten()
                        .find(|s| who.contains(s));
                    Some(context.unwrap_or_else(|| {
                        let (a, b) = word_span(&words[i]);
                        self.longest_turn_among(who, a, b)
                    }))
                }
            })
            .collect()
    }

    /// Speaker of the turn closest to `[a, b]`.
    fn nearest(&self, a: f64, b: f64) -> u32 {
        let gap = |t: &Turn| (t.start - b).max(a - t.end).max(0.0);
        self.turns
            .iter()
            .min_by(|x, y| gap(x).total_cmp(&gap(y)))
            .map_or(0, |t| t.speaker)
    }

    /// Of `who`, the speaker with the longest turn overlapping `[a, b]`.
    fn longest_turn_among(&self, who: &[u32], a: f64, b: f64) -> u32 {
        self.turns
            .iter()
            .filter(|t| who.contains(&t.speaker) && t.start < b && t.end > a)
            .max_by(|x, y| (x.end - x.start).total_cmp(&(y.end - y.start)))
            .map_or(who[0], |t| t.speaker)
    }
}

/// A word's time span for overlap tests. A zero-length word (a timestamp
/// without a duration) still needs some extent to land in a turn.
fn word_span(w: &Word) -> (f64, f64) {
    if w.end - w.start >= 0.02 {
        (w.start, w.end)
    } else {
        (w.start - 0.01, w.start.max(w.end) + 0.01)
    }
}

/// Words of a whisper segment from its text tokens: (token bytes, DTW time
/// in centiseconds, or negative when whisper has none). Returns the
/// segment text — the tokens joined, trimmed — and its words, each starting
/// `shift` seconds from its first token's DTW time and ending where the next
/// word starts (the last at `end`).
///
/// A word starts at a token that begins with a space, or — for scripts
/// written without spaces (Chinese, Japanese, Thai, …) — at every token
/// that starts with a letter of such a script. `None` when the tokens lack
/// DTW times or are not valid UTF-8 together.
pub fn whisper_words(tokens: &[(&[u8], i64)], shift: f64, end: f64) -> Option<(String, Vec<Word>)> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut token_at: Vec<(usize, f64)> = Vec::with_capacity(tokens.len());
    for &(tok, t_dtw) in tokens {
        if tok.is_empty() {
            continue;
        }
        if t_dtw < 0 {
            return None;
        }
        token_at.push((bytes.len(), t_dtw as f64 / 100.0 + shift));
        bytes.extend_from_slice(tok);
    }
    // Single tokens may be fragments of one character (whisper splits rare
    // characters into byte pieces); only the joined text must be valid.
    let raw = String::from_utf8(bytes).ok()?;
    let lead = raw.len() - raw.trim_start().len();
    let text = raw.trim();
    let mut words: Vec<Word> = Vec::new();
    for (at, start) in token_at {
        if !raw.is_char_boundary(at) {
            continue;
        }
        let rest = &raw[at..];
        let opens = words.is_empty()
            || rest.starts_with(' ')
            || rest.chars().next().is_some_and(is_unspaced_letter);
        // A word starts after its leading space; offsets move with the
        // trimmed start of the text, and a word made only of trailing space
        // is gone with it.
        let at = (at + rest.len() - rest.trim_start().len()).saturating_sub(lead);
        if !opens || at >= text.len() || words.last().is_some_and(|w| w.at >= at) {
            continue;
        }
        words.push(Word {
            start,
            end: start,
            at,
        });
    }
    let n = words.len();
    for i in 0..n {
        let next = if i + 1 < n { words[i + 1].start } else { end };
        words[i].end = next.max(words[i].start);
    }
    Some((text.to_string(), words))
}

/// Letters of scripts written without spaces between words.
fn is_unspaced_letter(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0EFF   // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x3040..=0x30FF // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF)
}

/// Words of a CTC decode (GigaAM): character tokens with start times in
/// seconds from `offset`, a space token between words. Returns the text —
/// words joined by single spaces — and the words; a word ends where its
/// last token does (`token_secs` after its start), or at the next word's
/// start if that comes first. `None` when tokens and times do not match up.
pub fn ctc_words(
    tokens: &[String],
    times: &[f32],
    offset: f64,
    token_secs: f64,
) -> Option<(String, Vec<Word>)> {
    if tokens.len() != times.len() {
        return None;
    }
    let mut text = String::new();
    let mut words: Vec<Word> = Vec::new();
    let mut in_word = false;
    for (tok, &t) in tokens.iter().zip(times) {
        let t = offset + t as f64;
        // BPE vocabularies mark a word start with "▁" instead of a space.
        let (space_before, body) = match tok.strip_prefix('▁') {
            Some(rest) => (true, rest),
            None => (false, tok.as_str()),
        };
        let body_trimmed = body.trim();
        if space_before || body.starts_with(char::is_whitespace) {
            in_word = false;
        }
        if body_trimmed.is_empty() {
            in_word = false;
            continue;
        }
        if !in_word {
            if !text.is_empty() {
                text.push(' ');
            }
            words.push(Word {
                start: t,
                end: t + token_secs,
                at: text.len(),
            });
            in_word = true;
        }
        text.push_str(body_trimmed);
        let w = words.last_mut().expect("a word was just opened");
        w.end = w.end.max(t + token_secs);
        if body.ends_with(char::is_whitespace) {
            in_word = false;
        }
    }
    for i in 1..words.len() {
        let next = words[i].start;
        let prev = &mut words[i - 1];
        prev.end = prev.end.min(next).max(prev.start);
    }
    Some((text, words))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str, words: &[(f64, f64, usize)]) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            speaker: None,
            words: words
                .iter()
                .map(|&(start, end, at)| Word { start, end, at })
                .collect(),
        }
    }

    fn turn(start: f64, end: f64, speaker: u32) -> Turn {
        Turn {
            start,
            end,
            speaker,
        }
    }

    /// Words at one second each, starting at `t0`, located in `text`.
    fn words_of(text: &str, t0: f64) -> Vec<(f64, f64, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        for (i, w) in text.split(' ').enumerate() {
            out.push((t0 + i as f64, t0 + i as f64 + 1.0, at));
            at += w.len() + 1;
        }
        out
    }

    fn summary(segs: &[Segment]) -> Vec<(f64, f64, &str, u32)> {
        segs.iter()
            .map(|s| (s.start, s.end, s.text.as_str(), s.speaker.unwrap()))
            .collect()
    }

    #[test]
    fn splits_a_segment_at_the_speaker_change() {
        let text = "How are you? Fine, thanks.";
        let s = seg(0.0, 5.0, text, &words_of(text, 0.0));
        let (out, speakers) = attribute(vec![s], &[turn(0.0, 3.0, 7), turn(3.0, 5.0, 2)]);
        assert_eq!(
            summary(&out),
            vec![
                (0.0, 3.0, "How are you?", 0),
                (3.0, 5.0, "Fine, thanks.", 1)
            ]
        );
        assert_eq!(
            speakers,
            vec![
                SpeakerTime {
                    id: 0,
                    seconds: 3.0
                },
                SpeakerTime {
                    id: 1,
                    seconds: 2.0
                }
            ]
        );
        // The text is untouched: pieces joined give it back.
        let joined = out
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, text);
    }

    #[test]
    fn speakers_are_numbered_by_first_appearance() {
        let a = seg(0.0, 2.0, "one two", &words_of("one two", 0.0));
        let b = seg(3.0, 5.0, "three four", &words_of("three four", 3.0));
        let c = seg(6.0, 8.0, "five six", &words_of("five six", 6.0));
        let turns = [turn(0.0, 2.0, 5), turn(3.0, 5.0, 1), turn(6.0, 8.0, 5)];
        let (out, speakers) = attribute(vec![a, b, c], &turns);
        assert_eq!(
            out.iter().map(|s| s.speaker.unwrap()).collect::<Vec<_>>(),
            vec![0, 1, 0]
        );
        assert_eq!(speakers.len(), 2);
        assert_eq!(
            speakers[0],
            SpeakerTime {
                id: 0,
                seconds: 4.0
            }
        );
        // A diarized speaker nobody's words went to is left out.
        let (_, speakers) = attribute(
            vec![seg(0.0, 2.0, "one two", &words_of("one two", 0.0))],
            &[turn(0.0, 2.0, 3), turn(9.0, 10.0, 4)],
        );
        assert_eq!(
            speakers,
            vec![SpeakerTime {
                id: 0,
                seconds: 2.0
            }]
        );
    }

    #[test]
    fn word_goes_to_the_speaker_with_most_overlap() {
        // "b" (1–2 s) is 0.7 s speaker 1, 0.3 s speaker 0.
        let s = seg(0.0, 3.0, "a b c", &words_of("a b c", 0.0));
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.3, 0), turn(1.3, 3.0, 1)]);
        assert_eq!(
            summary(&out),
            vec![(0.0, 1.0, "a", 0), (1.0, 3.0, "b c", 1)]
        );
    }

    #[test]
    fn nested_back_channel_does_not_take_the_phrase() {
        // Speaker 0 talks 0–10 s; speaker 1's "mm-hmm" at 4–5 s lies inside.
        // The word spoken at 4–5 s is entirely crosstalk: it stays with the
        // speaker of the words around it.
        let text = "one two three four five six seven eight nine ten";
        let s = seg(0.0, 10.0, text, &words_of(text, 0.0));
        let (out, speakers) = attribute(vec![s], &[turn(0.0, 10.0, 0), turn(4.0, 5.0, 1)]);
        assert_eq!(summary(&out), vec![(0.0, 10.0, text, 0)]);
        assert_eq!(speakers.len(), 1);
    }

    #[test]
    fn exclusive_short_turn_inside_a_phrase_is_cut_out() {
        // Diarization gives 4–5 s to speaker 1 alone: that word is theirs.
        let text = "one two three four five six";
        let s = seg(0.0, 6.0, text, &words_of(text, 0.0));
        let turns = [turn(0.0, 4.0, 0), turn(4.0, 5.0, 1), turn(5.0, 6.0, 0)];
        let (out, speakers) = attribute(vec![s], &turns);
        assert_eq!(
            summary(&out),
            vec![
                (0.0, 4.0, "one two three four", 0),
                (4.0, 5.0, "five", 1),
                (5.0, 6.0, "six", 0)
            ]
        );
        assert_eq!(speakers[0].seconds, 5.0);
        assert_eq!(speakers[1].seconds, 1.0);
    }

    #[test]
    fn crosstalk_without_context_goes_to_the_longest_turn() {
        let s = seg(4.0, 5.0, "mm-hmm", &[(4.2, 4.8, 0)]);
        let (out, _) = attribute(vec![s], &[turn(0.0, 10.0, 3), turn(4.0, 5.0, 1)]);
        assert_eq!(out[0].speaker, Some(0));
        // Raw speaker 3 (the long turn) became id 0.
        let s = seg(4.0, 5.0, "mm-hmm", &[(4.2, 4.8, 0)]);
        let turns = [turn(0.0, 1.0, 1), turn(0.0, 10.0, 3), turn(4.0, 5.0, 1)];
        let (out, _) = attribute(vec![s], &turns);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn words_outside_every_turn_take_the_nearest() {
        let text = "a b c d";
        let s = seg(0.0, 4.0, text, &words_of(text, 0.0));
        // Turns leave 1.5–2.5 s out; "b" (1–2) is half in speaker 0's turn,
        // "c" (2–3) half in speaker 1's.
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.5, 0), turn(2.5, 4.0, 1)]);
        assert_eq!(
            summary(&out),
            vec![(0.0, 2.0, "a b", 0), (2.0, 4.0, "c d", 1)]
        );
        // A segment entirely in a gap.
        let s = seg(5.0, 6.0, "late", &[(5.0, 6.0, 0)]);
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.0, 0), turn(6.5, 9.0, 1)]);
        assert_eq!(out[0].speaker, Some(0)); // raw 1 → first appearance → 0
    }

    #[test]
    fn last_word_running_past_the_segment_is_clamped() {
        // The recognizer gives "week?" until 11.3 s, into the next speaker's
        // turn; the segment (fitted to the voice) ends at 10.4 s.
        let s = seg(8.0, 10.4, "next week?", &[(8.0, 10.2, 0), (10.2, 11.3, 5)]);
        let (out, _) = attribute(vec![s], &[turn(7.9, 10.35, 0), turn(10.9, 14.0, 1)]);
        assert_eq!(summary(&out), vec![(8.0, 10.4, "next week?", 0)]);
    }

    #[test]
    fn segments_without_words_are_attributed_whole() {
        let s = seg(0.0, 4.0, "no word timing here", &[]);
        let (out, speakers) = attribute(vec![s], &[turn(0.0, 1.0, 0), turn(1.0, 4.0, 1)]);
        assert_eq!(summary(&out), vec![(0.0, 4.0, "no word timing here", 0)]);
        assert_eq!(
            speakers,
            vec![SpeakerTime {
                id: 0,
                seconds: 4.0
            }]
        );
        // Broken offsets (out of order, past the end) fall back the same way.
        let s = seg(0.0, 2.0, "ab cd", &[(0.0, 1.0, 3), (1.0, 2.0, 0)]);
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.0, 0), turn(1.0, 2.0, 1)]);
        assert_eq!(out.len(), 1);
        let s = seg(0.0, 2.0, "ab", &[(0.0, 1.0, 0), (1.0, 2.0, 9)]);
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.0, 0), turn(1.0, 2.0, 1)]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn no_turns_means_one_speaker() {
        let a = seg(0.0, 2.0, "one two", &words_of("one two", 0.0));
        let b = seg(3.0, 4.0, "three", &[]);
        let (out, speakers) = attribute(vec![a, b], &[]);
        assert!(out.iter().all(|s| s.speaker == Some(0)));
        assert_eq!(out.len(), 2);
        assert_eq!(
            speakers,
            vec![SpeakerTime {
                id: 0,
                seconds: 3.0
            }]
        );
        let (out, speakers) = attribute(Vec::new(), &[turn(0.0, 1.0, 0)]);
        assert!(out.is_empty() && speakers.is_empty());
    }

    #[test]
    fn nothing_is_dropped_and_times_stay_ordered() {
        // Alternating speakers every word, word times running a little past
        // the segment and backwards at one point.
        let text = "a b c d e";
        let s = seg(
            1.0,
            5.0,
            text,
            &[
                (0.8, 1.9, 0),
                (1.9, 3.1, 2),
                (2.9, 3.5, 4),
                (3.5, 4.4, 6),
                (4.4, 5.6, 8),
            ],
        );
        let turns = [
            turn(0.0, 1.9, 0),
            turn(1.9, 3.0, 1),
            turn(3.0, 3.5, 0),
            turn(3.5, 4.4, 1),
            turn(4.4, 6.0, 0),
        ];
        let (out, _) = attribute(vec![s], &turns);
        let joined = out
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, text);
        assert_eq!(out.first().unwrap().start, 1.0);
        assert_eq!(out.last().unwrap().end, 5.0);
        for w in out.windows(2) {
            assert!(w[0].end <= w[1].start + 1e-9 && w[0].start <= w[0].end);
            assert_ne!(w[0].speaker, w[1].speaker);
        }
    }

    #[test]
    fn cjk_segments_split_without_inventing_spaces() {
        // 你好 | 谢谢: byte offsets 0 and 6.
        let s = seg(0.0, 2.0, "你好谢谢", &[(0.0, 1.0, 0), (1.0, 2.0, 6)]);
        let (out, _) = attribute(vec![s], &[turn(0.0, 1.0, 0), turn(1.0, 2.0, 1)]);
        assert_eq!(
            summary(&out),
            vec![(0.0, 1.0, "你好", 0), (1.0, 2.0, "谢谢", 1)]
        );
    }

    #[test]
    fn whisper_words_follow_leading_spaces() {
        let toks: Vec<(&[u8], i64)> = vec![
            (b" Hello", 110),
            (b",", 140),
            (b" wor", 160),
            (b"ld", 170),
            (b".", 190),
            (b" Bye", 300),
        ];
        let (text, words) = whisper_words(&toks, -0.1, 4.0).unwrap();
        assert_eq!(text, "Hello, world. Bye");
        let at: Vec<usize> = words.iter().map(|w| w.at).collect();
        assert_eq!(at, vec![0, 7, 14]);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(words[0].start, 1.0) && close(words[0].end, 1.5));
        assert!(close(words[1].start, 1.5) && close(words[1].end, 2.9));
        assert!(close(words[2].start, 2.9) && close(words[2].end, 4.0));
        // No DTW time: no words.
        assert!(whisper_words(&[(b" Hi".as_slice(), -1)], 0.0, 1.0).is_none());
        // Nothing but space.
        assert_eq!(
            whisper_words(&[(b" ".as_slice(), 5)], 0.0, 1.0).unwrap().0,
            ""
        );
    }

    #[test]
    fn whisper_words_split_cjk_per_character_token() {
        // "你好，世界" where "世" arrives split over two tokens.
        let shi = "世".as_bytes();
        let toks: Vec<(&[u8], i64)> = vec![
            ("你".as_bytes(), 0),
            ("好".as_bytes(), 20),
            ("，".as_bytes(), 40),
            (&shi[..2], 60),
            (&shi[2..], 60),
            ("界".as_bytes(), 80),
        ];
        let (text, words) = whisper_words(&toks, 0.0, 1.0).unwrap();
        assert_eq!(text, "你好，世界");
        let pieces: Vec<&str> = words
            .iter()
            .enumerate()
            .map(|(i, w)| &text[w.at..words.get(i + 1).map_or(text.len(), |n| n.at)])
            .collect();
        assert_eq!(pieces, vec!["你", "好，", "世", "界"]);
        // Invalid UTF-8 overall: no words.
        assert!(whisper_words(&[(&shi[..2], 0)], 0.0, 1.0).is_none());
    }

    #[test]
    fn ctc_words_from_character_tokens() {
        let toks: Vec<String> = ["п", "р", "и", " ", "в", "е", "т", " ", " ", "м", "и", "р"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let times: Vec<f32> = (0..toks.len()).map(|i| i as f32 * 0.04).collect();
        let (text, words) = ctc_words(&toks, &times, 10.0, 0.04).unwrap();
        assert_eq!(text, "при вет мир");
        assert_eq!(
            words.iter().map(|w| w.at).collect::<Vec<_>>(),
            vec![0, 7, 14]
        );
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(words[0].start, 10.0) && close(words[0].end, 10.12));
        assert!(close(words[1].start, 10.16) && close(words[2].start, 10.36));
        assert!(close(words[2].end, 10.48));
        // BPE-style word marks.
        let toks: Vec<String> = ["▁при", "вет", "▁мир"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (text, words) = ctc_words(&toks, &[0.0, 0.2, 0.5], 0.0, 0.04).unwrap();
        assert_eq!(text, "привет мир");
        assert_eq!(words.len(), 2);
        assert!(ctc_words(&toks, &[0.0], 0.0, 0.04).is_none());
        assert_eq!(ctc_words(&[], &[], 0.0, 0.04).unwrap().0, "");
    }
}
