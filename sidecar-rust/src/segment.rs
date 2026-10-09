// Energy-based silence splitter for long-file transcription.
//
// GigaAM is a CTC model trained on short utterances: an hour of audio fed
// in one go is both slow and noticeably less accurate. `transcribe_file`
// therefore cuts the recording into chunks of at most a few tens of seconds,
// placing every cut inside a pause so no word is split in half. Frame energy
// is all it takes — it needs no extra model download and runs at thousands
// of times real time.
//
// The voiced threshold is derived from the recording itself (the noise floor
// and speech level around each point), not from a fixed dBFS value, so a
// whispered voice memo and a hot-mastered podcast are split at the same
// places.

/// One piece of the input. Chunks tile `[0, audio.len())` exactly, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start: usize,
    pub end: usize,
    /// First sample of the first voiced frame inside the chunk.
    pub voiced_start: usize,
    /// One past the last sample of the last voiced frame inside the chunk.
    /// For a silent chunk the voiced range is empty (`voiced_start ==
    /// voiced_end == start`).
    pub voiced_end: usize,
    /// No frame in the chunk rose above the voiced threshold — nothing for
    /// a recognizer to do here.
    pub silent: bool,
}

const FRAME_MS: usize = 20;
/// A run of unvoiced frames at least this long counts as a real pause and is
/// preferred for a cut over merely "quieter" spots inside speech.
const MIN_PAUSE_MS: usize = 300;
/// Cuts are never placed closer than this to the chunk start, and the last
/// chunk is kept at least this long, so no tiny slivers come out.
const MIN_CHUNK_MS: usize = 1000;
/// A voiced run shorter than this — a click, a knock — is not
/// taken for the start of speech by `VoicedFrames::first_sustained`.
const MIN_VOICED_RUN_MS: usize = 100;
/// Smoothing window (in frames, 100 ms) for the fallback "quietest spot"
/// search: a single low frame inside a vowel is not a word gap.
const SMOOTH_FRAMES: usize = 5;
/// Absolute floor for the voiced threshold (-80 dBFS). Only matters for
/// digitally silent or noise-gated input where the measured floor is ~0.
const ABS_FLOOR: f32 = 1e-4;
/// The voiced threshold of each `FLOOR_STEP_MS` stretch is estimated from
/// the `FLOOR_WINDOW_MS` of audio around it rather than from the whole file.
/// Long enough to take in several pauses even in fast speech.
const FLOOR_WINDOW_MS: usize = 30_000;
const FLOOR_STEP_MS: usize = 5_000;

/// Which 20 ms frames of a recording carry voice, by the same adaptive
/// threshold the splitter cuts on.
pub struct VoicedFrames {
    frame_len: usize,
    voiced: Vec<bool>,
    /// Length of the analyzed audio in samples.
    len: usize,
}

impl VoicedFrames {
    pub fn analyze(audio: &[f32], sample_rate: u32) -> Self {
        let frame_len = frame_len(sample_rate);
        let energy = frame_energies(audio, frame_len);
        Self::from_energy(&energy, frame_len, audio.len())
    }

    fn from_energy(energy: &[f32], frame_len: usize, len: usize) -> Self {
        // One threshold for a whole file breaks on mixed input: once a tenth
        // of it is digital silence (a silent intro, a muted stretch) its
        // noise floor is 0, the threshold drops to ABS_FLOOR, and the room
        // noise in every pause of the actual speech reads as voice. A local
        // estimate confines that to the stretch next to the silence, and
        // follows a noise floor that drifts over an hour-long recording.
        // Input shorter than one window gets a single threshold, as before.
        let window = FLOOR_WINDOW_MS / FRAME_MS;
        let step = FLOOR_STEP_MS / FRAME_MS;
        let n = energy.len();
        let mut voiced = Vec::with_capacity(n);
        for block_start in (0..n).step_by(step) {
            let block = block_start..(block_start + step).min(n);
            // Centred on the block, shifted inwards at either end of the file.
            let lo = ((block.start + block.end) / 2)
                .saturating_sub(window / 2)
                .min(n.saturating_sub(window));
            let threshold = voiced_threshold(&energy[lo..(lo + window).min(n)]);
            voiced.extend(energy[block].iter().map(|&e| e > threshold));
        }
        Self {
            frame_len,
            voiced,
            len,
        }
    }

