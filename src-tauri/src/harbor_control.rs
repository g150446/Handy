use anyhow::{anyhow, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::fmt;
use std::future::Future;
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
    pairing: tokio::sync::Mutex<()>,
}

#[derive(Clone, Debug, Default)]
struct HarborRuntimeState {
    active: bool,
    session_id: u64,
    messages: Vec<HarborTurn>,
    is_sending: bool,
    last_error: Option<HarborControlError>,
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
    pub last_error: Option<HarborControlError>,
    pub paired: bool,
    pub status: String,
    pub directories: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HarborControlErrorCode {
    AuthenticationFailed,
    UntrustedResponse,
    Unreachable,
    PairingFailed,
    ProtocolError,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct HarborControlError {
    pub code: HarborControlErrorCode,
    pub http_status: Option<u16>,
    pub detail: Option<String>,
}

#[derive(Debug)]
enum HarborClientError {
    AuthenticationFailed {
        status: Option<u16>,
        detail: Option<String>,
    },
    UntrustedResponse {
        status: Option<u16>,
        detail: Option<String>,
    },
    Transport(String),
    Pairing(String),
    Protocol(String),
}

impl HarborClientError {
    fn is_repairable_auth(&self) -> bool {
        matches!(
            self,
            Self::AuthenticationFailed { .. } | Self::UntrustedResponse { .. }
        )
    }

    fn to_control_error(&self) -> HarborControlError {
        match self {
            Self::AuthenticationFailed { status, detail } => HarborControlError {
                code: HarborControlErrorCode::AuthenticationFailed,
                http_status: *status,
                detail: detail.clone(),
            },
            Self::UntrustedResponse { status, detail } => HarborControlError {
                code: HarborControlErrorCode::UntrustedResponse,
                http_status: *status,
                detail: detail.clone(),
            },
            Self::Transport(detail) => HarborControlError {
                code: HarborControlErrorCode::Unreachable,
                http_status: None,
                detail: Some(detail.clone()),
            },
            Self::Pairing(detail) => HarborControlError {
                code: HarborControlErrorCode::PairingFailed,
                http_status: None,
                detail: Some(detail.clone()),
            },
            Self::Protocol(detail) => HarborControlError {
                code: HarborControlErrorCode::ProtocolError,
                http_status: None,
                detail: Some(detail.clone()),
            },
        }
    }

    fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::AuthenticationFailed { .. } => "authentication_failed",
            Self::UntrustedResponse { .. } => "untrusted_response",
            Self::Transport(_) => "unreachable",
            Self::Pairing(_) => "pairing_failed",
            Self::Protocol(_) => "protocol_error",
        }
    }

    fn http_status(&self) -> Option<u16> {
        match self {
            Self::AuthenticationFailed { status, .. } | Self::UntrustedResponse { status, .. } => {
                *status
            }
            _ => None,
        }
    }
}

impl fmt::Display for HarborClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationFailed { status, detail } => {
                write!(f, "Terminal Harbor authentication failed")?;
                write_error_context(f, *status, detail.as_deref())
            }
            Self::UntrustedResponse { status, detail } => {
                write!(f, "Terminal Harbor returned an untrusted response")?;
                write_error_context(f, *status, detail.as_deref())
            }
            Self::Transport(detail) => write!(f, "Terminal Harbor is unreachable: {detail}"),
            Self::Pairing(detail) => write!(f, "Terminal Harbor pairing failed: {detail}"),
            Self::Protocol(detail) => write!(f, "Terminal Harbor protocol error: {detail}"),
        }
    }
}

impl std::error::Error for HarborClientError {}

