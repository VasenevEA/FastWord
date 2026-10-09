// FastWord Rust sidecar.
//
// Reads line-delimited JSON requests from stdin, transcribes Float32 PCM
// audio with whisper.cpp (Metal-accelerated), writes line-delimited JSON
// responses to stdout. Holds the model in RAM and evicts it after
// FASTWORD_IDLE_EVICT seconds of inactivity.
//
// Mirrors the protocol of the previous Python sidecar exactly so the Swift
// side does not need any change beyond the binary path.
//
// `transcribe_file` extends it for long recordings: the audio arrives as a
// path to a raw f32 file instead of inline base64, the job writes
// `{"id","progress"}` lines while it runs, and its final line carries
// timestamped `segments` next to the usual `text`. Requests are still served
// one at a time, so the app runs file jobs in a sidecar process of their own
// (cancelling one by killing it) and dictation never waits behind an hour of
// audio.
//
// With `"diarize": true` a file job also tells the speakers apart: the
// diarization runs in a child process next to the recognizer (see
// diarize.rs), each word goes to the speaker talking at the time, and the
// final line's segments carry a `speaker` and a `speakers` summary — or,
// when diarization fails, a `warning` and the plain transcript.

use std::cmp::Reverse;
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters, WhisperState,
};

mod diarize;
mod gigaam;
mod merge;
mod recluster;
mod segment;

use merge::{SpeakerTime, Word};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    cmd: String,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    audio_b64: Option<String>,
    #[serde(default)]
    language: Option<String>,
    /// Whisper.cpp no-speech threshold (0.0 disables, default 0.6).
    #[serde(default)]
    no_speech_thold: Option<f32>,
    /// Vocabulary hint passed to Whisper. Used by the Swift side to bias
    /// language detection when the user keeps the picker on "Auto".
    #[serde(default)]
    initial_prompt: Option<String>,
    /// "whisper" (default) or "gigaam" — selects the inference engine.
    #[serde(default)]
    engine: Option<String>,
    /// `transcribe_file` only: path to headerless little-endian f32 mono
    /// PCM at 16 kHz. An hour of audio is ~230 MB — far too much for base64
    /// on one JSON line — so the client hands it over as a file.
    #[serde(default)]
    audio_path: Option<String>,
    /// `transcribe_file` only: also find out who spoke when. The app sends
    /// it (and `num_speakers`) only when the user asked for speakers, so
    /// other requests stay exactly as they were.
    #[serde(default)]
    diarize: Option<bool>,
    /// With `diarize`: how many speakers there are; 0 or absent = decide
    /// automatically.
    #[serde(default)]
    num_speakers: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
struct Response {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// `transcribe_file` progress line (0..1). Such a line has neither
    /// `text` nor `error`; the job's final line follows later.
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<f64>,
    /// `transcribe_file` result: the transcript with timestamps.
    #[serde(skip_serializing_if = "Option::is_none")]
    segments: Option<Vec<Segment>>,
    /// Diarized `transcribe_file` result: talk time per speaker id.
    #[serde(skip_serializing_if = "Option::is_none")]
    speakers: Option<Vec<SpeakerTime>>,
    /// The job succeeded, but not entirely: diarization was asked for and
    /// failed, so the transcript comes without speakers.
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

/// One timestamped piece of a file transcript, in seconds from the start of
/// the file. Mirrors `TranscriptSegment` on the Swift side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
    /// Diarized jobs only: who said it, 0-based by first appearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker: Option<u32>,
    /// Word timings, for attributing speakers; never sent to the client.
    #[serde(skip)]
    words: Vec<Word>,
}

impl Segment {
    fn new(start: f64, end: f64, text: String) -> Self {
        Self {
            start,
            end,
            text,
            speaker: None,
            words: Vec::new(),
        }
    }
}

/// What a `transcribe_file` job produces.
#[derive(Debug, Default)]
struct FileTranscript {
    text: String,
    segments: Vec<Segment>,
    speakers: Option<Vec<SpeakerTime>>,
    warning: Option<String>,
}

impl FileTranscript {
    /// `text` is the segments joined with single spaces, so a client that
    /// only knows the old protocol still gets the whole transcript.
    fn plain(segments: Vec<Segment>) -> Self {
        Self {
            text: joined_text(&segments),
            segments,
            ..Default::default()
        }
    }
}

fn joined_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Response {
    fn ok(id: String, text: String) -> Self {
        Self {
            id,
            text: Some(text),
            ..Default::default()
        }
    }

    fn err(id: String, error: String) -> Self {
        Self {
            id,
            error: Some(error),
            ..Default::default()
        }
    }

    fn progress(id: String, progress: f64) -> Self {
        Self {
            id,
            progress: Some(progress),
            ..Default::default()
        }
    }

    fn transcript(id: String, t: FileTranscript) -> Self {
        Self {
            id,
            text: Some(t.text),
            segments: Some(t.segments),
            speakers: t.speakers,
            warning: t.warning,
            ..Default::default()
        }
    }
}

/// Writes one protocol line to stdout. Every line goes through here:
/// progress lines are written from inside a running job (whisper's progress
/// callback), so the lock is taken per line instead of being held by the
/// request loop, and lines never interleave.
fn emit(resp: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(resp).expect("Response always serializes");
    line.push('\n');
    let mut out = io::stdout().lock();
    out.write_all(line.as_bytes())?;
    out.flush()
}

/// Turns an engine's raw progress reports into protocol progress: whole
/// percents only and strictly increasing, so a job writes at most ~100
/// lines however often (or however unevenly) the engine reports.
struct Progress<F: FnMut(f64)> {
    /// `None` until the first report. That one goes out even at 0%: it is
    /// the client's signal that the model is loaded and inference started.
    last_pct: Option<u32>,
    sink: F,
}

impl<F: FnMut(f64)> Progress<F> {
    fn new(sink: F) -> Self {
        Self {
            last_pct: None,
            sink,
        }
    }

    fn percent(&mut self, pct: u32) {
        let pct = pct.min(100);
        if self.last_pct.is_none_or(|last| pct > last) {
            self.last_pct = Some(pct);
            (self.sink)(pct as f64 / 100.0);
        }
    }

    fn fraction(&mut self, fraction: f64) {
        if fraction.is_finite() {
            // The epsilon keeps e.g. 0.29 (28.999… once scaled) at 29%.
            self.percent((fraction.clamp(0.0, 1.0) * 100.0 + 1e-9).floor() as u32);
        }
    }
}

fn log(msg: &str) {
    // Not eprintln!: it panics — and with panic = "abort", crashes — when the
    // app is gone and stderr is a closed pipe.
    let _ = writeln!(io::stderr(), "[sidecar] {msg}");
}

struct ModelHolder {
    model_path: PathBuf,
    ctx: Option<WhisperContext>,
    /// `ctx` was loaded with DTW token timestamps (word timing).
    dtw: bool,
    last_used: Instant,
}

impl ModelHolder {
    fn new(model_path: PathBuf) -> Self {
        Self {
            model_path,
            ctx: None,
            dtw: false,
            last_used: Instant::now(),
        }
    }

