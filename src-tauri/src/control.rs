use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, WebviewUrl,
    WebviewWindowBuilder,
};

use crate::settings::{self, ApiKeySource, GROQ_PROVIDER_ID};

pub const CONTROL_WINDOW_LABEL: &str = "control";
const CONTROL_WINDOW_WIDTH: f64 = 230.0;
const CONTROL_WINDOW_HEIGHT: f64 = 280.0;
const CONTROL_WINDOW_RIGHT_OFFSET: f64 = 24.0;
const CONTROL_WINDOW_TOP_OFFSET: f64 = 24.0;

#[derive(Default)]
pub struct ControlModeState {
    inner: Mutex<ControlRuntimeState>,
    suppress_next_reopen: Mutex<bool>,
    last_pasted_text: Mutex<Option<String>>,
    /// Name of the app that was frontmost when control mode was activated.
    /// Used to restore focus before sending undo.
    prev_frontmost_app: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Default)]
struct ControlRuntimeState {
    active: bool,
    session_id: u64,
    messages: Vec<ControlTurn>,
    is_sending: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ControlStateSnapshot {
    pub active: bool,
    pub session_id: u64,
    pub messages: Vec<ControlTurn>,
    pub is_sending: bool,
    pub last_error: Option<String>,
    pub api_key_source: ApiKeySource,
    pub has_last_pasted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ControlTurn {
    pub role: String,
    pub content: String,
}

pub fn initialize(app_handle: &AppHandle) {
    app_handle.manage(ControlModeState::default());
}

pub fn consume_reopen_suppression(app_handle: &AppHandle) -> bool {
    let state = app_handle.state::<ControlModeState>();
    let mut suppress = state.suppress_next_reopen.lock().unwrap();
    let should_suppress = *suppress;
    *suppress = false;
    should_suppress
}

pub fn get_mode_snapshot(app_handle: &AppHandle) -> ControlStateSnapshot {
    let inner = app_handle
        .state::<ControlModeState>()
        .inner
        .lock()
        .unwrap()
        .clone();
    build_snapshot(app_handle, &inner)
}

pub fn toggle_mode(app_handle: &AppHandle) -> Result<ControlStateSnapshot, String> {
    let next_active = !get_mode_snapshot(app_handle).active;
    set_mode(app_handle, next_active)
}

pub fn deactivate_mode(app_handle: &AppHandle) -> Result<ControlStateSnapshot, String> {
    set_mode(app_handle, false)
}

pub async fn submit_voice_prompt(
    app_handle: &AppHandle,
    prompt: String,
) -> Result<ControlStateSnapshot, String> {
    log::info!("Submitting voice prompt to control backend");
    submit_prompt(app_handle, prompt).await
}

pub fn set_last_pasted_text(app: &AppHandle, text: String) {
    let state = app.state::<ControlModeState>();
    *state.last_pasted_text.lock().unwrap() = Some(text);
}

pub fn clear_last_pasted_text(app: &AppHandle) {
    let state = app.state::<ControlModeState>();
    *state.last_pasted_text.lock().unwrap() = None;
}

fn get_last_pasted_text(app: &AppHandle) -> Option<String> {
    app.state::<ControlModeState>()
        .last_pasted_text
        .lock()
        .unwrap()
        .clone()
}

fn set_mode(app_handle: &AppHandle, active: bool) -> Result<ControlStateSnapshot, String> {
    // Capture frontmost app BEFORE we take focus (macOS only)
    #[cfg(target_os = "macos")]
    if active {
        if let Some(prev_app) = get_frontmost_app_name() {
            *app_handle
                .state::<ControlModeState>()
                .prev_frontmost_app
                .lock()
                .unwrap() = Some(prev_app);
        }
    }

    let runtime_state = {
        let state = app_handle.state::<ControlModeState>();
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
        if let Some(window) = app_handle.get_webview_window(CONTROL_WINDOW_LABEL) {
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
                    .state::<ControlModeState>()
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

fn build_control_system_prompt(last_pasted: Option<&str>) -> String {
    let mut prompt = String::from(
        "You are a voice control assistant for a desktop app called Handy.\n\
         You can either perform control actions OR respond conversationally.\n\n\
         AVAILABLE ACTIONS:\n\
         - Undo the last pasted text: respond ONLY with the JSON: {\"action\":\"undo_last_input\"}\n\
         - Press the Enter key: respond ONLY with the JSON: {\"action\":\"send_enter_key\"}\n\n\
         Use action JSON ONLY when the user clearly wants that specific action.\n\
         For all other requests, respond normally as a helpful assistant.",
    );
    if let Some(text) = last_pasted {
        let preview = if text.len() > 100 { &text[..100] } else { text };
        prompt.push_str(&format!("\n\nThe user's last pasted text was: \"{}\"", preview));
    }
    prompt
}

async fn submit_prompt(
    app_handle: &AppHandle,
    prompt: String,
) -> Result<ControlStateSnapshot, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Control prompt is empty".to_string());
    }

    let (messages_for_request, provider, api_key, model, optimistic_snapshot) = {
        let settings = settings::get_settings(app_handle);
        let provider = settings
            .post_process_provider(GROQ_PROVIDER_ID)
            .cloned()
            .ok_or_else(|| "Groq provider is not configured".to_string())?;

        let resolved_api_key =
            settings::resolve_post_process_api_key(&settings, GROQ_PROVIDER_ID);
        if resolved_api_key.value.trim().is_empty() {
            return Err("Groq API key is not configured".to_string());
        }

        let model = settings
            .post_process_models
            .get(GROQ_PROVIDER_ID)
            .cloned()
            .unwrap_or_default();
        if model.trim().is_empty() {
            return Err("Groq model is not configured".to_string());
        }

        let state = app_handle.state::<ControlModeState>();
        let mut inner = state.inner.lock().unwrap();
        if !inner.active {
            return Err("Control mode is not active".to_string());
        }

        log::info!(
            "Control prompt accepted (source=voice_or_ui, session_id={}, chars={})",
            inner.session_id,
            prompt.chars().count()
        );

        inner.messages.push(ControlTurn {
            role: "user".to_string(),
            content: prompt.clone(),
        });
        inner.is_sending = true;
        inner.last_error = None;

        // Build message list with system prompt prepended
        let last_pasted = get_last_pasted_text(app_handle);
        let system_prompt = build_control_system_prompt(last_pasted.as_deref());
        let mut messages_for_request: Vec<(String, String)> =
            vec![("system".to_string(), system_prompt)];
        for turn in &inner.messages {
            messages_for_request.push((turn.role.clone(), turn.content.clone()));
        }

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

    log::info!("Sending control request to Groq (model={model})");
    match crate::llm_client::send_chat_messages(&provider, api_key, &model, messages_for_request)
        .await
    {
        Ok(Some(reply)) => {
            // Check if the reply is an action JSON.
            // Strip markdown code fences (```json ... ``` or ``` ... ```) before parsing.
            let reply_trimmed = reply.trim();
            let json_candidate = strip_markdown_code_fence(reply_trimmed);
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(json_candidate) {
                if json_val.get("action").and_then(|a| a.as_str()) == Some("send_enter_key") {
                    let enter_message = execute_enter_key_action(app_handle).await;

                    let snapshot = {
                        let state = app_handle.state::<ControlModeState>();
                        let mut inner = state.inner.lock().unwrap();
                        inner.messages.push(ControlTurn {
                            role: "assistant".to_string(),
                            content: enter_message,
                        });
                        inner.is_sending = false;
                        inner.last_error = None;
                        build_snapshot(app_handle, &inner)
                    };
                    log::info!("Executed send_enter_key action from control mode");
                    emit_state_changed(app_handle, &snapshot);
                    schedule_auto_exit(app_handle, snapshot.session_id);
                    return Ok(snapshot);
                }

                if json_val.get("action").and_then(|a| a.as_str()) == Some("undo_last_input") {
                    let undo_message = execute_undo_last_input(app_handle).await;
                    clear_last_pasted_text(app_handle);

                    let snapshot = {
                        let state = app_handle.state::<ControlModeState>();
                        let mut inner = state.inner.lock().unwrap();
                        inner.messages.push(ControlTurn {
                            role: "assistant".to_string(),
                            content: undo_message,
                        });
                        inner.is_sending = false;
                        inner.last_error = None;
                        build_snapshot(app_handle, &inner)
                    };
                    log::info!("Executed undo_last_input action from control mode");
                    emit_state_changed(app_handle, &snapshot);
                    schedule_auto_exit(app_handle, snapshot.session_id);
                    return Ok(snapshot);
                }
            }

            // Normal text response
            let snapshot = {
                let state = app_handle.state::<ControlModeState>();
                let mut inner = state.inner.lock().unwrap();
                inner.messages.push(ControlTurn {
                    role: "assistant".to_string(),
                    content: reply,
                });
                inner.is_sending = false;
                inner.last_error = None;
                build_snapshot(app_handle, &inner)
            };
            log::info!(
                "Received Groq response for control mode (chars={})",
                snapshot
                    .messages
                    .last()
                    .map(|message| message.content.chars().count())
                    .unwrap_or(0)
            );
            emit_state_changed(app_handle, &snapshot);
            schedule_auto_exit(app_handle, snapshot.session_id);
            Ok(snapshot)
        }
        Ok(None) => {
            let err = "Groq returned an empty response".to_string();
            set_error_state(app_handle, &err)
        }
        Err(err) => set_error_state(app_handle, &err),
    }
}

/// Terminal emulators that use Ctrl+U to clear the current input line.
const TERMINAL_APP_NAMES: &[&str] = &[
    "Terminal", // macOS Terminal.app
    "iTerm2",
    "kitty",
    "Alacritty",
    "WezTerm",
    "Warp",
    "Hyper",
    "Ghostty",
];

/// Strip markdown code fences so `{"action":...}` can be parsed even when the
/// LLM wraps it in ```json ... ``` or ``` ... ```.
fn strip_markdown_code_fence(s: &str) -> &str {
    let s = s.trim();
    // Match an opening fence like ```json or just ```
    let after_open = if let Some(rest) = s.strip_prefix("```") {
        // skip optional language tag up to first newline
        if let Some(pos) = rest.find('\n') {
            rest[pos + 1..].trim_start()
        } else {
            return s;
        }
    } else {
        return s;
    };
    // Strip trailing ```
    if let Some(body) = after_open.strip_suffix("```") {
        body.trim()
    } else {
        after_open.trim()
    }
}

fn is_terminal_app(name: &str) -> bool {
    TERMINAL_APP_NAMES
        .iter()
        .any(|t| t.eq_ignore_ascii_case(name))
}

/// Returns the name of the currently frontmost application via osascript (macOS only).
#[cfg(target_os = "macos")]
fn get_frontmost_app_name() -> Option<String> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to name of (first application process whose frontmost is true)",
        ])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

async fn execute_undo_last_input(app: &AppHandle) -> String {
    // Retrieve the app that was frontmost when control mode was toggled on
    let prev_app = app
        .state::<ControlModeState>()
        .prev_frontmost_app
        .lock()
        .unwrap()
        .clone();

    // Hide the control window
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        let _ = window.hide();
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(ref app_name) = prev_app {
            let (keystroke, message) = if is_terminal_app(app_name) {
                // Ctrl+U: readline kill-line, clears current shell prompt
                (
                    r#"keystroke "u" using control down"#,
                    "[ターミナル行をクリアしました]",
                )
            } else {
                // Cmd+Z: standard undo
                (
                    r#"keystroke "z" using command down"#,
                    "[直前の入力を取り消しました]",
                )
            };

            log::info!(
                "Restoring focus to '{}' and sending {}",
                app_name,
                if is_terminal_app(app_name) {
                    "Ctrl+U"
                } else {
                    "Cmd+Z"
                }
            );

            let script = format!(
                "tell application \"{}\" to activate\ndelay 0.3\ntell application \"System Events\" to {}",
                app_name, keystroke
            );
            tauri::async_runtime::spawn_blocking(move || {
                std::process::Command::new("osascript")
                    .args(["-e", &script])
                    .output()
                    .ok();
            })
            .await
            .ok();

            // Re-show the control window
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
                let _ = window.show();
                let _ = window.set_focus();
            }

            return message.to_string();
        } else {
            log::warn!("No previous app recorded; Cmd+Z may not reach the right app");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            send_undo_via_enigo(app);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        send_undo_via_enigo(app);
    }

    // Re-show the control window
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }

