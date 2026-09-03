# Harbor Control Architecture

Phase 1 targets **Terminal Harbor only** via the existing mobile bridge HTTP API.
Handy adds an independent Harbor Control Mode; Groq Control Mode and normal
paste transcription stay unchanged.

Related projects:

- `/Users/kazami/projects/harbor/terminal-harbor` — desktop terminal + HTTP bridge `:7780`
- `/Users/kazami/projects/harbor/terminal-harbor-mobile` — Flutter client + OpenAPI contract

## Goals

| Phase | Goal |
|-------|------|
| **Phase 1 (now)** | Handy STT controls Terminal Harbor workspace switch via `POST /v1/voice/intent` |
| **Phase 2 (later)** | Same control plane can target other apps without rewriting Harbor |

## Non-goals (Phase 1)

- Do not drive Harbor by OS key injection from Groq Control Mode
- Do not put generic desktop automation inside Terminal Harbor
- Do not invent a second protocol when Harbor OpenAPI already exists
- No TTS readout in Phase 1

## Flow

```
Handy STT (Harbor Control Mode active)
        │  transcript only (no clipboard paste)
        ▼
HMAC-signed POST /v1/voice/intent
        ▼
Terminal Harbor bridge
  local Ollama lfm2.5:latest + public workspace labels
        ▼
unambiguous switch_workspace → activate + focus window
else no-op (ambiguous / unsupported / model_unavailable / failed)
```

## Handy implementation

| Piece | Location |
|-------|----------|
| HMAC pair + Keychain + voice client | `src-tauri/src/harbor_control.rs` |
| Mode toggle | BLE double-tap `0x12` / legacy `0x03`; binding `harbor_control` (default macOS `option+shift+h`) |
| Transcription routing | `actions.rs` — Harbor active → `/v1/voice/intent`, no paste |
| Settings (General) | pair URI paste + shortcut |
| Overlay window | label `harbor-control` |

**Same-Mac default:** Handy calls `POST /v1/pair/local` on
`http://127.0.0.1:7780` (loopback only on the Harbor side). No QR paste is
required when Terminal Harbor is running on this Mac. Harbor Control Mode and
Settings → General auto-try this path; manual `harbor://pair` URI remains as
fallback.

**Speech context:** While Harbor Control Mode is active, Handy fetches
`GET /v1/workspaces` and feeds public labels (directory basenames, agents,
Codex/Claude aliases) into Whisper `initial_prompt` and post-STT custom-word
correction. Saying a sidebar directory name switches that workspace when the
match is unique.

Derived device secret is stored in macOS Keychain service
`ai.handy.terminal-harbor`. Settings keep `harbor_server_id`,
`harbor_client_id`, and `harbor_base_url` only.

## Harbor side

API **1.7.0** `POST /v1/voice/intent` in:

1. `terminal-harbor-mobile/openapi/harbor-mobile.yaml`
2. `terminal-harbor/wezterm-gui/src/harbor_mobile.rs`
3. Handy client above

See Harbor mobile `docs/voice-control.md` and desktop `docs/mobile-bridge.md`.

## Control Mode vs Harbor path

| | Groq Control Mode | Harbor Control Mode |
|--|-------------------|---------------------|
| Activation | (no BLE path; internal / future) | BLE double-tap `0x12` / legacy `0x03`, or `harbor_control` shortcut |
| Execution | Groq tools → frontmost app | Harbor HTTP |
| Paste | May inject keys | Suppressed |

Modes are mutually exclusive: enabling one deactivates the other. BLE double-tap toggles Harbor ↔ normal input (not Groq Control).

## Verification

- Pair URI → Keychain secret → signed `/v1/voice/intent`
- Harbor mode: “クロードの方” / “コーデックス” switches and focuses; ambiguous no-op
- Ollama down → `model_unavailable`; state unchanged
- Normal STT and Groq Control Mode still paste / inject as before