    /// Loads the model if needed. `word_times` asks for a context with DTW
    /// token timestamps, which whisper.cpp only sets up at load time: a
    /// context loaded without them is reloaded. That happens at most once,
    /// for a diarized file job — and the app runs file jobs in a sidecar
    /// process of their own, where nothing is loaded yet. A DTW context
    /// decodes exactly what a plain one does (the alignment is an extra
    /// pass after each window), so it serves every later request as is.
    /// Models without a known alignment-head preset get no DTW.
    fn ensure_loaded(&mut self, word_times: bool) -> Result<&WhisperContext> {
        let preset = word_times.then(|| dtw_preset(&self.model_path)).flatten();
        if preset.is_some() && !self.dtw {
            self.ctx = None;
        }
        if self.ctx.is_none() {
            if !self.model_path.exists() {
                return Err(anyhow!(
                    "whisper model file not found at {} (set FASTWORD_MODEL or download one)",
                    self.model_path.display()
                ));
            }
            log(&format!("loading model {}", self.model_path.display()));
            let path_str = self
                .model_path
                .to_str()
                .context("non-utf8 model path")?
                .to_owned();
            let mut params = WhisperContextParameters::default();
            if let Some(model_preset) = preset.clone() {
                params.dtw_parameters(DtwParameters {
                    mode: DtwMode::ModelPreset { model_preset },
                    ..Default::default()
                });
            }
            let ctx = WhisperContext::new_with_params(&path_str, params)
                .context("failed to load whisper model")?;
            self.ctx = Some(ctx);
            self.dtw = preset.is_some();
            log(if self.dtw {
                "model loaded (with word timing)"
            } else {
                "model loaded"
            });
        }
        self.last_used = Instant::now();
        Ok(self.ctx.as_ref().unwrap())
    }

    fn transcribe(
        &mut self,
        audio: &[f32],
        language: Option<&str>,
        no_speech_thold: f32,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        let ctx = self.ensure_loaded(false)?;
        let mut state = ctx.create_state().context("create state")?;

        let params = whisper_params(language, no_speech_thold, initial_prompt);
        state.full(params, audio).context("whisper.full")?;

        let mut out = String::new();
        for segment in state.as_iter() {
            out.push_str(&segment.to_str_lossy().context("get segment text")?);
        }
        Ok(out.trim().to_string())
    }

    /// Whole-file transcription with timestamps. whisper.cpp walks the
    /// buffer in 30 s windows by itself; `on_progress` gets its percent.
    /// With `word_times` the segments carry their words, when the model has
    /// a DTW preset (see `ensure_loaded`).
    fn transcribe_file(
        &mut self,
        audio: &[f32],
        language: Option<&str>,
        no_speech_thold: f32,
        initial_prompt: Option<&str>,
        word_times: bool,
        on_progress: impl FnMut(i32) + 'static,
    ) -> Result<Vec<Segment>> {
        check_whisper_length(audio.len() as u64)?;
        let ctx = self.ensure_loaded(word_times)?;
        let eot = ctx.token_eot();
        let words_wanted = word_times && self.dtw;
        let ctx = self.ctx.as_ref().expect("loaded above");
        let mut state = ctx.create_state().context("create state")?;
        let voice = segment::VoicedFrames::analyze(audio, 16_000);

        // Empty language means auto-detect here. Leaving it unset (as
        // dictation does) would get whisper.cpp's own default, "en".
        let language = match language.filter(|l| !l.is_empty()) {
            Some(lang) => lang,
            None => detect_language(&mut state, audio, &voice).unwrap_or("auto"),
        };
        let mut params = whisper_params(Some(language), no_speech_thold, initial_prompt);
        // Stop decoded text from one 30 s window being fed to the next as a
        // prompt — that feedback is what turns one hallucinated line into a
        // loop repeated for minutes. `no_context` alone only discards text
        // left over from an earlier whisper_full call on this state; the
        // rolling context *within* one call is governed by n_max_text_ctx.
        // With it at 0 the initial_prompt is not used either, which costs
        // little: the app only sends one as a language hint, and whisper's
        // language detection never reads the prompt.
        params.set_no_context(true);
        params.set_n_max_text_ctx(0);
        // whisper-rs never frees this closure (a few bytes per job).
        params.set_progress_callback_safe(on_progress);

        state.full(params, audio).context("whisper.full")?;

        // Timestamps are in centiseconds. The last window can report an end
        // a little past the buffer, so clamp to the real duration.
        let duration = audio.len() as f64 / 16_000.0;
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment.to_str_lossy().context("get segment text")?;
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let start = (segment.start_timestamp() as f64 / 100.0).clamp(0.0, duration);
            let end = (segment.end_timestamp() as f64 / 100.0).clamp(start, duration);
            let mut seg = Segment::new(start, end, text.to_string());
            if words_wanted {
                seg.words = segment_words(&segment, eot, end, text).unwrap_or_default();
            }
            segments.push(seg);
        }
        fit_to_voice(&voice, &mut segments);
        segments.sort_by(|a, b| a.start.total_cmp(&b.start));
        // A long job can outlast the idle-eviction window; it counts as use.
        self.last_used = Instant::now();
        Ok(segments)
    }

    fn evict_if_idle(&mut self, idle_after: Duration) {
        if self.ctx.is_some() && self.last_used.elapsed() > idle_after {
            log("evicting idle model");
            self.ctx = None;
        }
    }
}

/// whisper.cpp's DTW times run late: the first word of a turn came out
/// ~0.24 s after its onset (median over the prototype's turns). With the
/// turbo model, moving words 0.1–0.2 s earlier put the fewest on the wrong
/// side of a speaker change; 0.4 s was clearly worse.
const DTW_SHIFT_SECS: f64 = -0.1;

/// The words of one decoded segment from its DTW token times, if they add up
/// to exactly the segment's text.
fn segment_words(
    segment: &whisper_rs::WhisperSegment<'_>,
    eot: whisper_rs::WhisperTokenId,
    end: f64,
    text: &str,
) -> Option<Vec<Word>> {
    let mut tokens: Vec<(&[u8], i64)> = Vec::with_capacity(segment.n_tokens().max(0) as usize);
    for i in 0..segment.n_tokens() {
        let token = segment.get_token(i)?;
        let data = token.token_data();
        // Special tokens (timestamps, end of text, …) are not in the text.
        if data.id >= eot {
            continue;
        }
        tokens.push((token.to_bytes().ok()?, data.t_dtw));
    }
    let (joined, words) = merge::whisper_words(&tokens, DTW_SHIFT_SECS, end)?;
    (joined == text).then_some(words)
}

/// The DTW alignment-head preset for a whisper.cpp model file, from its
/// standard name (`ggml-large-v3-turbo-q5_0.bin` → large-v3-turbo). The
/// preset must match the model's architecture exactly, so anything else —
/// distilled or fine-tuned models, renamed files — gets none.
fn dtw_preset(model_path: &Path) -> Option<DtwModelPreset> {
    let name = model_path.file_name()?.to_str()?.to_ascii_lowercase();
    let stem = name.strip_prefix("ggml-")?.strip_suffix(".bin")?;
    // Quantized variants ("-q5_0", "-q8_0") keep the architecture.
    let base = match stem.rsplit_once('-') {
        Some((base, q))
            if q.len() > 1
                && q.starts_with('q')
                && q[1..].starts_with(|c: char| c.is_ascii_digit()) =>
        {
            base
        }
        _ => stem,
    };
    Some(match base {
        "tiny" => DtwModelPreset::Tiny,
        "tiny.en" => DtwModelPreset::TinyEn,
        "base" => DtwModelPreset::Base,
        "base.en" => DtwModelPreset::BaseEn,
        "small" => DtwModelPreset::Small,
        "small.en" => DtwModelPreset::SmallEn,
        "medium" => DtwModelPreset::Medium,
        "medium.en" => DtwModelPreset::MediumEn,
        "large-v1" => DtwModelPreset::LargeV1,
        "large-v2" => DtwModelPreset::LargeV2,
        "large-v3" => DtwModelPreset::LargeV3,
        "large-v3-turbo" => DtwModelPreset::LargeV3Turbo,
        _ => return None,
    })
}

