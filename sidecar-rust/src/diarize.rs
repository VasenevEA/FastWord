// Speaker diarization ("who spoke when") for `transcribe_file`.
//
// sherpa-onnx's offline diarization (pyannote segmentation-3.0 + 3D-Speaker
// CAM++ embeddings) is native code that can take the whole process down:
// SIGBUS on some inputs (k2-fsa/sherpa-onnx issues #3253 and #1466, still
// open). A crash must never cost the user a transcript that took minutes to
// make, so diarization runs in a child process — this same binary started
// with `--diarize-child` — next to the recognizer: one request line in on
// stdin, `{"progress":p}` lines and then one result line out on stdout. If
// the child dies, the parent still has its transcript and reports a warning.
//
// The child exits by itself once its stdin closes, so killing the sidecar
// (the app's way to cancel a file job) takes the child with it.
//
// Only sherpa's segmentation is used as is. Its clustering is not reliable
// enough for speakers (see recluster.rs): the child cuts its turns at pauses,
// embeds each piece itself and clusters the pieces.

use std::ffi::{c_void, CString};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use sherpa_onnx_sys as sys;

use crate::merge::Turn;
use crate::recluster::{self, Piece, SpeakerCount};
use crate::segment::VoicedFrames;
use crate::Progress;

/// First argument that turns the sidecar binary into a diarization child.
pub const CHILD_ARG: &str = "--diarize-child";

/// File names inside FASTWORD_DIARIZATION_MODEL.
const SEGMENTATION_MODEL: &str = "segmentation.onnx";
const EMBEDDING_MODEL: &str = "embedding.onnx";

const SAMPLE_RATE: usize = 16_000;

/// sherpa's clustering threshold (its default). Its speakers are not used,
/// but stretches it puts in one cluster it also joins into one turn, so
/// this still decides which speaker changes survive as turn boundaries. With
/// the re-clustering below, 0.5 did as well as 0.3 and better than 0.6.
const SHERPA_THRESHOLD: f32 = 0.5;
/// pyannote windows (10 s) advance by this fraction of their length. sherpa
/// defaults to 0.1; 0.2 halves the time (RTF ≈ 0.04 instead of 0.08 on an
/// M1 Pro) without losing accuracy on the corpus, while 0.3, faster still,
/// placed boundaries worse (higher DER on every multi-speaker item).
const WINDOW_SHIFT_RATIO: f32 = 0.2;
/// sherpa's turns are cut at pauses at least this long before embedding:
/// two people taking turns with a short pause between them often come out
/// of sherpa as one turn when their voices are alike.
const PIECE_GAP_SECS: f64 = 0.15;
/// A piece shorter than this joins its neighbour in the same turn: its
/// embedding would be mostly noise. Short enough to keep a "Yes." or "Me
/// too." on its own — clustering folds a stray short piece into the voice
/// it is closest to anyway. On the corpus 0.5 s put a quarter fewer words
/// with the wrong speaker than 1 s, and DER was lower on all but one item.
const MIN_PIECE_SECS: f64 = 0.5;
/// Pieces are embedded in chunks of at most this length; longer audio does
/// not make one embedding better, and the model cost grows with it.
const MAX_CHUNK_SECS: f64 = 6.0;
/// Shorter stretches are not embedded at all.
const MIN_CHUNK_SECS: f64 = 0.3;

/// The model directory from FASTWORD_DIARIZATION_MODEL, if it holds both
/// models. The app sets it only once the models are installed.
pub fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("FASTWORD_DIARIZATION_MODEL")?);
    (dir.join(SEGMENTATION_MODEL).is_file() && dir.join(EMBEDDING_MODEL).is_file()).then_some(dir)
}

/// The request a child gets on its stdin.
#[derive(Debug, Serialize, Deserialize)]
struct ChildRequest {
    audio_path: String,
    /// 0 = decide automatically.
    num_speakers: u32,
    model_dir: String,
}

/// One line of child output: a progress report, or the final turns or
/// error. `stats` is for the log only.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ChildLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turns: Option<Vec<Turn>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<String>,
}

enum Event {
    Done(Result<Vec<Turn>, String>),
    /// The child's stdout closed without a result line.
    Eof,
}

/// A running diarization child.
pub struct Job {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
}