fn write_error_context(
    f: &mut fmt::Formatter<'_>,
    status: Option<u16>,
    detail: Option<&str>,
) -> fmt::Result {
    if let Some(status) = status {
        write!(f, " (HTTP {status}")?;
        if let Some(detail) = detail {
            write!(f, ": {detail}")?;
        }
        write!(f, ")")?;
    } else if let Some(detail) = detail {
        write!(f, ": {detail}")?;
    }
    Ok(())
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
        return set_status(
            app,
            "音声入力が空です",
            Some(HarborControlError {
                code: HarborControlErrorCode::ProtocolError,
                http_status: None,
                detail: Some("empty transcript".into()),
            }),
        );
    }

    // Local Handy mode switches (Desktop / normal) before Harbor HTTP intent.
    if let Some(intent) = crate::preferred_control::match_mode_switch_intent(&text) {
        if !matches!(intent, crate::preferred_control::ModeSwitchIntent::Harbor) {
            let label = match intent {
                crate::preferred_control::ModeSwitchIntent::Desktop => {
                    "デスクトップ操作に切り替えました"
                }
                crate::preferred_control::ModeSwitchIntent::Normal => "通常入力モードに戻りました",
                crate::preferred_control::ModeSwitchIntent::Harbor => unreachable!(),
            };
            let note = {
                let state = app.state::<HarborControlState>();
                let mut inner = state.inner.lock().unwrap();
                if inner.active {
                    inner.messages.push(HarborTurn {
                        role: "user".into(),
                        content: text.clone(),
                    });
                    inner.messages.push(HarborTurn {
                        role: "assistant".into(),
                        content: label.into(),
                    });
                    inner.is_sending = false;
                    inner.last_error = None;
                    inner.status = label.into();
                    snapshot(app, &inner)
                } else {
                    snapshot(app, &inner)
                }
            };
            emit(app, &note);
            crate::preferred_control::apply_mode_switch_intent(app, intent)?;
            return Ok(());
        }
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

    let mut recovery_used = false;
    let labels_result =
        with_pairing_recovery(app, &mut recovery_used, || refresh_workspace_labels(app)).await;
    let response = match labels_result {
        Ok(_) => {
            with_pairing_recovery(app, &mut recovery_used, || send_voice_intent(app, &text)).await
        }
        Err(err)
            if matches!(
                err,
                HarborClientError::AuthenticationFailed { .. }
                    | HarborClientError::UntrustedResponse { .. }
                    | HarborClientError::Pairing(_)
            ) =>
        {
            Err(err)
        }
        Err(err) => {
            log::warn!("Terminal Harbor workspace labels could not be refreshed: {err}");
            with_pairing_recovery(app, &mut recovery_used, || send_voice_intent(app, &text)).await
        }
    };
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
                inner.last_error = None;
            }
            Err(err) => {
                let control_error = err.to_control_error();
                inner.status = if matches!(
                    control_error.code,
                    HarborControlErrorCode::AuthenticationFailed
                        | HarborControlErrorCode::UntrustedResponse
                        | HarborControlErrorCode::PairingFailed
                ) {
                    "認証エラー".into()
                } else {
                    "接続エラー".into()
                };
                inner.last_error = Some(control_error);
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
        "model_unavailable" => "OpenRouter 応答なし / キー不可".into(),
        "failed" => "失敗".into(),
        other => other.to_string(),
    }
}

