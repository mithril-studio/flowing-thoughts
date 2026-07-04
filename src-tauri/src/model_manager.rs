use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelId {
    SmallQ5,
    BaseQ5,
    TinyEn,
    BaseEn,
    DistilSmallEn,
}

impl ModelId {
    pub fn as_str(self) -> &'static str {
        self.spec().id
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "whisper-small-q5" => Some(ModelId::SmallQ5),
            "whisper-base-q5" => Some(ModelId::BaseQ5),
            "whisper-tiny-en" => Some(ModelId::TinyEn),
            "whisper-base-en" => Some(ModelId::BaseEn),
            "distil-small-en" => Some(ModelId::DistilSmallEn),
            _ => None,
        }
    }

    pub fn all() -> [ModelId; 5] {
        [
            ModelId::SmallQ5,
            ModelId::BaseQ5,
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::DistilSmallEn,
        ]
    }

    /// Multilingual models accept a language hint (or auto-detect); the
    /// `.en` variants only transcribe English.
    pub fn is_multilingual(self) -> bool {
        matches!(self, ModelId::SmallQ5 | ModelId::BaseQ5)
    }

    fn spec(self) -> ModelSpec {
        match self {
            ModelId::SmallQ5 => ModelSpec {
                id: "whisper-small-q5",
                display_name: "Whisper Small (English + Dutch)",
                description: "Best quality under 0.5 GB RAM. Recommended.",
                filename: "ggml-small-q5_1.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
                sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
                size_bytes: 190_085_487,
                multilingual: true,
            },
            ModelId::BaseQ5 => ModelSpec {
                id: "whisper-base-q5",
                display_name: "Whisper Base (English + Dutch)",
                description: "Fast and tiny. Lower accuracy than Small.",
                filename: "ggml-base-q5_1.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin",
                sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
                size_bytes: 59_707_625,
                multilingual: true,
            },
            ModelId::TinyEn => ModelSpec {
                id: "whisper-tiny-en",
                display_name: "Whisper Tiny (English only)",
                description: "Fastest, English only.",
                filename: "ggml-tiny.en.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
                sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
                size_bytes: 77_704_715,
                multilingual: false,
            },
            ModelId::BaseEn => ModelSpec {
                id: "whisper-base-en",
                display_name: "Whisper Base (English only)",
                description: "Balanced, English only.",
                filename: "ggml-base.en.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
                sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
                size_bytes: 147_964_211,
                multilingual: false,
            },
            ModelId::DistilSmallEn => ModelSpec {
                id: "distil-small-en",
                display_name: "Distil-Whisper Small (English only)",
                description: "High English accuracy, no Dutch.",
                filename: "ggml-distil-small.en.bin",
                url: "https://huggingface.co/distil-whisper/distil-small.en/resolve/main/ggml-distil-small.en.bin",
                sha256: "7691eb11167ab7aaf6b3e05d8266f2fd9ad89c550e433f86ac266ebdee6c970a",
                size_bytes: 336_191_657,
                multilingual: false,
            },
        }
    }
}

struct ModelSpec {
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
    size_bytes: u64,
    multilingual: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledModel {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub filename: String,
    pub installed: bool,
    pub expected_size_bytes: u64,
    pub local_path: Option<String>,
    pub multilingual: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadProgressEvent {
    pub id: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadCompleteEvent {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadErrorEvent {
    pub id: String,
    pub message: String,
}

pub fn models_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("models"))
}

pub fn model_path(id: ModelId) -> Result<PathBuf, String> {
    Ok(models_dir()?.join(id.spec().filename))
}

pub fn list_installed() -> Result<Vec<InstalledModel>, String> {
    let dir = models_dir()?;
    let mut out = Vec::new();
    for id in ModelId::all() {
        let spec = id.spec();
        let path = dir.join(spec.filename);
        let installed = path.exists();
        out.push(InstalledModel {
            id: spec.id.to_string(),
            display_name: spec.display_name.to_string(),
            description: spec.description.to_string(),
            filename: spec.filename.to_string(),
            installed,
            expected_size_bytes: spec.size_bytes,
            local_path: if installed {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            },
            multilingual: spec.multilingual,
        });
    }
    Ok(out)
}

pub fn delete_model(id: ModelId) -> Result<(), String> {
    let path = model_path(id)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete model file: {e}"))?;
    }
    Ok(())
}

pub async fn download_model(app: AppHandle, id: ModelId) -> Result<(), String> {
    let spec = id.spec();
    let dir = models_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create models directory: {e}"))?;

    let final_path = dir.join(spec.filename);
    let temp_path = dir.join(format!("{}.partial", spec.filename));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let response = client
        .get(spec.url)
        .send()
        .await
        .map_err(|e| format!("Failed to start download for {}: {e}", spec.id))?;

    if !response.status().is_success() {
        return Err(format!(
            "Download for {} returned HTTP {}",
            spec.id,
            response.status()
        ));
    }

    let total_bytes = response.content_length().unwrap_or(spec.size_bytes);
    let mut file = File::create(&temp_path)
        .await
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    let mut hasher = Sha256::new();
    let mut bytes_downloaded: u64 = 0;
    let mut last_emit = Instant::now();
    let emit_interval = Duration::from_millis(200);

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download stream error: {e}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Failed to write model chunk: {e}"))?;
        bytes_downloaded += chunk.len() as u64;

        if last_emit.elapsed() >= emit_interval {
            let _ = app.emit(
                "model-download-progress",
                DownloadProgressEvent {
                    id: spec.id.to_string(),
                    bytes_downloaded,
                    total_bytes,
                },
            );
            last_emit = Instant::now();
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("Failed to flush model file: {e}"))?;
    drop(file);

    let digest = hasher.finalize();
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    if hex != spec.sha256 {
        let _ = std::fs::remove_file(&temp_path);
        let msg = format!(
            "SHA256 mismatch for {}: expected {}, got {}",
            spec.id, spec.sha256, hex
        );
        let _ = app.emit(
            "model-download-error",
            DownloadErrorEvent {
                id: spec.id.to_string(),
                message: msg.clone(),
            },
        );
        return Err(msg);
    }

    std::fs::rename(&temp_path, &final_path)
        .map_err(|e| format!("Failed to finalize model file: {e}"))?;

    let _ = app.emit(
        "model-download-progress",
        DownloadProgressEvent {
            id: spec.id.to_string(),
            bytes_downloaded: total_bytes,
            total_bytes,
        },
    );
    let _ = app.emit(
        "model-download-complete",
        DownloadCompleteEvent {
            id: spec.id.to_string(),
            path: final_path.to_string_lossy().into_owned(),
        },
    );

    Ok(())
}
