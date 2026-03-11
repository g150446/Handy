# Control Mode + BLE Recording — Implementation Notes

## Overview

Control mode is a voice-driven assistant mode that:
1. Activates via **double-click** on the BLE device (AtomEchoS3R)
2. Immediately starts recording the user's voice
3. On **single-click**, stops recording and submits the transcription to OpenRouter
4. Displays the LLM response, then **automatically exits** control mode after 2 seconds

---

## Key Files

| File | Role |
|------|------|
| `src-tauri/src/ble/mod.rs` | BLE event handler — manages all button events and recording state |
| `src-tauri/src/control.rs` | Control mode state machine, LLM interaction, auto-exit timer |
| `src-tauri/src/managers/audio.rs` | `AudioRecordingManager` — BLE recording start/stop paths |

---

## BLE Event Protocol (AtomEchoS3R → Host)

Device sends 2-byte notifications on the TX characteristic:

| Byte 0 | Byte 1 | Meaning |
|--------|--------|---------|
| `AUDIO_SYNC_BYTE` | — | Audio PCM packet (16-bit LE, 16 kHz, mono) |
| `EVENT_SYNC_BYTE` | `0x01` | Physical button pressed → start recording |
| `EVENT_SYNC_BYTE` | `0x02` | Physical button released → stop recording |
| `EVENT_SYNC_BYTE` | `0x03` | Double-click detected |

Host → Device commands (written to RX characteristic):

| Byte | Meaning |
|------|---------|
| `0x01` | Start streaming audio (app-initiated) |
| `0x00` | Stop streaming audio (app-initiated) |

**Important firmware behavior**: When the host sends `0x01` to start app-initiated streaming,
the device enters "app-initiated mode" and does **not** send `0x02` events on button release.
Physical button presses still generate `0x01` events in this mode.

---

## Control Mode Recording State Machine

### Shared state in `BleRecordingManager`

```
recording_samples        Arc<Mutex<Vec<f32>>>   PCM buffer
is_recording             Arc<Mutex<bool>>        Accumulate incoming packets?
device_button_active     Arc<Mutex<bool>>        Was recording started by physical button?
discard_next_stop_event  Arc<Mutex<bool>>        Ignore the next 0x02 event
control_mode_capturing   Arc<Mutex<bool>>        Next 0x01 = user's stop command?
```

### Event flow for control mode

```
User double-clicks
        │
        ▼
   0x03 event received
        │
        ├─ recording_samples = []
        ├─ is_recording = true
        ├─ control_mode_capturing = false   ← not ready for user stop yet
        ├─ discard_next_stop_event = true
        ├─ send_ble_button_event(true)      → coordinator: Recording stage
        └─ toggle_mode() → control window shown
        │
        ▼
   0x01 event (device sends this as part of double-click gesture)
        │  control_mode_capturing == false
        ├─ device_button_active = true      ← device is streaming from physical press
        └─ (no coordinator event — already in Recording stage)
        │
        ▼
   0x02 event (button release from double-click)
        │  discard_next_stop_event == true → DISCARD
        ├─ discard_next_stop_event = false
        └─ spawn resume_streaming_command() with 100ms delay
                │
                ├─ device_button_active = false  ← now app-initiated mode
                ├─ is_recording = true
                ├─ sends 0x01 to device → device resumes streaming
                └─ control_mode_capturing = true  ← ready for user's stop press
        │
        ▼
   User speaks …
        │  Audio packets accumulate in recording_samples
        │
        ▼
   User single-clicks (press)
        │  0x01 event, control_mode_capturing == true
        ├─ control_mode_capturing = false
        ├─ device_button_active = true
        ├─ discard_next_stop_event = true   ← discard the upcoming 0x02 release
        └─ send_ble_button_event(false)     → coordinator: stop() → Processing stage
                │
                ▼
           stop_recording()
                ├─ take_device_button_samples()  (device_button_active=true → fast sync path)
                │       ├─ is_recording = false
                │       ├─ device_button_active = false
                │       └─ returns all PCM samples
                └─ transcription → submit_voice_prompt() → OpenRouter
        │
        ▼
   0x02 event (release of stop press) → DISCARDED (discard_next_stop_event=true)
        │
        ▼
   OpenRouter response received
        ├─ displayed in control window
        └─ schedule_auto_exit(session_id) → 2 s later → deactivate_mode()
```

---

## Why `device_button_active` Matters for Stop

`AudioRecordingManager.stop_recording()` has two BLE stop paths:

```
device_button_active == true   →  take_device_button_samples()
                                   Fast, synchronous. Returns buffer immediately.

device_button_active == false  →  stop_recording_command() async
                                   Sends 0x00 to device, waits 500 ms for in-flight packets.
                                   Collected via mpsc channel with 5-second timeout.
```

Always ensure `device_button_active = true` before triggering the stop in control mode,
otherwise you risk a 5-second timeout with 0 samples returned.

---

## Why Stop on Press (not Release)

After `resume_streaming_command()` sends `0x01` to the device, the device enters
app-initiated streaming mode. In this mode, physical button **releases do not generate
`0x02` events**. Presses still generate `0x01` events. Therefore:

- Stopping on `0x02` (release) does not work in control mode → no event arrives.
- Stopping on `0x01` (press) works reliably.

The `control_mode_capturing` flag gates this: it is `false` during the double-click
gesture (so the double-click's own `0x01` does not immediately stop recording) and
becomes `true` only after the gesture completes and `resume_streaming_command` finishes.

---

## LLM Response Handling (`control.rs`)

### Action JSON (undo)

If the LLM replies with `{"action":"undo_last_input"}`, `execute_undo_last_input()` is called:
- Terminal apps (iTerm2, Terminal.app, etc.) → `Ctrl+U` (readline kill-line)
- Other apps → `Cmd+Z` (standard undo)

The app list is in `TERMINAL_APP_NAMES` in `control.rs`.

### Markdown fence stripping

Some models wrap JSON in ` ```json … ``` ` fences. `strip_markdown_code_fence()` in
`control.rs` strips these before attempting `serde_json::from_str`.

### Auto-exit

After every successful LLM response (both action and text replies), `schedule_auto_exit()`
is called. It waits 2 seconds and then calls `deactivate_mode()` — but only if
`session_id` hasn't changed (user didn't already exit manually).

---

## Normal BLE Recording (non-control mode)

For reference, the normal push-to-talk flow is unaffected:

```
0x01 press  →  recording_samples=[], is_recording=true, device_button_active=true
               send_ble_button_event(true)  → coordinator start

0x02 release →  device_button_active=true
               send_ble_button_event(false)  → coordinator stop
               take_device_button_samples()  → fast sync return
```