impl Job {
    /// Starts diarizing the raw f32 file at `audio_path`. `on_progress`
    /// gets the child's progress (0..1) from a reader thread, and `None`
    /// once it has failed.
    pub fn start(
        audio_path: &str,
        num_speakers: u32,
        model_dir: &Path,
        mut on_progress: impl FnMut(Option<f64>) + Send + 'static,
    ) -> io::Result<Job> {
        let exe = std::env::current_exe()?;
        let mut command = Command::new(exe);
        command
            .arg(CHILD_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // A console program started from a windowless one would get a
        // console window of its own.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let request = ChildRequest {
            audio_path: audio_path.to_string(),
            num_speakers,
            model_dir: model_dir.to_string_lossy().into_owned(),
        };
        let mut line = serde_json::to_string(&request).expect("request serializes");
        line.push('\n');
        if let Err(e) = stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }

        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<ChildLine>(&line) else {
                    continue;
                };
                if let Some(p) = msg.progress {
                    on_progress(Some(p));
                } else if let Some(turns) = msg.turns {
                    if let Some(stats) = msg.stats {
                        crate::log(&format!("diarization: {stats}"));
                    }
                    let _ = tx.send(Event::Done(Ok(turns)));
                    return;
                } else if let Some(err) = msg.error {
                    on_progress(None);
                    let _ = tx.send(Event::Done(Err(err)));
                    return;
                }
            }
            on_progress(None);
            let _ = tx.send(Event::Eof);
        });
        Ok(Job {
            child,
            stdin: Some(stdin),
            events,
        })
    }

    /// Waits for the child's turns until `deadline`. The error is a short
    /// reason for the transcript's warning.
    pub fn finish(mut self, deadline: Instant) -> Result<Vec<Turn>, String> {
        let event = self
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        // The child exits once its stdin closes, result or not.
        self.stdin = None;
        match event {
            Ok(Event::Done(result)) => result,
            Ok(Event::Eof) | Err(RecvTimeoutError::Disconnected) => {
                // Give a crashed child a moment to be reaped for its status.
                let status = wait_until(&mut self.child, Instant::now() + Duration::from_secs(5));
                Err(match status {
                    Some(status) => describe_exit(status),
                    None => "the speaker detector stopped responding".into(),
                })
            }
            Err(RecvTimeoutError::Timeout) => Err("speaker detection timed out".into()),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Never leave a child behind, whatever happened to the job.
        self.stdin = None;
        if wait_until(&mut self.child, Instant::now() + Duration::from_millis(500)).is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("the speaker detector crashed (signal {signal})");
        }
    }
    match status.code() {
        Some(0) => "the speaker detector exited without a result".into(),
        Some(code) => format!("the speaker detector failed (exit code {code})"),
        None => "the speaker detector crashed".into(),
    }
}

// ---------------------------------------------------------------- child

/// Entry point of `fastword-sidecar --diarize-child`. Never returns.
pub fn child_main() -> ! {
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        crate::hard_exit(0);
    }
    // The parent closes our stdin when it is done with us — or when it is
    // gone. Either way, stop at once, mid-inference or not.
    thread::spawn(|| {
        let mut rest = String::new();
        while io::stdin().lock().read_line(&mut rest).unwrap_or(0) > 0 {
            rest.clear();
        }
        crate::hard_exit(0);
    });
    let reply = match serde_json::from_str::<ChildRequest>(&line) {
        Ok(req) => match diarize_file(&req) {
            Ok((turns, stats)) => ChildLine {
                turns: Some(turns),
                stats: Some(stats),
                ..Default::default()
            },
            Err(e) => ChildLine {
                error: Some(e.to_string()),
                ..Default::default()
            },
        },
        Err(e) => ChildLine {
            error: Some(format!("bad request: {e}")),
            ..Default::default()
        },
    };
    let _ = write_line(&reply);
    crate::hard_exit(0);
}

