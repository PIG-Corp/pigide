use crate::error::{Error, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct CaptureState {
    pub samples: Vec<f32>,
    pub source_rate: u32,
    pub recording: bool,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            source_rate: 16000,
            recording: false,
        }
    }
}

pub struct Capture {
    state: Arc<Mutex<CaptureState>>,
    stream: Mutex<Option<cpal::Stream>>,
}

unsafe impl Send for Capture {}
unsafe impl Sync for Capture {}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState::default())),
            stream: Mutex::new(None),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.state.lock().recording
    }

    pub fn start(&self) -> Result<()> {
        if self.is_recording() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| Error::Voice("no input device".into()))?;
        let config = device
            .default_input_config()
            .map_err(|e| Error::Voice(format!("default config: {}", e)))?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        // Reset buffer.
        {
            let mut s = self.state.lock();
            s.samples.clear();
            s.source_rate = sample_rate;
            s.recording = true;
        }

        let state = self.state.clone();
        let err_fn = |e| tracing::warn!("audio stream error: {:?}", e);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    let mut s = state.lock();
                    if !s.recording {
                        return;
                    }
                    if channels == 1 {
                        s.samples.extend_from_slice(data);
                    } else {
                        for chunk in data.chunks(channels) {
                            let v = chunk.iter().sum::<f32>() / channels as f32;
                            s.samples.push(v);
                        }
                    }
                    // 60 s cap @ 48kHz ≈ 2.88M samples.
                    if s.samples.len() > 60 * 48_000 {
                        let cut = s.samples.len() - 60 * 48_000;
                        s.samples.drain(0..cut);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    let mut s = state.lock();
                    if !s.recording {
                        return;
                    }
                    if channels == 1 {
                        s.samples
                            .extend(data.iter().map(|&v| v as f32 / i16::MAX as f32));
                    } else {
                        for chunk in data.chunks(channels) {
                            let v: f32 = chunk
                                .iter()
                                .map(|&s| s as f32 / i16::MAX as f32)
                                .sum::<f32>()
                                / channels as f32;
                            s.samples.push(v);
                        }
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _| {
                    let mut s = state.lock();
                    if !s.recording {
                        return;
                    }
                    if channels == 1 {
                        s.samples
                            .extend(data.iter().map(|&v| (v as f32 - 32768.0) / 32768.0));
                    } else {
                        for chunk in data.chunks(channels) {
                            let v: f32 = chunk
                                .iter()
                                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                .sum::<f32>()
                                / channels as f32;
                            s.samples.push(v);
                        }
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(Error::Voice(format!(
                    "unsupported sample format: {:?}",
                    other
                )))
            }
        }
        .map_err(|e| Error::Voice(format!("build stream: {}", e)))?;

        stream
            .play()
            .map_err(|e| Error::Voice(format!("play: {}", e)))?;
        *self.stream.lock() = Some(stream);
        Ok(())
    }

    /// Stop recording and return the captured samples + their source sample rate.
    pub fn stop(&self) -> Result<(Vec<f32>, u32)> {
        {
            let mut s = self.state.lock();
            s.recording = false;
        }
        // Drop the stream (this stops capture).
        *self.stream.lock() = None;
        let s = self.state.lock();
        Ok((s.samples.clone(), s.source_rate))
    }
}

/// Linear resampler from `src_rate` to 16000 Hz mono.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16000 {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return Vec::new();
    }
    let target_len = (samples.len() as f64 * 16000.0 / src_rate as f64) as usize;
    let mut out = Vec::with_capacity(target_len);
    for i in 0..target_len {
        let src_idx = i as f64 * src_rate as f64 / 16000.0;
        let lo = src_idx.floor() as usize;
        let hi = (lo + 1).min(samples.len() - 1);
        let frac = (src_idx - lo as f64) as f32;
        let v = samples[lo] * (1.0 - frac) + samples[hi] * frac;
        out.push(v);
    }
    out
}
