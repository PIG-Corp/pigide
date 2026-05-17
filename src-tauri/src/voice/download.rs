//! Whisper model registry: 6 quality tiers, downloaded on demand from
//! HuggingFace `ggerganov/whisper.cpp`.

use crate::error::{Error, Result};
use crate::events::EV_VOICE_DOWNLOAD;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelId {
    Tiny,
    Base,
    Small,
    Medium,
    Large,
    DistilLarge,
}

impl ModelId {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tiny" => Some(ModelId::Tiny),
            "base" => Some(ModelId::Base),
            "small" => Some(ModelId::Small),
            "medium" => Some(ModelId::Medium),
            "large" | "large-v3" => Some(ModelId::Large),
            "distil-large" | "large-turbo" => Some(ModelId::DistilLarge),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelId::Tiny => "tiny",
            ModelId::Base => "base",
            ModelId::Small => "small",
            ModelId::Medium => "medium",
            ModelId::Large => "large",
            ModelId::DistilLarge => "distil-large",
        }
    }
    pub fn filename(&self) -> &'static str {
        match self {
            ModelId::Tiny => "ggml-tiny.bin",
            ModelId::Base => "ggml-base.bin",
            ModelId::Small => "ggml-small.bin",
            ModelId::Medium => "ggml-medium.bin",
            ModelId::Large => "ggml-large-v3.bin",
            ModelId::DistilLarge => "ggml-large-v3-turbo.bin",
        }
    }
    pub fn approx_bytes(&self) -> u64 {
        match self {
            ModelId::Tiny => 75_000_000,
            ModelId::Base => 142_000_000,
            ModelId::Small => 466_000_000,
            ModelId::Medium => 1_500_000_000,
            ModelId::Large => 3_100_000_000,
            ModelId::DistilLarge => 1_500_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub filename: String,
    pub approx_bytes: u64,
    pub installed: bool,
    pub url: String,
}

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

fn model_url(id: ModelId) -> String {
    format!("{}{}", HF_BASE, id.filename())
}

pub fn cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().ok_or_else(|| Error::Other("no cache dir".into()))?;
    let dir = base.join("pigide");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn model_path(id: ModelId) -> Result<PathBuf> {
    Ok(cache_dir()?.join(id.filename()))
}

pub fn model_exists(id: ModelId) -> bool {
    let min = match id {
        ModelId::Tiny => 50_000_000,
        ModelId::Base => 100_000_000,
        ModelId::Small => 300_000_000,
        ModelId::Medium => 1_200_000_000,
        ModelId::Large => 2_500_000_000,
        ModelId::DistilLarge => 1_200_000_000,
    };
    match model_path(id) {
        Ok(p) => p.exists() && std::fs::metadata(&p).map(|m| m.len() > min).unwrap_or(false),
        Err(_) => false,
    }
}

pub fn list_models() -> Vec<ModelInfo> {
    [
        ModelId::Tiny,
        ModelId::Base,
        ModelId::Small,
        ModelId::Medium,
        ModelId::Large,
        ModelId::DistilLarge,
    ]
    .iter()
    .map(|id| ModelInfo {
        id: id.as_str().to_string(),
        filename: id.filename().to_string(),
        approx_bytes: id.approx_bytes(),
        installed: model_exists(*id),
        url: model_url(*id),
    })
    .collect()
}

pub async fn ensure_model(id: ModelId, app: Option<AppHandle>) -> Result<PathBuf> {
    let path = model_path(id)?;
    if model_exists(id) {
        return Ok(path);
    }
    download_model(id, &path, app).await?;
    Ok(path)
}

async fn download_model(id: ModelId, target: &Path, app: Option<AppHandle>) -> Result<()> {
    let url = model_url(id);
    tracing::info!("downloading whisper {} to {}", id.as_str(), target.display());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()?;
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Voice(format!(
            "download {} -> {}",
            url,
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(id.approx_bytes());
    let mut stream = resp.bytes_stream();
    let tmp = target.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    use tokio::io::AsyncWriteExt;
    let mut downloaded: u64 = 0;
    let mut last_emit: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| Error::Other(format!("download: {}", e)))?;
        file.write_all(&bytes).await?;
        downloaded += bytes.len() as u64;
        if let Some(app) = &app {
            if downloaded - last_emit > 1_000_000 || downloaded == total {
                last_emit = downloaded;
                let _ = app.emit(
                    EV_VOICE_DOWNLOAD,
                    serde_json::json!({
                        "bytes": downloaded,
                        "total": total,
                        "model_id": id.as_str()
                    }),
                );
            }
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, target).await?;
    Ok(())
}