    "[直前の入力を取り消しました]".to_string()
}

async fn execute_enter_key_action(app: &AppHandle) -> String {
    let prev_app = app
        .state::<ControlModeState>()
        .prev_frontmost_app
        .lock()
        .unwrap()
        .clone();

    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        let _ = window.hide();
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(ref app_name) = prev_app {
            log::info!("Restoring focus to '{}' and sending Return key", app_name);
            let script = format!(
                "tell application \"{}\" to activate\ndelay 0.3\n\
                 tell application \"System Events\" to key code 36",
                app_name
            );
            tauri::async_runtime::spawn_blocking(move || {
                std::process::Command::new("osascript")
                    .args(["-e", &script])
                    .output()
                    .ok();
            })
            .await
            .ok();
        } else {
            log::warn!("No previous app recorded; sending Return via enigo");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            send_enter_via_enigo(app);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        send_enter_via_enigo(app);
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    if let Some(window) = app.get_webview_window(CONTROL_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }

    "[Enterキーを送信しました]".to_string()
}

fn send_enter_via_enigo(app: &AppHandle) {
    let app_clone = app.clone();
    app.run_on_main_thread(move || {
        use crate::input::EnigoState;
        use enigo::{Direction, Key, Keyboard};
        if let Some(enigo_state) = app_clone.try_state::<EnigoState>() {
            let mut enigo = enigo_state.0.lock().unwrap();
            let _ = enigo.key(Key::Return, Direction::Click);
        }
    })
    .ok();
}

fn send_undo_via_enigo(app: &AppHandle) {
    // macOS virtual key codes: Z = 0x06 (kVK_ANSI_Z)
    let app_clone = app.clone();
    app.run_on_main_thread(move || {
        use crate::input::EnigoState;
        use enigo::{Direction, Key, Keyboard};
        if let Some(enigo_state) = app_clone.try_state::<EnigoState>() {
            let mut enigo = enigo_state.0.lock().unwrap();
            #[cfg(target_os = "macos")]
            let (modifier_key, z_key) = (Key::Meta, Key::Other(6));
            #[cfg(target_os = "windows")]
            let (modifier_key, z_key) = (Key::Control, Key::Other(0x5A));
            #[cfg(target_os = "linux")]
            let (modifier_key, z_key) = (Key::Control, Key::Unicode('z'));
            let _ = enigo.key(modifier_key, Direction::Press);
            let _ = enigo.key(z_key, Direction::Click);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = enigo.key(modifier_key, Direction::Release);
        }
    })
    .ok();
}

/// After displaying the response, wait 2 seconds then automatically exit control mode.
/// Guards against firing if the user already exited manually (session_id check).
fn schedule_auto_exit(app_handle: &AppHandle, session_id: u64) {
    let app = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // Only deactivate if the session hasn't changed (user hasn't already exited)
        let current_session = app
            .state::<ControlModeState>()
            .inner
            .lock()
            .unwrap()
            .session_id;
        if current_session == session_id {
            log::info!("Auto-exiting control mode after response display (session {session_id})");
            let _ = deactivate_mode(&app);
            crate::overlay::show_normal_input_overlay(&app);
        }
    });
}

