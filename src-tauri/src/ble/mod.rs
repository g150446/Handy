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
//!   Byte 2   : event code  (0x01 = recording started, 0x02 = recording stopped)
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
use btleplug::api::{
    Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use tauri::Emitter;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::{Arc, Mutex};
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
        }
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
        matches!(*self.state.lock().unwrap(), ConnectionState::Connected { .. })
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
        tokio::time::sleep(std::time::Duration::from_secs(scan_secs)).await;
        central.stop_scan().await?;

        let mut matched: Option<Peripheral> = None;
        for p in central.peripherals().await? {
            if let Ok(Some(props)) = p.properties().await {
                if props
                    .local_name
                    .as_deref()
                    .unwrap_or("")
                    .contains(BLE_DEVICE_NAME)
                {
                    matched = Some(p);
                    break;
                }
            }
        }

        let device = matched
            .ok_or_else(|| anyhow::anyhow!("AtomEchoS3R not found during scan"))?;

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
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        central.stop_scan().await?;

        let mut matched: Option<Peripheral> = None;
        for p in central.peripherals().await? {
            if p.id().to_string() == address {
                matched = Some(p);
                break;
            }
        }

        let device = matched
            .ok_or_else(|| anyhow::anyhow!("Device not found: {}", address))?;

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
        let recording_samples = self.recording_samples.clone();
        let is_recording = self.is_recording.clone();
        let device_button_active = self.device_button_active.clone();
        let state = self.state.clone();
        let app_handle = self.app_handle.clone();

        tauri::async_runtime::spawn(async move {
            let mut stream = match peripheral.notifications().await {
                Ok(s) => s,
                Err(e) => {
                    error!("BLE notification stream error: {}", e);
                    return;
                }
            };

            debug!("BLE notification listener running");

            while let Some(notif) = stream.next().await {
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
                                info!("BLE event: device button pressed – start recording");
                                // Start accumulating samples immediately.
                                *recording_samples.lock().unwrap() = Vec::new();
                                *is_recording.lock().unwrap() = true;
                                *device_button_active.lock().unwrap() = true;

                                // Trigger the transcription pipeline (push-to-talk press).
                                send_ble_button_event(&app_handle, true);
                            }
                            0x02 => {
                                info!("BLE event: device button released – stop recording");
                                // Trigger the transcription pipeline (push-to-talk release).
                                // is_recording stays true so in-flight packets are captured;
                                // stop_recording_command() will clear it.
                                send_ble_button_event(&app_handle, false);
                            }
                            other => {
                                debug!("BLE event: unknown code {:#04x}", other);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Stream closed → connection lost.
            let was_recording = *is_recording.lock().unwrap();
            *state.lock().unwrap() = ConnectionState::Disconnected;
            *is_recording.lock().unwrap() = false;
            *device_button_active.lock().unwrap() = false;
            info!("BLE connection lost (notification stream closed)");

            let disconnected_status = BleStatus {
                connected: false,
                device_name: None,
                device_address: None,
            };
            if let Err(e) = app_handle.emit("ble-status-changed", &disconnected_status) {
                error!("Failed to emit ble-status-changed: {e}");
            }

            // Cancel any recording that was in progress so the coordinator
            // doesn't get stuck in Stage::Recording.
            if was_recording {
                send_ble_button_event(&app_handle, false);
            }
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
        info!("BLE device-button: collected {} samples synchronously", samples.len());
        Some(samples)
    }

    // ─────────────────────────────────────────────── recording commands ──

    /// Send start-recording command (`0x01`) to the device (app-initiated).
    /// If the physical device button already started recording, this is a no-op.
    pub async fn start_recording_command(&self) -> Result<()> {
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

        peripheral
            .write(&rx_char, &[0x01], WriteType::WithoutResponse)
            .await?;

        debug!("BLE: sent start recording (0x01)");
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
        *self.is_recording.lock().unwrap() = false;
        *self.device_button_active.lock().unwrap() = false;
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
