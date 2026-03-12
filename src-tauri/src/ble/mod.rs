//! BLE audio manager for the AtomEchoS3R device.
//!
//! Packet formats
//! ──────────────
//! Audio packet:
//!   Byte 0   : sequence number
//!   Byte 1   : 0xAA  (audio sync byte)
//!   Bytes 2… : i16 LE PCM samples @ 16 kHz mono (device sends 64 samples = 128 bytes)
//!
//! Event packet (3 bytes):
//!   Byte 0   : 0x00  (reserved)
//!   Byte 1   : 0x55  (event sync byte)
//!   Byte 2   : event code
//!              0x01 = recording started
//!              0x02 = recording stopped
//!              0x03 = toggle conversation mode
//!
//! Characteristic UUIDs
//! ─────────────────────
//!   Service : 00000001-0000-1000-8000-00805f9b34fb
//!   TX (notify, device → host) : 00000002-0000-1000-8000-00805f9b34fb
//!   RX (write,  host → device) : 00000003-0000-1000-8000-00805f9b34fb
//!
//! Recording commands sent to RX
//! ──────────────────────────────
//!   Start : 0x01
//!   Stop  : 0x00
//!
//! Device-button flow
//! ──────────────────
//! When the user presses the physical button on the M5Atom:
//!   device button press   → event 0x01 → BleManager sets is_recording=true,
//!                           calls TranscriptionCoordinator (push-to-talk press)
//!   device button release → event 0x02 → BleManager calls TranscriptionCoordinator
//!                           (push-to-talk release) → triggers transcription pipeline

use anyhow::Result;
use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager as TauriManager};
use uuid::{uuid, Uuid};

pub const BLE_DEVICE_NAME: &str = "AtomEchoS3R";

pub const SERVICE_UUID: Uuid = uuid!("00000001-0000-1000-8000-00805f9b34fb");
/// TX characteristic: device → host, notify
pub const TX_CHAR_UUID: Uuid = uuid!("00000002-0000-1000-8000-00805f9b34fb");
/// RX characteristic: host → device, write
pub const RX_CHAR_UUID: Uuid = uuid!("00000003-0000-1000-8000-00805f9b34fb");

const AUDIO_SYNC_BYTE: u8 = 0xAA;
const EVENT_SYNC_BYTE: u8 = 0x55;
/// Minimum PCM bytes required to accept an audio packet.
const MIN_PCM_BYTES: usize = 2;
const RECONNECT_TASK_STALE_AFTER: Duration = Duration::from_secs(45);
const BLE_RESUME_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BLE_RECONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);

// ── BLE binding id used when the physical device button triggers recording ──
const BLE_BUTTON_BINDING: &str = "transcribe";
const BLE_BUTTON_SOURCE: &str = "ble_button";

/// Status returned to the frontend.
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BleStatus {
    pub connected: bool,
    pub device_name: Option<String>,
    pub device_address: Option<String>,
}

/// Internal connection state.
#[derive(Debug, Clone, PartialEq)]
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected {
        device_name: String,
        device_address: String,
    },
}

/// Manages a BLE connection to an AtomEchoS3R device.
#[derive(Clone)]
pub struct BleManager {
    app_handle: tauri::AppHandle,
    peripheral: Arc<Mutex<Option<Peripheral>>>,
    state: Arc<Mutex<ConnectionState>>,
    /// Samples accumulated during the current recording.
    recording_samples: Arc<Mutex<Vec<f32>>>,
    /// Whether audio packets should be accumulated right now.
    is_recording: Arc<Mutex<bool>>,
    /// True when the device's physical button started the current recording
    /// (as opposed to the app sending 0x01).  Used to skip redundant commands.
    device_button_active: Arc<Mutex<bool>>,
    /// Set when a double-click cancels an in-progress BLE recording so the
    /// subsequent device stop event does not trigger transcription.
    discard_next_stop_event: Arc<Mutex<bool>>,
    /// Set while control mode is actively recording and the next intentional
    /// user click should stop that recording.
    control_mode_capturing: Arc<Mutex<bool>>,
    /// The device emits event 0x01 both for a physical button press and for the
    /// app-initiated "recording started" acknowledgement. When control mode
    /// auto-starts recording, ignore that first ACK so we keep waiting for the
    /// user's actual next single-click.
    ignore_next_control_mode_start_ack: Arc<Mutex<bool>>,
    /// Disabled by explicit user disconnect so we do not immediately reconnect
    /// against the user's intent.
    allow_auto_reconnect: Arc<Mutex<bool>>,
    reconnect_task_started_at: Arc<Mutex<Option<Instant>>>,
    stream_command_epoch: Arc<AtomicU64>,
}

