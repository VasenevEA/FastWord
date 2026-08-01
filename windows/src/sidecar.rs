// Client for the fastword-sidecar process: spawn, line-delimited JSON over
// stdio, synchronous request/response (the sidecar processes requests
// sequentially anyway).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, PartialEq)]
pub struct Request {
    pub id: String,
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_speech_thold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Request {
    pub fn warmup(id: &str) -> Self {
        Self::bare(id, "warmup")
    }

    pub fn info(id: &str) -> Self {
        Self::bare(id, "info")
    }

    fn bare(id: &str, cmd: &str) -> Self {
        Self {
            id: id.to_string(),
            cmd: cmd.into(),
            sample_rate: None,
            audio_b64: None,
            language: None,
            no_speech_thold: None,
            initial_prompt: None,
            engine: None,
        }
    }

    /// Build a transcribe request the way the macOS client does: empty
    /// language means auto-detect, `skip_empty` maps onto whisper's
    /// no-speech threshold (0.0 disables the filter).
    pub fn transcribe(
        id: &str,
        pcm: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
        skip_empty: bool,
        engine: Option<&str>,
    ) -> Self {
        let bytes = crate::audio::pcm_to_le_bytes(pcm);
        Self {
            id: id.to_string(),
            cmd: "transcribe".into(),
            sample_rate: Some(16_000),
            audio_b64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            language: Some(language.to_string()),
            no_speech_thold: Some(if skip_empty { 0.6 } else { 0.0 }),
            initial_prompt: initial_prompt.map(str::to_string),
            engine: engine.map(str::to_string),
        }
    }
}

/// Newest installed CUDA toolkit's DLL directory (`bin\x64`), if any.
fn newest_cuda_bin() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("ProgramFiles")?)
        .join("NVIDIA GPU Computing Toolkit")
        .join("CUDA");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("bin").join("x64").join("cublas64_13.dll").exists()
            || p.join("bin").join("cublas64_12.dll").exists())
        .collect();
    versions.sort();
    let newest = versions.pop()?;
    let x64 = newest.join("bin").join("x64");
    Some(if x64.exists() { x64 } else { newest.join("bin") })
}

/// Extract the backend name from an `info` response line. Unknown or
/// malformed lines count as "cpu" (older sidecars had no `info` at all).
pub fn parse_backend(info: &str) -> &str {
    info.split_whitespace()
        .find_map(|tok| tok.strip_prefix("backend="))
        .filter(|b| !b.is_empty())
        .unwrap_or("cpu")
}