/// whisper.cpp takes the sample count as a C `int`. A longer buffer (~37 h
/// at 16 kHz) would wrap around and come back empty or cut short.
const WHISPER_MAX_SAMPLES: u64 = i32::MAX as u64;

fn check_whisper_length(samples: u64) -> Result<()> {
    if samples <= WHISPER_MAX_SAMPLES {
        return Ok(());
    }
    let hours = |n: u64| n as f64 / (16_000.0 * 3600.0);
    Err(anyhow!(
        "the recording is {:.1} h long; the Whisper engine takes at most {:.0} h per file — split it or use GigaAM",
        hours(samples),
        hours(WHISPER_MAX_SAMPLES)
    ))
}

/// `detect_language` starts listening where the voice first holds up: at
/// least `LANG_MIN_VOICE_SECS` of it within `LANG_ONSET_WINDOW_SECS`.
const LANG_ONSET_WINDOW_SECS: usize = 5;
const LANG_MIN_VOICE_SECS: usize = 1;

/// The language of the first speech in `audio`, for a file job that asked
/// for auto-detection. whisper.cpp's own "auto" judges the first 30 s only,
/// so a recording that opens with silence or room tone gets a guess made
/// on nothing, and the whole file is then decoded in that language — in
/// effect translated. `None` (leave it to whisper) when nothing is voiced
/// or detection fails.
fn detect_language(
    state: &mut WhisperState,
    audio: &[f32],
    voice: &segment::VoicedFrames,
) -> Option<&'static str> {
    // Not simply the first voiced frame: a knock or a click in a silent
    // intro is voiced too, and 30 s from there can be that one knock and
    // nothing else. Only audio without any sustained voice (a few short
    // words) falls back to it.
    let from = voice
        .first_sustained(LANG_ONSET_WINDOW_SECS * 16_000, LANG_MIN_VOICE_SECS * 16_000)
        .or_else(|| voice.tighten(0, audio.len()).map(|(from, _)| from))?;
    let window = &audio[from..(from + 30 * 16_000).min(audio.len())];
    let threads = num_cpus_or_default() as usize;
    // whisper_full computes the mel of the whole buffer again afterwards,
    // so borrowing the job's state for this is fine.
    state.pcm_to_mel(window, threads).ok()?;
    let (id, _) = state.lang_detect(0, threads).ok()?;
    let lang = whisper_rs::get_lang_str(id)?;
    log(&format!(
        "detected language {lang} at {:.1}s",
        from as f64 / 16_000.0
    ));
    Some(lang)
}

/// How far `fit_to_voice` may move a boundary between two whisper segments
/// to land it in a pause.
const SNAP_TO_PAUSE_SECS: f64 = 1.0;
/// A whisper segment with no voice anywhere in it is dropped from this
/// length up.
const DROP_UNVOICED_SECS: f64 = 1.0;

/// Aligns whisper's segment times with the speech in the audio. whisper.cpp
/// lays segments back to back and — the turbo model especially — puts the
/// boundary a little off the real pause (typically ~0.1 s early, sometimes
/// seconds away), so every pause ends up inside some segment: a subtitle
/// shows up before anyone speaks, and the client's paragraph breaks, which
/// look for gaps between segments, never happen.
///
/// A boundary with a pause within reach moves into it — the earlier segment
/// then ends where its speech does and the later one starts where speech
/// resumes — and each segment is trimmed to the voiced audio inside it. A
/// boundary in continuous speech stays where whisper put it.
///
/// A segment with nothing voiced in it is whisper hallucinating over
/// silence or steady noise — turbo's "Thank you." or "Продолжение
/// следует..." stretched over a whole 30 s window — and is dropped. Only a
/// short one stays, with whisper's times: those are off by a few tenths of
/// a second at times, so it may be a real word whose voice lies just
/// outside its span.
fn fit_to_voice(voice: &segment::VoicedFrames, segments: &mut Vec<Segment>) {
    let pauses = voice.pauses();
    let to_sample = |secs: f64| (secs.max(0.0) * 16_000.0).round() as usize;
    let to_secs = |sample: usize| (sample as f64 / 16.0).round() / 1000.0;
    let reach = to_sample(SNAP_TO_PAUSE_SECS);

    for i in 1..segments.len() {
        let (prev, next) = (&segments[i - 1], &segments[i]);
        let at = to_sample((prev.end + next.start) / 2.0);
        // Both segments must keep some of their own span.
        let (lo, hi) = (to_sample(prev.start), to_sample(next.end));
        let first = pauses.partition_point(|&(_, b)| b + reach < at);
        let nearest = pauses[first..]
            .iter()
            .take_while(|&&(a, _)| a <= at + reach)
            .filter(|&&(a, b)| a > lo && b < hi)
            // Closest to the boundary (0 when it is inside), then longest.
            .min_by_key(|&&(a, b)| {
                (
                    a.saturating_sub(at).max(at.saturating_sub(b)),
                    Reverse(b - a),
                )
            });
        if let Some(&(a, b)) = nearest {
            segments[i - 1].end = to_secs(a);
            segments[i].start = to_secs(b);
        }
    }

    segments.retain_mut(|seg| {
        match voice.tighten(to_sample(seg.start), to_sample(seg.end)) {
            Some((from, to)) => {
                seg.start = to_secs(from);
                seg.end = to_secs(to);
                true
            }
            None => seg.end - seg.start < DROP_UNVOICED_SECS,
        }
    });
}

/// Decoding parameters shared by dictation and file jobs, so both treat
/// language, prompt, no-speech threshold and threads the same way.
fn whisper_params<'a>(
    language: Option<&'a str>,
    no_speech_thold: f32,
    initial_prompt: Option<&str>,
) -> FullParams<'a, 'static> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    params.set_n_threads(num_cpus_or_default());
    // Whisper's own estimate of how likely a segment is "no speech"; if
    // the probability exceeds this threshold, the segment is dropped.
    // Default is 0.6; the caller can tune from the Swift side.
    if no_speech_thold > 0.0 {
        params.set_no_speech_thold(no_speech_thold);
    }
    if let Some(lang) = language {
        if !lang.is_empty() {
            params.set_language(Some(lang));
        }
    }
    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }
    }
    params
}

fn num_cpus_or_default() -> i32 {
    // FASTWORD_THREADS overrides; otherwise physical cores. On SMT machines
    // (e.g. 18C/36T Xeons) logical-core counts oversubscribe the memory
    // bus and are measurably slower for whisper.
    if let Some(n) = std::env::var("FASTWORD_THREADS")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    num_cpus::get_physical().max(1) as i32
}