fn set_error_state(app_handle: &AppHandle, err: &str) -> Result<ControlStateSnapshot, String> {
    let snapshot = {
        let state = app_handle.state::<ControlModeState>();
        let mut inner = state.inner.lock().unwrap();
        inner.is_sending = false;
        inner.last_error = Some(err.to_string());
        build_snapshot(app_handle, &inner)
    };
    emit_state_changed(app_handle, &snapshot);
    Err(err.to_string())
}

fn build_snapshot(app_handle: &AppHandle, inner: &ControlRuntimeState) -> ControlStateSnapshot {
    let settings = settings::get_settings(app_handle);
    let api_key_source =
        settings::resolve_post_process_api_key(&settings, GROQ_PROVIDER_ID).source;
    let has_last_pasted = app_handle
        .state::<ControlModeState>()
        .last_pasted_text
        .lock()
        .unwrap()
        .is_some();

    ControlStateSnapshot {
        active: inner.active,
        session_id: inner.session_id,
        messages: inner.messages.clone(),
        is_sending: inner.is_sending,
        last_error: inner.last_error.clone(),
        api_key_source,
        has_last_pasted,
    }
}

fn ensure_window(app_handle: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app_handle.get_webview_window(CONTROL_WINDOW_LABEL) {
        return Ok(window);
    }

    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        CONTROL_WINDOW_LABEL,
        WebviewUrl::App("/".into()),
    )
    .title("Control Mode")
    .inner_size(CONTROL_WINDOW_WIDTH, CONTROL_WINDOW_HEIGHT)
    .min_inner_size(190.0, 210.0)
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
    let _work_area_height = work_area.size.height as f64 / scale;
    let work_area_x = work_area.position.x as f64 / scale;
    let work_area_y = work_area.position.y as f64 / scale;

    let x = work_area_x + work_area_width - CONTROL_WINDOW_WIDTH - CONTROL_WINDOW_RIGHT_OFFSET;
    let y = work_area_y + CONTROL_WINDOW_TOP_OFFSET;

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

fn emit_state_changed(app_handle: &AppHandle, snapshot: &ControlStateSnapshot) {
    let _ = app_handle.emit("control-mode-changed", snapshot);

    if let Some(window) = app_handle.get_webview_window(CONTROL_WINDOW_LABEL) {
        let _ = window.emit("control-mode-changed", snapshot);
    }

    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.emit("control-mode-changed", snapshot);
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_control_mode(app: AppHandle) -> Result<ControlStateSnapshot, String> {
    Ok(get_mode_snapshot(&app))
}

#[tauri::command]
#[specta::specta]
pub async fn send_control_message(
    app: AppHandle,
    prompt: String,
) -> Result<ControlStateSnapshot, String> {
    submit_prompt(&app, prompt).await
}