fn write_line(line: &ChildLine) -> io::Result<()> {
    let mut text = serde_json::to_string(line).expect("ChildLine serializes");
    text.push('\n');
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// Shares of the child's progress bar: sherpa's segmentation (no progress
/// reports of its own, estimated from time), sherpa's embeddings (reported
/// through its callback), and the re-embedding of pieces here. Measured on
/// an M1 Pro: about 27%, 59% and 14% of the time.
const SEGMENTATION_SHARE: f64 = 0.25;
const SHERPA_EMBEDDING_SHARE: f64 = 0.6;
/// Expected segmentation time per second of audio, for the estimate: about
/// 0.013 measured on an M1 Pro under load. A slower machine's bar waits at
/// the end of the segmentation share; a faster one's jumps ahead.
const SEGMENTATION_SECS_PER_SEC: f64 = 0.015;

struct ChildProgress {
    progress: Mutex<Progress<fn(f64)>>,
    sherpa_started: std::sync::atomic::AtomicBool,
}

fn emit_progress(p: f64) {
    let _ = write_line(&ChildLine {
        progress: Some(p),
        ..Default::default()
    });
}

impl ChildProgress {
    fn report(&self, p: f64) {
        if let Ok(mut progress) = self.progress.lock() {
            progress.fraction(p);
        }
    }
}

type ProgressCallback = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;

extern "C" {
    // In the sherpa-onnx C API (and the static library we link), but not in
    // the Rust bindings, whose `process` takes no progress callback.
    fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
        sd: *const sys::OfflineSpeakerDiarization,
        samples: *const f32,
        n: i32,
        callback: Option<ProgressCallback>,
        arg: *mut c_void,
    ) -> *const sys::OfflineSpeakerDiarizationResult;
}

/// sherpa calls this once per embedding computed — after segmentation, which
/// has no reports of its own.
unsafe extern "C" fn on_sherpa_progress(done: i32, total: i32, arg: *mut c_void) -> i32 {
    let progress = &*(arg as *const ChildProgress);
    progress
        .sherpa_started
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if total > 0 {
        let f = (done as f64 / total as f64).clamp(0.0, 1.0);
        progress.report(SEGMENTATION_SHARE + f * SHERPA_EMBEDDING_SHARE);
    }
    0
}

/// A sherpa-onnx diarizer, through the C API so the progress callback can
/// be used.
struct Diarizer(*const sys::OfflineSpeakerDiarization);

impl Diarizer {
    fn create(model_dir: &Path, threads: i32) -> anyhow::Result<Self> {
        let path = |name: &str| -> anyhow::Result<CString> {
            let p = model_dir.join(name);
            if !p.is_file() {
                anyhow::bail!("speaker model {} is missing", p.display());
            }
            Ok(CString::new(p.to_string_lossy().into_owned())?)
        };
        let (segmentation, embedding) = (path(SEGMENTATION_MODEL)?, path(EMBEDDING_MODEL)?);
        let cpu = CString::new("cpu").expect("no NUL");
        let config = sys::OfflineSpeakerDiarizationConfig {
            segmentation: sys::OfflineSpeakerSegmentationModelConfig {
                pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: segmentation.as_ptr(),
                    window_shift_ratio: WINDOW_SHIFT_RATIO,
                },
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            embedding: sys::SpeakerEmbeddingExtractorConfig {
                model: embedding.as_ptr(),
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            // Never a fixed cluster count: sherpa's num_clusters returns
            // fewer speakers than asked, and 1 crashed older releases.
            clustering: sys::FastClusteringConfig {
                num_clusters: -1,
                threshold: SHERPA_THRESHOLD,
                compute_confidence: 0,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        // sherpa copies the strings during creation.
        let sd = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        if sd.is_null() {
            anyhow::bail!("could not load the speaker models");
        }
        Ok(Self(sd))
    }

    /// Raw turns `(start, end, sherpa speaker)` in seconds, sorted by start.
    fn process(
        &self,
        audio: &[f32],
        progress: &ChildProgress,
    ) -> anyhow::Result<Vec<(f64, f64, i32)>> {
        let n =
            i32::try_from(audio.len()).map_err(|_| anyhow::anyhow!("the recording is too long"))?;
        let result = unsafe {
            SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
                self.0,
                audio.as_ptr(),
                n,
                Some(on_sherpa_progress),
                progress as *const ChildProgress as *mut c_void,
            )
        };
        if result.is_null() {
            anyhow::bail!("speaker segmentation failed");
        }
        let mut turns = Vec::new();
        unsafe {
            let count = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result);
            if count > 0 {
                let segs = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
                if !segs.is_null() {
                    for s in std::slice::from_raw_parts(segs, count as usize) {
                        turns.push((s.start as f64, s.end as f64, s.speaker));
                    }
                    sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segs);
                }
            }
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
        }
        Ok(turns)
    }
}

impl Drop for Diarizer {
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) };
    }
}

/// ONNX Runtime threads in the child: diarization runs next to the
/// recognizer, and beyond four the gain is small (RTF 0.079 at 4 threads,
/// 0.067 at 6–8 on an M1 Pro).
fn child_threads() -> i32 {
    (num_cpus::get_physical() as i32).clamp(1, 4)
}