fn decode_pcm(b64: &str) -> Result<Vec<f32>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("base64")?;
    if raw.len() % 4 != 0 {
        return Err(anyhow!("audio length not multiple of 4 bytes"));
    }
    let mut floats = Vec::with_capacity(raw.len() / 4);
    extend_f32_le(&mut floats, &raw);
    Ok(floats)
}

/// Appends little-endian f32 samples; `bytes.len()` is a multiple of 4.
/// A NaN or infinity (a corrupt float WAV/CAF, which AVFoundation and
/// ffmpeg pass through as is) becomes silence: left in, a single one
/// silently empties the whole whisper window or GigaAM chunk it lands in.
fn extend_f32_le(out: &mut Vec<f32>, bytes: &[u8]) {
    out.extend(bytes.chunks_exact(4).map(|b| {
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        if v.is_finite() {
            v
        } else {
            0.0
        }
    }));
}

/// Reads the headerless little-endian f32 file behind `audio_path`. An hour
/// at 16 kHz is ~230 MB, so it is converted block by block straight into the
/// sample buffer rather than held in memory twice.
fn read_pcm_file(path: &Path) -> Result<Vec<f32>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow!("audio file not found: {}", path.display()));
        }
        Err(e) => return Err(anyhow!("cannot access audio file {}: {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(anyhow!("audio path is not a file: {}", path.display()));
    }
    let len = meta.len();
    if len % 4 != 0 {
        return Err(anyhow!(
            "audio file {} is {len} bytes, not a whole number of f32 samples",
            path.display()
        ));
    }
    let mut file =
        File::open(path).map_err(|e| anyhow!("cannot open audio file {}: {e}", path.display()))?;
    let mut samples = Vec::with_capacity((len / 4) as usize);
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..n])
            .map_err(|e| anyhow!("cannot read audio file {}: {e}", path.display()))?;
        extend_f32_le(&mut samples, &buf[..n]);
        left -= n as u64;
    }
    Ok(samples)
}

struct Engines {
    whisper: Mutex<ModelHolder>,
    gigaam_dir: Option<PathBuf>,
    gigaam: Mutex<Option<gigaam::Recognizer>>,
}

impl Engines {
    fn new(whisper_path: PathBuf, gigaam_dir: Option<PathBuf>) -> Self {
        Self {
            whisper: Mutex::new(ModelHolder::new(whisper_path)),
            gigaam_dir,
            gigaam: Mutex::new(None),
        }
    }

    fn evict_if_idle(&self, idle_after: Duration) {
        self.whisper.lock().unwrap().evict_if_idle(idle_after);
        // GigaAM doesn't expose a 'free' equivalent for OfflineRecognizer; the
        // recognizer holds the ONNX session for its lifetime. We could drop
        // the inner Option to free it, but loading takes 5-10 seconds, so for
        // now we keep it warm. (TODO: optional eviction in the future.)
    }

    /// Runs `f` with the GigaAM recognizer, loading it on first use.
    fn with_gigaam<T>(&self, f: impl FnOnce(&gigaam::Recognizer) -> Result<T>) -> Result<T> {
        let mut guard = self.gigaam.lock().unwrap();
        if guard.is_none() {
            let dir = self
                .gigaam_dir
                .as_deref()
                .ok_or_else(|| anyhow!("GigaAM model directory not configured (FASTWORD_GIGAAM_MODEL)"))?;
            log(&format!("loading GigaAM from {}", dir.display()));
            let rec = gigaam::Recognizer::load(dir)?;
            log("GigaAM loaded");
            *guard = Some(rec);
        }
        f(guard.as_ref().unwrap())
    }

    fn run_gigaam(&self, audio: &[f32]) -> Result<String> {
        self.with_gigaam(|rec| rec.transcribe(audio, 16_000))
    }
}

/// Which inference backend this binary was compiled with. The client uses
/// this to suggest a GPU build when it finds a capable GPU next to a
/// CPU-only sidecar.
fn backend_name() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else if cfg!(target_os = "macos") {
        "metal"
    } else {
        "cpu"
    }
}

/// What a successful command produces. Only `transcribe_file` carries
/// timestamps; everything else stays the bare text line it always was.
enum Reply {
    Text(String),
    Transcript(FileTranscript),
}

fn handle(req: Request, engines: &Arc<Engines>) -> Response {
    let id = req.id.clone();
    let res: Result<Reply> = (|| {
        match req.cmd.as_str() {
            "info" => Ok(Reply::Text(format!(
                "fastword-sidecar {} backend={}",
                env!("CARGO_PKG_VERSION"),
                backend_name()
            ))),
            "warmup" => {
                let silence = vec![0.0f32; 16_000 / 2]; // 0.5s
                match req.engine.as_deref() {
                    // Preload the engine that will actually serve requests —
                    // a GigaAM-only install has no whisper model at all.
                    Some("gigaam") => {
                        let _ = engines.run_gigaam(&silence)?;
                    }
                    _ => {
                        let mut h = engines.whisper.lock().unwrap();
                        let _ = h.transcribe(&silence, None, 0.0, None)?;
                    }
                }
                Ok(Reply::Text(String::new()))
            }
            "transcribe" => {
                let audio_b64 = req
                    .audio_b64
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing audio_b64"))?;
                let audio = decode_pcm(audio_b64)?;
                if audio.len() < 1600 {
                    return Ok(Reply::Text(String::new()));
                }
                let engine = req.engine.as_deref().unwrap_or("whisper");
                match engine {
                    "gigaam" => engines.run_gigaam(&audio),
                    _ => {
                        let thold = req.no_speech_thold.unwrap_or(0.6);
                        let mut h = engines.whisper.lock().unwrap();
                        h.transcribe(
                            &audio,
                            req.language.as_deref(),
                            thold,
                            req.initial_prompt.as_deref(),
                        )
                    }
                }
                .map(Reply::Text)
            }
            "transcribe_file" => transcribe_file(&req, engines).map(Reply::Transcript),
            other => Err(anyhow!("unknown cmd: {other}")),
        }
    })();

    match res {
        Ok(Reply::Text(text)) => Response::ok(id, text),
        Ok(Reply::Transcript(t)) => Response::transcript(id, t),
        Err(err) => {
            log(&format!("error: {err:?}"));
            Response::err(id, err.to_string())
        }
    }
}

