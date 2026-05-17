use crate::error::{Error, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Which backend the live `WhisperContext` is actually running on. Decided at
/// `open()` time — we try GPU first when a `gpu-*` feature is enabled, and
/// fall back to CPU if the GPU init fails (no toolkit, no device, OOM, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Gpu,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Gpu => "gpu",
        }
    }
}

/// `true` when the binary was compiled with one of the GPU backends.
const GPU_COMPILED_IN: bool = cfg!(any(
    feature = "gpu-cuda",
    feature = "gpu-vulkan",
    feature = "gpu-hipblas",
    feature = "gpu-metal",
));

/// Force CPU at runtime via env (`PIGIDE_WHISPER_CPU=1`). Useful for A/B
/// benchmarking on the same binary.
fn force_cpu_env() -> bool {
    std::env::var("PIGIDE_WHISPER_CPU")
        .ok()
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

pub struct Whisper {
    ctx: WhisperContext,
    pub model_id: String,
    pub backend: Backend,
}

impl Whisper {
    pub fn open(model: &Path, model_id: &str) -> Result<Self> {
        let path = model
            .to_str()
            .ok_or_else(|| Error::Voice("non-utf8 model path".into()))?;

        let want_gpu = GPU_COMPILED_IN && !force_cpu_env();

        if want_gpu {
            let mut params = WhisperContextParameters::default();
            params.use_gpu(true);
            match WhisperContext::new_with_params(path, params) {
                Ok(ctx) => {
                    tracing::info!(
                        "whisper: GPU backend initialized (model={})",
                        model_id
                    );
                    return Ok(Self {
                        ctx,
                        model_id: model_id.to_string(),
                        backend: Backend::Gpu,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "whisper: GPU init failed, falling back to CPU: {}",
                        e
                    );
                }
            }
        } else if !GPU_COMPILED_IN {
            tracing::info!(
                "whisper: CPU backend (binary built without gpu-* feature)"
            );
        } else {
            tracing::info!("whisper: CPU backend (PIGIDE_WHISPER_CPU set)");
        }

        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| Error::Voice(format!("whisper init: {}", e)))?;
        Ok(Self {
            ctx,
            model_id: model_id.to_string(),
            backend: Backend::Cpu,
        })
    }

    /// `samples_16k` must be 16 kHz mono f32, range -1..=1.
    pub fn transcribe(&self, samples_16k: &[f32], language: &str) -> Result<String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| Error::Voice(format!("create_state: {}", e)))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let n_threads = match self.backend {
            Backend::Gpu => 2,
            Backend::Cpu => std::cmp::max(1, num_cpus_safe() as i32 / 2),
        };
        params.set_n_threads(n_threads);
        params.set_translate(false);
        params.set_print_realtime(false);
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        let lang = if language.is_empty() || language == "auto" {
            None
        } else {
            Some(language)
        };
        params.set_language(lang);

        state
            .full(params, samples_16k)
            .map_err(|e| Error::Voice(format!("full: {}", e)))?;
        let n = state.full_n_segments();
        let mut out = String::new();
        for i in 0..n {
            let seg = state
                .get_segment(i)
                .ok_or_else(|| Error::Voice(format!("missing segment {}", i)))?;
            let text = seg
                .to_str()
                .map_err(|e| Error::Voice(format!("seg text: {}", e)))?;
            out.push_str(text);
        }
        Ok(out.trim().to_string())
    }
}

fn num_cpus_safe() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}
