use crate::audio_feedback;
use crate::audio_toolkit::audio::{list_input_devices, list_output_devices};
use crate::ble::{BleManager, BleStatus};
use crate::managers::audio::{AudioRecordingManager, MicrophoneMode};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings, AudioSource};
use log::warn;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Serialize, Type)]
pub struct CustomSounds {
    start: bool,
    stop: bool,
}

fn custom_sound_exists(app: &AppHandle, sound_type: &str) -> bool {
    crate::portable::resolve_app_data(app, &format!("custom_{}.wav", sound_type))
        .map_or(false, |path| path.exists())
}

#[tauri::command]
#[specta::specta]
pub fn check_custom_sounds(app: AppHandle) -> CustomSounds {
    CustomSounds {
        start: custom_sound_exists(&app, "start"),
        stop: custom_sound_exists(&app, "stop"),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct AudioDevice {
    pub index: String,
    pub name: String,
    pub is_default: bool,
}

#[tauri::command]
#[specta::specta]
pub fn update_microphone_mode(app: AppHandle, always_on: bool) -> Result<(), String> {
    // Update settings
    let mut settings = get_settings(&app);
    settings.always_on_microphone = always_on;
    write_settings(&app, settings);

    // Update the audio manager mode
    let rm = app.state::<Arc<AudioRecordingManager>>();
    let new_mode = if always_on {
        MicrophoneMode::AlwaysOn
    } else {
        MicrophoneMode::OnDemand
    };

    rm.update_mode(new_mode)
        .map_err(|e| format!("Failed to update microphone mode: {}", e))
}

#[tauri::command]
#[specta::specta]
pub fn get_microphone_mode(app: AppHandle) -> Result<bool, String> {
    let settings = get_settings(&app);
    Ok(settings.always_on_microphone)
}

#[tauri::command]
#[specta::specta]
pub fn get_available_microphones() -> Result<Vec<AudioDevice>, String> {
    let devices =
        list_input_devices().map_err(|e| format!("Failed to list audio devices: {}", e))?;

    let mut result = vec![AudioDevice {
        index: "default".to_string(),
        name: "Default".to_string(),
        is_default: true,
    }];

    result.extend(devices.into_iter().map(|d| AudioDevice {
        index: d.index,
        name: d.name,
        is_default: false, // The explicit default is handled separately
    }));

    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn set_selected_microphone(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.selected_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);

    // Update the audio manager to use the new device
    let rm = app.state::<Arc<AudioRecordingManager>>();
    rm.update_selected_device()
        .map_err(|e| format!("Failed to update selected device: {}", e))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
    Ok(settings
        .selected_microphone
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn get_available_output_devices() -> Result<Vec<AudioDevice>, String> {
    let devices =
        list_output_devices().map_err(|e| format!("Failed to list output devices: {}", e))?;

    let mut result = vec![AudioDevice {
        index: "default".to_string(),
        name: "Default".to_string(),
        is_default: true,
    }];

    result.extend(devices.into_iter().map(|d| AudioDevice {
        index: d.index,
        name: d.name,
        is_default: false, // The explicit default is handled separately
    }));

    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn set_selected_output_device(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.selected_output_device = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_output_device(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
    Ok(settings
        .selected_output_device
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub async fn play_test_sound(app: AppHandle, sound_type: String) {
    let sound = match sound_type.as_str() {
        "start" => audio_feedback::SoundType::Start,
        "stop" => audio_feedback::SoundType::Stop,
        _ => {
            warn!("Unknown sound type: {}", sound_type);
            return;
        }
    };
    audio_feedback::play_test_sound(&app, sound);
}

#[tauri::command]
#[specta::specta]
pub fn set_clamshell_microphone(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.clamshell_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_clamshell_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
    Ok(settings
        .clamshell_microphone
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn is_recording(app: AppHandle) -> bool {
    let audio_manager = app.state::<Arc<AudioRecordingManager>>();
    audio_manager.is_recording()
}

// ──────────────────────────────────────────────────────── BLE commands ──

/// Return the current BLE connection status.
#[tauri::command]
#[specta::specta]
pub fn ble_get_status(app: AppHandle) -> BleStatus {
    app.state::<Arc<BleManager>>().status()
}

/// Scan for nearby AtomEchoS3R devices.
/// Returns a list of `"name (address)"` display strings.
#[tauri::command]
#[specta::specta]
pub async fn ble_scan_devices(app: AppHandle, duration_secs: u64) -> Result<Vec<String>, String> {
    app.state::<Arc<BleManager>>()
        .scan_devices(duration_secs)
        .await
        .map_err(|e| e.to_string())
}

/// Connect to the first AtomEchoS3R found within `scan_secs` seconds.
#[tauri::command]
#[specta::specta]
pub async fn ble_connect_first(app: AppHandle, scan_secs: u64) -> Result<BleStatus, String> {
    let ble = app.state::<Arc<BleManager>>();
    ble.connect_first(scan_secs)
        .await
        .map_err(|e| e.to_string())?;
    // Pre-load transcription model so it's ready before the user presses the button.
    // Without this, model loading starts on first button press and can starve the
    // BLE event loop (causing disconnection).
    app.state::<Arc<TranscriptionManager>>().initiate_model_load();
    Ok(ble.status())
}

/// Connect to a specific BLE device by address.
#[tauri::command]
#[specta::specta]
pub async fn ble_connect_by_address(
    app: AppHandle,
    address: String,
) -> Result<BleStatus, String> {
    let ble = app.state::<Arc<BleManager>>();
    ble.connect_by_address(&address)
        .await
        .map_err(|e| e.to_string())?;
    // Pre-load transcription model so it's ready before the user presses the button.
    app.state::<Arc<TranscriptionManager>>().initiate_model_load();
    Ok(ble.status())
}

/// Disconnect the BLE device.
#[tauri::command]
#[specta::specta]
pub async fn ble_disconnect(app: AppHandle) -> Result<(), String> {
    app.state::<Arc<BleManager>>()
        .disconnect()
        .await
        .map_err(|e| e.to_string())
}

/// Set the active audio source (microphone or BLE).
/// When switching to BLE, the caller should also set `ble_device_address` via
/// `ble_connect_by_address` or `ble_connect_first`.
#[tauri::command]
#[specta::specta]
pub fn set_audio_source(app: AppHandle, source: AudioSource) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.audio_source = source;
    write_settings(&app, settings);
    Ok(())
}

/// Get the currently configured audio source.
#[tauri::command]
#[specta::specta]
pub fn get_audio_source(app: AppHandle) -> AudioSource {
    get_settings(&app).audio_source
}
