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

/// Pinned SHA-256 digests for each model, captured out-of-band from
/// HuggingFace's LFS `x-linked-etag` (the canonical content hash). Verified
/// after download so a MITM'd connection or a compromised mirror can't slip a
/// malformed/backdoored GGML file into the whisper.cpp mmap parser. If a
/// model's digest is ever rotated upstream, update it here.
fn expected_sha256(id: ModelId) -> &'static str {
    match id {
        ModelId::Tiny => "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        ModelId::Base => "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        ModelId::Small => "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        ModelId::Medium => "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        ModelId::Large => "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        ModelId::DistilLarge => "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    }
}

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
        Ok(p) => {
            p.exists()
                && std::fs::metadata(&p)
                    .map(|m| m.len() > min)
                    .unwrap_or(false)
        }
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
    tracing::info!(
        "downloading whisper {} to {}",
        id.as_str(),
        target.display()
    );
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

    // Verify integrity BEFORE promoting the .part file into place. A MITM or
    // a compromised HF mirror could serve a malformed GGML that whisper.cpp
    // mmaps and parses — a known RCE surface. The pinned digest is the
    // out-of-band trust anchor.
    verify_sha256(&tmp, expected_sha256(id)).await?;

    tokio::fs::rename(&tmp, target).await?;
    Ok(())
}

/// Stream-hash `path` with SHA-256 and compare (case-insensitive hex) against
/// `expected`. On mismatch, delete the file and return an error so a bad
/// download is never left on disk or used. Hashing is done on a blocking
/// thread to keep large reads off the async runtime.
async fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let path = path.to_path_buf();
    let expected = expected.to_ascii_lowercase();
    let hash_path = path.clone();
    let actual = tokio::task::spawn_blocking(move || -> Result<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut f = std::fs::File::open(&hash_path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20]; // 1 MiB chunks
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|e| Error::Other(format!("hash task join: {}", e)))??;

    if actual != expected {
        // Don't leave a corrupt/tampered artifact lying around.
        let _ = tokio::fs::remove_file(path).await;
        return Err(Error::Voice(format!(
            "whisper model integrity check failed: expected sha256 {}, got {}",
            expected, actual
        )));
    }
    tracing::info!("whisper model sha256 verified ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verify_sha256_accepts_matching_digest() {
        let dir = std::env::temp_dir().join(format!("pigide-sha-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob.bin");
        std::fs::write(&p, b"hello world").unwrap();
        // sha256("hello world")
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(&p, expected).await.is_ok());
        // File survives a successful check.
        assert!(p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verify_sha256_rejects_and_deletes_on_mismatch() {
        let dir = std::env::temp_dir().join(format!("pigide-sha-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob.bin");
        std::fs::write(&p, b"tampered payload").unwrap();
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let res = verify_sha256(&p, wrong).await;
        assert!(res.is_err());
        // Corrupt/tampered artifact must be removed.
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_model_has_a_pinned_digest() {
        for id in [
            ModelId::Tiny,
            ModelId::Base,
            ModelId::Small,
            ModelId::Medium,
            ModelId::Large,
            ModelId::DistilLarge,
        ] {
            let d = expected_sha256(id);
            assert_eq!(d.len(), 64, "{:?} digest must be 64 hex chars", id);
            assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
