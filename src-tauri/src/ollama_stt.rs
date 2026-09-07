use crate::settings::{get_settings, ModelUnloadTimeout};
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tauri::AppHandle;

pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
pub const GEMMA4_E2B_ID: &str = "gemma4-e2b";
pub const GEMMA4_E4B_ID: &str = "gemma4-e4b";
pub const GEMMA4_E2B_TAG: &str = "gemma4:e2b";
pub const GEMMA4_E4B_TAG: &str = "gemma4:e4b";

const TAGS_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: Option<ChatMessage>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullProgress {
    pub status: Option<String>,
    pub digest: Option<String>,
    pub total: Option<u64>,
    pub completed: Option<u64>,
    pub error: Option<String>,
}

pub fn normalize_ollama_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let without_v1 = trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    if without_v1.starts_with("http://") || without_v1.starts_with("https://") {
        without_v1.to_string()
    } else if without_v1.is_empty() {
        DEFAULT_OLLAMA_BASE_URL.to_string()
    } else {
        format!("http://{without_v1}")
    }
}

pub fn resolve_base_url(app: &AppHandle) -> String {
    if let Ok(host) = std::env::var("OLLAMA_HOST") {
        if !host.trim().is_empty() {
            return normalize_ollama_url(&host);
        }
    }
    let settings = get_settings(app);
    if !settings.ollama_base_url.trim().is_empty() {
        return normalize_ollama_url(&settings.ollama_base_url);
    }
    DEFAULT_OLLAMA_BASE_URL.to_string()
}

pub fn tag_matches(installed: &str, wanted: &str) -> bool {
    let installed = installed.to_lowercase();
    let wanted = wanted.to_lowercase();
    if installed == wanted {
        return true;
    }
    installed.starts_with(&format!("{wanted}-"))
}

pub fn tags_include(installed: &[String], wanted: &str) -> bool {
    installed.iter().any(|name| tag_matches(name, wanted))
}

pub fn keep_alive_for(timeout: ModelUnloadTimeout) -> Value {
    match timeout {
        ModelUnloadTimeout::Never => json!(-1),
        ModelUnloadTimeout::Immediately => json!(0),
        ModelUnloadTimeout::Min2 => json!("2m"),
        ModelUnloadTimeout::Min5 => json!("5m"),
        ModelUnloadTimeout::Min10 => json!("10m"),
        ModelUnloadTimeout::Min15 => json!("15m"),
        ModelUnloadTimeout::Hour1 => json!("1h"),
        ModelUnloadTimeout::Sec5 => json!("5s"),
    }
}

pub fn transcription_prompt(language: &str, translate_to_english: bool) -> String {
    let mut prompt = String::from(
        "Transcribe the spoken audio verbatim. Output only the transcript. Do not add quotes, labels, or commentary.",
    );
    if translate_to_english {
        prompt.push_str(" Transcribe into English.");
    } else if language != "auto" && !language.is_empty() {
        prompt.push_str(&format!(
            " The speech is {}.",
            language_display_name(language)
        ));
    }
    prompt
}

fn language_display_name(code: &str) -> &str {
    match code {
        "en" => "English",
        "ja" => "Japanese",
        "zh" | "zh-Hans" | "zh-Hant" => "Chinese",
        "ko" => "Korean",
        "de" => "German",
        "es" => "Spanish",
        "fr" => "French",
        "it" => "Italian",
        "pt" => "Portuguese",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "ar" => "Arabic",
        "hi" => "Hindi",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "tr" => "Turkish",
        "sv" => "Swedish",
        "da" => "Danish",
        "fi" => "Finnish",
        "no" => "Norwegian",
        "cs" => "Czech",
        "hu" => "Hungarian",
        "ro" => "Romanian",
        "el" => "Greek",
        "he" => "Hebrew",
        "id" => "Indonesian",
        "ms" => "Malay",
        "yue" => "Cantonese",
        other => other,
    }
}

fn unreachable_error(err: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("Ollama is not reachable. Start Ollama and pull gemma4:e2b or gemma4:e4b. ({err})")
}