/// Locate the sidecar binary: next to our own exe first (packaged layout),
/// then the development layout by walking up to the repo root.
pub fn find_sidecar_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let packaged = dir.join("fastword-sidecar.exe");
    if packaged.exists() {
        return Some(packaged);
    }
    // Dev layout: walk up until we find sidecar-rust/Cargo.toml.
    let mut cur = dir.to_path_buf();
    for _ in 0..8 {
        if cur.join("sidecar-rust").join("Cargo.toml").exists() {
            let dev = cur
                .join("sidecar-rust")
                .join("target")
                .join("release")
                .join("fastword-sidecar.exe");
            if dev.exists() {
                return Some(dev);
            }
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// The parameters needed to (re)start the sidecar — kept by the controller
/// so a crashed sidecar can be respawned without restarting the app.
pub struct SidecarSpawner {
    pub exe: PathBuf,
    pub model: PathBuf,
    pub idle_evict_secs: u64,
}

impl SidecarSpawner {
    pub fn spawn(&self) -> Result<SidecarClient> {
        SidecarClient::spawn(&self.exe, &self.model, self.idle_evict_secs)
    }
}

pub struct SidecarClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl SidecarClient {
    pub fn spawn(sidecar_exe: &Path, model_path: &Path, idle_evict_secs: u64) -> Result<Self> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;

        // A CUDA-built sidecar needs cublas from CUDA's bin\x64 on PATH.
        // The installer adds it to the machine PATH, but processes started
        // before the install (or odd shells) may carry a stale one — append
        // the newest toolkit dir ourselves when present.
        let mut path = std::env::var("PATH").unwrap_or_default();
        if let Some(cuda_bin) = newest_cuda_bin() {
            path = format!("{};{}", path, cuda_bin.display());
        }

        let mut cmd = Command::new(sidecar_exe);
        cmd.env("FASTWORD_MODEL", model_path)
            .env("FASTWORD_IDLE_EVICT", idle_evict_secs.to_string())
            .env("PATH", path);
        // Point the sidecar at the GigaAM model dir when installed — it's
        // only loaded on the first engine=gigaam request, so this is free
        // for users who never enable it (mirrors the macOS client).
        if crate::models::gigaam_installed() {
            cmd.env("FASTWORD_GIGAAM_MODEL", crate::models::gigaam_dir());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| format!("starting sidecar {}", sidecar_exe.display()))?;

        let stdin = child.stdin.take().context("sidecar stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("sidecar stdout")?);

        // Drain stderr into the log so the pipe never fills up and blocks
        // the sidecar, and so crashes leave a trace to debug.
        let stderr = child.stderr.take().context("sidecar stderr")?;
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                crate::logging::log(&format!("sidecar: {line}"));
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    pub fn warmup(&mut self) -> Result<String> {
        let id = self.take_id();
        self.roundtrip(&Request::warmup(&id))
    }

    /// Version/backend line, e.g. "fastword-sidecar 0.2.0 backend=cpu".
    pub fn info(&mut self) -> Result<String> {
        let id = self.take_id();
        self.roundtrip(&Request::info(&id))
    }

    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
        skip_empty: bool,
        engine: Option<&str>,
    ) -> Result<String> {
        let id = self.take_id();
        let req = Request::transcribe(&id, pcm, language, initial_prompt, skip_empty, engine);
        self.roundtrip(&req)
    }

    /// False if the sidecar process has exited.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn take_id(&mut self) -> String {
        let id = self.next_id;
        self.next_id += 1;
        id.to_string()
    }

    fn roundtrip(&mut self, req: &Request) -> Result<String> {
        let mut line = serde_json::to_string(req).context("serializing request")?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .context("writing to sidecar (did it crash?)")?;
        self.stdin.flush().context("flushing sidecar stdin")?;

        loop {
            let mut buf = String::new();
            let n = self
                .stdout
                .read_line(&mut buf)
                .context("reading from sidecar")?;
            if n == 0 {
                return Err(anyhow!("sidecar exited unexpectedly"));
            }
            if buf.trim().is_empty() {
                continue;
            }
            let resp: Response =
                serde_json::from_str(buf.trim()).context("decoding sidecar response")?;
            if resp.id != req.id {
                // Stale response from an earlier, abandoned request.
                continue;
            }
            if let Some(err) = resp.error {
                return Err(anyhow!(err));
            }
            return Ok(resp.text.unwrap_or_default());
        }
    }
}

impl Drop for SidecarClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_request_wire_format() {
        let req = Request::warmup("42");
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"id":"42","cmd":"warmup"}"#
        );
    }

    #[test]
    fn gigaam_engine_request_matches_swift_client() {
        // Mirrors what SidecarClient.swift sends when GigaAM is active.
        let req = Request::transcribe("1", &[0.0f32], "ru", None, true, Some("gigaam"));
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(v["engine"], "gigaam");
        assert_eq!(v["language"], "ru");
    }

    #[test]
    fn transcribe_request_matches_sidecar_protocol() {
        let req = Request::transcribe("1", &[0.0f32], "ru", None, true, None);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(v["cmd"], "transcribe");
        assert_eq!(v["sample_rate"], 16000);
        assert_eq!(v["language"], "ru");
        assert_eq!(v["no_speech_thold"], 0.6);
        // One f32 zero sample = 4 zero bytes.
        assert_eq!(v["audio_b64"], "AAAAAA==");
        assert!(v.get("initial_prompt").is_none());
        assert!(v.get("engine").is_none());
    }

    #[test]
    fn skip_empty_off_disables_no_speech_filter() {
        let req = Request::transcribe("1", &[], "", Some("hint"), false, None);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(v["no_speech_thold"], 0.0);
        assert_eq!(v["initial_prompt"], "hint");
        assert_eq!(v["language"], "");
    }

    #[test]
    fn info_request_wire_format() {
        let req = Request::info("7");
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"id":"7","cmd":"info"}"#
        );
    }

    #[test]
    fn parses_backend_from_info_line() {
        assert_eq!(parse_backend("fastword-sidecar 0.2.0 backend=cuda"), "cuda");
        assert_eq!(parse_backend("fastword-sidecar 0.2.0 backend=cpu"), "cpu");
        // Older sidecar / unexpected reply → assume cpu.
        assert_eq!(parse_backend("unknown cmd: info"), "cpu");
        assert_eq!(parse_backend(""), "cpu");
        assert_eq!(parse_backend("backend="), "cpu");
    }

    #[test]
    fn response_parses_text_and_error_variants() {
        let ok: Response = serde_json::from_str(r#"{"id":"1","text":"hi"}"#).unwrap();
        assert_eq!(ok.text.as_deref(), Some("hi"));
        assert!(ok.error.is_none());

        let err: Response = serde_json::from_str(r#"{"id":"2","error":"boom"}"#).unwrap();
        assert_eq!(err.error.as_deref(), Some("boom"));
    }
}
