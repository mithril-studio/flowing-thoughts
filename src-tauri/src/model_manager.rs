use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelId {
    LargeV3TurboQ5,
    SmallQ5,
    BaseQ5,
    TinyEn,
    BaseEn,
    DistilSmallEn,
    /// Any other whisper.cpp GGML file the user dropped or downloaded into the
    /// models directory, identified by its filename (e.g. `ggml-medium-q5_0.bin`).
    Custom(String),
}

impl ModelId {
    /// Stable identifier persisted in settings and used as the event key.
    /// Built-ins keep their historical ids; custom models use the filename
    /// without `.bin`.
    pub fn id(&self) -> String {
        match self {
            ModelId::Custom(filename) => filename.trim_end_matches(".bin").to_string(),
            builtin => builtin.spec().id.to_string(),
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "whisper-large-v3-turbo-q5" => Some(ModelId::LargeV3TurboQ5),
            "whisper-small-q5" => Some(ModelId::SmallQ5),
            "whisper-base-q5" => Some(ModelId::BaseQ5),
            "whisper-tiny-en" => Some(ModelId::TinyEn),
            "whisper-base-en" => Some(ModelId::BaseEn),
            "distil-small-en" => Some(ModelId::DistilSmallEn),
            other if is_valid_custom_stem(other) && !is_reserved_filename(&format!("{other}.bin")) => {
                let filename = format!("{other}.bin");
                // A custom id that points at a built-in's file is that built-in.
                Some(
                    ModelId::builtins()
                        .into_iter()
                        .find(|b| b.spec().filename == filename)
                        .unwrap_or(ModelId::Custom(filename)),
                )
            }
            _ => None,
        }
    }

    pub fn builtins() -> [ModelId; 6] {
        [
            ModelId::LargeV3TurboQ5,
            ModelId::SmallQ5,
            ModelId::BaseQ5,
            ModelId::TinyEn,
            ModelId::BaseEn,
            ModelId::DistilSmallEn,
        ]
    }

    /// Multilingual models accept a language hint (or auto-detect); the
    /// `.en` variants only transcribe English. whisper.cpp names English-only
    /// files with a `.en` infix (`ggml-base.en.bin`), which is the only
    /// signal we have for custom files.
    pub fn is_multilingual(&self) -> bool {
        match self {
            ModelId::Custom(filename) => !filename.contains(".en"),
            builtin => builtin.spec().multilingual,
        }
    }

    pub fn filename(&self) -> String {
        match self {
            ModelId::Custom(filename) => filename.clone(),
            builtin => builtin.spec().filename.to_string(),
        }
    }

    fn spec(&self) -> ModelSpec {
        match self {
            ModelId::LargeV3TurboQ5 => ModelSpec {
                id: "whisper-large-v3-turbo-q5",
                display_name: "Whisper Large v3 Turbo (English + Dutch)",
                description: "Best Dutch accuracy. ~1 GB RAM, still under a second on Apple Silicon. Recommended.",
                filename: "ggml-large-v3-turbo-q5_0.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
                sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
                size_bytes: 574_041_195,
                multilingual: true,
            },
            ModelId::SmallQ5 => ModelSpec {
                id: "whisper-small-q5",
                display_name: "Whisper Small (English + Dutch)",
                description: "Good quality under 0.5 GB RAM. Fastest multilingual option.",
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
            ModelId::Custom(_) => unreachable!("custom models have no static spec"),
        }
    }
}

/// Filename stem a user may add: plain characters only, no path tricks, and
/// never the VAD file, which must stay out of the model picker.
fn is_valid_custom_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= 200
        && !stem.starts_with('.')
        && !stem.contains("..")
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The VAD file lives in the same directory but is not a speech model.
fn is_reserved_filename(filename: &str) -> bool {
    filename == VAD_SPEC.filename
}

/// Where a custom model comes from: either a whisper.cpp catalog name such as
/// `medium-q5_0` (looked up in ggerganov/whisper.cpp on Hugging Face) or a
/// direct URL to a `.bin` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomSource {
    pub url: String,
    pub filename: String,
}

pub fn resolve_custom_source(input: &str) -> Result<CustomSource, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a model name (e.g. medium-q5_0) or a .bin URL".to_string());
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        let path = input.split(['?', '#']).next().unwrap_or("");
        let filename = path.rsplit('/').next().unwrap_or("").to_string();
        let stem = filename
            .strip_suffix(".bin")
            .ok_or_else(|| "URL must point at a .bin GGML file".to_string())?;
        if !is_valid_custom_stem(stem) || is_reserved_filename(&filename) {
            return Err(format!("Unsupported model filename: {filename}"));
        }
        return Ok(CustomSource {
            url: input.to_string(),
            filename,
        });
    }
    let stem = input.trim_start_matches("ggml-").trim_end_matches(".bin");
    if !is_valid_custom_stem(stem) {
        return Err(format!(
            "Unsupported model name: {input}. Use letters, digits, '-', '_' and '.' only"
        ));
    }
    let filename = format!("ggml-{stem}.bin");
    if is_reserved_filename(&filename) {
        return Err(format!("{filename} is the voice-activity model, not a speech model"));
    }
    Ok(CustomSource {
        url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{filename}"),
        filename,
    })
}

