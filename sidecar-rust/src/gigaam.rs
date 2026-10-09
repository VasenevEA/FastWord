// GigaAM-v3 ASR engine via sherpa-onnx.
//
// Loaded lazily — held in a Mutex inside `EngineHolder` (see main.rs). The
// FASTWORD_GIGAAM_MODEL env var must point at a directory containing
// `model.int8.onnx` and `tokens.txt` (the layout from
// `csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v3-russian-...`).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sherpa_onnx::{
    OfflineNemoEncDecCtcModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineRecognizerResult,
};

use crate::merge::{self, Word};
use crate::segment;
use crate::Segment;

/// Longest piece of a file handed to GigaAM at once. The model was trained
/// on utterance-length clips, and its attention cost grows quadratically
/// with input length, so hours of audio go through it 20 s at a time.
const FILE_CHUNK_SECS: f32 = 20.0;
/// A pause at least this long ends a segment. Each voiced stretch of a
/// chunk between such pauses is decoded and time-stamped on its own, so a
/// subtitle does not show up seconds before its words and the client's
/// paragraph breaks (gaps between segments) fall where the speaker paused.
/// Pauses between words and most sentences are shorter and stay inside.
const SEGMENT_GAP_SECS: f32 = 1.0;
/// Audio kept on each side of a stretch's voiced range when decoding it, so
/// soft onsets and trailing consonants just under the voiced threshold are
/// not clipped.
const VOICED_PAD_SECS: f32 = 0.25;
/// Time step of GigaAM's CTC output (4x subsampled 10 ms frames): the
/// resolution of its token timestamps, and the length given to a token.
const CTC_FRAME_SECS: f64 = 0.04;

pub struct Recognizer {
    inner: OfflineRecognizer,
}

impl Recognizer {
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model.int8.onnx");
        let tokens_path = model_dir.join("tokens.txt");
        if !model_path.exists() {
            return Err(anyhow!(
                "GigaAM model.int8.onnx not found at {}",
                model_path.display()
            ));
        }
        if !tokens_path.exists() {
            return Err(anyhow!(
                "GigaAM tokens.txt not found at {}",
                tokens_path.display()
            ));
        }

        let model_str = model_path
            .to_str()
            .context("non-utf8 GigaAM model path")?
            .to_owned();
        let tokens_str = tokens_path
            .to_str()
            .context("non-utf8 GigaAM tokens path")?
            .to_owned();

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
            model: Some(model_str.into()),
            ..Default::default()
        };
        config.model_config.tokens = Some(tokens_str.into());
        config.model_config.num_threads = num_threads();

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx OfflineRecognizer::create returned None"))?;

        Ok(Self { inner: recognizer })
    }

    pub fn transcribe(&self, audio: &[f32], sample_rate: u32) -> Result<String> {
        Ok(self.decode(audio, sample_rate)?.text.trim().to_string())
    }

    fn decode(&self, audio: &[f32], sample_rate: u32) -> Result<OfflineRecognizerResult> {
        let stream = self.inner.create_stream();
        stream.accept_waveform(sample_rate as i32, audio);
        self.inner.decode(&stream);
        stream
            .get_result()
            .ok_or_else(|| anyhow!("sherpa-onnx returned no result"))
    }

    /// Long-file path for `transcribe_file`: cut on pauses into chunks,
    /// split each chunk further at pauses of `SEGMENT_GAP_SECS` or more,
    /// and decode every voiced stretch on its own fresh stream, time-stamped
    /// with its voiced range. Silent chunks and long pauses never reach the
    /// model. Segments carry their words, timed by the CTC tokens, for
    /// speaker attribution. `on_progress` gets the fraction of the input
    /// processed so far.
    pub fn transcribe_long(
        &self,
        audio: &[f32],
        sample_rate: u32,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<Vec<Segment>> {
        // The recognizer is loaded by now: tell the client work has begun.
        on_progress(0.0);
        let chunks = segment::split_on_silence(audio, sample_rate, FILE_CHUNK_SECS);
        let voice = segment::VoicedFrames::analyze(audio, sample_rate);
        let pad = (VOICED_PAD_SECS * sample_rate as f32) as usize;
        let gap = (SEGMENT_GAP_SECS * sample_rate as f32) as usize;
        let secs = |sample: usize| (sample as f64 * 1000.0 / sample_rate as f64).round() / 1000.0;
        let mut segments = Vec::new();
        for chunk in &chunks {
            for (start, end) in voice.voiced_runs(chunk.voiced_start, chunk.voiced_end, gap) {
                let from = start.saturating_sub(pad).max(chunk.start);
                let to = (end + pad).min(chunk.end);
                let result = self.decode(&audio[from..to], sample_rate)?;
                // CTC output can carry doubled word separators ("конца  если");
                // harmless in a dictation paste, ugly in subtitles.
                let text = result.text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    let words = words(&result, from as f64 / sample_rate as f64, &text);
                    let mut seg = Segment::new(secs(start), secs(end), text);
                    seg.words = words;
                    segments.push(seg);
                }
            }
            on_progress(chunk.end as f64 / audio.len() as f64);
        }
        Ok(segments)
    }
}

/// The words of a decoded stretch that began `offset` seconds into the
/// file — empty unless the tokens spell out exactly `text`.
fn words(result: &OfflineRecognizerResult, offset: f64, text: &str) -> Vec<Word> {
    let Some(times) = result.timestamps.as_deref() else {
        return Vec::new();
    };
    match merge::ctc_words(&result.tokens, times, offset, CTC_FRAME_SECS) {
        Some((joined, words)) if joined == text => words,
        _ => Vec::new(),
    }
}

/// ONNX Runtime threads for GigaAM. Left at 0, sherpa-onnx runs a single
/// thread: RTF 0.19 on an M1 Pro, half a second for a short dictation clip.
/// Four threads are ~3.5x faster there with bit-identical output; beyond
/// that the gain flattens and then reverses once threads land on efficiency
/// cores (10 threads was slower than 4). Four also leaves cores for a
/// dictation sidecar running next to a file job. FASTWORD_THREADS overrides,
/// as it does for whisper.
fn num_threads() -> i32 {
    std::env::var("FASTWORD_THREADS")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| (num_cpus::get_physical() as i32).clamp(1, 4))
}

pub fn default_model_dir() -> Option<PathBuf> {
    std::env::var_os("FASTWORD_GIGAAM_MODEL").map(PathBuf::from)
}
