// Microphone capture via WASAPI (cpal), producing 16 kHz mono Float32 PCM —
// the format the sidecar expects.
//
// The recorder opens the system default input device with its default
// config, downmixes to mono on the fly and resamples to 16 kHz when the
// recording is stopped. The DSP helpers are pure functions with tests.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Average interleaved multi-channel frames down to mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Linear-interpolation resampler. Whisper is robust to the slight rolloff
/// of linear interpolation at these rates (48k/44.1k → 16k).
pub fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx];
        let b = if idx + 1 < input.len() { input[idx + 1] } else { a };
        out.push(a + (b - a) * frac);
    }
    out
}

/// f32 samples → little-endian bytes (the sidecar's wire format).
pub fn pcm_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    device_rate: u32,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(Vec::new())),
            device_rate: TARGET_SAMPLE_RATE,
        }
    }

    /// Open the current default input device and start capturing. Builds a
    /// fresh stream every time so a mic plugged in after launch is picked up.
    pub fn start(&mut self) -> Result<()> {
        self.stop_stream();
        self.buffer.lock().unwrap().clear();

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device — is a microphone connected?"))?;
        let config = device
            .default_input_config()
            .context("querying default input config")?;
        let channels = config.channels() as usize;
        self.device_rate = config.sample_rate().0;

        let buffer = Arc::clone(&self.buffer);
        let err_fn = |e| crate::logging::log(&format!("audio stream error: {e}"));

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    buffer.lock().unwrap().extend(downmix(data, channels));
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    let floats: Vec<f32> =
                        data.iter().map(|&s| s as f32 / 32768.0).collect();
                    buffer.lock().unwrap().extend(downmix(&floats, channels));
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _| {
                    let floats: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect();
                    buffer.lock().unwrap().extend(downmix(&floats, channels));
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("unsupported input sample format: {other:?}")),
        };
        stream.play().context("starting input stream")?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Stop capturing and return the recording as 16 kHz mono samples.
    pub fn stop(&mut self) -> Vec<f32> {
        self.stop_stream();
        let raw = std::mem::take(&mut *self.buffer.lock().unwrap());
        resample_linear(&raw, self.device_rate, TARGET_SAMPLE_RATE)
    }

    /// Stop capturing and throw the audio away.
    pub fn cancel(&mut self) {
        self.stop_stream();
        self.buffer.lock().unwrap().clear();
    }

    fn stop_stream(&mut self) {
        self.stream = None; // dropping the stream stops capture
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_mono_is_identity() {
        let data = vec![0.1, 0.2, 0.3];
        assert_eq!(downmix(&data, 1), data);
    }

    #[test]
    fn downmix_stereo_averages() {
        let data = vec![1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix(&data, 2), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let data = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&data, 16_000, 16_000), data);
    }

    #[test]
    fn resample_halves_length_for_2x_rate() {
        let data: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let out = resample_linear(&data, 32_000, 16_000);
        assert_eq!(out.len(), 500);
        // Linear interp over a linear ramp reproduces the ramp exactly.
        assert!((out[10] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn resample_48k_to_16k() {
        let data: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.001).sin()).collect();
        let out = resample_linear(&data, 48_000, 16_000);
        assert_eq!(out.len(), 1600);
    }

    #[test]
    fn resample_empty_input() {
        assert!(resample_linear(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn pcm_bytes_are_little_endian() {
        let bytes = pcm_to_le_bytes(&[1.0f32]);
        assert_eq!(bytes, 1.0f32.to_le_bytes().to_vec());
        assert_eq!(pcm_to_le_bytes(&[0.0, 1.0]).len(), 8);
    }
}
