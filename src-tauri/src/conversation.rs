use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, WebviewUrl,
    WebviewWindowBuilder,
};

use crate::settings::{self, ApiKeySource, OPENROUTER_PROVIDER_ID};

pub const CONVERSATION_WINDOW_LABEL: &str = "conversation";
const CONVERSATION_WINDOW_WIDTH: f64 = 460.0;
const CONVERSATION_WINDOW_HEIGHT: f64 = 560.0;
const CONVERSATION_WINDOW_RIGHT_OFFSET: f64 = 24.0;
const CONVERSATION_WINDOW_BOTTOM_OFFSET: f64 = 24.0;

#[derive(Default)]
pub struct ConversationModeState {
    inner: Mutex<ConversationRuntimeState>,
    suppress_next_reopen: Mutex<bool>,
}

#[derive(Debug, Clone, Default)]
struct ConversationRuntimeState {
    active: bool,
    session_id: u64,
    messages: Vec<ConversationTurn>,
    is_sending: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ConversationStateSnapshot {
    pub active: bool,
    pub session_id: u64,
    pub messages: Vec<ConversationTurn>,
    pub is_sending: bool,
    pub last_error: Option<String>,
    pub api_key_source: ApiKeySource,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

pub fn initialize(app_handle: &AppHandle) {
    app_handle.manage(ConversationModeState::default());
}

pub fn consume_reopen_suppression(app_handle: &AppHandle) -> bool {
    let state = app_handle.state::<ConversationModeState>();
    let mut suppress = state.suppress_next_reopen.lock().unwrap();
    let should_suppress = *suppress;
    *suppress = false;
    should_suppress
}

pub fn get_mode_snapshot(app_handle: &AppHandle) -> ConversationStateSnapshot {
    let inner = app_handle
        .state::<ConversationModeState>()
        .inner
        .lock()
        .unwrap()
        .clone();
    build_snapshot(app_handle, &inner)
}

pub fn toggle_mode(app_handle: &AppHandle) -> Result<ConversationStateSnapshot, String> {
    let next_active = !get_mode_snapshot(app_handle).active;
    set_mode(app_handle, next_active)
}

pub fn deactivate_mode(app_handle: &AppHandle) -> Result<ConversationStateSnapshot, String> {
    set_mode(app_handle, false)
}

pub async fn submit_voice_prompt(
    app_handle: &AppHandle,
    prompt: String,
) -> Result<ConversationStateSnapshot, String> {
    log::info!("Submitting voice prompt to conversation backend");
    submit_prompt(app_handle, prompt).await
}

fn set_mode(app_handle: &AppHandle, active: bool) -> Result<ConversationStateSnapshot, String> {
    let runtime_state = {
        let state = app_handle.state::<ConversationModeState>();
        let mut inner = state.inner.lock().unwrap();

        if inner.active != active {
            inner.active = active;
            inner.is_sending = false;
            inner.last_error = None;
            inner.messages.clear();
            if active {
                inner.session_id = inner.session_id.saturating_add(1);
            }
        }

        inner.clone()
    };

    let snapshot = build_snapshot(app_handle, &runtime_state);

    if snapshot.active {
        let window = ensure_window(app_handle)?;
        update_window_position(app_handle, &window);

        if let Some(main_window) = app_handle.get_webview_window("main") {
            let _ = main_window.hide();
        }

        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;

        #[cfg(target_os = "macos")]
        app_handle
            .set_activation_policy(tauri::ActivationPolicy::Regular)
            .map_err(|e| e.to_string())?;
    } else {
        if let Some(window) = app_handle.get_webview_window(CONVERSATION_WINDOW_LABEL) {
            let _ = window.hide();
        }

        if let Some(main_window) = app_handle.get_webview_window("main") {
            let _ = main_window.hide();
        }

        #[cfg(target_os = "macos")]
        {
            let settings = settings::get_settings(app_handle);
            let tray_available =
                settings.show_tray_icon && !app_handle.state::<crate::CliArgs>().no_tray;
            let main_window_visible = app_handle
                .get_webview_window("main")
                .and_then(|window| window.is_visible().ok())
                .unwrap_or(false);

            if tray_available && !main_window_visible {
                *app_handle
                    .state::<ConversationModeState>()
                    .suppress_next_reopen
                    .lock()
                    .unwrap() = true;
                app_handle
                    .set_activation_policy(tauri::ActivationPolicy::Accessory)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    emit_state_changed(app_handle, &snapshot);

    Ok(snapshot)
}

async fn submit_prompt(
    app_handle: &AppHandle,
    prompt: String,
) -> Result<ConversationStateSnapshot, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Conversation prompt is empty".to_string());
    }

    let (messages_for_request, provider, api_key, model, optimistic_snapshot) = {
        let settings = settings::get_settings(app_handle);
        let provider = settings
            .post_process_provider(OPENROUTER_PROVIDER_ID)
            .cloned()
            .ok_or_else(|| "OpenRouter provider is not configured".to_string())?;

        let resolved_api_key =
            settings::resolve_post_process_api_key(&settings, OPENROUTER_PROVIDER_ID);
        if resolved_api_key.value.trim().is_empty() {
            return Err("OpenRouter API key is not configured".to_string());
        }

        let model = settings
            .post_process_models
            .get(OPENROUTER_PROVIDER_ID)
            .cloned()
            .unwrap_or_default();
        if model.trim().is_empty() {
            return Err("OpenRouter model is not configured".to_string());
        }

        let state = app_handle.state::<ConversationModeState>();
        let mut inner = state.inner.lock().unwrap();
        if !inner.active {
            return Err("Conversation mode is not active".to_string());
        }

        log::info!(
            "Conversation prompt accepted (source=voice_or_ui, session_id={}, chars={})",
            inner.session_id,
            prompt.chars().count()
        );

        inner.messages.push(ConversationTurn {
            role: "user".to_string(),
            content: prompt.clone(),
        });
        inner.is_sending = true;
        inner.last_error = None;

        let messages_for_request = inner
            .messages
            .iter()
            .map(|turn| (turn.role.clone(), turn.content.clone()))
            .collect::<Vec<_>>();
        let optimistic_snapshot = build_snapshot(app_handle, &inner);

        (
            messages_for_request,
            provider,
            resolved_api_key.value,
            model,
            optimistic_snapshot,
        )
    };

    emit_state_changed(app_handle, &optimistic_snapshot);

    log::info!("Sending conversation request to OpenRouter (model={model})");
    match crate::llm_client::send_chat_messages(&provider, api_key, &model, messages_for_request)
        .await
    {
        Ok(Some(reply)) => {
            let snapshot = {
                let state = app_handle.state::<ConversationModeState>();
                let mut inner = state.inner.lock().unwrap();
                inner.messages.push(ConversationTurn {
                    role: "assistant".to_string(),
                    content: reply,
                });
                inner.is_sending = false;
                inner.last_error = None;
                build_snapshot(app_handle, &inner)
            };
            log::info!(
                "Received OpenRouter response for conversation (chars={})",
                snapshot
                    .messages
                    .last()
                    .map(|message| message.content.chars().count())
                    .unwrap_or(0)
            );
            emit_state_changed(app_handle, &snapshot);
            Ok(snapshot)
        }
        Ok(None) => {
            let err = "OpenRouter returned an empty response".to_string();
            set_error_state(app_handle, &err)
        }
        Err(err) => set_error_state(app_handle, &err),
    }
}

fn set_error_state(app_handle: &AppHandle, err: &str) -> Result<ConversationStateSnapshot, String> {
    let snapshot = {
        let state = app_handle.state::<ConversationModeState>();
        let mut inner = state.inner.lock().unwrap();
        inner.is_sending = false;
        inner.last_error = Some(err.to_string());
        build_snapshot(app_handle, &inner)
    };
    emit_state_changed(app_handle, &snapshot);
    Err(err.to_string())
}

fn build_snapshot(
    app_handle: &AppHandle,
    inner: &ConversationRuntimeState,
) -> ConversationStateSnapshot {
    let settings = settings::get_settings(app_handle);
    let api_key_source =
        settings::resolve_post_process_api_key(&settings, OPENROUTER_PROVIDER_ID).source;

    ConversationStateSnapshot {
        active: inner.active,
        session_id: inner.session_id,
        messages: inner.messages.clone(),
        is_sending: inner.is_sending,
        last_error: inner.last_error.clone(),
        api_key_source,
    }
}

fn ensure_window(app_handle: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app_handle.get_webview_window(CONVERSATION_WINDOW_LABEL) {
        return Ok(window);
    }

    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        CONVERSATION_WINDOW_LABEL,
        WebviewUrl::App("/".into()),
    )
    .title("OpenRouter Conversation")
    .inner_size(CONVERSATION_WINDOW_WIDTH, CONVERSATION_WINDOW_HEIGHT)
    .min_inner_size(380.0, 420.0)
    .resizable(true)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    builder.build().map_err(|e| e.to_string())
}

fn update_window_position(app_handle: &AppHandle, window: &tauri::WebviewWindow) {
    if let Some((x, y)) = calculate_window_position(app_handle) {
        let _ = window.set_position(Position::Logical(tauri::LogicalPosition { x, y }));
    }
}

fn calculate_window_position(app_handle: &AppHandle) -> Option<(f64, f64)> {
    let monitor = get_monitor_with_cursor(app_handle)?;
    let work_area = monitor.work_area();
    let scale = monitor.scale_factor();
    let work_area_width = work_area.size.width as f64 / scale;
    let work_area_height = work_area.size.height as f64 / scale;
    let work_area_x = work_area.position.x as f64 / scale;
    let work_area_y = work_area.position.y as f64 / scale;

    let x =
        work_area_x + work_area_width - CONVERSATION_WINDOW_WIDTH - CONVERSATION_WINDOW_RIGHT_OFFSET;
    let y = work_area_y
        + work_area_height
        - CONVERSATION_WINDOW_HEIGHT
        - CONVERSATION_WINDOW_BOTTOM_OFFSET;

    Some((x.max(work_area_x), y.max(work_area_y)))
}

fn get_monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some(mouse_location) = crate::input::get_cursor_position(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                if is_mouse_within_monitor(mouse_location, monitor.position(), monitor.size()) {
                    return Some(monitor);
                }
            }
        }
    }

    app_handle.primary_monitor().ok().flatten()
}