#[derive(Clone, Copy)]
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

/// One file to fetch into the models directory. Built-ins carry a pinned
/// SHA-256; custom downloads verify against the hash Hugging Face advertises
/// when it does, and are stored unverified (and logged as such) when it doesn't.
#[derive(Debug, Clone)]
struct DownloadJob {
    id: String,
    filename: String,
    url: String,
    sha256: Option<String>,
    size_bytes: u64,
}

impl From<ModelSpec> for DownloadJob {
    fn from(spec: ModelSpec) -> Self {
        DownloadJob {
            id: spec.id.to_string(),
            filename: spec.filename.to_string(),
            url: spec.url.to_string(),
            sha256: Some(spec.sha256.to_string()),
            size_bytes: spec.size_bytes,
        }
    }
}

/// Silero VAD, as packaged for whisper.cpp.
///
/// This is not a transcription model and deliberately never appears in the
/// model picker — it is a prerequisite that gates whether audio reaches the
/// decoder at all. Whisper is an autoregressive language model with no
/// "output nothing" state, so on silence it emits whatever its training data
/// associated with silence (subtitle credits, "Thank you.", or a continuation
/// of our own vocabulary prompt) and reports high confidence while doing it.
/// The only reliable fix is to not hand it silence.
const VAD_SPEC: ModelSpec = ModelSpec {
    id: "silero-vad",
    display_name: "Silero VAD",
    description: "Voice activity detection. Keeps silence away from the decoder.",
    filename: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
    sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
    size_bytes: 885_098,
    multilingual: true,
};

pub fn vad_model_path() -> Result<PathBuf, String> {
    Ok(models_dir()?.join(VAD_SPEC.filename))
}

pub fn vad_model_installed() -> bool {
    vad_model_path().map(|p| p.exists()).unwrap_or(false)
}

/// Fetch the VAD model if it isn't on disk yet. Cheap (865 KB) next to the
/// 190 MB speech model, so this runs unprompted at startup. A failure here is
/// never fatal: transcription still works, just without the silence gate.
pub async fn ensure_vad_model(app: AppHandle) -> Result<(), String> {
    if vad_model_installed() {
        return Ok(());
    }
    download_job(app, VAD_SPEC.into()).await
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
    /// True for files the user added themselves (not in the built-in catalog).
    pub custom: bool,
    /// Catalog entries the user removed from the picker (never downloaded).
    pub hidden: bool,
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

pub fn model_path(id: &ModelId) -> Result<PathBuf, String> {
    Ok(models_dir()?.join(id.filename()))
}

/// `hidden` holds catalog ids the user removed from the picker; the entries
/// are still returned (flagged) so the UI can offer to restore them.
pub fn list_installed(hidden: &[String]) -> Result<Vec<InstalledModel>, String> {
    let dir = models_dir()?;
    let mut out = Vec::new();
    for id in ModelId::builtins() {
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
            custom: false,
            hidden: !installed && hidden.iter().any(|h| h == spec.id),
        });
    }

    // Anything else ending in .bin is a user-added whisper.cpp model.
    let builtin_files: Vec<String> = ModelId::builtins().iter().map(|b| b.filename()).collect();
    let mut customs: Vec<InstalledModel> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let filename = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = filename.strip_suffix(".bin") else { continue };
            if builtin_files.contains(&filename)
                || filename == VAD_SPEC.filename
                || !is_valid_custom_stem(stem)
            {
                continue;
            }
            let id = ModelId::Custom(filename.clone());
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            customs.push(InstalledModel {
                id: id.id(),
                display_name: format!("{} (custom)", stem.trim_start_matches("ggml-")),
                description: "Your own whisper.cpp GGML model.".to_string(),
                filename: filename.clone(),
                installed: true,
                expected_size_bytes: size,
                local_path: Some(dir.join(&filename).to_string_lossy().into_owned()),
                multilingual: id.is_multilingual(),
                custom: true,
                hidden: false,
            });
        }
    }
    customs.sort_by(|a, b| a.filename.cmp(&b.filename));
    out.extend(customs);
    Ok(out)
}

pub fn delete_model(id: &ModelId) -> Result<(), String> {
    let path = model_path(id)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete model file: {e}"))?;
    }
    Ok(())
}