impl BleManager {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle,
            peripheral: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            recording_samples: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(Mutex::new(false)),
            device_button_active: Arc::new(Mutex::new(false)),
            discard_next_stop_event: Arc::new(Mutex::new(false)),
            control_mode_capturing: Arc::new(Mutex::new(false)),
            ignore_next_control_mode_start_ack: Arc::new(Mutex::new(false)),
            allow_auto_reconnect: Arc::new(Mutex::new(true)),
            reconnect_task_started_at: Arc::new(Mutex::new(None)),
            stream_command_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    fn try_begin_reconnect_task(&self, trigger: &str) -> bool {
        let mut started_at = self.reconnect_task_started_at.lock().unwrap();

        if let Some(previous_start) = *started_at {
            let age = previous_start.elapsed();
            if age <= RECONNECT_TASK_STALE_AFTER {
                debug!(
                    "Skipping BLE reconnect task for {trigger} because another task has been running for {:?}",
                    age
                );
                return false;
            }

            warn!(
                "Recovering stale BLE reconnect task before {trigger}; previous task age was {:?}",
                age
            );
        }

        *started_at = Some(Instant::now());
        true
    }

    fn finish_reconnect_task(&self) {
        *self.reconnect_task_started_at.lock().unwrap() = None;
    }

    // ──────────────────────────────────────────────────────── status ──