fn diarize_file(req: &ChildRequest) -> anyhow::Result<(Vec<Turn>, String)> {
    let started = Instant::now();
    let audio = crate::read_pcm_file(Path::new(&req.audio_path))?;
    let audio_secs = audio.len() as f64 / SAMPLE_RATE as f64;
    let model_dir = Path::new(&req.model_dir);
    let threads = child_threads();
    let diarizer = Diarizer::create(model_dir, threads)?;
    let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
        model: Some(
            model_dir
                .join(EMBEDDING_MODEL)
                .to_string_lossy()
                .into_owned(),
        ),
        num_threads: threads,
        debug: false,
        provider: Some("cpu".into()),
    })
    .ok_or_else(|| anyhow::anyhow!("could not load the speaker embedding model"))?;

    let progress = Arc::new(ChildProgress {
        progress: Mutex::new(Progress::new(emit_progress as fn(f64))),
        sherpa_started: Default::default(),
    });
    progress.report(0.0);
    // Segmentation reports nothing: estimate it from the time it usually takes.
    {
        let progress = Arc::clone(&progress);
        let expected = (audio_secs * SEGMENTATION_SECS_PER_SEC).max(0.5);
        let t0 = Instant::now();
        thread::spawn(move || {
            while !progress
                .sherpa_started
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let f = (t0.elapsed().as_secs_f64() / expected).min(0.95);
                progress.report(f * SEGMENTATION_SHARE);
                thread::sleep(Duration::from_millis(250));
            }
        });
    }

    let raw = diarizer.process(&audio, &progress)?;
    progress
        .sherpa_started
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let sherpa_secs = started.elapsed().as_secs_f64();
    let sherpa_speakers = {
        let mut ids: Vec<i32> = raw.iter().map(|t| t.2).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };

    let voice = VoicedFrames::analyze(&audio, SAMPLE_RATE as u32);
    let spans = split_turns(&raw, &voice);
    let mut pieces = Vec::with_capacity(spans.len());
    for (i, span) in spans.iter().enumerate() {
        let voice = embed_piece(&extractor, &audio, span, &spans);
        pieces.push(Piece {
            start: span.start,
            end: span.end,
            voice,
        });
        progress.report(
            SEGMENTATION_SHARE
                + SHERPA_EMBEDDING_SHARE
                + (1.0 - SEGMENTATION_SHARE - SHERPA_EMBEDDING_SHARE) * (i + 1) as f64
                    / spans.len() as f64,
        );
    }
    let count = match req.num_speakers {
        0 => SpeakerCount::Auto,
        n => SpeakerCount::Exact(n as usize),
    };
    let labels = recluster::cluster(&pieces, count);
    let turns = join_pieces(&spans, &labels);
    progress.report(1.0);
    let speakers = labels.iter().max().map_or(0, |m| m + 1);
    let stats = format!(
        "{audio_secs:.1}s of audio in {:.1}s (sherpa {sherpa_secs:.1}s, {} turns, {sherpa_speakers} clusters) → {} pieces, {speakers} speakers",
        started.elapsed().as_secs_f64(),
        raw.len(),
        pieces.len(),
    );
    Ok((turns, stats))
}

/// A piece of one sherpa turn.
#[derive(Debug, Clone, PartialEq)]
struct Span {
    start: f64,
    end: f64,
    /// Index of the sherpa turn it came from.
    turn: usize,
    /// sherpa's speaker for that turn.
    sherpa: i32,
}

/// Cuts every sherpa turn at its pauses of `PIECE_GAP_SECS` or more,
/// joining pieces shorter than `MIN_PIECE_SECS` to the one before (or after)
/// them. The pieces tile their turn — cuts fall in the middle of a pause —
/// so no speech is lost between them.
fn split_turns(raw: &[(f64, f64, i32)], voice: &VoicedFrames) -> Vec<Span> {
    let to_sample = |t: f64| (t.max(0.0) * SAMPLE_RATE as f64).round() as usize;
    let to_secs = |s: usize| s as f64 / SAMPLE_RATE as f64;
    let mut out = Vec::new();
    for (i, &(start, end, sherpa)) in raw.iter().enumerate() {
        if end <= start {
            continue;
        }
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for (a, b) in voice.voiced_runs(to_sample(start), to_sample(end), to_sample(PIECE_GAP_SECS))
        {
            let (a, b) = (to_secs(a), to_secs(b));
            match runs.last_mut() {
                Some(last) if last.1 - last.0 < MIN_PIECE_SECS || b - a < MIN_PIECE_SECS => {
                    last.1 = b
                }
                _ => runs.push((a, b)),
            }
        }
        if runs.is_empty() {
            runs.push((start, end));
        }
        let n = runs.len();
        for k in 0..n {
            out.push(Span {
                start: if k == 0 {
                    start
                } else {
                    (runs[k - 1].1 + runs[k].0) / 2.0
                },
                end: if k + 1 == n {
                    end
                } else {
                    (runs[k].1 + runs[k + 1].0) / 2.0
                },
                turn: i,
                sherpa,
            });
        }
    }
    out
}