pub async fn list_tags(base_url: &str) -> Result<Vec<String>> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder().timeout(TAGS_TIMEOUT).build()?;
    let response = client.get(&url).send().await.map_err(unreachable_error)?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Ollama tags request failed with HTTP {}",
            response.status()
        ));
    }
    let body: TagsResponse = response.json().await?;
    Ok(body
        .models
        .into_iter()
        .filter_map(|m| m.name.or(m.model))
        .collect())
}

pub async fn start_pull(base_url: &str, tag: &str) -> Result<reqwest::Response> {
    let url = format!("{}/api/pull", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()?;
    let response = client
        .post(&url)
        .json(&json!({ "model": tag, "stream": true }))
        .send()
        .await
        .map_err(unreachable_error)?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("Ollama pull failed with HTTP {status}: {text}"));
    }
    Ok(response)
}

fn blocking_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(INFERENCE_TIMEOUT)
        .build()
        .map_err(|e| anyhow!(e))
}

fn chat(base_url: &str, body: &Value) -> Result<ChatResponse> {
    let url = format!("{}/api/chat", base_url.trim_end_matches('/'));
    let client = blocking_client()?;
    let response = client
        .post(&url)
        .json(body)
        .send()
        .map_err(unreachable_error)?;
    let status = response.status();
    let text = response.text().map_err(|e| anyhow!(e))?;
    if !status.is_success() {
        return Err(anyhow!("Ollama chat failed with HTTP {status}: {text}"));
    }
    serde_json::from_str(&text).map_err(|e| anyhow!("Failed to parse Ollama chat response: {e}"))
}

pub fn preload(base_url: &str, tag: &str, timeout: ModelUnloadTimeout) -> Result<()> {
    info!("Preloading Ollama model {tag}");
    let body = json!({
        "model": tag,
        "messages": [],
        "stream": false,
        "keep_alive": keep_alive_for(timeout),
    });
    let response = chat(base_url, &body)?;
    if let Some(error) = response.error {
        return Err(anyhow!("Failed to load Ollama model {tag}: {error}"));
    }
    Ok(())
}

pub fn unload(base_url: &str, tag: &str) -> Result<()> {
    debug!("Unloading Ollama model {tag}");
    let body = json!({
        "model": tag,
        "messages": [],
        "stream": false,
        "keep_alive": 0,
    });
    if let Err(e) = chat(base_url, &body) {
        warn!("Failed to unload Ollama model {tag}: {e}");
    }
    Ok(())
}

pub fn transcribe(
    base_url: &str,
    tag: &str,
    wav_bytes: &[u8],
    language: &str,
    translate_to_english: bool,
    timeout: ModelUnloadTimeout,
) -> Result<String> {
    let audio_b64 = BASE64.encode(wav_bytes);
    let prompt = transcription_prompt(language, translate_to_english);
    let body = json!({
        "model": tag,
        "messages": [{
            "role": "user",
            "content": prompt,
            "images": [audio_b64],
        }],
        "stream": false,
        "think": false,
        "keep_alive": keep_alive_for(timeout),
        "options": {
            "temperature": 0.0,
        },
    });
    let response = chat(base_url, &body)?;
    if let Some(error) = response.error {
        return Err(anyhow!("Ollama transcription failed: {error}"));
    }
    let text = response
        .message
        .and_then(|m| m.content)
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .to_string();
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_v1_and_adds_scheme() {
        assert_eq!(
            normalize_ollama_url("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        assert_eq!(
            normalize_ollama_url("127.0.0.1:11434"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(normalize_ollama_url(""), DEFAULT_OLLAMA_BASE_URL);
    }

    #[test]
    fn tag_match_allows_quant_suffix() {
        assert!(tag_matches("gemma4:e2b", "gemma4:e2b"));
        assert!(tag_matches("gemma4:e2b-q4_0", "gemma4:e2b"));
        assert!(!tag_matches("gemma4:e4b", "gemma4:e2b"));
        assert!(!tag_matches("gemma4:e2b", "gemma4:e4b"));
    }

    #[test]
    fn prompt_includes_language_and_translation() {
        let ja = transcription_prompt("ja", false);
        assert!(ja.contains("Japanese"));
        let en = transcription_prompt("auto", true);
        assert!(en.contains("English"));
        let auto = transcription_prompt("auto", false);
        assert!(!auto.contains("The speech is"));
    }
}