    pub fn status(&self) -> BleStatus {
        match &*self.state.lock().unwrap() {
            ConnectionState::Disconnected | ConnectionState::Connecting => BleStatus {
                connected: false,
                device_name: None,
                device_address: None,
            },
            ConnectionState::Connected {
                device_name,
                device_address,
            } => BleStatus {
                connected: true,
                device_name: Some(device_name.clone()),
                device_address: Some(device_address.clone()),
            },
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(
            *self.state.lock().unwrap(),
            ConnectionState::Connected { .. }
        )
    }

    // ──────────────────────────────────────────────────────── scanning ──

    /// Scan for nearby AtomEchoS3R devices.
    /// Returns a list of `"name (peripheral-id)"` display strings.
    pub async fn scan_devices(&self, duration_secs: u64) -> Result<Vec<String>> {
        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let central = adapters
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No Bluetooth adapter found"))?;

        central.start_scan(ScanFilter::default()).await?;
        tokio::time::sleep(std::time::Duration::from_secs(duration_secs)).await;
        central.stop_scan().await?;

        let mut found = Vec::new();
        for p in central.peripherals().await? {
            if let Ok(Some(props)) = p.properties().await {
                let name = props.local_name.unwrap_or_default();
                if name.contains(BLE_DEVICE_NAME) {
                    // On macOS, BDAddr is always 00:00:00:00:00:00 (CoreBluetooth privacy).
                    // Use PeripheralId (UUID) as the stable identifier instead.
                    let id = p.id().to_string();
                    info!("Found BLE device: {} ({})", name, id);
                    found.push(format!("{} ({})", name, id));
                }
            }
        }
        Ok(found)
    }

    async fn find_matching_peripheral(
        &self,
        central: &Adapter,
        timeout: std::time::Duration,
        preferred_id: Option<&str>,
    ) -> Result<Option<Peripheral>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut fallback_match: Option<Peripheral> = None;

        while tokio::time::Instant::now() < deadline {
            for peripheral in central.peripherals().await? {
                if let Some(id) = preferred_id {
                    if peripheral.id().to_string() == id {
                        return Ok(Some(peripheral));
                    }
                }

                if let Ok(Some(props)) = peripheral.properties().await {
                    if props
                        .local_name
                        .as_deref()
                        .unwrap_or("")
                        .contains(BLE_DEVICE_NAME)
                    {
                        if preferred_id.is_none() {
                            return Ok(Some(peripheral));
                        }
                        if fallback_match.is_none() {
                            fallback_match = Some(peripheral);
                        }
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        Ok(fallback_match)
    }

    // ────────────────────────────────────────────────────── connection ──

    /// Connect to the first AtomEchoS3R found within `scan_secs` seconds.
    pub async fn connect_first(&self, scan_secs: u64) -> Result<()> {
        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let central = adapters
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No Bluetooth adapter found"))?;

        *self.state.lock().unwrap() = ConnectionState::Connecting;

        central.start_scan(ScanFilter::default()).await?;
        let matched = self
            .find_matching_peripheral(&central, std::time::Duration::from_secs(scan_secs), None)
            .await?;
        central.stop_scan().await?;

        let device = matched.ok_or_else(|| anyhow::anyhow!("AtomEchoS3R not found during scan"))?;

        self.do_connect(device).await
    }

    /// Connect to a specific device by its PeripheralId string.
    pub async fn connect_by_address(&self, address: &str) -> Result<()> {
        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let central = adapters
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No Bluetooth adapter found"))?;

        *self.state.lock().unwrap() = ConnectionState::Connecting;

        central.start_scan(ScanFilter::default()).await?;
        let matched = self
            .find_matching_peripheral(&central, std::time::Duration::from_secs(8), Some(address))
            .await?;

        central.stop_scan().await?;

        if matched.is_some() {
            if matched
                .as_ref()
                .is_some_and(|peripheral| peripheral.id().to_string() != address)
            {
                warn!(
                    "BLE device id {} was not found; using name-based fallback peripheral {}",
                    address,
                    matched.as_ref().unwrap().id()
                );
            }
        } else {
            warn!(
                "BLE device id {} was not found; falling back to scan by device name also failed",
                address
            );
        }

        let device = matched.ok_or_else(|| anyhow::anyhow!("Device not found: {}", address))?;

        self.do_connect(device).await
    }

    async fn do_connect(&self, device: Peripheral) -> Result<()> {
        device.connect().await?;
        device.discover_services().await?;

        let device_address = device.id().to_string();
        let device_name = {
            let props = device.properties().await?;
            props
                .and_then(|p| p.local_name)
                .unwrap_or_else(|| BLE_DEVICE_NAME.to_string())
        };

        // Subscribe to TX notifications.
        let chars = device.characteristics();
        let tx_char = chars
            .iter()
            .find(|c| c.uuid == TX_CHAR_UUID && c.properties.contains(CharPropFlags::NOTIFY))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TX notify characteristic not found"))?;

        device.subscribe(&tx_char).await?;

        let listener_device = device.clone();
        *self.peripheral.lock().unwrap() = Some(device);
        *self.state.lock().unwrap() = ConnectionState::Connected {
            device_name: device_name.clone(),
            device_address: device_address.clone(),
        };
        *self.allow_auto_reconnect.lock().unwrap() = true;

        let mut settings = crate::settings::get_settings(&self.app_handle);
        settings.ble_device_address = Some(device_address.clone());
        crate::settings::write_settings(&self.app_handle, settings);

        info!("BLE connected: {} ({})", device_name, device_address);

        self.spawn_notification_listener(listener_device);

        // Allow BLE to stabilise before the caller sends commands.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let status = self.status();
        if let Err(e) = self.app_handle.emit("ble-status-changed", &status) {
            error!("Failed to emit ble-status-changed: {e}");
        }

        Ok(())
    }

    fn spawn_notification_listener(&self, peripheral: Peripheral) {
        let ble = self.clone();
        let recording_samples = self.recording_samples.clone();
        let is_recording = self.is_recording.clone();
        let device_button_active = self.device_button_active.clone();
        let discard_next_stop_event = self.discard_next_stop_event.clone();
        let control_mode_capturing = self.control_mode_capturing.clone();
        let ignore_next_control_mode_start_ack = self.ignore_next_control_mode_start_ack.clone();
        let app_handle = self.app_handle.clone();

        tauri::async_runtime::spawn(async move {
            let mut stream = match peripheral.notifications().await {
                Ok(s) => s,
                Err(e) => {
                    error!("BLE notification stream error: {}", e);
                    ble.handle_connection_loss("notification stream error", false);
                    return;
                }
            };

            debug!("BLE notification listener running");

            // Use a 10-second heartbeat timeout so the loop exits if btleplug's
            // notification stream hangs after a CoreBluetooth event receiver death
            // (the stream may never return None in that case).
            let mut disconnect_reason = "notification stream closed";
            loop {
                let notif =
                    match tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
                        .await
                    {
                        Ok(Some(n)) => n,
                        Ok(None) => break, // stream closed cleanly
                        Err(_) => {
                            // No notification for 10 s – verify peripheral is still up.
                            let still_connected = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                peripheral.is_connected(),
                            )
                            .await
                            .ok()
                            .and_then(|r| r.ok())
                            .unwrap_or(false);

                            if !still_connected {
                                warn!("BLE: heartbeat detected peripheral disconnected");
                                disconnect_reason = "heartbeat detected disconnect";
                                break;
                            }
                            continue; // still connected, just quiet
                        }
                    };

                if notif.uuid != TX_CHAR_UUID {
                    continue;
                }
                let data = &notif.value;
                if data.len() < 2 {
                    continue;
                }

                match data[1] {
                    // ── Audio packet ──────────────────────────────────── //
                    AUDIO_SYNC_BYTE => {
                        if !*is_recording.lock().unwrap() {
                            continue;
                        }
                        let pcm = &data[2..];
                        if pcm.len() < MIN_PCM_BYTES {
                            continue;
                        }
                        let new_samples: Vec<f32> = pcm
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
                            .collect();
                        recording_samples.lock().unwrap().extend(new_samples);
                    }

                    // ── Event packet ──────────────────────────────────── //
                    EVENT_SYNC_BYTE => {
                        if data.len() < 3 {
                            continue;
                        }
                        match data[2] {
                            0x01 => {
                                if *is_recording.lock().unwrap()
                                    && crate::control::get_mode_snapshot(&app_handle).active
                                {
                                    if *ignore_next_control_mode_start_ack.lock().unwrap() {
                                        *ignore_next_control_mode_start_ack.lock().unwrap() = false;
                                        debug!(
                                            "BLE event: ignoring control-mode recording_started ACK"
                                        );
                                        continue;
                                    }
                                    if *control_mode_capturing.lock().unwrap() {
                                        // User's intentional stop press: stop on press (not release),
                                        // because the device may not send 0x02 in app-initiated mode.
                                        info!("BLE event: press during control mode – stopping recording now");
                                        *control_mode_capturing.lock().unwrap() = false;
                                        *device_button_active.lock().unwrap() = true;
                                        *discard_next_stop_event.lock().unwrap() = true;
                                        send_ble_button_event(&app_handle, false);
                                    } else {
                                        // Double-click's second press received while still in setup phase.
                                        // Arm capture mode immediately so the user can stop recording
                                        // even if the 0x02 (button-release) event never arrives
                                        // (e.g. button held down, BLE packet loss).
                                        info!("BLE event: press during control mode recording – arming capture mode now");
                                        *device_button_active.lock().unwrap() = true;
                                        *control_mode_capturing.lock().unwrap() = true;
                                    }
                                } else {
                                    let control_snapshot =
                                        crate::control::get_mode_snapshot(&app_handle);
                                    info!("BLE event: device button pressed – start recording");
                                    // Start accumulating samples immediately.
                                    *recording_samples.lock().unwrap() = Vec::new();
                                    *is_recording.lock().unwrap() = true;
                                    *device_button_active.lock().unwrap() = true;
                                    *discard_next_stop_event.lock().unwrap() = false;
                                    debug!(
                                        "BLE press routed as normal recording start (control_active={}, session_id={})",
                                        control_snapshot.active,
                                        control_snapshot.session_id
                                    );

                                    // Trigger the transcription pipeline (push-to-talk press).
                                    send_ble_button_event(&app_handle, true);
                                }
                            }
                            0x02 => {
                                if *discard_next_stop_event.lock().unwrap() {
                                    let should_resume_streaming =
                                        *control_mode_capturing.lock().unwrap();
                                    info!(
                                        "BLE event: ignoring discarded stop event (resume_streaming={should_resume_streaming})"
                                    );
                                    *discard_next_stop_event.lock().unwrap() = false;

                                    // Only the synthetic stop produced by the double-click setup
                                    // should resume device streaming. The later stop-click release
                                    // must stay ignored, otherwise the device starts recording
                                    // again and the first post-exit single click becomes a stop.
                                    let control_snapshot =
                                        crate::control::get_mode_snapshot(&app_handle);
                                    if should_resume_streaming
                                        && control_snapshot.active
                                        && *is_recording.lock().unwrap()
                                    {
                                        let ble_clone = ble.clone();
                                        let capturing_clone = control_mode_capturing.clone();
                                        let app_handle_clone = app_handle.clone();
                                        let control_session_id = control_snapshot.session_id;
                                        tauri::async_runtime::spawn(async move {
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                100,
                                            ))
                                            .await;
                                            let current_snapshot =
                                                crate::control::get_mode_snapshot(
                                                    &app_handle_clone,
                                                );
                                            if !current_snapshot.active
                                                || current_snapshot.session_id != control_session_id
                                            {
                                                info!(
                                                    "Skipping BLE resume after double-click because control mode changed (expected session {}, active={}, current session {})",
                                                    control_session_id,
                                                    current_snapshot.active,
                                                    current_snapshot.session_id
                                                );
                                                return;
                                            }
                                            if let Err(e) =
                                                ble_clone.resume_streaming_command().await
                                            {
                                                error!(
                                                    "Failed to resume BLE streaming after double-click: {e}"
                                                );
                                                return;
                                            }
                                            let current_snapshot =
                                                crate::control::get_mode_snapshot(
                                                    &app_handle_clone,
                                                );
                                            if !current_snapshot.active
                                                || current_snapshot.session_id != control_session_id
                                            {
                                                info!(
                                                    "Skipping control-mode capture arming because control mode changed during BLE resume (expected session {}, active={}, current session {})",
                                                    control_session_id,
                                                    current_snapshot.active,
                                                    current_snapshot.session_id
                                                );
                                                return;
                                            }
                                            // Now the double-click gesture is fully committed;
                                            // the next physical 0x01 press is the user's stop command.
                                            *capturing_clone.lock().unwrap() = true;
                                        });
                                    }
                                    continue;
                                }
                                if crate::control::get_mode_snapshot(&app_handle).active
                                    && *is_recording.lock().unwrap()
                                    && *control_mode_capturing.lock().unwrap()
                                {
                                    info!(
                                        "BLE event: control-mode stop acknowledged without press event"
                                    );
                                    *control_mode_capturing.lock().unwrap() = false;
                                    send_ble_button_event(&app_handle, false);
                                    continue;
                                }
                                // Only stop when a physical button press was registered
                                // (device_button_active=true). This filters out spurious 0x02
                                // events the device may emit in response to our 0x01 commands.
                                if *device_button_active.lock().unwrap() {
                                    let control_snapshot =
                                        crate::control::get_mode_snapshot(&app_handle);
                                    info!("BLE event: device button released – stop recording");
                                    // is_recording stays true so in-flight packets are captured;
                                    // stop_recording_command() will clear it.
                                    debug!(
                                        "BLE release routed as recording stop (control_active={}, session_id={})",
                                        control_snapshot.active,
                                        control_snapshot.session_id
                                    );
                                    send_ble_button_event(&app_handle, false);
                                } else {
                                    debug!("BLE event: 0x02 ignored (no active button press)");
                                }
                            }
                            0x03 => {
                                let recording_was_active = *is_recording.lock().unwrap()
                                    || *device_button_active.lock().unwrap();

                                if recording_was_active {
                                    info!("BLE event: cancel recording and toggle control mode");
                                    *discard_next_stop_event.lock().unwrap() = true;
                                    cancel_ble_recording(&app_handle);
                                } else {
                                    info!("BLE event: toggle control mode");
                                }
                                match crate::control::toggle_mode(&app_handle) {
                                    Ok(snapshot) => {
                                        if !snapshot.active {
                                            crate::overlay::show_normal_input_overlay(&app_handle);
                                        } else {
                                            // Auto-start app-initiated recording upon entering control mode.
                                            // Ignore the immediate recording-started ACK (0x01) that the
                                            // device emits in response to our start command. After that,
                                            // wait for the next single-click and accept either a 0x01
                                            // press or a direct 0x02 stop acknowledgement from firmware.
                                            info!("BLE: control mode active – auto-starting recording");
                                            *recording_samples.lock().unwrap() = Vec::new();
                                            *is_recording.lock().unwrap() = true;
                                            *control_mode_capturing.lock().unwrap() = true;
                                            *discard_next_stop_event.lock().unwrap() = false;
                                            *ignore_next_control_mode_start_ack.lock().unwrap() =
                                                true;
                                            send_ble_button_event(&app_handle, true);
                                        }
                                    }
                                    Err(err) => {
                                        error!("Failed to toggle control mode from BLE: {err}");
                                    }
                                }
                            }
                            other => {
                                debug!("BLE event: unknown code {:#04x}", other);
                            }
                        }
                    }
                    _ => {}
                } // end `match data[1]`
            } // end `loop`

            // Stream closed (or heartbeat disconnect) → connection lost.
            let was_recording = *is_recording.lock().unwrap();
            drop(recording_samples);
            drop(is_recording);
            drop(device_button_active);
            drop(discard_next_stop_event);
            drop(app_handle);
            ble.handle_connection_loss(disconnect_reason, was_recording);
        });
    }

    fn reset_transient_ble_state(&self) {
        *self.is_recording.lock().unwrap() = false;
        *self.device_button_active.lock().unwrap() = false;
        *self.control_mode_capturing.lock().unwrap() = false;
        *self.discard_next_stop_event.lock().unwrap() = false;
        *self.ignore_next_control_mode_start_ack.lock().unwrap() = false;
        *self.recording_samples.lock().unwrap() = Vec::new();
    }

    /// Reset all control-mode-related flags to their initial state.
    /// Called when control mode deactivates to ensure a clean BLE state
    /// for normal single-click recording.
    pub fn reset_control_mode_state(&self) {
        self.reset_transient_ble_state();
    }

    fn handle_interrupted_recording(&self, context: &str, was_recording: bool) {
        if !was_recording {
            return;
        }

        if crate::control::get_mode_snapshot(&self.app_handle).active {
            info!("{context} during control mode – cancelling recording and deactivating");
            cancel_ble_recording(&self.app_handle);
            let _ = crate::control::deactivate_mode(&self.app_handle);
            crate::overlay::show_normal_input_overlay(&self.app_handle);
        } else {
            send_ble_button_event(&self.app_handle, false);
        }
    }

    pub fn next_stream_command_epoch(&self) -> u64 {
        self.stream_command_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current_stream_command_epoch(&self, epoch: u64) -> bool {
        self.stream_command_epoch.load(Ordering::SeqCst) == epoch
    }

    fn handle_connection_loss(&self, reason: &str, was_recording: bool) {
        let already_disconnected =
            matches!(*self.state.lock().unwrap(), ConnectionState::Disconnected);
        *self.peripheral.lock().unwrap() = None;
        *self.state.lock().unwrap() = ConnectionState::Disconnected;
        self.reset_transient_ble_state();

        if already_disconnected {
            debug!("BLE connection loss ignored because state was already disconnected ({reason})");
        } else {
            info!("BLE connection lost ({reason})");
        }

        let disconnected_status = BleStatus {
            connected: false,
            device_name: None,
            device_address: None,
        };
        if let Err(e) = self
            .app_handle
            .emit("ble-status-changed", &disconnected_status)
        {
            error!("Failed to emit ble-status-changed: {e}");
        }

        self.handle_interrupted_recording("BLE disconnect", was_recording);

        self.schedule_auto_reconnect(reason.to_string());
    }

    fn schedule_auto_reconnect(&self, reason: String) {
        if !*self.allow_auto_reconnect.lock().unwrap() {
            info!("Skipping BLE auto-reconnect after {reason} because it was disabled");
            return;
        }

        if !self.try_begin_reconnect_task("auto reconnect") {
            return;
        }

        let ble = self.clone();
        tauri::async_runtime::spawn(async move {
            info!("Starting BLE auto-reconnect after {reason}");
            let mut attempt: u32 = 0;

            loop {
                attempt = attempt.saturating_add(1);

                let settings = crate::settings::get_settings(&ble.app_handle);
                let address = match (
                    *ble.allow_auto_reconnect.lock().unwrap(),
                    settings.audio_source,
                    settings.ble_device_address.clone(),
                ) {
                    (false, _, _) => {
                        info!("Stopping BLE auto-reconnect because it was disabled");
                        break;
                    }
                    (_, crate::settings::AudioSource::Ble, Some(address)) => address,
                    _ => {
                        info!("Stopping BLE auto-reconnect because BLE is no longer the active source");
                        break;
                    }
                };

                if ble.is_connected() {
                    debug!("Stopping BLE auto-reconnect because the device is already connected");
                    break;
                }

                let delay_secs = if attempt <= 3 { 2 } else { 5 };
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;

                if ble.is_connected() {
                    debug!("Skipping BLE reconnect attempt because the device reconnected already");
                    break;
                }

                info!("BLE auto-reconnect attempt {} to {}", attempt, address);
                match tokio::time::timeout(
                    BLE_RECONNECT_ATTEMPT_TIMEOUT,
                    ble.connect_by_address(&address),
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("BLE auto-reconnect succeeded on attempt {}", attempt);
                        ble.app_handle
                            .state::<Arc<crate::managers::transcription::TranscriptionManager>>()
                            .initiate_model_load();
                        break;
                    }
                    Ok(Err(err)) => {
                        warn!("BLE auto-reconnect attempt {} failed: {}", attempt, err);
                    }
                    Err(_) => {
                        warn!(
                            "BLE auto-reconnect attempt {} timed out after {:?}",
                            attempt, BLE_RECONNECT_ATTEMPT_TIMEOUT
                        );
                    }
                }
            }

            ble.finish_reconnect_task();
        });
    }

    pub fn handle_possible_system_resume(&self, gap: std::time::Duration) {
        if !*self.allow_auto_reconnect.lock().unwrap() {
            debug!("Skipping BLE resume recovery because auto-reconnect is disabled");
            return;
        }

        let settings = crate::settings::get_settings(&self.app_handle);
        let Some(address) = settings.ble_device_address.clone() else {
            debug!("Skipping BLE resume recovery because there is no remembered device");
            return;
        };

        if settings.audio_source != crate::settings::AudioSource::Ble {
            debug!("Skipping BLE resume recovery because BLE is not the active audio source");
            return;
        }

        if !self.try_begin_reconnect_task("resume recovery") {
            return;
        }

        let ble = self.clone();
        tauri::async_runtime::spawn(async move {
            info!(
                "Detected possible system resume after {:?}; refreshing BLE connection",
                gap
            );

            let stale_peripheral = ble.peripheral.lock().unwrap().take();
            if let Some(peripheral) = stale_peripheral {
                match tokio::time::timeout(BLE_RESUME_DISCONNECT_TIMEOUT, peripheral.disconnect())
                    .await
                {
                    Ok(Ok(())) => {
                        info!("BLE resume recovery disconnected stale peripheral");
                    }
                    Ok(Err(err)) => {
                        debug!("BLE resume recovery disconnect returned error: {err}");
                    }
                    Err(_) => {
                        warn!(
                            "BLE resume recovery disconnect timed out after {:?}",
                            BLE_RESUME_DISCONNECT_TIMEOUT
                        );
                    }
                }
            }

            let was_recording =
                *ble.is_recording.lock().unwrap() || *ble.device_button_active.lock().unwrap();
            *ble.state.lock().unwrap() = ConnectionState::Disconnected;
            ble.reset_transient_ble_state();

            if let Err(err) = ble.app_handle.emit(
                "ble-status-changed",
                &BleStatus {
                    connected: false,
                    device_name: None,
                    device_address: None,
                },
            ) {
                error!("Failed to emit ble-status-changed during resume recovery: {err}");
            }

            ble.handle_interrupted_recording("BLE resume recovery", was_recording);

            match tokio::time::timeout(
                BLE_RECONNECT_ATTEMPT_TIMEOUT,
                ble.connect_by_address(&address),
            )
            .await
            {
                Ok(Ok(())) => {
                    info!("BLE resume recovery reconnect succeeded");
                    ble.app_handle
                        .state::<Arc<crate::managers::transcription::TranscriptionManager>>()
                        .initiate_model_load();
                }
                Ok(Err(err)) => {
                    warn!("BLE resume recovery reconnect failed: {err}");
                    ble.finish_reconnect_task();
                    ble.schedule_auto_reconnect("resume recovery failure".to_string());
                    return;
                }
                Err(_) => {
                    warn!(
                        "BLE resume recovery reconnect timed out after {:?}",
                        BLE_RECONNECT_ATTEMPT_TIMEOUT
                    );
                    ble.finish_reconnect_task();
                    ble.schedule_auto_reconnect("resume recovery timeout".to_string());
                    return;
                }
            }

            ble.finish_reconnect_task();
        });
    }

    // ──────────────────────────── synchronous sample collection (device button) ──

    /// Synchronously collect accumulated samples when the device's physical button
    /// stopped recording.  Returns `None` if device-button mode is not active
    /// (meaning recording was app-initiated via `start_recording_command()`).
    ///
    /// This avoids the async/sync bridge (`std::sync::mpsc` blocking a tokio thread)
    /// that causes a 5-second timeout when called from inside an async task.
    pub fn take_device_button_samples(&self) -> Option<Vec<f32>> {
        if !*self.device_button_active.lock().unwrap() {
            return None;
        }
        // Stop accumulating; any packets still in-flight will be discarded.
        *self.is_recording.lock().unwrap() = false;
        *self.device_button_active.lock().unwrap() = false;
        let samples = std::mem::take(&mut *self.recording_samples.lock().unwrap());
        info!(
            "BLE device-button: collected {} samples synchronously",
            samples.len()
        );
        Some(samples)
    }

    // ─────────────────────────────────────────────── recording commands ──

    /// Re-send 0x01 to the device to resume audio streaming **without** resetting the
    /// sample buffer.  Used after a double-click button-release is discarded so that
    /// the user's speech is captured even though the device stopped streaming on release.
    pub async fn resume_streaming_command(&self) -> Result<()> {
        let peripheral = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to BLE device"))?;

        let rx_char = peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == RX_CHAR_UUID)
            .ok_or_else(|| anyhow::anyhow!("RX write characteristic not found"))?;

        // Arm accumulation for app-initiated streaming; do NOT clear recording_samples.
        *self.device_button_active.lock().unwrap() = false;
        *self.is_recording.lock().unwrap() = true;

        peripheral
            .write(&rx_char, &[0x01], WriteType::WithoutResponse)
            .await?;

        debug!("BLE: sent resume streaming (0x01) for control mode");
        Ok(())
    }