/// The parts of `span` where no other sherpa speaker talks: crosstalk would
/// mix two voices into one embedding.
fn exclusive_parts(span: &Span, spans: &[Span]) -> Vec<(f64, f64)> {
    let mut parts = vec![(span.start, span.end)];
    for other in spans {
        if other.sherpa == span.sherpa || other.end <= span.start || other.start >= span.end {
            continue;
        }
        parts = parts
            .into_iter()
            .flat_map(|(a, b)| {
                if other.end <= a || other.start >= b {
                    vec![(a, b)]
                } else {
                    [(a, other.start), (other.end, b)]
                        .into_iter()
                        .filter(|(x, y)| y > x)
                        .collect()
                }
            })
            .collect();
    }
    parts
}

/// The piece's voice: the duration-weighted sum of unit embeddings of its
/// crosstalk-free audio, in chunks of at most `MAX_CHUNK_SECS`.
fn embed_piece(
    extractor: &SpeakerEmbeddingExtractor,
    audio: &[f32],
    span: &Span,
    spans: &[Span],
) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for (a, b) in exclusive_parts(span, spans) {
        if b - a < MIN_CHUNK_SECS {
            continue;
        }
        let k = ((b - a) / MAX_CHUNK_SECS).ceil().max(1.0) as usize;
        let step = (b - a) / k as f64;
        for j in 0..k {
            let from = ((a + step * j as f64) * SAMPLE_RATE as f64) as usize;
            let to = (((a + step * (j + 1) as f64) * SAMPLE_RATE as f64) as usize).min(audio.len());
            if from >= to {
                continue;
            }
            let Some(stream) = extractor.create_stream() else {
                continue;
            };
            stream.accept_waveform(SAMPLE_RATE as i32, &audio[from..to]);
            stream.input_finished();
            if !extractor.is_ready(&stream) {
                continue;
            }
            let Some(embedding) = extractor.compute(&stream) else {
                continue;
            };
            let v = recluster::weighted_unit(&embedding, step as f32);
            match sum.as_mut() {
                Some(s) => s.iter_mut().zip(&v).for_each(|(x, y)| *x += y),
                None => sum = Some(v),
            }
        }
    }
    sum
}