    /// Narrows the sample range `[start, end)` to the voiced frames inside
    /// it, or `None` when none of them is voiced (or the range is empty).
    pub fn tighten(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let frames = start / self.frame_len..end.div_ceil(self.frame_len).min(self.voiced.len());
        let first = frames.clone().find(|&f| self.voiced[f])?;
        let last = frames.rev().find(|&f| self.voiced[f])?;
        let from = (first * self.frame_len).max(start);
        let to = ((last + 1) * self.frame_len).min(end).min(self.len);
        (from < to).then_some((from, to))
    }

    /// Pauses — unvoiced runs of at least `MIN_PAUSE_MS` — as sample
    /// ranges, in order.
    pub fn pauses(&self) -> Vec<(usize, usize)> {
        let min_pause = (MIN_PAUSE_MS / FRAME_MS).max(1);
        runs(&self.voiced, false, min_pause)
            .into_iter()
            .map(|(a, b)| (a * self.frame_len, (b * self.frame_len).min(self.len)))
            .collect()
    }

    /// The voiced stretches of `[start, end)`: the range split at every
    /// unvoiced run of at least `min_gap` samples, each piece narrowed by
    /// `tighten`, pieces with no voice left out.
    pub fn voiced_runs(&self, start: usize, end: usize, min_gap: usize) -> Vec<(usize, usize)> {
        if start >= end {
            return Vec::new();
        }
        let fl = self.frame_len;
        let first = (start / fl).min(self.voiced.len());
        let last = end.div_ceil(fl).clamp(first, self.voiced.len());
        let gaps = runs(&self.voiced[first..last], false, min_gap.div_ceil(fl).max(1));
        let mut out = Vec::with_capacity(gaps.len() + 1);
        let mut from = start;
        for (a, b) in gaps {
            out.extend(self.tighten(from, ((first + a) * fl).clamp(from, end)));
            from = ((first + b) * fl).clamp(from, end);
        }
        out.extend(self.tighten(from, end));
        out
    }

    /// First sample of the first stretch that sounds like speech rather than
    /// a noise: a voiced frame followed, within `window` samples, by at least
    /// `min_voiced` samples of voice. Only voiced runs of `MIN_VOICED_RUN_MS`
    /// or more count, so a knock or a click in a silent intro — voiced, but
    /// alone — is passed over. `None` when nothing qualifies.
    pub fn first_sustained(&self, window: usize, min_voiced: usize) -> Option<usize> {
        let fl = self.frame_len;
        let n = self.voiced.len();
        let mut solid = vec![false; n];
        for (a, b) in runs(&self.voiced, true, (MIN_VOICED_RUN_MS / FRAME_MS).max(1)) {
            solid[a..b].fill(true);
        }
        let mut prefix = Vec::with_capacity(n + 1);
        prefix.push(0usize);
        for &s in &solid {
            prefix.push(prefix.last().unwrap() + s as usize);
        }
        let (window, need) = (window.div_ceil(fl).max(1), min_voiced.div_ceil(fl));
        (0..n)
            .find(|&f| solid[f] && prefix[(f + window).min(n)] - prefix[f] >= need)
            .map(|f| f * fl)
    }
}

fn frame_len(sample_rate: u32) -> usize {
    (sample_rate.max(1) as usize * FRAME_MS / 1000).max(1)
}

