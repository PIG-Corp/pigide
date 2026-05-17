//! Real-Whisper smoke benchmark — initializes the actual `Whisper` context
//! against a downloaded ggml model and times one batch transcription.
//!
//! Skipped automatically when the model file isn't present (CI, fresh
//! checkout). Run locally with:
//!
//! ```bash
//! # CPU baseline
//! cargo test --test bench_whisper_backend --release -- --nocapture --ignored
//!
//! # Force CPU even on a GPU build
//! PIGIDE_WHISPER_CPU=1 cargo test --test bench_whisper_backend \
//!     --release --features gpu-cuda -- --nocapture --ignored
//!
//! # GPU run
//! cargo test --test bench_whisper_backend --release \
//!     --features gpu-cuda -- --nocapture --ignored
//! ```
//!
//! The bench reports backend (cpu/gpu) and decode wall-time. It does NOT
//! assert a hard latency ceiling — the absolute number depends on the host.

use pigide_lib::voice::whisper::Whisper;
use std::path::PathBuf;
use std::time::Instant;

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"))
        .join("pigide")
}

/// 10 s of 16 kHz mono — a 220 Hz sine swept lightly so Whisper has *something*
/// to chew on. Real speech would be more representative, but for a backend
/// smoke test we only care about end-to-end inference time and that a model
/// loads on the chosen backend.
fn synth_audio(seconds: usize) -> Vec<f32> {
    let n = seconds * 16_000;
    (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            0.2 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
        })
        .collect()
}

#[test]
#[ignore = "requires a downloaded ggml model in ~/.cache/pigide; run manually"]
fn smoke_one_pass() {
    let model_path = cache_dir().join("ggml-small.bin");
    if !model_path.exists() {
        eprintln!(
            "SKIP: model not found at {} (run PigIDE once to download)",
            model_path.display()
        );
        return;
    }

    let init_start = Instant::now();
    let whisper = Whisper::open(&model_path, "small").expect("open whisper");
    let init_ms = init_start.elapsed().as_millis();

    let samples = synth_audio(10);

    // Warm pass — first inference allocates state and JITs CUDA kernels.
    let _ = whisper.transcribe(&samples, "auto").expect("warm pass");

    let runs = 3;
    let mut times: Vec<u128> = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t0 = Instant::now();
        let _ = whisper.transcribe(&samples, "auto").expect("transcribe");
        times.push(t0.elapsed().as_millis());
    }
    times.sort();
    let median = times[times.len() / 2];

    println!(
        "whisper smoke: backend={} init={}ms decode_ms(10s_audio)={:?} median={}ms",
        whisper.backend.as_str(),
        init_ms,
        times,
        median,
    );
}