/// `transcribe_file`: a long recording from a raw f32 file. Writes progress
/// lines for `req.id` while the engine runs; the caller writes the final one.
fn transcribe_file(req: &Request, engines: &Engines) -> Result<FileTranscript> {
    let path = req
        .audio_path
        .as_deref()
        .ok_or_else(|| anyhow!("missing audio_path"))?;
    let rate = req.sample_rate.unwrap_or(16_000);
    if rate != 16_000 {
        return Err(anyhow!(
            "unsupported sample_rate {rate}: transcribe_file expects 16000 Hz audio"
        ));
    }
    let engine = req.engine.as_deref().unwrap_or("whisper");
    if engine != "gigaam" {
        // Refuse an over-long file before reading gigabytes of it.
        if let Ok(meta) = std::fs::metadata(path) {
            check_whisper_length(meta.len() / 4)?;
        }
    }
    let diarize = req.diarize == Some(true);
    let audio = read_pcm_file(Path::new(path))?;
    if audio.len() < 1600 {
        let mut t = FileTranscript::plain(Vec::new());
        if diarize {
            t.speakers = Some(Vec::new());
        }
        return Ok(t);
    }

    let started = Instant::now();
    let id = req.id.clone();
    let progress = Arc::new(Mutex::new(Overall::new(move |p| {
        // A closed stdout shows up again on the final line, where main()
        // stops on it; nothing useful to do about it mid-job.
        let _ = emit(&Response::progress(id.clone(), p));
    })));

    // Diarization runs in its own process while this one transcribes.
    let num_speakers = req.num_speakers.unwrap_or(0);
    let job: Option<Result<diarize::Job, String>> = (diarize && num_speakers != 1).then(|| {
        let dir = diarize::model_dir().ok_or("the speaker models are not installed")?;
        let p = Arc::clone(&progress);
        diarize::Job::start(path, num_speakers, &dir, move |f| {
            p.lock().unwrap().diarization(f)
        })
        .map_err(|e| format!("could not start the speaker detector: {e}"))
    });
    if matches!(job, Some(Ok(_))) {
        progress.lock().unwrap().diarization(Some(0.0));
    }

    let asr = {
        let p = Arc::clone(&progress);
        move |f: f64| p.lock().unwrap().asr(f)
    };
    let segments = match engine {
        "gigaam" => {
            engines.with_gigaam(|rec| rec.transcribe_long(&audio, 16_000, &mut |f| asr(f)))?
        }
        _ => {
            let thold = req.no_speech_thold.unwrap_or(0.6);
            let mut h = engines.whisper.lock().unwrap();
            h.transcribe_file(
                &audio,
                req.language.as_deref(),
                thold,
                req.initial_prompt.as_deref(),
                diarize,
                move |pct| asr(pct.max(0) as f64 / 100.0),
            )?
        }
    };
    log(&format!(
        "transcribe_file ({engine}): {:.1}s of audio in {:.1}s, {} segments",
        audio.len() as f64 / 16_000.0,
        started.elapsed().as_secs_f64(),
        segments.len()
    ));

    // The plain text is the transcript as recognized, before any segment is
    // cut at a speaker change (a cut inside Chinese text has no space).
    let mut t = FileTranscript::plain(segments);
    if !diarize {
        return Ok(t);
    }
    progress.lock().unwrap().asr(1.0);
    let turns = match job {
        // One speaker by the user's word: nothing to detect.
        None => Ok(Vec::new()),
        Some(Err(reason)) => Err(reason),
        Some(Ok(job)) => {
            // Generous: diarization runs at ~0.04 of real time. This only
            // catches a child that hangs instead of crashing.
            let audio_secs = audio.len() as f64 / 16_000.0;
            job.finish(started + Duration::from_secs_f64(120.0 + audio_secs))
        }
    };
    // A child that dies from here on must not add a progress line after
    // the final one.
    progress.lock().unwrap().close();
    match turns {
        Ok(turns) => {
            let (segments, speakers) = merge::attribute(std::mem::take(&mut t.segments), &turns);
            log(&format!(
                "diarized: {} turns, {} speakers, {} segments, {:.1}s in all",
                turns.len(),
                speakers.len(),
                segments.len(),
                started.elapsed().as_secs_f64()
            ));
            t.segments = segments;
            t.speakers = Some(speakers);
        }
        Err(reason) => {
            log(&format!("diarization failed: {reason}"));
            t.warning = Some(format!("Speaker detection failed: {reason}."));
        }
    }
    Ok(t)
}

/// Progress of a file job: the recognizer's, or with diarization running
/// alongside, the average of both — so the bar reaches the end when the
/// slower of the two does. Nothing goes out before the recognizer's first
/// report, the client's sign that its model is loaded.
struct Overall<F: FnMut(f64)> {
    progress: Progress<F>,
    asr: Option<f64>,
    /// `None` without diarization, or once it has failed: from then on the
    /// bar follows the recognizer alone (holding still until it catches up).
    diarization: Option<f64>,
    closed: bool,
}

impl<F: FnMut(f64)> Overall<F> {
    fn new(sink: F) -> Self {
        Self {
            progress: Progress::new(sink),
            asr: None,
            diarization: None,
            closed: false,
        }
    }

    /// No more progress lines: the job's result is about to go out.
    fn close(&mut self) {
        self.closed = true;
    }

    fn asr(&mut self, f: f64) {
        self.asr = Some(f);
        self.update();
    }

    /// The child's progress, or `None` when it failed.
    fn diarization(&mut self, f: Option<f64>) {
        self.diarization = f;
        self.update();
    }

    fn update(&mut self) {
        let Some(asr) = self.asr.filter(|_| !self.closed) else {
            return;
        };
        match self.diarization {
            Some(d) => self
                .progress
                .fraction((asr.clamp(0.0, 1.0) + d.clamp(0.0, 1.0)) / 2.0),
            None => self.progress.fraction(asr),
        }
    }
}

fn evict_loop(engines: Arc<Engines>, idle_after: Duration) {
    loop {
        thread::sleep(Duration::from_secs(30));
        engines.evict_if_idle(idle_after);
    }
}

/// Exits as soon as the app that started us is gone. An idle sidecar sees
/// stdin close and stops by itself, but one in the middle of a long
/// `transcribe_file` only reads stdin again when the job is done — after an
/// app crash or force-quit it would keep the GPU busy for minutes, for
/// nobody. `_exit` skips the ggml-metal static teardown that aborts while a
/// model is still loaded (see main).
#[cfg(unix)]
fn exit_with_parent() {
    let parent = std::os::unix::process::parent_id();
    // Started by launchd/init itself: there is no app to outlive.
    if parent == 1 {
        return;
    }
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let now = std::os::unix::process::parent_id();
        // An orphan is re-parented. On macOS always to launchd — there are
        // no subreapers — and a mere change of parent means nothing: a
        // debugger attaching (lldb -p, Instruments) becomes the parent for
        // as long as it stays. Elsewhere a subreaper (systemd --user, a
        // container's init) may adopt it instead of pid 1.
        let orphaned = if cfg!(target_os = "macos") {
            now == 1
        } else {
            now != parent
        };
        if orphaned {
            hard_exit(0);
        }
    });
}

