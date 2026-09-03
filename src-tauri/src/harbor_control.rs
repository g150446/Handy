use anyhow::{anyhow, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use uuid::Uuid;

use crate::settings;

pub const WINDOW_LABEL: &str = "harbor-control";
const EVENT_NAME: &str = "harbor-control-changed";
const AUTH_VERSION: &str = "hmac-sha256-v1";
const KEYCHAIN_SERVICE: &str = "ai.handy.terminal-harbor";
const VOICE_PATH: &str = "/v1/voice/intent";
const LOCAL_PAIR_PATH: &str = "/v1/pair/local";
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:7780";
const WINDOW_WIDTH: f64 = 300.0;
const WINDOW_HEIGHT: f64 = 320.0;
const WINDOW_RIGHT_OFFSET: f64 = 24.0;
const WINDOW_TOP_OFFSET: f64 = 24.0;

#[derive(Default)]
pub struct HarborControlState {
    inner: Mutex<HarborRuntimeState>,
}

#[derive(Clone, Debug, Default)]
struct HarborRuntimeState {
    active: bool,
    session_id: u64,
    messages: Vec<HarborTurn>,
    is_sending: bool,
    last_error: Option<String>,
    status: String,
    /// Public workspace labels used for STT biasing (directory basenames first).
    stt_labels: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HarborTurn {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct HarborControlSnapshot {
    pub active: bool,
    pub session_id: u64,
    pub messages: Vec<HarborTurn>,
    pub is_sending: bool,
    pub last_error: Option<String>,
    pub paired: bool,
    pub status: String,
    pub directories: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HarborPairStatus {
    pub paired: bool,
    pub server_id: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PairResponse {
    server_id: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    endpoints: Option<Vec<EndpointDto>>,
}

#[derive(Debug, Deserialize)]
struct LocalPairResponse {
    server_id: String,
    client_id: String,
    local_pair_token: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdentityResponse {
    server_id: String,
}

#[derive(Debug, Deserialize)]
struct EndpointDto {
    #[serde(default)]
    #[allow(dead_code)]
    kind: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct VoiceResponse {
    outcome: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct WorkspacesResponse {
    workspaces: Vec<WorkspaceDto>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceDto {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

pub fn initialize(app: &AppHandle) {
    app.manage(HarborControlState::default());
}

pub fn is_active(app: &AppHandle) -> bool {
    app.state::<HarborControlState>()
        .inner
        .lock()
        .unwrap()
        .active
}

pub fn toggle(app: &AppHandle) -> Result<HarborControlSnapshot, String> {
    if is_active(app) {
        deactivate(app)
    } else {
        begin_session(app)
    }
}

pub fn begin_session(app: &AppHandle) -> Result<HarborControlSnapshot, String> {
    if crate::control::get_mode_snapshot(app).active {
        let _ = crate::control::deactivate_mode(app);
    }
    {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        if !inner.active {
            inner.active = true;
            inner.session_id = inner.session_id.saturating_add(1);
            inner.messages.clear();
            inner.last_error = None;
            inner.is_sending = false;
            inner.status = "接続中…".into();
        }
    }
    show_window(app)?;
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = ensure_local_pairing(&app_handle).await;
    });
    let snapshot = {
        let state = app.state::<HarborControlState>();
        let inner = state.inner.lock().unwrap().clone();
        snapshot(app, &inner)
    };
    emit(app, &snapshot);
    Ok(snapshot)
}

pub fn deactivate(app: &AppHandle) -> Result<HarborControlSnapshot, String> {
    let snapshot = {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        inner.active = false;
        inner.is_sending = false;
        inner.status = "非アクティブ".into();
        snapshot(app, &inner)
    };
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
    emit(app, &snapshot);
    Ok(snapshot)
}

pub async fn submit_transcript(app: &AppHandle, text: String) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return set_status(app, "音声入力が空です", Some("empty_transcript".into()));
    }
    if !paired(app) {
        let _ = ensure_local_pairing(app).await;
    } else {
        let _ = refresh_workspace_labels(app).await;
    }
    let optimistic = {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        if !inner.active {
            return Err("Harbor Control Mode is not active".into());
        }
        inner.messages.push(HarborTurn {
            role: "user".into(),
            content: text.clone(),
        });
        inner.is_sending = true;
        inner.last_error = None;
        inner.status = "解析中…".into();
        snapshot(app, &inner)
    };
    emit(app, &optimistic);

    let response = send_voice_intent(app, &text).await;
    let final_snapshot = {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        inner.is_sending = false;
        match response {
            Ok(response) => {
                inner.messages.push(HarborTurn {
                    role: "assistant".into(),
                    content: response.message.clone(),
                });
                inner.status = status_for_outcome(&response.outcome);
                if response.outcome != "executed" {
                    inner.last_error = Some(response.outcome);
                } else {
                    inner.last_error = None;
                }
            }
            Err(err) => {
                let message = err.to_string();
                inner.last_error = Some(message.clone());
                inner.status = "接続エラー".into();
                inner.messages.push(HarborTurn {
                    role: "assistant".into(),
                    content: "Terminal Harbor を操作できませんでした".into(),
                });
            }
        }
        snapshot(app, &inner)
    };
    emit(app, &final_snapshot);
    Ok(())
}

fn status_for_outcome(outcome: &str) -> String {
    match outcome {
        "executed" => "切替成功".into(),
        "ambiguous" => "候補が曖昧".into(),
        "unsupported" => "未対応の命令".into(),
        "model_unavailable" => "Ollama 未起動 / モデル不可".into(),
        "failed" => "失敗".into(),
        other => other.to_string(),
    }
}

fn set_status(app: &AppHandle, message: &str, error: Option<String>) -> Result<(), String> {
    let snapshot = {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        inner.is_sending = false;
        inner.status = message.to_string();
        inner.last_error = error;
        snapshot(app, &inner)
    };
    emit(app, &snapshot);
    Err(message.to_string())
}

fn paired(app: &AppHandle) -> bool {
    let current = settings::get_settings(app);
    current.harbor_server_id.is_some()
        && current.harbor_client_id.is_some()
        && current.harbor_base_url.is_some()
}

fn snapshot(app: &AppHandle, inner: &HarborRuntimeState) -> HarborControlSnapshot {
    HarborControlSnapshot {
        active: inner.active,
        session_id: inner.session_id,
        messages: inner.messages.clone(),
        is_sending: inner.is_sending,
        last_error: inner.last_error.clone(),
        paired: paired(app),
        status: if inner.status.is_empty() {
            if paired(app) {
                "接続済み".into()
            } else {
                "未ペアリング".into()
            }
        } else {
            inner.status.clone()
        },
        directories: inner.stt_labels.clone(),
    }
}

/// Labels fed into STT while Harbor Control Mode is active (directory basenames + agents).
pub fn stt_context_words(app: &AppHandle) -> Vec<String> {
    if !is_active(app) {
        return Vec::new();
    }
    app.state::<HarborControlState>()
        .inner
        .lock()
        .unwrap()
        .stt_labels
        .clone()
}

/// Whisper initial_prompt glossary built from cached Harbor workspace labels.
pub fn whisper_initial_prompt(app: &AppHandle) -> Option<String> {
    let words = stt_context_words(app);
    if words.is_empty() {
        return None;
    }
    // Whisper treats this as preceding text; keep it short and name-heavy.
    let mut prompt = String::from(
        "Terminal Harbor workspaces and agents (prefer these spellings): ",
    );
    prompt.push_str(&words.join(", "));
    prompt.push('.');
    Some(prompt)
}

fn labels_from_workspaces(workspaces: &[WorkspaceDto]) -> Vec<String> {
    let mut labels = Vec::new();
    let mut push_unique = |value: &str| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        if !labels
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            labels.push(trimmed.to_string());
        }
    };
    for workspace in workspaces {
        if let Some(directory) = workspace.directory.as_deref() {
            push_unique(directory);
            // Hyphen/underscore forms help STT and fuzzy correction.
            if directory.contains('-') || directory.contains('_') {
                push_unique(&directory.replace('-', " ").replace('_', " "));
            }
        }
        if let Some(agent) = workspace.agent.as_deref() {
            push_unique(agent);
            match agent.to_ascii_lowercase().as_str() {
                "codex" => push_unique("コーデックス"),
                "claude" => push_unique("クロード"),
                _ => {}
            }
        }
        if let Some(name) = workspace.name.as_deref() {
            push_unique(name);
        }
    }
    labels
}

fn emit(app: &AppHandle, value: &HarborControlSnapshot) {
    let _ = app.emit(EVENT_NAME, value);
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.emit(EVENT_NAME, value);
    }
}

fn show_window(app: &AppHandle) -> Result<(), String> {
    let window = match app.get_webview_window(WINDOW_LABEL) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("/".into()))
            .title("Terminal Harbor Control")
            .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
            .min_inner_size(240.0, 220.0)
            .resizable(true)
            .visible(false)
            .always_on_top(true)
            .build()
            .map_err(|err| err.to_string())?,
    };
    position_window(app, &window);
    if let Some(main_window) = app.get_webview_window("main") {
        let _ = main_window.hide();
    }
    window.show().map_err(|err| err.to_string())?;
    window.set_focus().map_err(|err| err.to_string())?;
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn position_window(app: &AppHandle, window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return;
    };
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let work_width = size.width as f64 / scale;
    let x = work_width - WINDOW_WIDTH - WINDOW_RIGHT_OFFSET;
    let y = WINDOW_TOP_OFFSET;
    let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hmac_value(key: &[u8], value: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts key size");
    mac.update(value);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn canonical_request(method: &str, path: &str, timestamp: &str, nonce: &str, body: &[u8]) -> String {
    format!(
        "TH-HMAC-V1\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path,
        timestamp,
        nonce,
        sha256_hex(body)
    )
}

fn derive_device_key(token: &str, server_id: &str, client_id: &str, nonce: &[u8]) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(Some(server_id.as_bytes()), token.as_bytes());
    let mut info = b"terminal-harbor/device/v2\0".to_vec();
    info.extend_from_slice(client_id.as_bytes());
    info.push(0);
    info.extend_from_slice(nonce);
    let mut key = vec![0u8; 32];
    hk.expand(&info, &mut key).expect("valid HKDF output length");
    key
}

fn response_signature_valid(
    key: &[u8],
    nonce: &str,
    status: u16,
    body: &[u8],
    signature: &str,
) -> bool {
    let canonical = format!(
        "TH-HMAC-V1-RESPONSE\n{nonce}\n{status}\n{}",
        sha256_hex(body)
    );
    hmac_value(key, canonical.as_bytes()) == signature
}

fn random_client_nonce() -> Vec<u8> {
    let mut nonce = vec![0u8; 32];
    nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    nonce
}

fn looks_like_base_url(raw: &str) -> bool {
    reqwest::Url::parse(raw)
        .ok()
        .map(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().map(|host| !host.is_empty()).unwrap_or(false)
        })
        .unwrap_or(false)
}

fn candidate_base_urls(uri: &reqwest::Url) -> Vec<String> {
    let mut urls = Vec::new();
    let mut host = None;
    let mut port = None;
    for (key, value) in uri.query_pairs() {
        match key.as_ref() {
            "host" => host = Some(value.into_owned()),
            "port" => port = Some(value.into_owned()),
            "endpoint" => {
                if let Some((_, url)) = value.split_once(',') {
                    if looks_like_base_url(url) {
                        let url = url.trim_end_matches('/').to_string();
                        if !urls.iter().any(|existing| existing == &url) {
                            urls.push(url);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let (Some(host), Some(port)) = (host, port) {
        let legacy = format!("http://{host}:{port}");
        if looks_like_base_url(&legacy) && !urls.iter().any(|url| url == &legacy) {
            urls.push(legacy);
        }
    }
    if urls.is_empty() {
        urls.push(DEFAULT_BASE_URL.to_string());
    }
    urls.sort_by_key(|url| {
        if url.contains("127.0.0.1") || url.contains("localhost") {
            0
        } else if url.starts_with("https://") {
            1
        } else {
            2
        }
    });
    urls
}

async fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?)
}

async fn signed_request(
    method: &str,
    base_url: &str,
    path: &str,
    body: Vec<u8>,
    signing_key: &[u8],
    response_key: &[u8],
    client_id: Option<&str>,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let timestamp = now_unix().to_string();
    let nonce = URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes());
    let signature = hmac_value(
        signing_key,
        canonical_request(method, path, &timestamp, &nonce, &body).as_bytes(),
    );
    let client = http_client().await?;
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let mut request = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        other => anyhow::bail!("unsupported method {other}"),
    };
    request = request
        .header("Accept", "application/json")
        .header("X-Harbor-Timestamp", &timestamp)
        .header("X-Harbor-Nonce", &nonce)
        .header("X-Harbor-Signature", signature);
    if let Some(client_id) = client_id {
        request = request.header("X-Harbor-Client-Id", client_id);
    }
    if method == "POST" {
        request = request
            .header("Content-Type", "application/json")
            .body(body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let response_signature = response
        .headers()
        .get("x-harbor-response-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Terminal Harbor returned an unsigned response"))?;
    let response_body = response.bytes().await?.to_vec();
    if !response_signature_valid(response_key, &nonce, status, &response_body, &response_signature)
    {
        anyhow::bail!("Terminal Harbor response signature is invalid");
    }
    Ok((status, response_body))
}

async fn signed_post(
    base_url: &str,
    path: &str,
    body: Vec<u8>,
    signing_key: &[u8],
    response_key: &[u8],
    client_id: Option<&str>,
) -> anyhow::Result<(u16, Vec<u8>)> {
    signed_request(
        "POST",
        base_url,
        path,
        body,
        signing_key,
        response_key,
        client_id,
    )
    .await
}

async fn refresh_workspace_labels(app: &AppHandle) -> anyhow::Result<Vec<String>> {
    let current = settings::get_settings(app);
    let server_id = current
        .harbor_server_id
        .context("Terminal Harbor is not paired")?;
    let client_id = current
        .harbor_client_id
        .context("Terminal Harbor is not paired")?;
    let base_url = current
        .harbor_base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let key = load_secret(&server_id)?;
    let (status, body) = signed_request(
        "GET",
        &base_url,
        "/v1/workspaces",
        Vec::new(),
        &key,
        &key,
        Some(&client_id),
    )
    .await?;
    if status != 200 {
        anyhow::bail!("listing workspaces returned HTTP {status}");
    }
    let parsed: WorkspacesResponse =
        serde_json::from_slice(&body).context("parsing workspace list")?;
    let labels = labels_from_workspaces(&parsed.workspaces);
    {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        inner.stt_labels = labels.clone();
    }
    Ok(labels)
}

async fn fetch_identity(base_url: &str) -> anyhow::Result<IdentityResponse> {
    let client = http_client().await?;
    let response = client
        .get(format!("{}/v1/identity", base_url.trim_end_matches('/')))
        .header("Accept", "application/json")
        .send()
        .await
        .context("Terminal Harbor is not reachable on loopback")?;
    if !response.status().is_success() {
        anyhow::bail!("Terminal Harbor identity returned HTTP {}", response.status());
    }
    response
        .json::<IdentityResponse>()
        .await
        .context("parsing Terminal Harbor identity")
}

async fn pair_local_inner(app: &AppHandle) -> anyhow::Result<HarborPairStatus> {
    let identity = fetch_identity(DEFAULT_BASE_URL).await?;
    let client_id = Uuid::new_v4().to_string();
    let client_nonce = random_client_nonce();
    let body = serde_json::to_vec(&serde_json::json!({
        "auth_version": AUTH_VERSION,
        "client_id": client_id,
        "client_nonce": URL_SAFE_NO_PAD.encode(&client_nonce),
        "device_name": "Handy"
    }))?;
    let timestamp = now_unix().to_string();
    let request_nonce = URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes());
    let client = http_client().await?;
    let response = client
        .post(format!("{}{LOCAL_PAIR_PATH}", DEFAULT_BASE_URL))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("X-Harbor-Timestamp", &timestamp)
        .header("X-Harbor-Nonce", &request_nonce)
        .body(body)
        .send()
        .await
        .context("local pairing request failed")?;
    let status = response.status().as_u16();
    let response_signature = response
        .headers()
        .get("x-harbor-response-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Terminal Harbor returned an unsigned local pair response"))?;
    let response_body = response.bytes().await?.to_vec();
    if status != 200 {
        anyhow::bail!("local pairing rejected with HTTP {status}");
    }
    let parsed: LocalPairResponse = serde_json::from_slice(&response_body)
        .context("parsing local pair response")?;
    if parsed.server_id != identity.server_id {
        anyhow::bail!("local pair response came from a different Terminal Harbor");
    }
    let key = derive_device_key(
        &parsed.local_pair_token,
        &parsed.server_id,
        &parsed.client_id,
        &client_nonce,
    );
    if !response_signature_valid(&key, &request_nonce, status, &response_body, &response_signature)
    {
        anyhow::bail!("local pair response signature is invalid");
    }
    save_secret(&parsed.server_id, &key)?;
    let base_url = parsed
        .base_url
        .filter(|url| looks_like_base_url(url))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let mut current = settings::get_settings(app);
    current.harbor_server_id = Some(parsed.server_id.clone());
    current.harbor_client_id = Some(parsed.client_id);
    current.harbor_base_url = Some(base_url.clone());
    settings::write_settings(app, current);
    Ok(HarborPairStatus {
        paired: true,
        server_id: Some(parsed.server_id),
        base_url: Some(base_url),
    })
}

async fn existing_pairing_still_valid(app: &AppHandle, server_id: &str) -> bool {
    let current = settings::get_settings(app);
    let Some(client_id) = current.harbor_client_id.clone() else {
        return false;
    };
    let Ok(key) = load_secret(server_id) else {
        return false;
    };
    let base_url = current
        .harbor_base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    // Lightweight signed session probe.
    let timestamp = now_unix().to_string();
    let nonce = URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes());
    let body = Vec::new();
    let signature = hmac_value(
        &key,
        canonical_request("GET", "/v1/session", &timestamp, &nonce, &body).as_bytes(),
    );
    let Ok(client) = http_client().await else {
        return false;
    };
    let Ok(response) = client
        .get(format!("{}/v1/session", base_url.trim_end_matches('/')))
        .header("Accept", "application/json")
        .header("X-Harbor-Timestamp", &timestamp)
        .header("X-Harbor-Nonce", &nonce)
        .header("X-Harbor-Signature", signature)
        .header("X-Harbor-Client-Id", client_id)
        .send()
        .await
    else {
        return false;
    };
    let status = response.status().as_u16();
    let Some(response_signature) = response
        .headers()
        .get("x-harbor-response-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    else {
        return false;
    };
    let Ok(response_body) = response.bytes().await else {
        return false;
    };
    status == 200
        && response_signature_valid(
            &key,
            &nonce,
            status,
            &response_body,
            &response_signature,
        )
}

pub async fn ensure_local_pairing(app: &AppHandle) -> Result<HarborPairStatus, String> {
    let result = ensure_local_pairing_inner(app).await;
    let status = match &result {
        Ok(status) => {
            let _ = refresh_workspace_labels(app).await;
            let snap = {
                let state = app.state::<HarborControlState>();
                let mut inner = state.inner.lock().unwrap();
                let count = inner.stt_labels.len();
                inner.status = if count == 0 {
                    "接続済み · 認識待ち".into()
                } else {
                    format!("接続済み · 語彙 {count} 件")
                };
                inner.last_error = None;
                snapshot(app, &inner)
            };
            emit(app, &snap);
            status.clone()
        }
        Err(err) => {
            let message = err.to_string();
            let snap = {
                let state = app.state::<HarborControlState>();
                let mut inner = state.inner.lock().unwrap();
                inner.status = if message.contains("not reachable") {
                    "Terminal Harbor 未起動".into()
                } else {
                    "自動ペア失敗".into()
                };
                inner.last_error = Some(message.clone());
                snapshot(app, &inner)
            };
            emit(app, &snap);
            return Err(format!("{err:#}"));
        }
    };
    Ok(status)
}

async fn ensure_local_pairing_inner(app: &AppHandle) -> anyhow::Result<HarborPairStatus> {
    let identity = fetch_identity(DEFAULT_BASE_URL).await?;
    let current = settings::get_settings(app);
    if current.harbor_server_id.as_deref() == Some(identity.server_id.as_str())
        && existing_pairing_still_valid(app, &identity.server_id).await
    {
        return Ok(HarborPairStatus {
            paired: true,
            server_id: Some(identity.server_id),
            base_url: current.harbor_base_url.or_else(|| Some(DEFAULT_BASE_URL.to_string())),
        });
    }
    pair_local_inner(app).await
}

#[cfg(target_os = "macos")]
fn save_secret(server_id: &str, secret: &[u8]) -> anyhow::Result<()> {
    let mut child = Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            server_id,
            "-w",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("opening Keychain input")?
        .write_all(URL_SAFE_NO_PAD.encode(secret).as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("saving Terminal Harbor key in Keychain failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn load_secret(server_id: &str) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            server_id,
            "-w",
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("Terminal Harbor pairing key is missing from Keychain");
    }
    URL_SAFE_NO_PAD
        .decode(String::from_utf8(output.stdout)?.trim())
        .context("decoding Terminal Harbor pairing key")
}

#[cfg(not(target_os = "macos"))]
fn save_secret(_server_id: &str, _secret: &[u8]) -> anyhow::Result<()> {
    anyhow::bail!("Terminal Harbor pairing currently requires macOS Keychain")
}

#[cfg(not(target_os = "macos"))]
fn load_secret(_server_id: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("Terminal Harbor pairing currently requires macOS Keychain")
}

async fn pair_inner(app: &AppHandle, raw_uri: &str) -> anyhow::Result<HarborPairStatus> {
    let uri = reqwest::Url::parse(raw_uri.trim()).context("invalid Terminal Harbor pair URI")?;
    if uri.scheme() != "harbor" || uri.host_str() != Some("pair") {
        anyhow::bail!("not a Terminal Harbor pair URI");
    }
    let query: std::collections::HashMap<String, String> =
        uri.query_pairs().into_owned().collect();
    if query.get("auth").map(String::as_str) != Some(AUTH_VERSION) {
        anyhow::bail!("pair URI does not use HMAC authentication");
    }
    let token = query.get("token").context("pair URI is missing its token")?;
    let server_id = query
        .get("sid")
        .context("pair URI is missing its server id")?;
    Uuid::parse_str(server_id).context("pair URI has an invalid server id")?;
    let client_id = Uuid::new_v4().to_string();
    let client_nonce = random_client_nonce();
    let key = derive_device_key(token, server_id, &client_id, &client_nonce);
    let body = serde_json::to_vec(&serde_json::json!({
        "auth_version": AUTH_VERSION,
        "client_id": client_id,
        "client_nonce": URL_SAFE_NO_PAD.encode(&client_nonce),
        "device_name": "Handy"
    }))?;

    let mut last_error = None;
    let mut paired = None;
    for candidate in candidate_base_urls(&uri) {
        match signed_post(
            &candidate,
            "/v1/pair",
            body.clone(),
            token.as_bytes(),
            &key,
            None,
        )
        .await
        {
            Ok((status, response_body)) if status == 200 => {
                paired = Some((candidate, response_body));
                break;
            }
            Ok((status, _)) => {
                last_error = Some(anyhow!("Terminal Harbor pairing was rejected ({status})"));
            }
            Err(err) => last_error = Some(err),
        }
    }
    let (connected_url, response_body) = paired.ok_or_else(|| {
        last_error.unwrap_or_else(|| anyhow!("Terminal Harbor pairing was rejected"))
    })?;
    let response: PairResponse = serde_json::from_slice(&response_body)?;
    if response.server_id != *server_id {
        anyhow::bail!("pair response came from a different Terminal Harbor");
    }
    let response_client_id = response.client_id.unwrap_or(client_id);
    let preferred_url = response
        .endpoints
        .unwrap_or_default()
        .into_iter()
        .map(|endpoint| endpoint.url.trim_end_matches('/').to_string())
        .find(|url| {
            looks_like_base_url(url) && (url.contains("127.0.0.1") || url.contains("localhost"))
        })
        .unwrap_or(connected_url);

    save_secret(server_id, &key)?;
    let mut current = settings::get_settings(app);
    current.harbor_server_id = Some(server_id.clone());
    current.harbor_client_id = Some(response_client_id);
    current.harbor_base_url = Some(preferred_url.clone());
    settings::write_settings(app, current);

    let snap = {
        let state = app.state::<HarborControlState>();
        let mut inner = state.inner.lock().unwrap();
        inner.status = "接続済み".into();
        inner.last_error = None;
        snapshot(app, &inner)
    };
    emit(app, &snap);
    Ok(HarborPairStatus {
        paired: true,
        server_id: Some(server_id.clone()),
        base_url: Some(preferred_url),
    })
}

async fn send_voice_intent(app: &AppHandle, text: &str) -> anyhow::Result<VoiceResponse> {
    let current = settings::get_settings(app);
    let server_id = current
        .harbor_server_id
        .context("Terminal Harbor is not paired")?;
    let client_id = current
        .harbor_client_id
        .context("Terminal Harbor is not paired")?;
    let base_url = current
        .harbor_base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let key = load_secret(&server_id)?;
    let body = serde_json::to_vec(&serde_json::json!({"text": text}))?;
    let (status, response_body) = signed_post(
        &base_url,
        VOICE_PATH,
        body,
        &key,
        &key,
        Some(&client_id),
    )
    .await?;
    if status != 200 {
        anyhow::bail!("Terminal Harbor voice endpoint returned HTTP {status}");
    }
    serde_json::from_slice(&response_body).context("parsing Terminal Harbor voice response")
}

#[tauri::command]
#[specta::specta]
pub fn get_harbor_control(app: AppHandle) -> HarborControlSnapshot {
    let state = app.state::<HarborControlState>();
    let inner = state.inner.lock().unwrap().clone();
    snapshot(&app, &inner)
}

#[tauri::command]
#[specta::specta]
pub fn toggle_harbor_control(app: AppHandle) -> Result<HarborControlSnapshot, String> {
    toggle(&app)
}

#[tauri::command]
#[specta::specta]
pub fn deactivate_harbor_control(app: AppHandle) -> Result<HarborControlSnapshot, String> {
    deactivate(&app)
}

#[tauri::command]
#[specta::specta]
pub async fn pair_terminal_harbor(
    app: AppHandle,
    pair_uri: String,
) -> Result<HarborPairStatus, String> {
    pair_inner(&app, &pair_uri)
        .await
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
#[specta::specta]
pub async fn ensure_terminal_harbor_local_pairing(
    app: AppHandle,
) -> Result<HarborPairStatus, String> {
    ensure_local_pairing(&app).await
}

#[tauri::command]
#[specta::specta]
pub fn get_terminal_harbor_pairing(app: AppHandle) -> HarborPairStatus {
    let current = settings::get_settings(&app);
    HarborPairStatus {
        paired: current.harbor_server_id.is_some() && current.harbor_client_id.is_some(),
        server_id: current.harbor_server_id,
        base_url: current.harbor_base_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_canonicalization_matches_bridge_contract() {
        assert_eq!(
            canonical_request("post", "/v1/voice/intent", "1", "nonce", b"{}"),
            format!(
                "TH-HMAC-V1\nPOST\n/v1/voice/intent\n1\nnonce\n{}",
                sha256_hex(b"{}")
            )
        );
    }

    #[test]
    fn device_key_is_stable_and_bound_to_client() {
        let nonce = b"0123456789abcdef0123456789abcdef";
        let one = derive_device_key("token", "server", "one", nonce);
        assert_eq!(one, derive_device_key("token", "server", "one", nonce));
        assert_ne!(one, derive_device_key("token", "server", "two", nonce));
    }

    #[test]
    fn client_nonce_is_32_bytes() {
        assert_eq!(random_client_nonce().len(), 32);
    }

    #[test]
    fn outcome_status_messages_cover_phase_one() {
        assert_eq!(status_for_outcome("executed"), "切替成功");
        assert_eq!(status_for_outcome("model_unavailable"), "Ollama 未起動 / モデル不可");
        assert_eq!(status_for_outcome("ambiguous"), "候補が曖昧");
    }

    #[test]
    fn workspace_labels_prefer_directories_and_agent_aliases() {
        let labels = labels_from_workspaces(&[
            WorkspaceDto {
                name: Some("ws1".into()),
                directory: Some("terminal-harbor".into()),
                agent: Some("Codex".into()),
            },
            WorkspaceDto {
                name: Some("ws2".into()),
                directory: Some("Handy".into()),
                agent: Some("Claude".into()),
            },
        ]);
        assert!(labels.iter().any(|v| v == "terminal-harbor"));
        assert!(labels.iter().any(|v| v == "terminal harbor"));
        assert!(labels.iter().any(|v| v == "Handy"));
        assert!(labels.iter().any(|v| v == "コーデックス"));
        assert!(labels.iter().any(|v| v == "クロード"));
    }
}