/// Turns from labelled pieces: consecutive pieces of one sherpa turn with
/// the same speaker become one turn again. Sorted by start.
fn join_pieces(spans: &[Span], labels: &[usize]) -> Vec<Turn> {
    let mut turns: Vec<(Turn, usize)> = Vec::new();
    for (span, &label) in spans.iter().zip(labels) {
        let speaker = label as u32;
        match turns.last_mut() {
            Some((t, turn)) if *turn == span.turn && t.speaker == speaker => t.end = span.end,
            _ => turns.push((
                Turn {
                    start: round_ms(span.start),
                    end: span.end,
                    speaker,
                },
                span.turn,
            )),
        }
    }
    let mut out: Vec<Turn> = turns
        .into_iter()
        .map(|(mut t, _)| {
            t.end = round_ms(t.end);
            t
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

fn round_ms(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: f64, end: f64, turn: usize, sherpa: i32) -> Span {
        Span {
            start,
            end,
            turn,
            sherpa,
        }
    }

    /// 220 Hz bursts at -10 dBFS separated by silences, as (secs, voiced).
    fn audio(parts: &[(f64, bool)]) -> Vec<f32> {
        let mut out = Vec::new();
        for &(secs, voiced) in parts {
            let n = (secs * SAMPLE_RATE as f64) as usize;
            out.extend((0..n).map(|i| {
                if voiced {
                    0.3 * (i as f32 * 0.0864).sin()
                } else {
                    0.0
                }
            }));
        }
        out
    }

    #[test]
    fn turns_are_cut_at_pauses_into_tiling_pieces() {
        // One sherpa turn 0–9 s: voice 0–3, pause 3–3.5, voice 3.5–6,
        // pause 6–6.1 (too short), voice 6.1–9.
        // Silence after the turn sets the noise floor, as room tone would.
        let a = audio(&[
            (3.0, true),
            (0.5, false),
            (2.5, true),
            (0.1, false),
            (2.9, true),
            (2.0, false),
        ]);
        let voice = VoicedFrames::analyze(&a, SAMPLE_RATE as u32);
        let spans = split_turns(&[(0.0, 9.0, 4)], &voice);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].start, 0.0);
        assert!((spans[0].end - 3.25).abs() < 0.03, "{spans:?}");
        assert_eq!(spans[1].start, spans[0].end);
        assert_eq!(spans[1].end, 9.0);
        assert!(spans.iter().all(|s| s.turn == 0 && s.sherpa == 4));
    }

    #[test]
    fn short_pieces_join_a_neighbour() {
        // voice 0–3, pause, 0.3 s blip, pause, voice 4.3–7.8.
        let a = audio(&[
            (3.0, true),
            (0.5, false),
            (0.3, true),
            (0.5, false),
            (3.5, true),
            (2.0, false),
        ]);
        let voice = VoicedFrames::analyze(&a, SAMPLE_RATE as u32);
        let spans = split_turns(&[(0.0, 7.8, 0)], &voice);
        assert_eq!(spans.len(), 2, "{spans:?}");
        // The blip went with the piece before it: the cut is in the pause
        // after it (3.8–4.3 s).
        assert!(spans[0].end > 3.9 && spans[0].end < 4.2, "{spans:?}");
        // Silence only: the turn stays whole.
        let quiet = vec![0.0f32; 5 * SAMPLE_RATE];
        let voice = VoicedFrames::analyze(&quiet, SAMPLE_RATE as u32);
        assert_eq!(
            split_turns(&[(1.0, 4.0, 2)], &voice),
            vec![span(1.0, 4.0, 0, 2)]
        );
    }

    #[test]
    fn exclusive_parts_leave_out_other_speakers() {
        let spans = vec![
            span(0.0, 10.0, 0, 0),
            span(2.0, 3.0, 1, 1),
            span(9.0, 12.0, 2, 1),
            span(4.0, 5.0, 3, 0),
        ];
        assert_eq!(
            exclusive_parts(&spans[0], &spans),
            vec![(0.0, 2.0), (3.0, 9.0)]
        );
        assert!(exclusive_parts(&spans[1], &spans).is_empty());
        assert_eq!(exclusive_parts(&spans[2], &spans), vec![(10.0, 12.0)]);
    }

    #[test]
    fn pieces_of_one_turn_rejoin_when_they_share_a_speaker() {
        let spans = vec![
            span(0.0, 2.0, 0, 0),
            span(2.0, 4.0, 0, 0),
            span(4.0, 6.0, 0, 0),
            span(6.5, 8.0, 1, 0),
            span(1.0, 1.5, 2, 1),
        ];
        let turns = join_pieces(&spans, &[0, 0, 1, 1, 0]);
        let t = |start, end, speaker| Turn {
            start,
            end,
            speaker,
        };
        assert_eq!(
            turns,
            vec![
                t(0.0, 4.0, 0),
                t(1.0, 1.5, 0),
                t(4.0, 6.0, 1),
                t(6.5, 8.0, 1)
            ]
        );
    }

    #[test]
    fn child_lines_round_trip() {
        let line = r#"{"turns":[{"start":0.5,"end":2.0,"speaker":1}],"stats":"x"}"#;
        let parsed: ChildLine = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed.turns.unwrap(),
            vec![Turn {
                start: 0.5,
                end: 2.0,
                speaker: 1
            }]
        );
        assert_eq!(
            serde_json::to_string(&ChildLine {
                progress: Some(0.25),
                ..Default::default()
            })
            .unwrap(),
            r#"{"progress":0.25}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn exit_descriptions() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            describe_exit(ExitStatus::from_raw(10)),
            "the speaker detector crashed (signal 10)"
        );
        assert_eq!(
            describe_exit(ExitStatus::from_raw(3 << 8)),
            "the speaker detector failed (exit code 3)"
        );
        assert_eq!(
            describe_exit(ExitStatus::from_raw(0)),
            "the speaker detector exited without a result"
        );
    }
}