    /// Send start-recording command (`0x01`) to the device (app-initiated).
    /// If the physical device button already started recording, this is a no-op.
    pub async fn start_recording_command(&self) -> Result<()> {
        let epoch = self.stream_command_epoch.load(Ordering::SeqCst);
        self.start_recording_command_for_epoch(epoch).await
    }

    pub async fn start_recording_command_for_epoch(&self, epoch: u64) -> Result<()> {
        if !self.is_current_stream_command_epoch(epoch) {
            debug!("BLE: skipping stale start command for epoch {}", epoch);
            return Ok(());
        }
        // If the device button already initiated recording, don't reset the buffer.
        if *self.device_button_active.lock().unwrap() {
            debug!("BLE: start_recording_command skipped (device button active)");
            return Ok(());
        }

        let peripheral = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to BLE device"))?;

        let rx_char = peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == RX_CHAR_UUID)
            .ok_or_else(|| anyhow::anyhow!("RX write characteristic not found"))?;

        // Clear sample buffer and arm accumulation.
        *self.recording_samples.lock().unwrap() = Vec::new();
        *self.is_recording.lock().unwrap() = true;

        if !self.is_current_stream_command_epoch(epoch) {
            debug!(
                "BLE: aborting start command before write because epoch {} is stale",
                epoch
            );
            return Ok(());
        }

