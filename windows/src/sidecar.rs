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
        Self {
            id: id.to_string(),
            cmd: "warmup".into(),
            sample_rate: None,
            audio_b64: None,
            language: None,
            no_speech_thold: None,
            initial_prompt: None,
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
        }
    }
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

        let mut child = Command::new(sidecar_exe)
            .env("FASTWORD_MODEL", model_path)
            .env("FASTWORD_IDLE_EVICT", idle_evict_secs.to_string())
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

    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
        skip_empty: bool,
    ) -> Result<String> {
        let id = self.take_id();
        let req = Request::transcribe(&id, pcm, language, initial_prompt, skip_empty);
        self.roundtrip(&req)
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
    fn transcribe_request_matches_sidecar_protocol() {
        let req = Request::transcribe("1", &[0.0f32], "ru", None, true);
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
        let req = Request::transcribe("1", &[], "", Some("hint"), false);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(v["no_speech_thold"], 0.0);
        assert_eq!(v["initial_prompt"], "hint");
        assert_eq!(v["language"], "");
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