fn set_status(
    app: &AppHandle,
    message: &str,
    error: Option<HarborControlError>,
) -> Result<(), String> {
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

fn can_auto_repair_locally(app: &AppHandle) -> bool {
    let current = settings::get_settings(app);
    let Some(base_url) = current.harbor_base_url else {
        return true;
    };
    reqwest::Url::parse(&base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PairingMarker {
    server_id: Option<String>,
    client_id: Option<String>,
    base_url: Option<String>,
}

fn pairing_marker(app: &AppHandle) -> PairingMarker {
    let current = settings::get_settings(app);
    PairingMarker {
        server_id: current.harbor_server_id,
        client_id: current.harbor_client_id,
        base_url: current.harbor_base_url,
    }
}

fn invalidate_pairing(app: &AppHandle, expected: &PairingMarker) {
    let mut current = settings::get_settings(app);
    let actual = PairingMarker {
        server_id: current.harbor_server_id.clone(),
        client_id: current.harbor_client_id.clone(),
        base_url: current.harbor_base_url.clone(),
    };
    if &actual != expected {
        return;
    }
    current.harbor_server_id = None;
    current.harbor_client_id = None;
    current.harbor_base_url = None;
    settings::write_settings(app, current);
    app.state::<HarborControlState>()
        .inner
        .lock()
        .unwrap()
        .stt_labels
        .clear();
}

fn claim_auth_recovery(error: &HarborClientError, recovery_used: &mut bool) -> bool {
    if error.is_repairable_auth() && !*recovery_used {
        *recovery_used = true;
        true
    } else {
        false
    }
}

async fn with_pairing_recovery<T, F, Fut>(
    app: &AppHandle,
    recovery_used: &mut bool,
    mut operation: F,
) -> Result<T, HarborClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, HarborClientError>>,
{
    let first_pairing = pairing_marker(app);
    match operation().await {
        Ok(value) => Ok(value),
        Err(first_error) if claim_auth_recovery(&first_error, recovery_used) => {
            log::warn!(
                "Terminal Harbor authentication failed; attempting one local repair (kind={}, status={:?})",
                first_error.diagnostic_kind(),
                first_error.http_status()
            );
            if !can_auto_repair_locally(app) {
                invalidate_pairing(app, &first_pairing);
                return Err(first_error);
            }
            if let Err(err) = ensure_local_pairing_inner(app).await {
                invalidate_pairing(app, &first_pairing);
                return Err(HarborClientError::Pairing(err.to_string()));
            }
            let repaired_pairing = pairing_marker(app);
            match operation().await {
                Ok(value) => {
                    log::info!("Terminal Harbor pairing recovered successfully");
                    Ok(value)
                }
                Err(second_error) => {
                    if second_error.is_repairable_auth() {
                        invalidate_pairing(app, &repaired_pairing);
                    }
                    Err(second_error)
                }
            }
        }
        Err(error) => {
            if error.is_repairable_auth() {
                invalidate_pairing(app, &first_pairing);
            }
            Err(error)
        }
    }
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
    let mut prompt =
        String::from("Terminal Harbor workspaces and agents (prefer these spellings): ");
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

fn canonical_request(
    method: &str,
    path: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> String {
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
    hk.expand(&info, &mut key)
        .expect("valid HKDF output length");
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
        .timeout(Duration::from_secs(20))
        .build()?)
}

fn safe_error_detail(body: &[u8]) -> Option<String> {
    let candidate = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let sanitized: String = candidate
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(160)
        .collect();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn validate_signed_response(
    response_key: &[u8],
    request_nonce: &str,
    status: u16,
    response_body: &[u8],
    response_signature: Option<&str>,
) -> Result<(), HarborClientError> {
    let detail = safe_error_detail(response_body);
    let Some(response_signature) = response_signature else {
        return Err(if status == 401 {
            HarborClientError::AuthenticationFailed {
                status: Some(status),
                detail,
            }
        } else {
            HarborClientError::UntrustedResponse {
                status: Some(status),
                detail,
            }
        });
    };
    if !response_signature_valid(
        response_key,
        request_nonce,
        status,
        response_body,
        response_signature,
    ) {
        return Err(HarborClientError::UntrustedResponse {
            status: Some(status),
            detail,
        });
    }
    Ok(())
}

async fn signed_request(
    method: &str,
    base_url: &str,
    path: &str,
    body: Vec<u8>,
    signing_key: &[u8],
    response_key: &[u8],
    client_id: Option<&str>,
) -> Result<(u16, Vec<u8>), HarborClientError> {
    let timestamp = now_unix().to_string();
    let nonce = URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes());
    let signature = hmac_value(
        signing_key,
        canonical_request(method, path, &timestamp, &nonce, &body).as_bytes(),
    );
    let client = http_client()
        .await
        .map_err(|err| HarborClientError::Transport(err.to_string()))?;
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let mut request = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        other => {
            return Err(HarborClientError::Protocol(format!(
                "unsupported method {other}"
            )))
        }
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
    let response = request
        .send()
        .await
        .map_err(|err| HarborClientError::Transport(err.to_string()))?;
    let status = response.status().as_u16();
    let response_signature = response
        .headers()
        .get("x-harbor-response-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let response_body = response
        .bytes()
        .await
        .map_err(|err| HarborClientError::Transport(err.to_string()))?
        .to_vec();
    validate_signed_response(
        response_key,
        &nonce,
        status,
        &response_body,
        response_signature.as_deref(),
    )?;
    Ok((status, response_body))
}

async fn signed_post(
    base_url: &str,
    path: &str,
    body: Vec<u8>,
    signing_key: &[u8],
    response_key: &[u8],
    client_id: Option<&str>,
) -> Result<(u16, Vec<u8>), HarborClientError> {
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

async fn refresh_workspace_labels(app: &AppHandle) -> Result<Vec<String>, HarborClientError> {
    let state = app.state::<HarborControlState>();
    let _pairing_guard = state.pairing.lock().await;
    let current = settings::get_settings(app);
    let server_id =
        current
            .harbor_server_id
            .ok_or_else(|| HarborClientError::AuthenticationFailed {
                status: None,
                detail: Some("pairing credentials are missing".into()),
            })?;
    let client_id =
        current
            .harbor_client_id
            .ok_or_else(|| HarborClientError::AuthenticationFailed {
                status: None,
                detail: Some("pairing credentials are missing".into()),
            })?;
    let base_url = current
        .harbor_base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let key = load_secret(&server_id).map_err(|_| HarborClientError::AuthenticationFailed {
        status: None,
        detail: Some("pairing key is missing or invalid".into()),
    })?;
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
        return Err(HarborClientError::Protocol(format!(
            "listing workspaces returned HTTP {status}: {}",
            safe_error_detail(&body).unwrap_or_else(|| "unknown error".into())
        )));
    }
    let parsed: WorkspacesResponse = serde_json::from_slice(&body)
        .map_err(|err| HarborClientError::Protocol(format!("parsing workspace list: {err}")))?;
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
        anyhow::bail!(
            "Terminal Harbor identity returned HTTP {}",
            response.status()
        );
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
    let parsed: LocalPairResponse =
        serde_json::from_slice(&response_body).context("parsing local pair response")?;
    if parsed.server_id != identity.server_id {
        anyhow::bail!("local pair response came from a different Terminal Harbor");
    }
    let key = derive_device_key(
        &parsed.local_pair_token,
        &parsed.server_id,
        &parsed.client_id,
        &client_nonce,
    );
    if !response_signature_valid(
        &key,
        &request_nonce,
        status,
        &response_body,
        &response_signature,
    ) {
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
        && response_signature_valid(&key, &nonce, status, &response_body, &response_signature)
}

pub async fn ensure_local_pairing(app: &AppHandle) -> Result<HarborPairStatus, String> {
    let result = ensure_local_pairing_inner(app).await;
    let status = match &result {
        Ok(status) => {
            let validated_pairing = pairing_marker(app);
            let labels_result = refresh_workspace_labels(app).await;
            if let Err(err) = &labels_result {
                if err.is_repairable_auth() {
                    invalidate_pairing(app, &validated_pairing);
                }
            }
            let snap = {
                let state = app.state::<HarborControlState>();
                let mut inner = state.inner.lock().unwrap();
                let count = inner.stt_labels.len();
                match &labels_result {
                    Ok(_) => {
                        inner.status = if count == 0 {
                            "接続済み · 認識待ち".into()
                        } else {
                            format!("接続済み · 語彙 {count} 件")
                        };
                        inner.last_error = None;
                    }
                    Err(err) if err.is_repairable_auth() => {
                        inner.status = "認証エラー".into();
                        inner.last_error = Some(err.to_control_error());
                    }
                    Err(err) => {
                        inner.status = "接続済み · 語彙取得失敗".into();
                        inner.last_error = Some(err.to_control_error());
                    }
                }
                snapshot(app, &inner)
            };
            emit(app, &snap);
            if let Err(err) = labels_result {
                if err.is_repairable_auth() {
                    return Err(err.to_string());
                }
            }
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
                inner.last_error = Some(HarborControlError {
                    code: if message.contains("not reachable") {
                        HarborControlErrorCode::Unreachable
                    } else {
                        HarborControlErrorCode::PairingFailed
                    },
                    http_status: None,
                    detail: Some(message.clone()),
                });
                snapshot(app, &inner)
            };
            emit(app, &snap);
            return Err(format!("{err:#}"));
        }
    };
    Ok(status)
}

async fn ensure_local_pairing_inner(app: &AppHandle) -> anyhow::Result<HarborPairStatus> {
    let state = app.state::<HarborControlState>();
    let _pairing_guard = state.pairing.lock().await;
    let identity = fetch_identity(DEFAULT_BASE_URL).await?;
    let current = settings::get_settings(app);
    if current.harbor_server_id.as_deref() == Some(identity.server_id.as_str())
        && existing_pairing_still_valid(app, &identity.server_id).await
    {
        return Ok(HarborPairStatus {
            paired: true,
            server_id: Some(identity.server_id),
            base_url: current
                .harbor_base_url
                .or_else(|| Some(DEFAULT_BASE_URL.to_string())),
        });
    }
    pair_local_inner(app).await
}

#[cfg(target_os = "macos")]
fn save_secret(server_id: &str, secret: &[u8]) -> anyhow::Result<()> {
    if secret.len() != 32 {
        anyhow::bail!("Terminal Harbor pairing key has an invalid length");
    }
    let encoded = URL_SAFE_NO_PAD.encode(secret);
    security_framework::passwords::set_generic_password(
        KEYCHAIN_SERVICE,
        server_id,
        encoded.as_bytes(),
    )
    .map_err(|_| anyhow!("saving Terminal Harbor key in Keychain failed"))?;
    let persisted = load_secret(server_id)?;
    if persisted != secret {
        anyhow::bail!("verifying Terminal Harbor key in Keychain failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn load_secret(server_id: &str) -> anyhow::Result<Vec<u8>> {
    let stored = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, server_id)
        .map_err(|_| anyhow!("Terminal Harbor pairing key is missing from Keychain"))?;
    decode_stored_secret(&stored)
}

fn decode_stored_secret(stored: &[u8]) -> anyhow::Result<Vec<u8>> {
    let encoded = std::str::from_utf8(stored)
        .context("decoding Terminal Harbor pairing key")?
        .trim();
    if encoded.is_empty() {
        anyhow::bail!("Terminal Harbor pairing key is empty");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("decoding Terminal Harbor pairing key")?;
    if decoded.len() != 32 {
        anyhow::bail!("Terminal Harbor pairing key has an invalid length");
    }
    Ok(decoded)
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
    let state = app.state::<HarborControlState>();
    let _pairing_guard = state.pairing.lock().await;
    let uri = reqwest::Url::parse(raw_uri.trim()).context("invalid Terminal Harbor pair URI")?;
    if uri.scheme() != "harbor" || uri.host_str() != Some("pair") {
        anyhow::bail!("not a Terminal Harbor pair URI");
    }
    let query: std::collections::HashMap<String, String> = uri.query_pairs().into_owned().collect();
    if query.get("auth").map(String::as_str) != Some(AUTH_VERSION) {
        anyhow::bail!("pair URI does not use HMAC authentication");
    }
    let token = query
        .get("token")
        .context("pair URI is missing its token")?;
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
            Err(err) => last_error = Some(err.into()),
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

async fn send_voice_intent(
    app: &AppHandle,
    text: &str,
) -> Result<VoiceResponse, HarborClientError> {
    let state = app.state::<HarborControlState>();
    let _pairing_guard = state.pairing.lock().await;
    let current = settings::get_settings(app);
    let server_id =
        current
            .harbor_server_id
            .ok_or_else(|| HarborClientError::AuthenticationFailed {
                status: None,
                detail: Some("pairing credentials are missing".into()),
            })?;
    let client_id =
        current
            .harbor_client_id
            .ok_or_else(|| HarborClientError::AuthenticationFailed {
                status: None,
                detail: Some("pairing credentials are missing".into()),
            })?;
    let base_url = current
        .harbor_base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let key = load_secret(&server_id).map_err(|_| HarborClientError::AuthenticationFailed {
        status: None,
        detail: Some("pairing key is missing or invalid".into()),
    })?;
    let body = serde_json::to_vec(&serde_json::json!({"text": text}))
        .map_err(|err| HarborClientError::Protocol(err.to_string()))?;
    let (status, response_body) =
        signed_post(&base_url, VOICE_PATH, body, &key, &key, Some(&client_id)).await?;
    if status != 200 {
        return Err(HarborClientError::Protocol(format!(
            "voice endpoint returned HTTP {status}: {}",
            safe_error_detail(&response_body).unwrap_or_else(|| "unknown error".into())
        )));
    }
    serde_json::from_slice(&response_body).map_err(|err| {
        HarborClientError::Protocol(format!("parsing Terminal Harbor voice response: {err}"))
    })
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
    fn stored_pairing_key_requires_valid_nonempty_32_byte_secret() {
        let key = [7u8; 32];
        let encoded = URL_SAFE_NO_PAD.encode(key);
        assert_eq!(decode_stored_secret(encoded.as_bytes()).unwrap(), key);
        assert!(decode_stored_secret(b"").is_err());
        assert!(decode_stored_secret(b"not-base64!").is_err());
        assert!(decode_stored_secret(URL_SAFE_NO_PAD.encode([1u8; 16]).as_bytes()).is_err());
    }

    #[test]
    fn signed_response_validation_distinguishes_auth_and_integrity_failures() {
        let key = [3u8; 32];
        let nonce = "request-nonce";
        let body = br#"{"ok":true}"#;
        let canonical = format!("TH-HMAC-V1-RESPONSE\n{nonce}\n200\n{}", sha256_hex(body));
        let signature = hmac_value(&key, canonical.as_bytes());
        assert!(validate_signed_response(&key, nonce, 200, body, Some(&signature)).is_ok());

        let unauthorized =
            validate_signed_response(&key, nonce, 401, br#"{"error":"unauthorized"}"#, None)
                .unwrap_err();
        assert!(matches!(
            unauthorized,
            HarborClientError::AuthenticationFailed {
                status: Some(401),
                detail: Some(ref detail),
            } if detail == "unauthorized"
        ));

        assert!(matches!(
            validate_signed_response(&key, nonce, 500, b"failed", None).unwrap_err(),
            HarborClientError::UntrustedResponse {
                status: Some(500),
                ..
            }
        ));
        assert!(matches!(
            validate_signed_response(&key, nonce, 200, body, Some("invalid")).unwrap_err(),
            HarborClientError::UntrustedResponse {
                status: Some(200),
                ..
            }
        ));
    }

    #[test]
    fn auth_recovery_budget_can_only_be_claimed_once() {
        let error = HarborClientError::AuthenticationFailed {
            status: Some(401),
            detail: Some("unauthorized".into()),
        };
        let mut used = false;
        assert!(claim_auth_recovery(&error, &mut used));
        assert!(!claim_auth_recovery(&error, &mut used));
        assert!(!claim_auth_recovery(
            &HarborClientError::Transport("offline".into()),
            &mut false
        ));
    }

    #[test]
    fn error_detail_is_bounded_and_control_characters_are_removed() {
        let detail = safe_error_detail(&vec![b'x'; 200]).unwrap();
        assert_eq!(detail.chars().count(), 160);
        assert_eq!(
            safe_error_detail(b"bad\nresponse").as_deref(),
            Some("bad response")
        );
    }

    #[test]
    fn outcome_status_messages_cover_phase_one() {
        assert_eq!(status_for_outcome("executed"), "切替成功");
        assert_eq!(
            status_for_outcome("model_unavailable"),
            "OpenRouter 応答なし / キー不可"
        );
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