        peripheral
            .write(&rx_char, &[0x01], WriteType::WithoutResponse)
            .await?;

        debug!("BLE: sent start recording (0x01)");
        Ok(())
    }

    pub async fn abort_recording_command_for_epoch(&self, epoch: u64) -> Result<()> {
        if !self.is_current_stream_command_epoch(epoch) {
            debug!("BLE: skipping stale abort command for epoch {}", epoch);
            return Ok(());
        }

        let peripheral = self
            .peripheral
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to BLE device"))?;

        let rx_char = peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == RX_CHAR_UUID)
            .ok_or_else(|| anyhow::anyhow!("RX write characteristic not found"))?;

        if !self.is_current_stream_command_epoch(epoch) {
            debug!(
                "BLE: abort command became stale before stop write for epoch {}",
                epoch
            );
            return Ok(());
        }

        peripheral
            .write(&rx_char, &[0x00], WriteType::WithoutResponse)
            .await?;
        self.reset_transient_ble_state();
        debug!("BLE: sent abort recording (0x00) for epoch {}", epoch);
        Ok(())
    }

    /// Stop recording and return all accumulated PCM samples (f32, 16 kHz, mono).
    ///
    /// - App-initiated: sends 0x00, then waits 500 ms for in-flight packets.
    /// - Device-button-initiated: device already stopped; waits 150 ms for
    ///   the last in-flight packets, then returns without sending 0x00.
    pub async fn stop_recording_command(&self) -> Result<Vec<f32>> {
        let device_button = *self.device_button_active.lock().unwrap();

        if device_button {
            // Device already stopped streaming; just drain in-flight packets.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        } else {
            let peripheral = self
                .peripheral
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Not connected to BLE device"))?;

            let rx_char = peripheral
                .characteristics()
                .into_iter()
                .find(|c| c.uuid == RX_CHAR_UUID)
                .ok_or_else(|| anyhow::anyhow!("RX write characteristic not found"))?;

            peripheral
                .write(&rx_char, &[0x00], WriteType::WithoutResponse)
                .await?;

            // Drain in-flight packets while is_recording is still true.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        *self.is_recording.lock().unwrap() = false;
        *self.device_button_active.lock().unwrap() = false;

        let samples = std::mem::take(&mut *self.recording_samples.lock().unwrap());
        debug!("BLE: collected {} samples", samples.len());
        Ok(samples)
    }

    // ───────────────────────────────────────────────────── disconnect ──

    pub async fn disconnect(&self) -> Result<()> {
        *self.allow_auto_reconnect.lock().unwrap() = false;
        self.finish_reconnect_task();
        self.reset_transient_ble_state();
        let peripheral = self.peripheral.lock().unwrap().take();
        if let Some(p) = peripheral {
            let _ = p.disconnect().await;
        }
        *self.state.lock().unwrap() = ConnectionState::Disconnected;
        info!("BLE disconnected");
        let status = self.status();
        if let Err(e) = self.app_handle.emit("ble-status-changed", &status) {
            error!("Failed to emit ble-status-changed: {e}");
        }
        Ok(())
    }
}

// ────────────────────────────────────── coordinator helper ──────────────────

/// Forward a device-button press/release event to the TranscriptionCoordinator
/// as a push-to-talk signal.
fn send_ble_button_event(app: &tauri::AppHandle, is_pressed: bool) {
    use crate::TranscriptionCoordinator;
    use tauri::Manager;

    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        coordinator.send_input(
            BLE_BUTTON_BINDING,
            BLE_BUTTON_SOURCE,
            is_pressed,
            true, // push_to_talk = true: start on press, stop+transcribe on release
        );
    } else {
        warn!("BLE button event: TranscriptionCoordinator not available");
    }
}

fn cancel_ble_recording(app: &tauri::AppHandle) {
    crate::utils::cancel_current_operation(app);
}
