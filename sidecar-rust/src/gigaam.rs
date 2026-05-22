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
};

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

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx OfflineRecognizer::create returned None"))?;

        Ok(Self { inner: recognizer })
    }

    pub fn transcribe(&self, audio: &[f32], sample_rate: u32) -> Result<String> {
        let stream = self.inner.create_stream();
        stream.accept_waveform(sample_rate as i32, audio);
        self.inner.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| anyhow!("sherpa-onnx returned no result"))?;
        Ok(result.text.trim().to_string())
    }
}

pub fn default_model_dir() -> Option<PathBuf> {
    std::env::var_os("FASTWORD_GIGAAM_MODEL").map(PathBuf::from)
}
