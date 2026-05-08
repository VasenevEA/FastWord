// FastWord Rust sidecar.
//
// Reads line-delimited JSON requests from stdin, transcribes Float32 PCM
// audio with whisper.cpp (Metal-accelerated), writes line-delimited JSON
// responses to stdout. Holds the model in RAM and evicts it after
// FASTWORD_IDLE_EVICT seconds of inactivity.
//
// Mirrors the protocol of the previous Python sidecar exactly so the Swift
// side does not need any change beyond the binary path.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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
}

#[derive(Debug, Serialize)]
struct Response {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn log(msg: &str) {
    eprintln!("[sidecar] {msg}");
}

struct ModelHolder {
    model_path: PathBuf,
    ctx: Option<WhisperContext>,
    last_used: Instant,
}

impl ModelHolder {
    fn new(model_path: PathBuf) -> Self {
        Self {
            model_path,
            ctx: None,
            last_used: Instant::now(),
        }
    }

    fn ensure_loaded(&mut self) -> Result<&WhisperContext> {
        if self.ctx.is_none() {
            log(&format!("loading model {}", self.model_path.display()));
            let path_str = self
                .model_path
                .to_str()
                .context("non-utf8 model path")?
                .to_owned();
            let params = WhisperContextParameters::default();
            let ctx = WhisperContext::new_with_params(&path_str, params)
                .context("failed to load whisper model")?;
            self.ctx = Some(ctx);
            log("model loaded");
        }
        self.last_used = Instant::now();
        Ok(self.ctx.as_ref().unwrap())
    }

    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        let ctx = self.ensure_loaded()?;
        let mut state = ctx.create_state().context("create state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_n_threads(num_cpus_or_default());
        if let Some(lang) = language {
            if !lang.is_empty() {
                params.set_language(Some(lang));
            }
        }

        state.full(params, audio).context("whisper.full")?;

        let n_segments = state.full_n_segments().context("full_n_segments")?;
        let mut out = String::new();
        for i in 0..n_segments {
            let seg = state
                .full_get_segment_text(i)
                .context("get segment text")?;
            out.push_str(&seg);
        }
        Ok(out.trim().to_string())
    }

    fn evict_if_idle(&mut self, idle_after: Duration) {
        if self.ctx.is_some() && self.last_used.elapsed() > idle_after {
            log("evicting idle model");
            self.ctx = None;
        }
    }
}

fn num_cpus_or_default() -> i32 {
    thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}

fn decode_pcm(b64: &str) -> Result<Vec<f32>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("base64")?;
    if raw.len() % 4 != 0 {
        return Err(anyhow!("audio length not multiple of 4 bytes"));
    }
    let n = raw.len() / 4;
    let mut floats = Vec::with_capacity(n);
    for i in 0..n {
        let bytes = [raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]];
        floats.push(f32::from_le_bytes(bytes));
    }
    Ok(floats)
}

fn handle(req: Request, holder: &Arc<Mutex<ModelHolder>>) -> Response {
    let id = req.id.clone();
    let res: Result<String> = (|| {
        match req.cmd.as_str() {
            "warmup" => {
                let silence = vec![0.0f32; 16_000 / 2]; // 0.5s
                let mut h = holder.lock().unwrap();
                let _ = h.transcribe(&silence, None)?;
                Ok(String::new())
            }
            "transcribe" => {
                let audio_b64 = req
                    .audio_b64
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing audio_b64"))?;
                let audio = decode_pcm(audio_b64)?;
                if audio.len() < 1600 {
                    return Ok(String::new());
                }
                let mut h = holder.lock().unwrap();
                h.transcribe(&audio, req.language.as_deref())
            }
            other => Err(anyhow!("unknown cmd: {other}")),
        }
    })();

    match res {
        Ok(text) => Response {
            id,
            text: Some(text),
            error: None,
        },
        Err(err) => {
            log(&format!("error: {err:?}"));
            Response {
                id,
                text: None,
                error: Some(err.to_string()),
            }
        }
    }
}

fn evict_loop(holder: Arc<Mutex<ModelHolder>>, idle_after: Duration) {
    loop {
        thread::sleep(Duration::from_secs(30));
        holder.lock().unwrap().evict_if_idle(idle_after);
    }
}

fn main() -> Result<()> {
    // Suppress whisper.cpp's own stderr noise once the model is loaded.
    whisper_rs::install_logging_hooks();

    let model_path = PathBuf::from(
        std::env::var("FASTWORD_MODEL")
            .unwrap_or_else(|_| String::from("models/ggml-large-v3-turbo-q5_0.bin")),
    );
    if !model_path.exists() {
        return Err(anyhow!(
            "model file not found at {} (set FASTWORD_MODEL)",
            model_path.display()
        ));
    }

    let idle_after_secs: u64 = std::env::var("FASTWORD_IDLE_EVICT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);

    let holder = Arc::new(Mutex::new(ModelHolder::new(model_path)));
    {
        let h = Arc::clone(&holder);
        thread::spawn(move || evict_loop(h, Duration::from_secs(idle_after_secs)));
    }

    log("ready");

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.context("stdin read")?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(err) => {
                let resp = Response {
                    id: String::new(),
                    text: None,
                    error: Some(format!("bad json: {err}")),
                };
                writeln!(out, "{}", serde_json::to_string(&resp).unwrap())?;
                out.flush()?;
                continue;
            }
        };
        let resp = handle(req, &holder);
        writeln!(out, "{}", serde_json::to_string(&resp).unwrap())?;
        out.flush()?;
    }

    Ok(())
}
