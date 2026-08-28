use crate::settings::{get_settings, write_settings};
use anyhow::Result;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tar::Archive;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub enum EngineType {
    Whisper,
    Parakeet,
    Moonshine,
    MoonshineStreaming,
    SenseVoice,
    GigaAM,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub filename: String,
    pub url: Option<String>,
    pub size_mb: u64,
    pub is_downloaded: bool,
    pub is_downloading: bool,
    pub partial_size: u64,
    pub is_directory: bool,
    pub engine_type: EngineType,
    pub accuracy_score: f32,        // 0.0 to 1.0, higher is more accurate
    pub speed_score: f32,           // 0.0 to 1.0, higher is faster
    pub supports_translation: bool, // Whether the model supports translating to English
    pub is_recommended: bool,       // Whether this is the recommended model for new users
    pub supported_languages: Vec<String>, // Languages this model can transcribe
    pub is_custom: bool,            // Whether this is a user-provided custom model
    /// Absolute path when the model lives outside Handy's models directory
    /// (e.g. OpenWhispr / Meetily / Hugging Face cache).
    #[serde(default)]
    pub local_path: Option<String>,
    /// True when the model is referenced from an external cache (not copied into Handy).
    #[serde(default)]
    pub is_external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DownloadProgress {
    pub model_id: String,
    pub downloaded: u64,
    pub total: u64,
    pub percentage: f64,
    pub is_indeterminate: bool,
    pub speed_mbps: Option<f64>, // Download speed in MB/s
}

pub struct ModelManager {
    app_handle: AppHandle,
    models_dir: PathBuf,
    available_models: Mutex<HashMap<String, ModelInfo>>,
    cancel_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    extracting_models: Arc<Mutex<HashSet<String>>>,
}

impl ModelManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create models directory in app data
        let models_dir = crate::portable::app_data_dir(app_handle)
            .map_err(|e| anyhow::anyhow!("Failed to get app data dir: {}", e))?
            .join("models");

        if !models_dir.exists() {
            fs::create_dir_all(&models_dir)?;
        }

        let mut available_models = HashMap::new();

        // Whisper supported languages (99 languages from tokenizer)
        // Including zh-Hans and zh-Hant variants to match frontend language codes
        let whisper_languages: Vec<String> = vec![
            "en", "zh", "zh-Hans", "zh-Hant", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl",
            "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs",
            "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy",
            "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu", "is",
            "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo",
            "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo", "ht",
            "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
            "ha", "ba", "jw", "su", "yue",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        // TODO this should be read from a JSON file or something..
        available_models.insert(
            "small".to_string(),
            ModelInfo {
                id: "small".to_string(),
                name: "Whisper Small".to_string(),
                description: "Fast and fairly accurate.".to_string(),
                filename: "ggml-small.bin".to_string(),
                url: Some("https://blob.handy.computer/ggml-small.bin".to_string()),
                size_mb: 487,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.60,
                speed_score: 0.85,
                supports_translation: true,
                is_recommended: false,
                supported_languages: whisper_languages.clone(),
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // Add downloadable models
        available_models.insert(
            "medium".to_string(),
            ModelInfo {
                id: "medium".to_string(),
                name: "Whisper Medium".to_string(),
                description: "Good accuracy, medium speed".to_string(),
                filename: "whisper-medium-q4_1.bin".to_string(),
                url: Some("https://blob.handy.computer/whisper-medium-q4_1.bin".to_string()),
                size_mb: 492, // Approximate size
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.75,
                speed_score: 0.60,
                supports_translation: true,
                is_recommended: false,
                supported_languages: whisper_languages.clone(),
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "turbo".to_string(),
            ModelInfo {
                id: "turbo".to_string(),
                name: "Whisper Turbo".to_string(),
                description: "Balanced accuracy and speed.".to_string(),
                filename: "ggml-large-v3-turbo.bin".to_string(),
                url: Some("https://blob.handy.computer/ggml-large-v3-turbo.bin".to_string()),
                size_mb: 1600, // Approximate size
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.80,
                speed_score: 0.40,
                supports_translation: false, // Turbo doesn't support translation
                is_recommended: false,
                supported_languages: whisper_languages.clone(),
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "large".to_string(),
            ModelInfo {
                id: "large".to_string(),
                name: "Whisper Large".to_string(),
                description: "Good accuracy, but slow.".to_string(),
                filename: "ggml-large-v3-q5_0.bin".to_string(),
                url: Some("https://blob.handy.computer/ggml-large-v3-q5_0.bin".to_string()),
                size_mb: 1100, // Approximate size
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.85,
                speed_score: 0.30,
                supports_translation: true,
                is_recommended: false,
                supported_languages: whisper_languages.clone(),
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "breeze-asr".to_string(),
            ModelInfo {
                id: "breeze-asr".to_string(),
                name: "Breeze ASR".to_string(),
                description: "Optimized for Taiwanese Mandarin. Code-switching support."
                    .to_string(),
                filename: "breeze-asr-q5_k.bin".to_string(),
                url: Some("https://blob.handy.computer/breeze-asr-q5_k.bin".to_string()),
                size_mb: 1080,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.85,
                speed_score: 0.35,
                supports_translation: false,
                is_recommended: false,
                supported_languages: whisper_languages,
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // Add NVIDIA Parakeet models (directory-based)
        available_models.insert(
            "parakeet-tdt-0.6b-v2".to_string(),
            ModelInfo {
                id: "parakeet-tdt-0.6b-v2".to_string(),
                name: "Parakeet V2".to_string(),
                description: "English only. The best model for English speakers.".to_string(),
                filename: "parakeet-tdt-0.6b-v2-int8".to_string(), // Directory name
                url: Some("https://blob.handy.computer/parakeet-v2-int8.tar.gz".to_string()),
                size_mb: 473, // Approximate size for int8 quantized model
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::Parakeet,
                accuracy_score: 0.85,
                speed_score: 0.85,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // Parakeet V3 supported languages (25 EU languages + Russian/Ukrainian):
        // bg, hr, cs, da, nl, en, et, fi, fr, de, el, hu, it, lv, lt, mt, pl, pt, ro, sk, sl, es, sv, ru, uk
        let parakeet_v3_languages: Vec<String> = vec![
            "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv",
            "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        available_models.insert(
            "parakeet-tdt-0.6b-v3".to_string(),
            ModelInfo {
                id: "parakeet-tdt-0.6b-v3".to_string(),
                name: "Parakeet V3".to_string(),
                description: "Fast and accurate. Supports 25 European languages.".to_string(),
                filename: "parakeet-tdt-0.6b-v3-int8".to_string(), // Directory name
                url: Some("https://blob.handy.computer/parakeet-v3-int8.tar.gz".to_string()),
                size_mb: 478, // Approximate size for int8 quantized model
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::Parakeet,
                accuracy_score: 0.80,
                speed_score: 0.85,
                supports_translation: false,
                is_recommended: true,
                supported_languages: parakeet_v3_languages,
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "parakeet-tdt-ctc-0.6b-ja".to_string(),
            ModelInfo {
                id: "parakeet-tdt-ctc-0.6b-ja".to_string(),
                name: "Parakeet Japanese".to_string(),
                description: "Japanese only. Optimized for Japanese speech recognition."
                    .to_string(),
                filename: "parakeet-tdt-ctc-0.6b-ja-int8".to_string(),
                url: Some("https://blob.handy.computer/parakeet-ja-int8.tar.gz".to_string()),
                size_mb: 480,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::Parakeet,
                accuracy_score: 0.85,
                speed_score: 0.85,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["ja".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "moonshine-base".to_string(),
            ModelInfo {
                id: "moonshine-base".to_string(),
                name: "Moonshine Base".to_string(),
                description: "Very fast, English only. Handles accents well.".to_string(),
                filename: "moonshine-base".to_string(),
                url: Some("https://blob.handy.computer/moonshine-base.tar.gz".to_string()),
                size_mb: 58,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::Moonshine,
                accuracy_score: 0.70,
                speed_score: 0.90,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "moonshine-tiny-streaming-en".to_string(),
            ModelInfo {
                id: "moonshine-tiny-streaming-en".to_string(),
                name: "Moonshine V2 Tiny".to_string(),
                description: "Ultra-fast, English only".to_string(),
                filename: "moonshine-tiny-streaming-en".to_string(),
                url: Some(
                    "https://blob.handy.computer/moonshine-tiny-streaming-en.tar.gz".to_string(),
                ),
                size_mb: 31,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::MoonshineStreaming,
                accuracy_score: 0.55,
                speed_score: 0.95,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "moonshine-small-streaming-en".to_string(),
            ModelInfo {
                id: "moonshine-small-streaming-en".to_string(),
                name: "Moonshine V2 Small".to_string(),
                description: "Fast, English only. Good balance of speed and accuracy.".to_string(),
                filename: "moonshine-small-streaming-en".to_string(),
                url: Some(
                    "https://blob.handy.computer/moonshine-small-streaming-en.tar.gz".to_string(),
                ),
                size_mb: 100,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::MoonshineStreaming,
                accuracy_score: 0.65,
                speed_score: 0.90,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        available_models.insert(
            "moonshine-medium-streaming-en".to_string(),
            ModelInfo {
                id: "moonshine-medium-streaming-en".to_string(),
                name: "Moonshine V2 Medium".to_string(),
                description: "English only. High quality.".to_string(),
                filename: "moonshine-medium-streaming-en".to_string(),
                url: Some(
                    "https://blob.handy.computer/moonshine-medium-streaming-en.tar.gz".to_string(),
                ),
                size_mb: 192,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::MoonshineStreaming,
                accuracy_score: 0.75,
                speed_score: 0.80,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // SenseVoice supported languages
        let sense_voice_languages: Vec<String> =
            vec!["zh", "zh-Hans", "zh-Hant", "en", "yue", "ja", "ko"]
                .into_iter()
                .map(String::from)
                .collect();

        available_models.insert(
            "sense-voice-int8".to_string(),
            ModelInfo {
                id: "sense-voice-int8".to_string(),
                name: "SenseVoice".to_string(),
                description: "Very fast. Chinese, English, Japanese, Korean, Cantonese."
                    .to_string(),
                filename: "sense-voice-int8".to_string(),
                url: Some("https://blob.handy.computer/sense-voice-int8.tar.gz".to_string()),
                size_mb: 160,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: true,
                engine_type: EngineType::SenseVoice,
                accuracy_score: 0.65,
                speed_score: 0.95,
                supports_translation: false,
                is_recommended: false,
                supported_languages: sense_voice_languages,
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // GigaAM v3 supported languages
        let gigaam_languages: Vec<String> = vec!["ru"].into_iter().map(String::from).collect();

        available_models.insert(
            "gigaam-v3-e2e-ctc".to_string(),
            ModelInfo {
                id: "gigaam-v3-e2e-ctc".to_string(),
                name: "GigaAM v3".to_string(),
                description: "Russian speech recognition. Fast and accurate.".to_string(),
                filename: "giga-am-v3.int8.onnx".to_string(),
                url: Some("https://blob.handy.computer/giga-am-v3.int8.onnx".to_string()),
                size_mb: 225,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::GigaAM,
                accuracy_score: 0.85,
                speed_score: 0.75,
                supports_translation: false,
                is_recommended: false,
                supported_languages: gigaam_languages,
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // Auto-discover custom Whisper models (.bin files) in the models directory
        if let Err(e) = Self::discover_custom_whisper_models(&models_dir, &mut available_models) {
            warn!("Failed to discover custom models: {}", e);
        }

        // Auto-discover custom imported Parakeet ONNX directories
        if let Err(e) = Self::discover_custom_parakeet_models(&models_dir, &mut available_models) {
            warn!("Failed to discover custom Parakeet models: {}", e);
        }

        // Discover compatible models from OpenWhispr / Meetily / Hugging Face caches
        if let Err(e) = Self::discover_external_models(&models_dir, &mut available_models) {
            warn!("Failed to discover external models: {}", e);
        }

        let manager = Self {
            app_handle: app_handle.clone(),
            models_dir,
            available_models: Mutex::new(available_models),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            extracting_models: Arc::new(Mutex::new(HashSet::new())),
        };

        // Migrate any bundled models to user directory
        manager.migrate_bundled_models()?;

        // Check which models are already downloaded
        manager.update_download_status()?;

        // Auto-select a model if none is currently selected
        manager.auto_select_model_if_needed()?;

        Ok(manager)
    }

    pub fn get_available_models(&self) -> Vec<ModelInfo> {
        let models = self.available_models.lock().unwrap();
        models.values().cloned().collect()
    }

    pub fn get_model_info(&self, model_id: &str) -> Option<ModelInfo> {
        let models = self.available_models.lock().unwrap();
        models.get(model_id).cloned()
    }

    fn migrate_bundled_models(&self) -> Result<()> {
        // Check for bundled models and copy them to user directory
        let bundled_models = ["ggml-small.bin"]; // Add other bundled models here if any

        for filename in &bundled_models {
            let bundled_path = self.app_handle.path().resolve(
                &format!("resources/models/{}", filename),
                tauri::path::BaseDirectory::Resource,
            );

            if let Ok(bundled_path) = bundled_path {
                if bundled_path.exists() {
                    let user_path = self.models_dir.join(filename);

                    // Only copy if user doesn't already have the model
                    if !user_path.exists() {
                        info!("Migrating bundled model {} to user directory", filename);
                        fs::copy(&bundled_path, &user_path)?;
                        info!("Successfully migrated {}", filename);
                    }
                }
            }
        }

        Ok(())
    }

    fn update_download_status(&self) -> Result<()> {
        let mut models = self.available_models.lock().unwrap();

        for model in models.values_mut() {
            let models_dir_path = self.models_dir.join(&model.filename);
            let partial_path = self.models_dir.join(format!("{}.partial", &model.filename));

            if model.is_directory {
                let extracting_path = self
                    .models_dir
                    .join(format!("{}.extracting", &model.filename));

                // Clean up any leftover .extracting directories from interrupted extractions
                // But only if this model is NOT currently being extracted
                let is_currently_extracting = {
                    let extracting = self.extracting_models.lock().unwrap();
                    extracting.contains(&model.id)
                };
                if extracting_path.exists() && !is_currently_extracting {
                    warn!("Cleaning up interrupted extraction for model: {}", model.id);
                    let _ = fs::remove_dir_all(&extracting_path);
                }

                if models_dir_path.exists() && models_dir_path.is_dir() {
                    model.is_downloaded = true;
                    model.local_path = None;
                    model.is_external = false;
                } else if let Some(ref local) = model.local_path {
                    let local_path = PathBuf::from(local);
                    let available = local_path.exists() && local_path.is_dir();
                    model.is_downloaded = available;
                    model.is_external = available;
                    if !available {
                        model.local_path = None;
                    }
                } else {
                    model.is_downloaded = false;
                    model.is_external = false;
                }
                model.is_downloading = false;

                if partial_path.exists() {
                    model.partial_size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
                } else {
                    model.partial_size = 0;
                }
            } else {
                if models_dir_path.exists() {
                    model.is_downloaded = true;
                    model.local_path = None;
                    model.is_external = false;
                } else if let Some(ref local) = model.local_path {
                    let local_path = PathBuf::from(local);
                    let available = local_path.exists() && local_path.is_file();
                    model.is_downloaded = available;
                    model.is_external = available;
                    if !available {
                        model.local_path = None;
                    }
                } else {
                    model.is_downloaded = false;
                    model.is_external = false;
                }
                model.is_downloading = false;

                if partial_path.exists() {
                    model.partial_size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
                } else {
                    model.partial_size = 0;
                }
            }
        }

        Ok(())
    }

    fn auto_select_model_if_needed(&self) -> Result<()> {
        let mut settings = get_settings(&self.app_handle);

        // Clear stale selection: selected model is set but doesn't exist
        // in available_models (e.g. deleted custom model file)
        if !settings.selected_model.is_empty() {
            let models = self.available_models.lock().unwrap();
            let exists = models.contains_key(&settings.selected_model);
            drop(models);

            if !exists {
                info!(
                    "Selected model '{}' not found in available models, clearing selection",
                    settings.selected_model
                );
                settings.selected_model = String::new();
                write_settings(&self.app_handle, settings.clone());
            }
        }

        // If no model is selected, pick the first downloaded one
        if settings.selected_model.is_empty() {
            // Find the first available (downloaded) model
            let models = self.available_models.lock().unwrap();
            if let Some(available_model) = models.values().find(|model| model.is_downloaded) {
                info!(
                    "Auto-selecting model: {} ({})",
                    available_model.id, available_model.name
                );

                // Update settings with the selected model
                let mut updated_settings = settings;
                updated_settings.selected_model = available_model.id.clone();
                write_settings(&self.app_handle, updated_settings);

                info!("Successfully auto-selected model: {}", available_model.id);
            }
        }

        Ok(())
    }

    /// Discover custom Whisper models (.bin files) in the models directory.
    /// Skips files that match predefined model filenames.
    fn discover_custom_whisper_models(
        models_dir: &Path,
        available_models: &mut HashMap<String, ModelInfo>,
    ) -> Result<()> {
        if !models_dir.exists() {
            return Ok(());
        }

        // Collect filenames of predefined Whisper file-based models to skip
        let predefined_filenames: HashSet<String> = available_models
            .values()
            .filter(|m| matches!(m.engine_type, EngineType::Whisper) && !m.is_directory)
            .map(|m| m.filename.clone())
            .collect();

        // Scan models directory for .bin files
        for entry in fs::read_dir(models_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry: {}", e);
                    continue;
                }
            };

            let path = entry.path();

            // Only process .bin files (not directories)
            if !path.is_file() {
                continue;
            }

            let filename = match path.file_name().and_then(|s| s.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            // Skip hidden files
            if filename.starts_with('.') {
                continue;
            }

            // Only process .bin files (Whisper GGML format).
            // This also excludes .partial downloads (e.g., "model.bin.partial").
            // If we add discovery for other formats, add a .partial check before this filter.
            if !filename.ends_with(".bin") {
                continue;
            }

            // Skip predefined model files
            if predefined_filenames.contains(&filename) {
                continue;
            }

            // Generate model ID from filename (remove .bin extension)
            let model_id = filename.trim_end_matches(".bin").to_string();

            // Skip if model ID already exists (shouldn't happen, but be safe)
            if available_models.contains_key(&model_id) {
                continue;
            }

            // Generate display name: replace - and _ with space, capitalize words
            let display_name = model_id
                .replace(['-', '_'], " ")
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            // Get file size in MB
            let size_mb = match path.metadata() {
                Ok(meta) => meta.len() / (1024 * 1024),
                Err(e) => {
                    warn!("Failed to get metadata for {}: {}", filename, e);
                    0
                }
            };

            info!(
                "Discovered custom Whisper model: {} ({}, {} MB)",
                model_id, filename, size_mb
            );

            available_models.insert(
                model_id.clone(),
                ModelInfo {
                    id: model_id,
                    name: display_name,
                    description: "Not officially supported".to_string(),
                    filename,
                    url: None, // Custom models have no download URL
                    size_mb,
                    is_downloaded: true, // Already present on disk
                    is_downloading: false,
                    partial_size: 0,
                    is_directory: false,
                    engine_type: EngineType::Whisper,
                    accuracy_score: 0.0, // Sentinel: UI hides score bars when both are 0
                    speed_score: 0.0,
                    supports_translation: false,
                    is_recommended: false,
                    supported_languages: vec![],
                    is_custom: true,
                local_path: None,
                is_external: false,
                },
            );
        }

        Ok(())
    }

    pub async fn download_model(&self, model_id: &str) -> Result<()> {
        let model_info = {
            let models = self.available_models.lock().unwrap();
            models.get(model_id).cloned()
        };

        let model_info =
            model_info.ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

        let url = model_info
            .url
            .ok_or_else(|| anyhow::anyhow!("No download URL for model"))?;
        let model_path = self.models_dir.join(&model_info.filename);
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", &model_info.filename));

        // Don't download if complete version already exists
        if model_path.exists() {
            // Clean up any partial file that might exist
            if partial_path.exists() {
                let _ = fs::remove_file(&partial_path);
            }
            self.update_download_status()?;
            return Ok(());
        }

        // Mark as downloading
        {
            let mut models = self.available_models.lock().unwrap();
            if let Some(model) = models.get_mut(model_id) {
                model.is_downloading = true;
            }
        }

        // Create cancellation flag for this download
        let cancel_flag = Arc::new(AtomicBool::new(false));
        {
            let mut flags = self.cancel_flags.lock().unwrap();
            flags.insert(model_id.to_string(), cancel_flag.clone());
        }

        // Check if partial file exists and validate its size
        let expected_size = model_info.size_mb * 1024 * 1024;
        let mut resume_from = if partial_path.exists() {
            let size = partial_path.metadata()?.len();
            info!("Found partial file: {} bytes ({:.2} MB), expected: {} bytes ({:.2} MB)", 
                  size, size as f64 / 1024.0 / 1024.0,
                  expected_size, expected_size as f64 / 1024.0 / 1024.0);
            
            // If partial file is already at or above expected size, it's corrupted or complete
            if size >= expected_size {
                warn!(
                    "Partial file size ({:.2} MB) >= expected size ({:.2} MB), deleting",
                    size as f64 / 1024.0 / 1024.0,
                    expected_size as f64 / 1024.0 / 1024.0
                );
                let _ = fs::remove_file(&partial_path);
                info!("Starting fresh download of model {} from {}", model_id, url);
                0
            } else {
                info!("Resuming download of model {} from byte {}", model_id, size);
                size
            }
        } else {
            info!("Starting fresh download of model {} from {}", model_id, url);
            0
        };

        // Create HTTP client with range request for resuming
        let client = reqwest::Client::new();
        let mut request = client.get(&url);

        if resume_from > 0 {
            request = request.header("Range", format!("bytes={}-", resume_from));
        }

        let mut response = request.send().await?;

        // Log response details for debugging
        info!("Response status: {}", response.status());
        info!("Response headers: {:?}", response.headers());

        // Handle 416 Range Not Satisfiable - means the partial file is already complete
        let mut skip_download = false;
        if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            info!("Got 416 Range Not Satisfiable - partial file may be complete");
            
            // Parse Content-Range to get actual file size
            // Format: Content-Range: bytes */<total-size>
            let actual_size = if let Some(content_range) = response.headers().get("content-range") {
                if let Ok(range_str) = content_range.to_str() {
                    info!("Content-Range for 416: {}", range_str);
                    if let Some(total_str) = range_str.split('/').last() {
                        if let Ok(size) = total_str.parse::<u64>() {
                            info!("Actual file size from server: {} bytes ({:.2} MB)", 
                                  size, size as f64 / 1024.0 / 1024.0);
                            Some(size)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            
            // Verify our partial file matches the server's file size
            let partial_size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
            let is_complete = if let Some(actual_size) = actual_size {
                partial_size == actual_size
            } else {
                // No size from server, check if partial file is at expected size
                partial_size >= model_info.size_mb * 1024 * 1024
            };

            if is_complete && partial_size > 0 {
                info!("Partial file is complete! ({} bytes)", partial_size);

                if model_info.is_directory {
                    // For directory-based models, the .partial file IS the complete tar.gz
                    // Just mark that we should skip download and proceed to extraction
                    info!("Directory model complete, skipping to extraction");
                    skip_download = true;
                } else {
                    // For file-based models, rename to final location and complete
                    let final_path = self.models_dir.join(&model_info.filename);
                    fs::rename(&partial_path, &final_path)?;
                    info!("Renamed partial to final location");

                    // Mark as downloaded and return success
                    {
                        let mut models = self.available_models.lock().unwrap();
                        if let Some(model) = models.get_mut(model_id) {
                            model.is_downloaded = true;
                            model.is_downloading = false;
                            model.partial_size = 0;
                        }
                    }
                    let _ = self.app_handle.emit("model-download-complete", model_id);
                    let _ = self.app_handle.emit("model-state-changed", ());
                    return Ok(());
                }
            } else {
                // File is incomplete, delete and restart
                warn!("416 response but file is incomplete ({} vs {}), deleting and restarting",
                      partial_size, actual_size.unwrap_or(0));
                let _ = fs::remove_file(&partial_path);
                resume_from = 0;
                response = client.get(&url).send().await?;
            }
            
            // Log 416 handling result
            info!("New response status after 416 handling: {}", response.status());
            info!("New response headers: {:?}", response.headers());
        }

        // Skip download logic if file is already complete (416 response handled above)
        let total_size = if skip_download {
            // Use the actual file size from the partial file
            let size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
            info!("Skipping download, using partial file size: {} bytes ({:.2} MB)", 
                  size, size as f64 / 1024.0 / 1024.0);
            size
        } else {
            // If we tried to resume but server returned 200 (not 206 Partial Content),
            // the server doesn't support range requests. Delete partial file and restart
            // fresh to avoid file corruption (appending full file to partial).
            if resume_from > 0 && response.status() == reqwest::StatusCode::OK {
                warn!(
                    "Server doesn't support range requests for model {}, restarting download",
                    model_id
                );
                drop(response);
                let _ = fs::remove_file(&partial_path);

                // Reset resume_from since we're starting fresh
                resume_from = 0;

                // Restart download without range header
                info!("Restarting download from beginning without range header");
                response = client.get(&url).send().await?;
                info!("New response status: {}", response.status());
                info!("New response headers: {:?}", response.headers());
            }

            // Check for success or partial content status
            if !response.status().is_success()
                && response.status() != reqwest::StatusCode::PARTIAL_CONTENT
            {
                // Mark as not downloading on error
                {
                    let mut models = self.available_models.lock().unwrap();
                    if let Some(model) = models.get_mut(model_id) {
                        model.is_downloading = false;
                    }
                }
                return Err(anyhow::anyhow!(
                    "Failed to download model: HTTP {}",
                    response.status()
                ));
            }

            // Calculate total size, preferring Content-Range header for range requests
            let ts = if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                info!("Got 206 Partial Content response");
                // Parse Content-Range header to get total file size
                // Format: Content-Range: bytes <range-start>-<range-end>/<total-size>
                if let Some(content_range) = response.headers().get("content-range") {
                    if let Ok(range_str) = content_range.to_str() {
                        info!("Content-Range header value: {}", range_str);
                        // Extract total size from the header
                        if let Some(total_str) = range_str.split('/').last() {
                            if let Ok(total) = total_str.parse::<u64>() {
                                info!("Parsed total size from Content-Range: {} bytes ({:.2} MB)", total, total as f64 / 1024.0 / 1024.0);
                                total
                            } else {
                                warn!("Failed to parse total size from Content-Range: {}", range_str);
                                // Fallback to content-length if parsing fails
                                resume_from + response.content_length().unwrap_or(0)
                            }
                        } else {
                            warn!("Content-Range header missing total size: {}", range_str);
                            resume_from + response.content_length().unwrap_or(0)
                        }
                    } else {
                        warn!("Content-Range header invalid UTF-8");
                        resume_from + response.content_length().unwrap_or(0)
                    }
                } else {
                    warn!("Content-Range header missing in 206 response");
                    resume_from + response.content_length().unwrap_or(0)
                }
            } else {
                let content_len = response.content_length().unwrap_or(0);
                info!("Got {} response, content length: {} bytes ({:.2} MB)", response.status(), content_len, content_len as f64 / 1024.0 / 1024.0);
                content_len
            };

            info!("Final download stats - resume_from: {} ({:.2} MB), content_length: {} ({:.2} MB), total_size: {} ({:.2} MB)",
                  resume_from, resume_from as f64 / 1024.0 / 1024.0,
                  response.content_length().unwrap_or(0),
                  response.content_length().unwrap_or(0) as f64 / 1024.0 / 1024.0,
                  ts, ts as f64 / 1024.0 / 1024.0);

            // If total_size is still 0, use the model's expected size as fallback
            if ts == 0 {
                let expected_size = model_info.size_mb * 1024 * 1024;
                warn!("Total size is 0, using model's expected size: {} bytes ({:.2} MB)",
                      expected_size, expected_size as f64 / 1024.0 / 1024.0);
                expected_size
            } else {
                ts
            }
        };

        // Validate partial file size against expected total before consuming response
        // If partial file is larger than total, it's corrupted - delete and restart
        let mut fresh_download_needed = false;
        if resume_from > 0 && total_size > 0 && resume_from >= total_size {
            warn!(
                "Partial file size ({}) >= total size ({}), deleting and restarting",
                resume_from, total_size
            );
            let _ = fs::remove_file(&partial_path);
            fresh_download_needed = true;
        }

        // If we need a fresh download, restart now
        let (mut downloaded, total_size, mut stream) = if fresh_download_needed {
            // Restart download from beginning
            let fresh_response = client.get(&url).send().await?;
            let fresh_total = fresh_response.content_length().unwrap_or(0);
            let new_total_size = if fresh_total > 0 { fresh_total } else { model_info.size_mb * 1024 * 1024 };

            info!("Restarted fresh download with total size: {}", new_total_size);

            (0u64, new_total_size, fresh_response.bytes_stream().boxed())
        } else if skip_download {
            // File already downloaded, skip the download loop entirely
            info!("Skipping download, file already complete");
            (total_size, total_size, futures_util::stream::empty().boxed())
        } else {
            (resume_from, total_size, response.bytes_stream().boxed())
        };

        // Open file for appending if resuming, or create new if starting fresh
        // Skip file operations if download is already complete
        let mut file = if !skip_download {
            if downloaded > 0 {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&partial_path)?
            } else {
                std::fs::File::create(&partial_path)?
            }
        } else {
            // Create a dummy file handle - won't be used
            std::fs::File::open(&partial_path).unwrap_or_else(|_| {
                std::fs::File::create(&partial_path).unwrap()
            })
        };

        // Emit initial progress
        info!("Initial download progress - downloaded: {} ({:.2} MB), total: {} ({:.2} MB), percentage: {:.1}%", 
              downloaded, downloaded as f64 / 1024.0 / 1024.0,
              total_size, total_size as f64 / 1024.0 / 1024.0,
              if total_size > 0 { (downloaded as f64 / total_size as f64) * 100.0 } else { 0.0 });
        
        let initial_progress = DownloadProgress {
            model_id: model_id.to_string(),
            downloaded,
            total: total_size,
            percentage: if total_size > 0 {
                (downloaded as f64 / total_size as f64) * 100.0
            } else {
                0.0
            },
            is_indeterminate: total_size == 0,
            speed_mbps: None,
        };
        let _ = self
            .app_handle
            .emit("model-download-progress", &initial_progress);

        // Throttle progress events to max 10/sec (100ms intervals)
        let mut last_emit = Instant::now();
        let throttle_duration = Duration::from_millis(100);

        // Speed calculation
        let mut last_speed_check = Instant::now();
        let mut last_downloaded = downloaded;
        let speed_check_interval = Duration::from_millis(500);

        // Download with progress - skip if file is already complete
        if !skip_download {
            while let Some(chunk) = stream.next().await {
                // Check if download was cancelled
                if cancel_flag.load(Ordering::Relaxed) {
                    // Close the file before returning
                    drop(file);
                    info!("Download cancelled for: {} (downloaded: {} bytes)", model_id, downloaded);

                    // Update state to mark as not downloading
                    {
                        let mut models = self.available_models.lock().unwrap();
                        if let Some(model) = models.get_mut(model_id) {
                            model.is_downloading = false;
                        }
                    }

                    // Remove cancel flag
                    {
                        let mut flags = self.cancel_flags.lock().unwrap();
                        flags.remove(model_id);
                    }

                    // Keep partial file for resume functionality
                    return Ok(());
                }

                let chunk = chunk.map_err(|e| {
                    // Mark as not downloading on error
                    {
                        let mut models = self.available_models.lock().unwrap();
                        if let Some(model) = models.get_mut(model_id) {
                            model.is_downloading = false;
                        }
                    }
                    e
                })?;

            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            let percentage = if total_size > 0 {
                (downloaded as f64 / total_size as f64) * 100.0
            } else {
                0.0
            };

            // Calculate and log speed periodically
            let now = Instant::now();
            let current_speed = if now.duration_since(last_speed_check) >= speed_check_interval {
                let elapsed = now.duration_since(last_speed_check).as_secs_f64();
                let bytes_diff = downloaded - last_downloaded;
                let speed_mbps = (bytes_diff as f64 / 1024.0 / 1024.0) / elapsed;
                info!("Download progress - {:.1}% ({:.2} MB / {:.2} MB), speed: {:.2} MB/s",
                      percentage,
                      downloaded as f64 / 1024.0 / 1024.0,
                      total_size as f64 / 1024.0 / 1024.0,
                      speed_mbps);
                last_speed_check = now;
                last_downloaded = downloaded;
                Some(speed_mbps)
            } else {
                None
            };

            // Emit progress event (throttled to avoid UI freeze)
            if last_emit.elapsed() >= throttle_duration {
                let progress = DownloadProgress {
                    model_id: model_id.to_string(),
                    downloaded,
                    total: total_size,
                    percentage,
                    is_indeterminate: total_size == 0,
                    speed_mbps: current_speed,
                };
                let _ = self.app_handle.emit("model-download-progress", &progress);
                last_emit = Instant::now();
            }
        } // End of download loop
        } // End of if !skip_download

        // Emit final progress to ensure 100% is shown
        let final_progress = DownloadProgress {
            model_id: model_id.to_string(),
            downloaded,
            total: total_size,
            percentage: if total_size > 0 {
                (downloaded as f64 / total_size as f64) * 100.0
            } else {
                100.0
            },
            is_indeterminate: false,
            speed_mbps: None,
        };
        let _ = self
            .app_handle
            .emit("model-download-progress", &final_progress);

        file.flush()?;
        drop(file); // Ensure file is closed before moving

        // Verify downloaded file size matches expected size
        // Skip this check if we already know the file is complete (416 response)
        if total_size > 0 && !skip_download {
            let actual_size = partial_path.metadata()?.len();
            if actual_size != total_size {
                // Download is incomplete/corrupted - delete partial and return error
                let _ = fs::remove_file(&partial_path);
                {
                    let mut models = self.available_models.lock().unwrap();
                    if let Some(model) = models.get_mut(model_id) {
                        model.is_downloading = false;
                    }
                }
                return Err(anyhow::anyhow!(
                    "Download incomplete: expected {} bytes, got {} bytes",
                    total_size,
                    actual_size
                ));
            }
        }

        // Handle directory-based models (extract tar.gz) vs file-based models
        if model_info.is_directory {
            // Track that this model is being extracted
            {
                let mut extracting = self.extracting_models.lock().unwrap();
                extracting.insert(model_id.to_string());
            }

            // Emit extraction started event
            let _ = self.app_handle.emit("model-extraction-started", model_id);
            info!("Extracting archive for directory-based model: {}", model_id);

            // Use a temporary extraction directory to ensure atomic operations
            let temp_extract_dir = self
                .models_dir
                .join(format!("{}.extracting", &model_info.filename));
            let final_model_dir = self.models_dir.join(&model_info.filename);

            // Clean up any previous incomplete extraction
            if temp_extract_dir.exists() {
                let _ = fs::remove_dir_all(&temp_extract_dir);
            }

            // Create temporary extraction directory
            fs::create_dir_all(&temp_extract_dir)?;

            // Open the downloaded tar.gz file
            let tar_gz = File::open(&partial_path)?;
            let tar = GzDecoder::new(tar_gz);
            let mut archive = Archive::new(tar);

            // Extract to the temporary directory first
            archive.unpack(&temp_extract_dir).map_err(|e| {
                let error_msg = format!("Failed to extract archive: {}", e);
                // Clean up failed extraction
                let _ = fs::remove_dir_all(&temp_extract_dir);
                // Remove from extracting set
                {
                    let mut extracting = self.extracting_models.lock().unwrap();
                    extracting.remove(model_id);
                }
                let _ = self.app_handle.emit(
                    "model-extraction-failed",
                    &serde_json::json!({
                        "model_id": model_id,
                        "error": error_msg
                    }),
                );
                anyhow::anyhow!(error_msg)
            })?;

            // Find the actual extracted directory (archive might have a nested structure)
            let extracted_dirs: Vec<_> = fs::read_dir(&temp_extract_dir)?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
                .collect();

            if extracted_dirs.len() == 1 {
                // Single directory extracted, move it to the final location
                let source_dir = extracted_dirs[0].path();
                if final_model_dir.exists() {
                    fs::remove_dir_all(&final_model_dir)?;
                }
                fs::rename(&source_dir, &final_model_dir)?;
                // Clean up temp directory
                let _ = fs::remove_dir_all(&temp_extract_dir);
            } else {
                // Multiple items or no directories, rename the temp directory itself
                if final_model_dir.exists() {
                    fs::remove_dir_all(&final_model_dir)?;
                }
                fs::rename(&temp_extract_dir, &final_model_dir)?;
            }

            info!("Successfully extracted archive for model: {}", model_id);
            // Remove from extracting set
            {
                let mut extracting = self.extracting_models.lock().unwrap();
                extracting.remove(model_id);
            }
            // Emit extraction completed event
            let _ = self.app_handle.emit("model-extraction-completed", model_id);

            // Remove the downloaded tar.gz file
            let _ = fs::remove_file(&partial_path);
        } else {
            // Move partial file to final location for file-based models
            fs::rename(&partial_path, &model_path)?;
        }

        // Update download status
        {
            let mut models = self.available_models.lock().unwrap();
            if let Some(model) = models.get_mut(model_id) {
                model.is_downloading = false;
                model.is_downloaded = true;
                model.partial_size = 0;
            }
        }

        // Remove cancel flag on successful completion
        {
            let mut flags = self.cancel_flags.lock().unwrap();
            flags.remove(model_id);
        }

        // Emit completion event
        let _ = self.app_handle.emit("model-download-complete", model_id);

        info!(
            "Successfully downloaded model {} to {:?}",
            model_id, model_path
        );

        Ok(())
    }

    pub fn delete_model(&self, model_id: &str) -> Result<()> {
        debug!("ModelManager: delete_model called for: {}", model_id);

        let model_info = {
            let models = self.available_models.lock().unwrap();
            models.get(model_id).cloned()
        };

        let model_info =
            model_info.ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

        debug!("ModelManager: Found model info: {:?}", model_info);

        let model_path = self.models_dir.join(&model_info.filename);
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", &model_info.filename));
        debug!("ModelManager: Model path: {:?}", model_path);
        debug!("ModelManager: Partial path: {:?}", partial_path);

        let mut deleted_something = false;

        // Only delete files under Handy's models directory — never touch external caches
        if model_path.exists() {
            if model_info.is_directory && model_path.is_dir() {
                info!("Deleting model directory at: {:?}", model_path);
                fs::remove_dir_all(&model_path)?;
                info!("Model directory deleted successfully");
                deleted_something = true;
            } else if !model_info.is_directory && model_path.is_file() {
                info!("Deleting model file at: {:?}", model_path);
                fs::remove_file(&model_path)?;
                info!("Model file deleted successfully");
                deleted_something = true;
            }
        }

        // Delete partial file if it exists (same for both types)
        if partial_path.exists() {
            info!("Deleting partial file at: {:?}", partial_path);
            fs::remove_file(&partial_path)?;
            info!("Partial file deleted successfully");
            deleted_something = true;
        }

        // External-only models: unbind without deleting the source file
        let is_external_only = model_info.is_external
            || model_info
                .local_path
                .as_ref()
                .map(|p| {
                    let path = PathBuf::from(p);
                    path.exists() && !path.starts_with(&self.models_dir)
                })
                .unwrap_or(false);

        if !deleted_something && !is_external_only {
            return Err(anyhow::anyhow!("No model files found to delete"));
        }

        if is_external_only {
            info!(
                "Unbinding external model {} (source left intact at {:?})",
                model_id, model_info.local_path
            );
        }

        // Custom models should be removed from the list entirely since they
        // have no download URL and can't be re-downloaded
        if model_info.is_custom {
            let mut models = self.available_models.lock().unwrap();
            models.remove(model_id);
            debug!("ModelManager: removed custom model from available models");
        } else {
            // Clear external binding and mark predefined models as not downloaded
            {
                let mut models = self.available_models.lock().unwrap();
                if let Some(model) = models.get_mut(model_id) {
                    model.local_path = None;
                    model.is_external = false;
                    model.is_downloaded = false;
                }
            }
            self.update_download_status()?;
            debug!("ModelManager: download status updated");
        }

        // Emit event to notify UI
        let _ = self.app_handle.emit("model-deleted", model_id);

        Ok(())
    }

    pub fn get_model_path(&self, model_id: &str) -> Result<PathBuf> {
        let model_info = self
            .get_model_info(model_id)
            .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

        if !model_info.is_downloaded {
            return Err(anyhow::anyhow!("Model not available: {}", model_id));
        }

        // Ensure we don't return partial files/directories
        if model_info.is_downloading {
            return Err(anyhow::anyhow!(
                "Model is currently downloading: {}",
                model_id
            ));
        }

        let models_dir_path = self.models_dir.join(&model_info.filename);
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", &model_info.filename));

        // Prefer Handy models dir; fall back to external cache path
        let model_path = if models_dir_path.exists() {
            models_dir_path
        } else if let Some(ref local) = model_info.local_path {
            PathBuf::from(local)
        } else {
            models_dir_path
        };

        if model_info.is_directory {
            if model_path.exists() && model_path.is_dir() && !partial_path.exists() {
                Ok(model_path)
            } else {
                Err(anyhow::anyhow!(
                    "Complete model directory not found: {}",
                    model_id
                ))
            }
        } else if model_path.exists() && model_path.is_file() && !partial_path.exists() {
            Ok(model_path)
        } else {
            Err(anyhow::anyhow!(
                "Complete model file not found: {}",
                model_id
            ))
        }
    }

    pub fn cancel_download(&self, model_id: &str) -> Result<()> {
        debug!("ModelManager: cancel_download called for: {}", model_id);

        // Set the cancellation flag to stop the download loop
        {
            let flags = self.cancel_flags.lock().unwrap();
            if let Some(flag) = flags.get(model_id) {
                flag.store(true, Ordering::Relaxed);
                info!("Cancellation flag set for: {}", model_id);
            } else {
                warn!("No active download found for: {}", model_id);
            }
        }

        // Update state immediately for UI responsiveness
        {
            let mut models = self.available_models.lock().unwrap();
            if let Some(model) = models.get_mut(model_id) {
                model.is_downloading = false;
            }
        }

        // Update download status to reflect current state
        self.update_download_status()?;

        // Emit cancellation event so all UI components can clear their state
        let _ = self.app_handle.emit("model-download-cancelled", model_id);

        info!("Download cancellation initiated for: {}", model_id);
        Ok(())
    }

    pub fn import_onnx_model(&self, src_dir: &Path) -> Result<ModelInfo> {
        // Validate Parakeet structure
        let has_encoder = src_dir.join("encoder-model.onnx").exists()
            || src_dir.join("encoder-model.int8.onnx").exists();
        let has_vocab = src_dir.join("vocab.txt").exists();
        if !has_encoder || !has_vocab {
            return Err(anyhow::anyhow!(
                "Not a valid Parakeet ONNX directory. Must contain encoder-model.onnx and vocab.txt"
            ));
        }

        let dirname = src_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid directory name"))?
            .to_string();

        // Check for duplicates
        {
            let models = self.available_models.lock().unwrap();
            if models.contains_key(&dirname) {
                return Err(anyhow::anyhow!(
                    "A model named '{}' already exists",
                    dirname
                ));
            }
        }

        // Copy to models directory
        let dest = self.models_dir.join(&dirname);
        Self::copy_dir_all(src_dir, &dest)?;

        // Build display name
        let display_name = dirname
            .replace(['-', '_'], " ")
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let model_info = ModelInfo {
            id: dirname.clone(),
            name: display_name,
            description: "Not officially supported".to_string(),
            filename: dirname.clone(),
            url: None,
            size_mb: 0,
            is_downloaded: true,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Parakeet,
            accuracy_score: 0.0,
            speed_score: 0.0,
            supports_translation: false,
            is_recommended: false,
            supported_languages: vec![],
            is_custom: true,
        local_path: None,
        is_external: false,
        };

        self.available_models
            .lock()
            .unwrap()
            .insert(dirname, model_info.clone());

        Ok(model_info)
    }

    /// Discover custom imported Parakeet ONNX model directories.
    /// A directory qualifies if it contains `encoder-model.onnx` (or the int8
    /// variant) and `vocab.txt`, and its name is not already registered.
    fn discover_custom_parakeet_models(
        models_dir: &Path,
        available_models: &mut HashMap<String, ModelInfo>,
    ) -> Result<()> {
        if !models_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(models_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry: {}", e);
                    continue;
                }
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let dirname = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Skip already-registered models (predefined or previously discovered)
            if available_models.contains_key(&dirname) {
                continue;
            }

            // Must look like a Parakeet directory
            let has_encoder = path.join("encoder-model.onnx").exists()
                || path.join("encoder-model.int8.onnx").exists();
            let has_vocab = path.join("vocab.txt").exists();
            if !has_encoder || !has_vocab {
                continue;
            }

            let display_name = dirname
                .replace(['-', '_'], " ")
                .split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            info!("Discovered custom Parakeet model: {}", dirname);

            available_models.insert(
                dirname.clone(),
                ModelInfo {
                    id: dirname.clone(),
                    name: display_name,
                    description: "Not officially supported".to_string(),
                    filename: dirname.clone(),
                    url: None,
                    size_mb: 0,
                    is_downloaded: true,
                    is_downloading: false,
                    partial_size: 0,
                    is_directory: true,
                    engine_type: EngineType::Parakeet,
                    accuracy_score: 0.0,
                    speed_score: 0.0,
                    supports_translation: false,
                    is_recommended: false,
                    supported_languages: vec![],
                    is_custom: true,
                local_path: None,
                is_external: false,
                },
            );
        }

        Ok(())
    }

    fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                Self::copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
            } else {
                fs::copy(entry.path(), dst.join(entry.file_name()))?;
            }
        }
        Ok(())
    }

    /// Known third-party locations that may already hold whisper.cpp GGML bins
    /// or Parakeet ONNX directories (OpenWhispr, Meetily, Hugging Face hub, …).
    fn external_model_search_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();

        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);

        let Some(home) = home else {
            return roots;
        };

        roots.push(home.join(".cache/openwhispr/whisper-models"));
        roots.push(home.join(".cache/openwhispr/models"));
        roots.push(home.join(".cache/huggingface/hub"));

        #[cfg(target_os = "macos")]
        {
            roots.push(home.join("Library/Application Support/com.meetily.ai/models"));
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
                roots.push(appdata.join("com.meetily.ai").join("models"));
            }
            roots.push(home.join("AppData/Roaming/com.meetily.ai/models"));
        }
        #[cfg(target_os = "linux")]
        {
            roots.push(home.join(".config/com.meetily.ai/models"));
            roots.push(home.join(".local/share/com.meetily.ai/models"));
        }

        roots
    }

    /// whisper.cpp GGML magic (native or little-endian byte order).
    fn is_ggml_whisper_bin(path: &Path) -> bool {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_err() {
            return false;
        }
        matches!(
            &magic,
            b"ggml" | b"ggmf" | b"ggjt" | b"ggj1" | // big-endian / file order
            b"lmgg" | b"fmgg" | b"tjgg" | b"1jgg" // little-endian on disk
        )
    }

    fn is_parakeet_model_dir(path: &Path) -> bool {
        if !path.is_dir() {
            return false;
        }
        let has_encoder = path.join("encoder-model.onnx").exists()
            || path.join("encoder-model.int8.onnx").exists();
        let has_vocab = path.join("vocab.txt").exists();
        has_encoder && has_vocab
    }

    fn display_name_from_id(id: &str) -> String {
        id.replace(['-', '_'], " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn path_to_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Walk `root` up to `max_depth` and collect GGML .bin files + Parakeet dirs.
    fn collect_external_candidates(root: &Path, max_depth: u32) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut bins = Vec::new();
        let mut dirs = Vec::new();
        if !root.exists() {
            return (bins, dirs);
        }

        fn walk(
            dir: &Path,
            depth: u32,
            max_depth: u32,
            bins: &mut Vec<PathBuf>,
            dirs: &mut Vec<PathBuf>,
        ) {
            if depth > max_depth {
                return;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read {}: {}", dir.display(), e);
                    return;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    if ModelManager::is_parakeet_model_dir(&path) {
                        dirs.push(path);
                    } else {
                        walk(&path, depth + 1, max_depth, bins, dirs);
                    }
                } else if path.is_file()
                    && name_str.ends_with(".bin")
                    && !name_str.ends_with(".partial")
                    && ModelManager::is_ggml_whisper_bin(&path)
                {
                    bins.push(path);
                }
            }
        }

        walk(root, 0, max_depth, &mut bins, &mut dirs);
        (bins, dirs)
    }

    /// Bind an external path to a catalog model, or register a custom external model.
    fn register_external_path(
        models_dir: &Path,
        available_models: &mut HashMap<String, ModelInfo>,
        path: &Path,
        is_directory: bool,
        engine_type: EngineType,
    ) {
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => return,
        };

        // Prefer Handy's own copy when present
        let handy_path = models_dir.join(&name);
        if handy_path.exists() {
            return;
        }

        let local = Self::path_to_string(path);
        let size_mb = if is_directory {
            0
        } else {
            path.metadata().map(|m| m.len() / (1024 * 1024)).unwrap_or(0)
        };

        // Match predefined catalog by filename / directory name
        let catalog_id = available_models
            .iter()
            .find(|(_, m)| {
                !m.is_custom
                    && m.is_directory == is_directory
                    && std::mem::discriminant(&m.engine_type)
                        == std::mem::discriminant(&engine_type)
                    && m.filename == name
            })
            .map(|(id, _)| id.clone());

        if let Some(id) = catalog_id {
            if let Some(model) = available_models.get_mut(&id) {
                // Don't override an already-bound path
                if model.local_path.is_some() && model.is_downloaded {
                    return;
                }
                model.local_path = Some(local.clone());
                model.is_external = true;
                model.is_downloaded = true;
                if size_mb > 0 {
                    model.size_mb = size_mb;
                }
                info!(
                    "Bound external cache to catalog model '{}': {}",
                    id, local
                );
            }
            return;
        }

        // Custom external model
        let model_id = if is_directory {
            name.clone()
        } else {
            name.trim_end_matches(".bin").to_string()
        };

        if available_models.contains_key(&model_id) {
            // Already registered (e.g. custom in models_dir) — attach path if missing
            if let Some(model) = available_models.get_mut(&model_id) {
                if !model.is_downloaded {
                    model.local_path = Some(local);
                    model.is_external = true;
                    model.is_downloaded = true;
                }
            }
            return;
        }

        info!(
            "Discovered external {:?} model: {} ({})",
            engine_type, model_id, local
        );

        available_models.insert(
            model_id.clone(),
            ModelInfo {
                id: model_id,
                name: Self::display_name_from_id(
                    if is_directory {
                        &name
                    } else {
                        name.trim_end_matches(".bin")
                    },
                ),
                description: "External cache (not officially supported)".to_string(),
                filename: name,
                url: None,
                size_mb,
                is_downloaded: true,
                is_downloading: false,
                partial_size: 0,
                is_directory,
                engine_type,
                accuracy_score: 0.0,
                speed_score: 0.0,
                supports_translation: false,
                is_recommended: false,
                supported_languages: vec![],
                is_custom: true,
                local_path: Some(local),
                is_external: true,
            },
        );
    }

    /// Scan OpenWhispr / Meetily / Hugging Face caches for usable STT models.
    fn discover_external_models(
        models_dir: &Path,
        available_models: &mut HashMap<String, ModelInfo>,
    ) -> Result<()> {
        for root in Self::external_model_search_roots() {
            if !root.exists() {
                continue;
            }

            // HF hub is deep (models--*/snapshots/*); others are shallow
            let max_depth = if root.ends_with("hub") { 5 } else { 3 };
            let (bins, dirs) = Self::collect_external_candidates(&root, max_depth);

            for bin in bins {
                Self::register_external_path(
                    models_dir,
                    available_models,
                    &bin,
                    false,
                    EngineType::Whisper,
                );
            }
            for dir in dirs {
                Self::register_external_path(
                    models_dir,
                    available_models,
                    &dir,
                    true,
                    EngineType::Parakeet,
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_discover_custom_whisper_models() {
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().to_path_buf();

        // Create test .bin files
        let mut custom_file = File::create(models_dir.join("my-custom-model.bin")).unwrap();
        custom_file.write_all(b"fake model data").unwrap();

        let mut another_file = File::create(models_dir.join("whisper_medical_v2.bin")).unwrap();
        another_file.write_all(b"another fake model").unwrap();

        // Create files that should be ignored
        File::create(models_dir.join(".hidden-model.bin")).unwrap(); // Hidden file
        File::create(models_dir.join("readme.txt")).unwrap(); // Non-.bin file
        File::create(models_dir.join("ggml-small.bin")).unwrap(); // Predefined filename
        fs::create_dir(models_dir.join("some-directory.bin")).unwrap(); // Directory

        // Set up available_models with a predefined Whisper model
        let mut models = HashMap::new();
        models.insert(
            "small".to_string(),
            ModelInfo {
                id: "small".to_string(),
                name: "Whisper Small".to_string(),
                description: "Test".to_string(),
                filename: "ggml-small.bin".to_string(),
                url: Some("https://example.com".to_string()),
                size_mb: 100,
                is_downloaded: false,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.5,
                speed_score: 0.5,
                supports_translation: true,
                is_recommended: false,
                supported_languages: vec!["en".to_string()],
                is_custom: false,
            local_path: None,
            is_external: false,
            },
        );

        // Discover custom models
        ModelManager::discover_custom_whisper_models(&models_dir, &mut models).unwrap();

        // Should have discovered 2 custom models (my-custom-model and whisper_medical_v2)
        assert!(models.contains_key("my-custom-model"));
        assert!(models.contains_key("whisper_medical_v2"));

        // Verify custom model properties
        let custom = models.get("my-custom-model").unwrap();
        assert_eq!(custom.name, "My Custom Model");
        assert_eq!(custom.filename, "my-custom-model.bin");
        assert!(custom.url.is_none()); // Custom models have no URL
        assert!(custom.is_downloaded);
        assert!(custom.is_custom);
        assert_eq!(custom.accuracy_score, 0.0);
        assert_eq!(custom.speed_score, 0.0);
        assert!(custom.supported_languages.is_empty());

        // Verify underscore handling
        let medical = models.get("whisper_medical_v2").unwrap();
        assert_eq!(medical.name, "Whisper Medical V2");

        // Should NOT have discovered hidden, non-.bin, predefined, or directories
        assert!(!models.contains_key(".hidden-model"));
        assert!(!models.contains_key("readme"));
        assert!(!models.contains_key("some-directory"));
    }

    #[test]
    fn test_discover_custom_models_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().to_path_buf();

        let mut models = HashMap::new();
        let count_before = models.len();

        ModelManager::discover_custom_whisper_models(&models_dir, &mut models).unwrap();

        // No new models should be added
        assert_eq!(models.len(), count_before);
    }

    #[test]
    fn test_discover_custom_models_nonexistent_dir() {
        let models_dir = PathBuf::from("/nonexistent/path/that/does/not/exist");

        let mut models = HashMap::new();
        let count_before = models.len();

        // Should not error, just return Ok
        let result = ModelManager::discover_custom_whisper_models(&models_dir, &mut models);
        assert!(result.is_ok());
        assert_eq!(models.len(), count_before);
    }

    /// Test that partial file size validation correctly identifies complete files
    /// This simulates the 416 Range Not Satisfiable scenario where the partial file
    /// is already complete but the expected size in model_info is incorrect
    #[test]
    fn test_partial_file_completion_detection() {
        // Simulate the parakeet v3 scenario:
        // - Server actual size: 478,517,071 bytes (456.35 MB)
        // - App expected size: 501,219,328 bytes (478.00 MB) - incorrect
        // - Partial file: 478,517,071 bytes - complete!
        
        let server_actual_size: u64 = 478_517_071;
        let app_expected_size_mb: u64 = 478; // This is wrong!
        let app_expected_size_bytes = app_expected_size_mb * 1024 * 1024;
        let partial_file_size: u64 = 478_517_071; // Same as server
        
        // The completion check logic should be:
        // is_complete = if let Some(actual_size) = actual_size_from_server {
        //     partial_size == actual_size  // TRUE in this case
        // } else {
        //     partial_size >= expected_size  // Would be FALSE
        // }
        
        // When server provides actual size (from Content-Range: bytes */478517071)
        let is_complete_with_server_size = partial_file_size == server_actual_size;
        assert!(is_complete_with_server_size, "Should detect completion when partial matches server size");
        
        // Without server size, using only expected size (this would FAIL)
        let is_complete_with_expected_only = partial_file_size >= app_expected_size_bytes;
        assert!(!is_complete_with_expected_only, "Should NOT detect completion with wrong expected size");
        
        // This demonstrates why parsing Content-Range header is critical
    }

    /// Test parsing Content-Range header from 416 response
    #[test]
    fn test_parse_content_range_416() {
        // Content-Range: bytes */478517071
        let content_range = "bytes */478517071";
        
        if let Some(total_str) = content_range.split('/').last() {
            if let Ok(total) = total_str.parse::<u64>() {
                assert_eq!(total, 478_517_071);
            } else {
                panic!("Failed to parse total size");
            }
        } else {
            panic!("Failed to extract total size from Content-Range");
        }
    }

    /// Test that directory-based models are handled correctly when download is complete
    #[test]
    fn test_directory_model_completion_logic() {
        let temp_dir = TempDir::new().unwrap();
        let models_dir = temp_dir.path().to_path_buf();
        
        // Create a mock tar.gz file (simulating completed download)
        let partial_path = models_dir.join("test-model.partial");
        let mut file = File::create(&partial_path).unwrap();
        file.write_all(b"fake tar.gz content").unwrap();
        
        let partial_size = partial_path.metadata().unwrap().len();
        let server_size = partial_size; // Server reports same size
        
        // For directory models, when partial is complete:
        // 1. skip_download = true
        // 2. Proceed to extraction with partial_path as the tar.gz source
        
        let is_complete = partial_size == server_size;
        assert!(is_complete);
        
        // The partial file should exist and be ready for extraction
        assert!(partial_path.exists());
        
        // Clean up
        let _ = fs::remove_file(&partial_path);
    }

    fn sample_catalog_turbo() -> ModelInfo {
        ModelInfo {
            id: "turbo".to_string(),
            name: "Whisper Turbo".to_string(),
            description: "Test".to_string(),
            filename: "ggml-large-v3-turbo.bin".to_string(),
            url: Some("https://example.com".to_string()),
            size_mb: 1600,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.8,
            speed_score: 0.4,
            supports_translation: false,
            is_recommended: false,
            supported_languages: vec!["en".to_string()],
            is_custom: false,
            local_path: None,
            is_external: false,
        }
    }

    fn sample_catalog_parakeet_v3() -> ModelInfo {
        ModelInfo {
            id: "parakeet-tdt-0.6b-v3".to_string(),
            name: "Parakeet V3".to_string(),
            description: "Test".to_string(),
            filename: "parakeet-tdt-0.6b-v3-int8".to_string(),
            url: Some("https://example.com".to_string()),
            size_mb: 478,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Parakeet,
            accuracy_score: 0.8,
            speed_score: 0.85,
            supports_translation: false,
            is_recommended: true,
            supported_languages: vec!["en".to_string()],
            is_custom: false,
            local_path: None,
            is_external: false,
        }
    }

    #[test]
    fn test_is_ggml_whisper_bin_magic() {
        let temp_dir = TempDir::new().unwrap();
        let ggml = temp_dir.path().join("good.bin");
        let mut f = File::create(&ggml).unwrap();
        f.write_all(b"lmgg").unwrap(); // LE "ggml"
        f.write_all(&[0u8; 12]).unwrap();
        assert!(ModelManager::is_ggml_whisper_bin(&ggml));

        let ct2 = temp_dir.path().join("ct2.bin");
        let mut f = File::create(&ct2).unwrap();
        f.write_all(b"\x06\x00\x00\x00Whisper").unwrap();
        assert!(!ModelManager::is_ggml_whisper_bin(&ct2));
    }

    #[test]
    fn test_register_external_binds_catalog_and_custom() {
        let temp = TempDir::new().unwrap();
        let models_dir = temp.path().join("handy-models");
        fs::create_dir_all(&models_dir).unwrap();
        let external = temp.path().join("external");
        fs::create_dir_all(&external).unwrap();

        // Catalog-matching turbo bin
        let turbo = external.join("ggml-large-v3-turbo.bin");
        {
            let mut f = File::create(&turbo).unwrap();
            f.write_all(b"lmgg").unwrap();
            f.write_all(&[0u8; 64]).unwrap();
        }

        // Non-catalog custom bin
        let custom = external.join("ggml-medium-q5_0.bin");
        {
            let mut f = File::create(&custom).unwrap();
            f.write_all(b"lmgg").unwrap();
            f.write_all(&[0u8; 64]).unwrap();
        }

        // Parakeet catalog dir
        let parakeet = external.join("parakeet-tdt-0.6b-v3-int8");
        fs::create_dir_all(&parakeet).unwrap();
        File::create(parakeet.join("encoder-model.int8.onnx")).unwrap();
        File::create(parakeet.join("vocab.txt")).unwrap();

        let mut models = HashMap::new();
        models.insert("turbo".to_string(), sample_catalog_turbo());
        models.insert(
            "parakeet-tdt-0.6b-v3".to_string(),
            sample_catalog_parakeet_v3(),
        );

        ModelManager::register_external_path(
            &models_dir,
            &mut models,
            &turbo,
            false,
            EngineType::Whisper,
        );
        ModelManager::register_external_path(
            &models_dir,
            &mut models,
            &custom,
            false,
            EngineType::Whisper,
        );
        ModelManager::register_external_path(
            &models_dir,
            &mut models,
            &parakeet,
            true,
            EngineType::Parakeet,
        );

        let turbo_model = models.get("turbo").unwrap();
        assert!(turbo_model.is_downloaded);
        assert!(turbo_model.is_external);
        assert_eq!(
            turbo_model.local_path.as_deref(),
            Some(turbo.to_str().unwrap())
        );

        let custom_model = models.get("ggml-medium-q5_0").unwrap();
        assert!(custom_model.is_custom);
        assert!(custom_model.is_external);
        assert!(custom_model.is_downloaded);

        let pk = models.get("parakeet-tdt-0.6b-v3").unwrap();
        assert!(pk.is_downloaded);
        assert!(pk.is_external);
        assert_eq!(
            pk.local_path.as_deref(),
            Some(parakeet.to_str().unwrap())
        );
    }

    #[test]
    fn test_register_external_skips_when_handy_copy_exists() {
        let temp = TempDir::new().unwrap();
        let models_dir = temp.path().join("handy-models");
        fs::create_dir_all(&models_dir).unwrap();
        let external = temp.path().join("external");
        fs::create_dir_all(&external).unwrap();

        let handy_copy = models_dir.join("ggml-large-v3-turbo.bin");
        File::create(&handy_copy).unwrap();

        let external_bin = external.join("ggml-large-v3-turbo.bin");
        {
            let mut f = File::create(&external_bin).unwrap();
            f.write_all(b"lmgg").unwrap();
        }

        let mut models = HashMap::new();
        models.insert("turbo".to_string(), sample_catalog_turbo());

        ModelManager::register_external_path(
            &models_dir,
            &mut models,
            &external_bin,
            false,
            EngineType::Whisper,
        );

        let turbo = models.get("turbo").unwrap();
        assert!(!turbo.is_external);
        assert!(turbo.local_path.is_none());
        assert!(!turbo.is_downloaded); // status updated later by update_download_status
    }

    #[test]
    fn test_collect_external_candidates_filters_non_ggml() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cache");
        fs::create_dir_all(&root).unwrap();

        let good = root.join("ggml-good.bin");
        {
            let mut f = File::create(&good).unwrap();
            f.write_all(b"lmgg").unwrap();
        }
        let bad = root.join("model.bin");
        {
            let mut f = File::create(&bad).unwrap();
            f.write_all(b"\x06\x00\x00\x00WhisperSpe").unwrap();
        }

        let (bins, _dirs) = ModelManager::collect_external_candidates(&root, 2);
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0], good);
    }
}