/// Ends the process at once, from any thread, without running exit-time
/// teardown: static destructors of the native libraries (ggml-metal, ONNX
/// Runtime) may abort while another thread still uses them.
fn hard_exit(code: i32) -> ! {
    #[cfg(unix)]
    {
        extern "C" {
            fn _exit(status: i32) -> !;
        }
        unsafe { _exit(code) }
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

fn main() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == diarize::CHILD_ARG)
    {
        #[cfg(unix)]
        exit_with_parent();
        diarize::child_main();
    }
    // Suppress whisper.cpp's own stderr noise once the model is loaded.
    whisper_rs::install_logging_hooks();

    let model_path = PathBuf::from(
        std::env::var("FASTWORD_MODEL")
            .unwrap_or_else(|_| String::from("models/ggml-large-v3-turbo-q5_0.bin")),
    );
    // Missing whisper model is not fatal: it is loaded lazily on the first
    // whisper request, and a GigaAM-only setup never makes one.
    if !model_path.exists() {
        log(&format!(
            "whisper model not found at {} — whisper requests will fail until it exists",
            model_path.display()
        ));
    }

    let idle_after_secs: u64 = std::env::var("FASTWORD_IDLE_EVICT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);

    let gigaam_dir = gigaam::default_model_dir();
    let engines = Arc::new(Engines::new(model_path, gigaam_dir));
    {
        let e = Arc::clone(&engines);
        thread::spawn(move || evict_loop(e, Duration::from_secs(idle_after_secs)));
    }

    #[cfg(unix)]
    exit_with_parent();

    log("ready");
    let result = serve(&engines);

    // Free the whisper context before exiting, however serve() ended. The
    // evict thread keeps `engines` alive past main, and ggml-metal's static
    // teardown asserts that every GPU buffer is already gone — so exiting
    // with a model loaded used to end in SIGABRT (a crash report per job).
    if let Ok(mut holder) = engines.whisper.lock() {
        holder.ctx = None;
    }
    match result {
        // The app closed our stdout mid-reply: there is nobody left to tell,
        // and main's error report would abort on the closed stderr too.
        Err(e)
            if e.downcast_ref::<io::Error>().map(io::Error::kind)
                == Some(io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// The request loop: one JSON request per stdin line, until EOF.
fn serve(engines: &Arc<Engines>) -> Result<()> {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("stdin read")?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(err) => {
                emit(&Response::err(String::new(), format!("bad json: {err}")))?;
                continue;
            }
        };
        emit(&handle(req, engines))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_pcm(samples: &[f32]) -> String {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn decode_pcm_roundtrip() {
        let samples = vec![0.0f32, 1.0, -1.0, 0.5, -0.25];
        let decoded = decode_pcm(&encode_pcm(&samples)).unwrap();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn decode_pcm_empty() {
        assert_eq!(decode_pcm("").unwrap(), Vec::<f32>::new());
    }

    #[test]
    fn decode_pcm_rejects_bad_base64() {
        assert!(decode_pcm("not base64!!!").is_err());
    }

    #[test]
    fn decode_pcm_rejects_unaligned_length() {
        // 3 bytes is not a whole number of f32 samples.
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        assert!(decode_pcm(&b64).is_err());
    }

    #[test]
    fn request_parses_minimal_transcribe() {
        let req: Request =
            serde_json::from_str(r#"{"id":"1","cmd":"transcribe","audio_b64":"AAAAAA=="}"#)
                .unwrap();
        assert_eq!(req.id, "1");
        assert_eq!(req.cmd, "transcribe");
        assert_eq!(req.audio_b64.as_deref(), Some("AAAAAA=="));
        assert!(req.language.is_none());
        assert!(req.engine.is_none());
    }

    #[test]
    fn request_parses_full_swift_payload() {
        // Mirrors what SidecarClient.swift actually sends.
        let req: Request = serde_json::from_str(
            r#"{"id":"abc","cmd":"transcribe","sample_rate":16000,"audio_b64":"AAAAAA==",
                "language":"ru","no_speech_thold":0.6,
                "initial_prompt":"Это голосовая заметка на русском.","engine":"gigaam"}"#,
        )
        .unwrap();
        assert_eq!(req.sample_rate, Some(16000));
        assert_eq!(req.language.as_deref(), Some("ru"));
        assert_eq!(req.engine.as_deref(), Some("gigaam"));
    }

    #[test]
    fn request_rejects_unknown_fields() {
        let res: Result<Request, _> =
            serde_json::from_str(r#"{"id":"1","cmd":"transcribe","bogus":true}"#);
        assert!(res.is_err());
    }

    #[test]
    fn info_reports_version_and_backend() {
        // `info` must not touch the model, so a dummy path is fine.
        let engines = Arc::new(Engines::new(PathBuf::from("does-not-exist.bin"), None));
        let req: Request = serde_json::from_str(r#"{"id":"i","cmd":"info"}"#).unwrap();
        let resp = handle(req, &engines);
        assert!(resp.error.is_none());
        let text = resp.text.unwrap();
        assert!(text.starts_with("fastword-sidecar "), "{text}");
        assert!(text.contains("backend="), "{text}");
    }

    #[test]
    fn response_serializes_without_null_fields() {
        let ok = Response {
            id: "1".into(),
            text: Some("hello".into()),
            error: None,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":"1","text":"hello"}"#
        );
        let err = Response {
            id: "2".into(),
            text: None,
            error: Some("boom".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"id":"2","error":"boom"}"#
        );
    }

    #[test]
    fn response_constructors_match_old_wire_format() {
        assert_eq!(
            serde_json::to_string(&Response::ok("1".into(), "hello".into())).unwrap(),
            r#"{"id":"1","text":"hello"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::err("2".into(), "boom".into())).unwrap(),
            r#"{"id":"2","error":"boom"}"#
        );
    }

    #[test]
    fn progress_line_serializes_alone() {
        assert_eq!(
            serde_json::to_string(&Response::progress("p".into(), 0.37)).unwrap(),
            r#"{"id":"p","progress":0.37}"#
        );
    }

    #[test]
    fn transcript_serializes_text_and_segments() {
        let segments = vec![
            Segment::new(0.0, 4.2, "Hello there.".into()),
            Segment::new(4.5, 6.0, "Bye.".into()),
        ];
        let json = serde_json::to_string(&Response::transcript(
            "a".into(),
            FileTranscript::plain(segments.clone()),
        ))
        .unwrap();
        assert_eq!(
            json,
            r#"{"id":"a","text":"Hello there. Bye.","segments":[{"start":0.0,"end":4.2,"text":"Hello there."},{"start":4.5,"end":6.0,"text":"Bye."}]}"#
        );
        // And it reads back as the same segments (what the Swift side decodes).
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let back: Vec<Segment> = serde_json::from_value(v["segments"].clone()).unwrap();
        assert_eq!(back, segments);
    }

    #[test]
    fn empty_transcript_keeps_empty_text_and_segments() {
        assert_eq!(
            serde_json::to_string(&Response::transcript(
                "e".into(),
                FileTranscript::plain(Vec::new())
            ))
            .unwrap(),
            r#"{"id":"e","text":"","segments":[]}"#
        );
    }

    #[test]
    fn progress_is_throttled_to_whole_percent_steps() {
        let mut seen = Vec::new();
        {
            let mut p = Progress::new(|v| seen.push(v));
            for f in [0.0, 0.004, 0.009, 0.01, 0.015, 0.29, 0.28, 0.5, f64::NAN, 1.7, 1.0] {
                p.fraction(f);
            }
        }
        assert_eq!(seen, vec![0.0, 0.01, 0.29, 0.5, 1.0]);

        let mut seen = Vec::new();
        {
            let mut p = Progress::new(|v| seen.push(v));
            for pct in [0, 0, 3, 3, 2, 37, 100, 100, 250] {
                p.percent(pct);
            }
        }
        assert_eq!(seen, vec![0.0, 0.03, 0.37, 1.0]);

        // A first report above 0% goes out as is.
        let mut seen = Vec::new();
        {
            let mut p = Progress::new(|v| seen.push(v));
            p.percent(15);
            p.percent(15);
        }
        assert_eq!(seen, vec![0.15]);
    }

    fn fitted(audio: &[f32], times: &[(f64, f64)]) -> Vec<(f64, f64)> {
        let mut segments: Vec<Segment> = times
            .iter()
            .map(|&(start, end)| Segment::new(start, end, "x".into()))
            .collect();
        fit_to_voice(&segment::VoicedFrames::analyze(audio, 16_000), &mut segments);
        segments.iter().map(|s| (s.start, s.end)).collect()
    }

    /// 220 Hz at -10 dBFS for `secs` seconds.
    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * 16_000.0) as usize)
            .map(|i| 0.3 * (i as f32 * 0.0864).sin())
            .collect()
    }

    #[test]
    fn fit_to_voice_snaps_boundaries_into_pauses() {
        // Speech 0–2 s, pause 2–4 s, continuous speech 4–9 s, silence 9–10 s.
        let mut audio = tone(2.0);
        audio.extend(vec![0.0; 32_000]);
        audio.extend(tone(5.0));
        audio.extend(vec![0.0; 16_000]);
        // whisper-style back-to-back segments: the first boundary is 0.1 s
        // early (a sliver of the first sentence would stay in the second
        // segment), the second sits in continuous speech, far from a pause.
        assert_eq!(
            fitted(&audio, &[(0.0, 1.9), (1.9, 6.5), (6.5, 10.0)]),
            vec![(0.0, 2.0), (4.0, 6.5), (6.5, 9.0)]
        );
        // A boundary inside the pause, or with a gap already there.
        assert_eq!(fitted(&audio, &[(0.0, 3.0), (3.0, 9.0)]), vec![(0.0, 2.0), (4.0, 9.0)]);
        assert_eq!(fitted(&audio, &[(0.0, 2.5), (3.8, 9.0)]), vec![(0.0, 2.0), (4.0, 9.0)]);
        // More than a second from the pause: whisper's boundary stands.
        assert_eq!(fitted(&audio, &[(0.0, 0.8), (0.8, 9.0)]), vec![(0.0, 0.8), (0.8, 9.0)]);
    }

    #[test]
    fn fit_to_voice_leaves_what_it_cannot_place() {
        let mut audio = tone(2.0);
        audio.extend(vec![0.0; 16_000]);
        // Short silence-only and empty segments keep whisper's times; a
        // lone segment is trimmed to its speech.
        assert_eq!(fitted(&audio, &[(2.2, 3.0)]), vec![(2.2, 3.0)]);
        assert_eq!(fitted(&audio, &[(1.0, 1.0)]), vec![(1.0, 1.0)]);
        assert_eq!(fitted(&audio, &[(0.0, 3.0)]), vec![(0.0, 2.0)]);
        assert!(fitted(&audio, &[]).is_empty());
    }

    #[test]
    fn fit_to_voice_drops_segments_over_silence() {
        // Speech 0–2 s, then 8 s of silence that whisper fills with a
        // hallucinated line or two.
        let mut audio = tone(2.0);
        audio.extend(vec![0.0; 8 * 16_000]);
        assert_eq!(fitted(&audio, &[(0.0, 2.1), (2.1, 10.0)]), vec![(0.0, 2.0)]);
        assert_eq!(
            fitted(&audio, &[(0.0, 2.1), (3.0, 4.0), (4.0, 10.0)]),
            vec![(0.0, 2.0)]
        );
        // Just under a second it stays.
        assert_eq!(
            fitted(&audio, &[(0.0, 2.1), (3.0, 3.99)]),
            vec![(0.0, 2.0), (3.0, 3.99)]
        );
        // Nothing but silence: nothing left.
        assert!(fitted(&vec![0.0; 10 * 16_000], &[(0.0, 5.0), (5.0, 10.0)]).is_empty());
    }

    /// A file in the system temp dir that is removed when dropped.
    struct TempFile(PathBuf);

    impl TempFile {
        fn with_bytes(bytes: &[u8]) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "fastword-sidecar-test-{}-{}.f32",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }

        fn with_samples(samples: &[f32]) -> Self {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            Self::with_bytes(&bytes)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn dummy_engines() -> Arc<Engines> {
        Arc::new(Engines::new(PathBuf::from("does-not-exist.bin"), None))
    }

    fn file_request(id: &str, path: &str, extra: &str) -> Request {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","cmd":"transcribe_file","audio_path":{}{extra}}}"#,
            serde_json::to_string(path).unwrap()
        ))
        .unwrap()
    }

    #[test]
    fn request_parses_transcribe_file() {
        let req: Request = serde_json::from_str(
            r#"{"id":"abc","cmd":"transcribe_file","audio_path":"/tmp/x.f32","sample_rate":16000,
                "language":"ru","engine":"gigaam","initial_prompt":"...","no_speech_thold":0.6}"#,
        )
        .unwrap();
        assert_eq!(req.cmd, "transcribe_file");
        assert_eq!(req.audio_path.as_deref(), Some("/tmp/x.f32"));
        assert_eq!(req.sample_rate, Some(16000));
        assert_eq!(req.engine.as_deref(), Some("gigaam"));
        assert!(req.audio_b64.is_none());
    }

    #[test]
    fn transcribe_file_requires_audio_path() {
        let req: Request = serde_json::from_str(r#"{"id":"f","cmd":"transcribe_file"}"#).unwrap();
        let resp = handle(req, &dummy_engines());
        assert_eq!(resp.id, "f");
        assert_eq!(resp.error.as_deref(), Some("missing audio_path"));
        assert!(resp.text.is_none() && resp.segments.is_none());
    }

    #[test]
    fn transcribe_file_reports_missing_file_with_path() {
        let path = std::env::temp_dir().join("fastword-sidecar-test-definitely-missing.f32");
        let req = file_request("m", path.to_str().unwrap(), "");
        let resp = handle(req, &dummy_engines());
        let err = resp.error.expect("error");
        assert!(err.contains(path.to_str().unwrap()), "{err}");
        assert!(resp.text.is_none());
    }

    #[test]
    fn transcribe_file_rejects_unaligned_file() {
        let file = TempFile::with_bytes(&[0u8; 4 * 2000 + 3]);
        let resp = handle(file_request("u", file.path(), ""), &dummy_engines());
        let err = resp.error.expect("error");
        assert!(err.contains(file.path()), "{err}");
    }

    #[test]
    fn transcribe_file_rejects_other_sample_rates() {
        let file = TempFile::with_samples(&vec![0.0; 48_000]);
        let resp = handle(
            file_request("r", file.path(), r#","sample_rate":48000"#),
            &dummy_engines(),
        );
        let err = resp.error.expect("error");
        assert!(err.contains("48000"), "{err}");
    }

    #[test]
    fn transcribe_file_tiny_file_is_empty_transcript() {
        // Below 1600 samples nothing reaches an engine — the dummy model
        // path would fail otherwise.
        let file = TempFile::with_samples(&vec![0.1; 1000]);
        let resp = handle(
            file_request("t", file.path(), r#","sample_rate":16000"#),
            &dummy_engines(),
        );
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"id":"t","text":"","segments":[]}"#
        );
    }

    #[test]
    fn read_pcm_file_roundtrip() {
        let samples: Vec<f32> = (0..300_000).map(|i| ((i % 1000) as f32 / 500.0) - 1.0).collect();
        let file = TempFile::with_samples(&samples);
        assert_eq!(read_pcm_file(Path::new(file.path())).unwrap(), samples);
        let empty = TempFile::with_bytes(&[]);
        assert!(read_pcm_file(Path::new(empty.path())).unwrap().is_empty());
    }

    #[test]
    fn read_pcm_file_turns_non_finite_samples_into_silence() {
        let file = TempFile::with_samples(&[0.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.2]);
        assert_eq!(
            read_pcm_file(Path::new(file.path())).unwrap(),
            vec![0.1, 0.0, 0.0, 0.0, -0.2]
        );
    }

    #[test]
    fn whisper_length_limit() {
        assert!(check_whisper_length(i32::MAX as u64).is_ok());
        let err = check_whisper_length(i32::MAX as u64 + 1).unwrap_err().to_string();
        assert!(err.contains("at most 37 h") && err.contains("GigaAM"), "{err}");
    }

    #[test]
    fn transcribe_file_refuses_over_long_file_before_reading_it() {
        // Sparse: 8 GB on paper, nothing on disk. Reading it would take
        // that much memory; the size check must come first.
        let file = TempFile::with_bytes(&[]);
        File::options()
            .write(true)
            .open(file.path())
            .unwrap()
            .set_len((i32::MAX as u64 + 1) * 4)
            .unwrap();
        let resp = handle(file_request("l", file.path(), ""), &dummy_engines());
        let err = resp.error.expect("error");
        assert!(err.contains("Whisper engine"), "{err}");
    }

    #[test]
    fn read_pcm_file_rejects_directories() {
        let dir = std::env::temp_dir();
        let err = read_pcm_file(&dir).unwrap_err().to_string();
        assert!(err.contains("not a file"), "{err}");
    }

    #[test]
    fn request_parses_diarize_fields() {
        let req: Request = serde_json::from_str(
            r#"{"id":"d","cmd":"transcribe_file","audio_path":"/tmp/x.f32","diarize":true,"num_speakers":3}"#,
        )
        .unwrap();
        assert_eq!(req.diarize, Some(true));
        assert_eq!(req.num_speakers, Some(3));
        let req: Request = serde_json::from_str(
            r#"{"id":"d","cmd":"transcribe_file","audio_path":"/x","diarize":true}"#,
        )
        .unwrap();
        assert_eq!(req.num_speakers, None);
        let req = file_request("p", "/x", "");
        assert!(req.diarize.is_none() && req.num_speakers.is_none());
        // A count must be a non-negative integer.
        assert!(serde_json::from_str::<Request>(
            r#"{"id":"d","cmd":"transcribe_file","num_speakers":-1}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Request>(
            r#"{"id":"d","cmd":"transcribe_file","num_speakers":"2"}"#
        )
        .is_err());
    }

    #[test]
    fn diarized_transcript_serializes_speakers_and_warning() {
        let mut a = Segment::new(0.0, 2.0, "Hi.".into());
        a.speaker = Some(0);
        a.words = vec![Word {
            start: 0.0,
            end: 1.0,
            at: 0,
        }];
        let mut b = Segment::new(2.5, 4.0, "Hello.".into());
        b.speaker = Some(1);
        let t = FileTranscript {
            text: "Hi. Hello.".into(),
            segments: vec![a, b],
            speakers: Some(vec![
                SpeakerTime {
                    id: 0,
                    seconds: 2.0,
                },
                SpeakerTime {
                    id: 1,
                    seconds: 1.5,
                },
            ]),
            warning: None,
        };
        assert_eq!(
            serde_json::to_string(&Response::transcript("s".into(), t)).unwrap(),
            r#"{"id":"s","text":"Hi. Hello.","segments":[{"start":0.0,"end":2.0,"text":"Hi.","speaker":0},{"start":2.5,"end":4.0,"text":"Hello.","speaker":1}],"speakers":[{"id":0,"seconds":2.0},{"id":1,"seconds":1.5}]}"#
        );
        let mut t = FileTranscript::plain(vec![Segment::new(0.0, 1.0, "x".into())]);
        t.warning = Some("Speaker detection failed: boom.".into());
        assert_eq!(
            serde_json::to_string(&Response::transcript("w".into(), t)).unwrap(),
            r#"{"id":"w","text":"x","segments":[{"start":0.0,"end":1.0,"text":"x"}],"warning":"Speaker detection failed: boom."}"#
        );
    }

    #[test]
    fn diarized_tiny_file_has_empty_speakers() {
        let file = TempFile::with_samples(&vec![0.1; 1000]);
        let resp = handle(
            file_request("t", file.path(), r#","diarize":true"#),
            &dummy_engines(),
        );
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"id":"t","text":"","segments":[],"speakers":[]}"#
        );
    }

    #[test]
    fn overall_progress_averages_and_stops_when_closed() {
        let mut seen = Vec::new();
        {
            let mut p = Overall::new(|v| seen.push(v));
            // Nothing before the recognizer's first report.
            p.diarization(Some(0.0));
            p.diarization(Some(0.3));
            p.asr(0.0);
            p.asr(0.5);
            p.diarization(Some(0.9));
            // Diarization failed: the recognizer alone, never backwards.
            p.diarization(None);
            p.asr(0.6);
            p.asr(0.8);
            p.close();
            p.asr(1.0);
        }
        assert_eq!(seen, vec![0.15, 0.4, 0.7, 0.8]);

        // Without diarization the recognizer's progress passes through as
        // before (whole percents, increasing).
        let mut seen = Vec::new();
        {
            let mut p = Overall::new(|v| seen.push(v));
            for pct in [0, 0, 3, 29, 28, 100] {
                p.asr(pct as f64 / 100.0);
            }
        }
        assert_eq!(seen, vec![0.0, 0.03, 0.29, 1.0]);
    }

    #[test]
    fn dtw_presets_follow_standard_model_names() {
        let preset = |name: &str| dtw_preset(Path::new(name)).map(|p| format!("{p:?}"));
        assert_eq!(
            preset("/m/ggml-large-v3-turbo-q5_0.bin").as_deref(),
            Some("LargeV3Turbo")
        );
        assert_eq!(
            preset("ggml-large-v3-turbo.bin").as_deref(),
            Some("LargeV3Turbo")
        );
        assert_eq!(preset("ggml-large-v3-q5_0.bin").as_deref(), Some("LargeV3"));
        assert_eq!(preset("ggml-large-v3.bin").as_deref(), Some("LargeV3"));
        assert_eq!(preset("ggml-tiny.bin").as_deref(), Some("Tiny"));
        assert_eq!(preset("ggml-tiny.en-q8_0.bin").as_deref(), Some("TinyEn"));
        assert_eq!(preset("ggml-medium.en.bin").as_deref(), Some("MediumEn"));
        assert_eq!(preset("GGML-SMALL.BIN").as_deref(), Some("Small"));
        // Other architectures or unknown names: no DTW.
        assert_eq!(preset("ggml-distil-large-v3.bin"), None);
        assert_eq!(preset("ggml-large.bin"), None);
        assert_eq!(preset("my-model.bin"), None);
        assert_eq!(preset("ggml-large-v3-turbo-custom.bin"), None);
    }
}