pub async fn download_model(app: AppHandle, id: ModelId) -> Result<(), String> {
    match id {
        ModelId::Custom(_) => Err(
            "Custom models are added by name or URL, not re-downloaded from the catalog".to_string(),
        ),
        builtin => download_job(app, builtin.spec().into()).await,
    }
}

/// Resolve a user-entered model name or URL, then fetch it in the background.
/// Returns the model id immediately so the UI can follow the same
/// `model-download-*` events built-ins emit.
pub async fn add_custom_model(app: AppHandle, source: &str) -> Result<String, String> {
    let resolved = resolve_custom_source(source)?;
    let id = ModelId::from_str(resolved.filename.trim_end_matches(".bin"))
        .ok_or_else(|| format!("Unsupported model filename: {}", resolved.filename))?;

    let job = match &id {
        ModelId::Custom(_) => {
            let (sha256, size_bytes) = hugging_face_metadata(&resolved.url).await;
            if sha256.is_none() {
                let _ = crate::storage::append_log(
                    "WARN",
                    &format!(
                        "Adding custom model {} without a published SHA-256; file will not be verified",
                        resolved.filename
                    ),
                );
            }
            DownloadJob {
                id: id.id(),
                filename: resolved.filename.clone(),
                url: resolved.url.clone(),
                sha256,
                size_bytes,
            }
        }
        // The name matched a catalog model: use its pinned hash.
        builtin => builtin.spec().into(),
    };

    let model_id = job.id.clone();
    let path = models_dir()?.join(&job.filename);
    if path.exists() {
        return Err(format!("{} is already installed", job.filename));
    }
    tauri::async_runtime::spawn(async move {
        if let Err(e) = download_job(app, job).await {
            let _ = crate::storage::append_log("ERROR", &format!("Custom model download failed: {e}"));
        }
    });
    Ok(model_id)
}

/// Hugging Face answers a HEAD on `/resolve/` with a redirect that carries the
/// LFS file's SHA-256 (`x-linked-etag`) and size (`x-linked-size`). Anything
/// else (other hosts, network trouble) yields no hash and a zero size.
async fn hugging_face_metadata(url: &str) -> (Option<String>, u64) {
    if !url.starts_with("https://huggingface.co/") {
        return (None, 0);
    }
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, 0),
    };
    let Ok(resp) = client.head(url).send().await else {
        return (None, 0);
    };
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_string())
    };
    let sha = header("x-linked-etag").filter(|v| v.len() == 64);
    let size = header("x-linked-size")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    (sha, size)
}

async fn download_job(app: AppHandle, spec: DownloadJob) -> Result<(), String> {
    let dir = models_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create models directory: {e}"))?;

    let final_path = dir.join(&spec.filename);
    let temp_path = dir.join(format!("{}.partial", spec.filename));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let response = client
        .get(&spec.url)
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
    if spec.sha256.as_deref().is_some_and(|expected| expected != hex) {
        let _ = std::fs::remove_file(&temp_path);
        let msg = format!(
            "SHA256 mismatch for {}: expected {}, got {}",
            spec.id,
            spec.sha256.as_deref().unwrap_or(""),
            hex
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_name_resolves_to_hugging_face_url() {
        let got = resolve_custom_source("medium-q5_0").unwrap();
        assert_eq!(got.filename, "ggml-medium-q5_0.bin");
        assert_eq!(
            got.url,
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin"
        );
        assert_eq!(resolve_custom_source("ggml-medium-q5_0.bin").unwrap(), got);
    }

    #[test]
    fn direct_url_keeps_its_filename() {
        let got = resolve_custom_source(
            "https://huggingface.co/someone/dutch-whisper/resolve/main/ggml-model.bin?download=true",
        )
        .unwrap();
        assert_eq!(got.filename, "ggml-model.bin");
        assert!(got.url.starts_with("https://huggingface.co/someone/"));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(resolve_custom_source("").is_err());
        assert!(resolve_custom_source("../etc/passwd").is_err());
        assert!(resolve_custom_source("https://example.com/model.zip").is_err());
        assert!(resolve_custom_source("ggml-silero-v5.1.2").is_err());
        assert!(ModelId::from_str("has space").is_none());
    }

    #[test]
    fn custom_ids_round_trip_and_detect_english_only() {
        let id = ModelId::from_str("ggml-medium-q5_0").unwrap();
        assert_eq!(id, ModelId::Custom("ggml-medium-q5_0.bin".to_string()));
        assert_eq!(id.id(), "ggml-medium-q5_0");
        assert!(id.is_multilingual());
        assert!(!ModelId::from_str("ggml-medium.en").unwrap().is_multilingual());
        assert_eq!(ModelId::from_str("ggml-small-q5_1"), Some(ModelId::SmallQ5));
        assert_eq!(ModelId::from_str("whisper-small-q5"), Some(ModelId::SmallQ5));
    }
}