/// Splits `audio` into chunks no longer than `max_chunk_secs`, cutting in
/// pauses where there are any and at the quietest nearby spot where there
/// are none (continuous speech, music).
pub fn split_on_silence(audio: &[f32], sample_rate: u32, max_chunk_secs: f32) -> Vec<Chunk> {
    if audio.is_empty() {
        return Vec::new();
    }
    let sr = sample_rate.max(1) as usize;
    let frame_len = frame_len(sample_rate);
    let energy = frame_energies(audio, frame_len);
    let frames = VoicedFrames::from_energy(&energy, frame_len, audio.len());
    let voiced = &frames.voiced;
    let n_frames = energy.len();

    let max_samples = (max_chunk_secs.max(0.0) as f64 * sr as f64) as usize;
    let max_frames = (max_samples / frame_len).max(1);
    let min_pause = (MIN_PAUSE_MS / FRAME_MS).max(1);
    let min_len = MIN_CHUNK_MS / FRAME_MS;
    let min_chunk = min_len.min(max_frames / 2).max(1);

    let pauses = runs(voiced, false, min_pause);
    let mut prefix = Vec::with_capacity(n_frames + 1);
    prefix.push(0.0f64);
    for &e in &energy {
        prefix.push(prefix.last().unwrap() + e as f64);
    }

    // Cut points in frames. A cut at frame `c` ends the current chunk just
    // before frame `c`.
    let mut cuts = Vec::new();
    let mut pos = 0usize;
    while n_frames - pos > max_frames {
        let lo = pos + min_chunk;
        // Leave at least `min_len` frames after the cut so the final chunk
        // isn't a sliver — possible whenever the limit leaves room for both.
        let hi = if max_frames >= min_chunk + min_len {
            (pos + max_frames).min(n_frames - min_len)
        } else {
            pos + max_frames
        };
        let cut = latest_pause_cut(&pauses, lo, hi).unwrap_or_else(|| {
            // No real pause: take the quietest spot, but only in the second
            // half of the allowed range so continuous speech doesn't turn
            // into a string of short chunks.
            let from = lo.max(pos + max_frames / 2).min(hi);
            quietest_cut(&prefix, from, hi)
        });
        cuts.push(cut);
        pos = cut;
    }

    let mut bounds: Vec<(usize, usize)> = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0usize;
    for cut in cuts {
        let end = cut * frame_len;
        bounds.push((start, end));
        start = end;
    }
    bounds.push((start, audio.len()));

    // Safety net for limits too small to reserve a full-length tail above.
    if bounds.len() >= 2 {
        let (tail_start, tail_end) = bounds[bounds.len() - 1];
        let (prev_start, _) = bounds[bounds.len() - 2];
        if tail_end - tail_start < min_len * frame_len && tail_end - prev_start <= max_samples {
            bounds.pop();
            bounds.last_mut().unwrap().1 = tail_end;
        }
    }

    bounds
        .into_iter()
        .map(|(start, end)| match frames.tighten(start, end) {
            Some((voiced_start, voiced_end)) => Chunk {
                start,
                end,
                voiced_start,
                voiced_end,
                silent: false,
            },
            None => Chunk {
                start,
                end,
                voiced_start: start,
                voiced_end: start,
                silent: true,
            },
        })
        .collect()
}

/// Per-frame RMS with the frame's own mean removed, so a DC offset from a
/// cheap interface doesn't read as permanent speech.
fn frame_energies(audio: &[f32], frame_len: usize) -> Vec<f32> {
    audio
        .chunks(frame_len)
        .map(|frame| {
            let n = frame.len() as f64;
            let mean = frame.iter().map(|&x| x as f64).sum::<f64>() / n;
            let var = frame
                .iter()
                .map(|&x| {
                    let d = x as f64 - mean;
                    d * d
                })
                .sum::<f64>()
                / n;
            let rms = var.sqrt() as f32;
            if rms.is_finite() {
                rms
            } else {
                0.0
            }
        })
        .collect()
}