fn is_mouse_within_monitor(
    mouse_pos: (i32, i32),
    monitor_pos: &PhysicalPosition<i32>,
    monitor_size: &PhysicalSize<u32>,
) -> bool {
    let (mouse_x, mouse_y) = mouse_pos;
    let PhysicalPosition {
        x: monitor_x,
        y: monitor_y,
    } = *monitor_pos;
    let PhysicalSize {
        width: monitor_width,
        height: monitor_height,
    } = *monitor_size;

    mouse_x >= monitor_x
        && mouse_x < (monitor_x + monitor_width as i32)
        && mouse_y >= monitor_y
        && mouse_y < (monitor_y + monitor_height as i32)
}

fn emit_state_changed(app_handle: &AppHandle, snapshot: &ConversationStateSnapshot) {
    let _ = app_handle.emit("conversation-mode-changed", snapshot);

    if let Some(window) = app_handle.get_webview_window(CONVERSATION_WINDOW_LABEL) {
        let _ = window.emit("conversation-mode-changed", snapshot);
    }

    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.emit("conversation-mode-changed", snapshot);
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_conversation_mode(app: AppHandle) -> Result<ConversationStateSnapshot, String> {
    Ok(get_mode_snapshot(&app))
}

#[tauri::command]
#[specta::specta]
pub async fn send_conversation_message(
    app: AppHandle,
    prompt: String,
) -> Result<ConversationStateSnapshot, String> {
    submit_prompt(&app, prompt).await
}