/// Voiced means "clearly above the noise floor" of `energy`, a stretch of
/// the recording. The floor is the 10th percentile of frame energy (pauses
/// between phrases), the speech level the 95th. The threshold sits 12 dB over the floor on a clean
/// recording, but never above the midpoint (in dB) between floor and speech,
/// so a noisy recording still has voiced frames — and never less than 6 dB
/// over the floor, so steady noise or hum with nothing on top stays silent.
/// Every term scales with the input, which is what makes quiet and loud
/// versions of one recording split identically.
fn voiced_threshold(energy: &[f32]) -> f32 {
    let mut sorted = energy.to_vec();
    sorted.sort_unstable_by(f32::total_cmp);
    let pick = |q: f32| sorted[((sorted.len() - 1) as f32 * q).round() as usize];
    let floor = pick(0.10);
    let speech = pick(0.95);
    let t = (floor * 4.0).min((floor * speech).sqrt()).max(floor * 2.0);
    t.max(ABS_FLOOR)
}

/// Runs of frames `[a, b)` whose voiced flag is `value`, at least `min_len`
/// long, in order.
fn runs(voiced: &[bool], value: bool, min_len: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run_start = None;
    for (i, &v) in voiced.iter().enumerate() {
        match (v == value, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(a)) => {
                if i - a >= min_len {
                    runs.push((a, i));
                }
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(a) = run_start {
        if voiced.len() - a >= min_len {
            runs.push((a, voiced.len()));
        }
    }
    runs
}

/// The latest pause that reaches into `[lo, hi]`, searching backwards from
/// `hi` so chunks stay as long as the limit allows. Cuts in the middle of
/// the part of the pause that is inside the range.
fn latest_pause_cut(pauses: &[(usize, usize)], lo: usize, hi: usize) -> Option<usize> {
    let upto = pauses.partition_point(|&(a, _)| a <= hi);
    for &(a, b) in pauses[..upto].iter().rev() {
        if b < lo {
            break;
        }
        let (from, to) = (a.max(lo), b.min(hi));
        if from <= to {
            return Some((from + to) / 2);
        }
    }
    None
}

/// The latest cut in `[lo, hi]` whose smoothed energy is within ~1 dB of
/// the quietest one there. Word gaps inside continuous speech are all about
/// equally quiet; among those, the latest keeps chunks near the limit.
fn quietest_cut(prefix: &[f64], lo: usize, hi: usize) -> usize {
    let n = prefix.len() - 1;
    let half = SMOOTH_FRAMES / 2;
    let score = |c: usize| {
        let a = c.saturating_sub(half);
        let b = (c + half + 1).min(n).max(a + 1);
        (prefix[b] - prefix[a]) / (b - a) as f64
    };
    let quietest = (lo..=hi).map(score).fold(f64::INFINITY, f64::min);
    (lo..=hi)
        .rev()
        .find(|&c| score(c) <= quietest * 1.12)
        .unwrap_or(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn secs(s: f32) -> usize {
        (s * SR as f32) as usize
    }

    /// Speech-like test signal: a 220 Hz carrier amplitude-modulated at a
    /// syllable rate, so its frame energy has dynamics like a voice does.
    fn speechy(dur: f32, amp: f32) -> Vec<f32> {
        (0..secs(dur))
            .map(|i| {
                let t = i as f32 / SR as f32;
                let env = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
                amp * env * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
            })
            .collect()
    }

    fn tone(dur: f32, amp: f32) -> Vec<f32> {
        (0..secs(dur))
            .map(|i| amp * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR as f32).sin())
            .collect()
    }

    /// Deterministic white-ish noise (LCG), so tests are reproducible.
    fn noise(dur: f32, amp: f32, seed: u32) -> Vec<f32> {
        let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..secs(dur))
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amp * ((s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    fn mix(a: &mut [f32], b: &[f32]) {
        for (x, y) in a.iter_mut().zip(b) {
            *x += y;
        }
    }

    /// `bursts` speech-like bursts of `burst` seconds, separated by `gap`
    /// seconds of silence. Returns the signal and the gap intervals.
    fn bursts(n: usize, burst: f32, gap: f32, amp: f32) -> (Vec<f32>, Vec<(usize, usize)>) {
        let mut audio = Vec::new();
        let mut gaps = Vec::new();
        for i in 0..n {
            audio.extend(speechy(burst, amp));
            if i + 1 < n {
                let g0 = audio.len();
                audio.extend(std::iter::repeat_n(0.0, secs(gap)));
                gaps.push((g0, audio.len()));
            }
        }
        (audio, gaps)
    }

    fn assert_tiles(chunks: &[Chunk], len: usize) {
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks.last().unwrap().end, len);
        for w in chunks.windows(2) {
            assert_eq!(
                w[0].end, w[1].start,
                "gap/overlap between {:?} and {:?}",
                w[0], w[1]
            );
        }
        for c in chunks {
            assert!(c.start < c.end, "empty chunk {c:?}");
            assert!(c.start <= c.voiced_start && c.voiced_start <= c.voiced_end);
            assert!(c.voiced_end <= c.end);
            assert_eq!(c.silent, c.voiced_start == c.voiced_end, "{c:?}");
        }
    }

    fn assert_max(chunks: &[Chunk], max_secs: f32) {
        let max = (max_secs as f64 * SR as f64) as usize;
        for c in chunks {
            assert!(
                c.end - c.start <= max,
                "chunk {c:?} longer than {max_secs}s"
            );
        }
    }

    #[test]
    fn empty_input_gives_no_chunks() {
        assert!(split_on_silence(&[], SR, 20.0).is_empty());
    }

    #[test]
    fn short_input_is_one_tight_chunk() {
        let mut audio = vec![0.0; secs(1.0)];
        audio.extend(speechy(3.0, 0.3));
        audio.extend(vec![0.0; secs(1.0)]);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_eq!(chunks.len(), 1);
        assert_tiles(&chunks, audio.len());
        let c = &chunks[0];
        assert!(!c.silent);
        // Tightened to the burst, within one 20 ms frame.
        assert!(c.voiced_start.abs_diff(secs(1.0)) <= 320, "{c:?}");
        assert!(c.voiced_end.abs_diff(secs(4.0)) <= 320, "{c:?}");
    }

    #[test]
    fn cuts_land_inside_silences() {
        let (audio, gaps) = bursts(5, 8.0, 1.5, 0.3);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert!(chunks.len() >= 3, "{chunks:?}");
        for c in &chunks[..chunks.len() - 1] {
            assert!(
                gaps.iter().any(|&(g0, g1)| g0 <= c.end && c.end <= g1),
                "cut at {} is not inside a silence: {gaps:?}",
                c.end
            );
        }
        for c in &chunks {
            assert!(!c.silent);
            // Voiced bounds hug the bursts: never inside a gap by more than
            // a frame.
            for &(g0, g1) in &gaps {
                assert!(!(g0 + 320 < c.voiced_start && c.voiced_start < g1), "{c:?}");
                assert!(!(g0 < c.voiced_end && c.voiced_end + 320 < g1), "{c:?}");
            }
        }
    }

    #[test]
    fn continuous_speech_gets_forced_cuts_near_the_limit() {
        // No pause anywhere: every cut is forced, at a quiet spot close to
        // the limit, so 65 s needs exactly four chunks.
        let audio = speechy(65.0, 0.3);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert_eq!(chunks.len(), 4, "{chunks:?}");
        for c in &chunks[..3] {
            assert!(c.end - c.start >= secs(19.0), "{c:?}");
        }
        assert!(chunks.iter().all(|c| !c.silent));
    }

    #[test]
    fn continuous_tone_gets_forced_cuts_within_limit() {
        let audio = tone(65.0, 0.3);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert!(chunks.len() >= 4, "{chunks:?}");
        for c in &chunks {
            assert!(c.end - c.start >= secs(1.0), "sliver {c:?}");
        }
    }

    #[test]
    fn no_sliver_just_over_the_limit() {
        let audio = speechy(20.5, 0.3);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert_eq!(chunks.len(), 2);
        for c in &chunks {
            assert!(c.end - c.start >= secs(0.98), "sliver {c:?}");
        }
    }

    #[test]
    fn pure_silence_is_all_silent() {
        let audio = vec![0.0f32; secs(50.0)];
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert!(chunks.iter().all(|c| c.silent), "{chunks:?}");
    }

    #[test]
    fn long_pause_in_the_middle_is_skipped() {
        let mut audio = speechy(5.0, 0.3);
        audio.extend(vec![0.0; secs(60.0)]);
        audio.extend(speechy(5.0, 0.3));
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert!(!chunks.first().unwrap().silent);
        assert!(!chunks.last().unwrap().silent);
        assert!(
            chunks[1..chunks.len() - 1].iter().all(|c| c.silent),
            "{chunks:?}"
        );
        // Only the two speech chunks would reach the recognizer.
        assert_eq!(chunks.iter().filter(|c| !c.silent).count(), 2);
    }

    #[test]
    fn loud_and_quiet_versions_split_identically() {
        let (mut loud, _) = bursts(6, 6.0, 1.2, 0.5);
        mix(&mut loud, &noise(100.0, 0.002, 7));
        let gain = 10f32.powf(-30.0 / 20.0);
        let quiet: Vec<f32> = loud.iter().map(|x| x * gain).collect();
        let a = split_on_silence(&loud, SR, 20.0);
        let b = split_on_silence(&quiet, SR, 20.0);
        assert!(a.len() >= 2);
        assert_eq!(a, b);
    }

    #[test]
    fn noisy_recording_still_cuts_in_gaps() {
        // Speech ~20 dB over a steady noise floor: a fixed dBFS threshold
        // would see one endless voiced run here.
        let (mut audio, gaps) = bursts(5, 7.0, 1.5, 0.3);
        mix(&mut audio, &noise(100.0, 0.03, 3));
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        assert!(chunks.iter().all(|c| !c.silent));
        for c in &chunks[..chunks.len() - 1] {
            assert!(
                gaps.iter().any(|&(g0, g1)| g0 <= c.end && c.end <= g1),
                "{}",
                c.end
            );
        }
    }

    /// Room-noise speech after an intro that holds more than a tenth of the
    /// file (the share that used to set the whole file's noise floor):
    /// pauses and cuts still follow the gaps between bursts. Only a gap
    /// within half a floor window of the intro may be missed.
    fn check_pauses_after_intro(intro: Vec<f32>) {
        let (mut speech, gaps) = bursts(6, 7.0, 1.5, 0.3);
        mix(&mut speech, &noise(60.0, 0.002, 5)); // ~-59 dBFS
        let offset = intro.len();
        let mut audio = intro;
        audio.extend(speech);
        assert!(offset * 10 > audio.len());
        let gaps: Vec<(usize, usize)> = gaps.iter().map(|&(a, b)| (a + offset, b + offset)).collect();

        let pauses = VoicedFrames::analyze(&audio, SR).pauses();
        let reach = secs(FLOOR_WINDOW_MS as f32 / 2000.0);
        let far = gaps.iter().filter(|&&(g0, _)| g0 >= offset + reach).collect::<Vec<_>>();
        assert!(far.len() >= 4, "{gaps:?}");
        for &&(g0, g1) in &far {
            assert!(
                pauses.iter().any(|&(a, b)| a.abs_diff(g0) <= 320 && b.abs_diff(g1) <= 320),
                "gap {g0}..{g1} not among pauses {pauses:?}"
            );
        }

        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert_max(&chunks, 20.0);
        for c in &chunks[..chunks.len() - 1] {
            assert!(
                c.end <= offset || gaps.iter().any(|&(g0, g1)| g0 <= c.end && c.end <= g1),
                "cut at {} is not inside a silence: {gaps:?}",
                c.end
            );
        }
    }

    #[test]
    fn digitally_silent_intro_keeps_pauses_in_noisy_speech() {
        check_pauses_after_intro(vec![0.0; secs(10.0)]);
    }

    #[test]
    fn quiet_intro_keeps_pauses_in_noisy_speech() {
        // Far below the room noise, but not digital silence.
        check_pauses_after_intro(noise(10.0, 0.0003, 13));
    }

    #[test]
    fn steady_noise_alone_is_silent() {
        let audio = noise(30.0, 0.05, 11);
        let chunks = split_on_silence(&audio, SR, 20.0);
        assert_tiles(&chunks, audio.len());
        assert!(chunks.iter().all(|c| c.silent), "{chunks:?}");
    }

    #[test]
    fn tighten_narrows_a_range_to_its_voice() {
        // Bursts at 0–4 s, 6–10 s and 12–16 s.
        let (audio, _) = bursts(3, 4.0, 2.0, 0.3);
        let v = VoicedFrames::analyze(&audio, SR);
        // Starting in a pause and ending in one: snaps to the burst between.
        let (a, b) = v.tighten(secs(4.5), secs(11.5)).unwrap();
        assert!(
            a.abs_diff(secs(6.0)) <= 320 && b.abs_diff(secs(10.0)) <= 320,
            "{a} {b}"
        );
        // Spanning a pause keeps both voiced ends (the pause stays inside).
        let (a, b) = v.tighten(secs(3.0), secs(7.0)).unwrap();
        assert_eq!((a, b), (secs(3.0), secs(7.0)));
        // Nothing voiced, or nothing at all.
        assert_eq!(v.tighten(secs(4.2), secs(5.8)), None);
        assert_eq!(v.tighten(secs(7.0), secs(7.0)), None);
        // A range running past the end is clamped to the audio, including
        // its last, partial frame.
        let mut odd = audio.clone();
        odd.extend(speechy(0.01, 0.3));
        let v = VoicedFrames::analyze(&odd, SR);
        let (_, b) = v.tighten(secs(13.0), odd.len() + 99_999).unwrap();
        assert_eq!(b, odd.len());
        assert!(VoicedFrames::analyze(&[], SR).tighten(0, 10).is_none());
    }

    #[test]
    fn pauses_are_the_gaps_between_bursts() {
        let (mut audio, gaps) = bursts(3, 4.0, 2.0, 0.3);
        // A 0.1 s dip inside speech is not a pause.
        audio[secs(1.0)..secs(1.1)].fill(0.0);
        let pauses = VoicedFrames::analyze(&audio, SR).pauses();
        assert_eq!(pauses.len(), gaps.len(), "{pauses:?}");
        for (&(a, b), &(g0, g1)) in pauses.iter().zip(&gaps) {
            assert!(
                a.abs_diff(g0) <= 320 && b.abs_diff(g1) <= 320,
                "{a}..{b} vs {g0}..{g1}"
            );
        }
    }

    #[test]
    fn voiced_runs_split_at_long_gaps_only() {
        // Bursts at 0–4 s, 6–10 s, 12–16 s and, after a 0.5 s gap, 16.5–18.5 s.
        let (mut audio, _) = bursts(3, 4.0, 2.0, 0.3);
        audio.extend(vec![0.0; secs(0.5)]);
        audio.extend(speechy(2.0, 0.3));
        let v = VoicedFrames::analyze(&audio, SR);
        let check = |start: f32, end: f32, gap: f32, want: &[(f32, f32)]| {
            let got = v.voiced_runs(secs(start), secs(end), secs(gap));
            assert_eq!(got.len(), want.len(), "{start}..{end} gap {gap}: {got:?}");
            for (&(a, b), &(wa, wb)) in got.iter().zip(want) {
                assert!(
                    a.abs_diff(secs(wa)) <= 320 && b.abs_diff(secs(wb)) <= 320,
                    "{start}..{end} gap {gap}: {got:?}"
                );
            }
        };
        check(0.0, 18.5, 1.0, &[(0.0, 4.0), (6.0, 10.0), (12.0, 18.5)]);
        check(0.0, 18.5, 0.3, &[(0.0, 4.0), (6.0, 10.0), (12.0, 16.0), (16.5, 18.5)]);
        check(0.0, 18.5, 3.0, &[(0.0, 18.5)]);
        // Ends inside gaps are dropped, cuts inside speech are kept.
        check(4.5, 11.5, 1.0, &[(6.0, 10.0)]);
        check(5.0, 13.0, 1.0, &[(6.0, 10.0), (12.0, 13.0)]);
        check(4.2, 5.8, 1.0, &[]);
        check(7.0, 7.0, 1.0, &[]);
        // Past the end of the audio: clamped like `tighten`.
        let got = v.voiced_runs(secs(11.0), audio.len() + 99_999, secs(1.0));
        assert_eq!(got.last().unwrap().1, audio.len(), "{got:?}");
    }

    #[test]
    fn first_sustained_skips_a_knock_in_a_silent_intro() {
        let (window, min_voiced) = (secs(5.0), secs(1.0));
        let knock = tone(0.15, 0.5);
        for room in [vec![0.0; secs(30.0)], noise(30.0, 0.001, 17)] {
            // A 150 ms knock at 1 s, nothing else until speech at 30 s.
            let mut audio = room;
            mix(&mut audio[secs(1.0)..], &knock);
            audio.extend(speechy(8.0, 0.3));
            let v = VoicedFrames::analyze(&audio, SR);
            let (first_voiced, _) = v.tighten(0, audio.len()).unwrap();
            assert!(first_voiced.abs_diff(secs(1.0)) <= 320, "{first_voiced}");
            let start = v.first_sustained(window, min_voiced).unwrap();
            assert!(start.abs_diff(secs(30.0)) <= 320, "{start}");
        }
        // Speech from the first frame.
        let v = VoicedFrames::analyze(&speechy(5.0, 0.3), SR);
        assert_eq!(v.first_sustained(window, min_voiced), Some(0));
    }

    #[test]
    fn first_sustained_is_none_without_speech() {
        let (window, min_voiced) = (secs(5.0), secs(1.0));
        // Knocks and single-frame clicks only: voiced, never sustained.
        let mut audio = vec![0.0; secs(20.0)];
        for at in [1.0, 3.0, 5.0, 9.0] {
            mix(&mut audio[secs(at)..], &tone(0.15, 0.5));
        }
        for at in [12.0, 12.5, 13.0, 13.5, 14.0, 14.5] {
            mix(&mut audio[secs(at)..], &tone(0.02, 0.5));
        }
        let v = VoicedFrames::analyze(&audio, SR);
        assert!(v.tighten(0, audio.len()).is_some());
        assert_eq!(v.first_sustained(window, min_voiced), None);
        // Half a second of speech is too little to count.
        let v = VoicedFrames::analyze(&speechy(0.5, 0.3), SR);
        assert_eq!(v.first_sustained(window, min_voiced), None);
        assert_eq!(VoicedFrames::analyze(&[], SR).first_sustained(window, min_voiced), None);
    }

    #[test]
    fn chunks_tile_the_input_for_many_signals() {
        let (b, _) = bursts(7, 4.3, 0.7, 0.2);
        let mut noisy = speechy(47.3, 0.1);
        mix(&mut noisy, &noise(50.0, 0.01, 5));
        let mut odd = speechy(33.0, 0.4);
        odd.extend(vec![0.0; 1234]);
        odd.extend(tone(0.013, 0.2));
        let signals: Vec<Vec<f32>> = vec![
            b,
            noisy,
            odd,
            tone(0.25, 0.3),
            vec![0.5; 7],
            noise(61.0, 0.2, 9),
            vec![0.0; secs(41.0) + 17],
        ];
        for audio in &signals {
            for max in [1.5f32, 5.0, 7.3, 20.0, 30.0] {
                let chunks = split_on_silence(audio, SR, max);
                assert_tiles(&chunks, audio.len());
                assert_max(&chunks, max);
            }
        }
    }
}
