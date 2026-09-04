# Harbor Control Architecture

Phase 1 targets **Terminal Harbor only** via the existing mobile bridge HTTP API.
Handy の **Harbor Control** は独立したコントロール面。通常の貼り付け転写および **Desktop Control** とは相互排他。

モード切替・優先設定の全体像: [`preferred-control-mode.md`](./preferred-control-mode.md)  
Desktop Control 詳細: [`control-mode.md`](./control-mode.md)

Related projects:

- `/Users/kazami/projects/harbor/terminal-harbor` — desktop terminal + HTTP bridge `:7780`
- `/Users/kazami/projects/harbor/terminal-harbor-mobile` — Flutter client + OpenAPI contract

## Goals

| Phase | Goal |
|-------|------|
| **Phase 1 (now)** | Handy STT controls Terminal Harbor workspace switch via `POST /v1/voice/intent` |
| **Phase 2 (later)** | Same control plane can target other apps without rewriting Harbor |

## Non-goals (Phase 1)

- Do not drive Harbor by OS key injection from Desktop Control
- Do not put generic desktop automation inside Terminal Harbor
- Do not invent a second protocol when Harbor OpenAPI already exists
- No TTS readout in Phase 1
- Harbor server does **not** switch Handy modes (Handy intercepts STT locally for mode phrases)

## Flow

```
Handy STT (Harbor Control active)
        │
        ├─ local phrase: デスクトップ操作 / normal → Handy mode switch (no HTTP)
        │
        └─ else
              transcript only (no clipboard paste)
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
| Preferred toggle (BLE / shortcut) | `preferred_control.rs` + setting `preferred_control_mode` |
| BLE double-tap `0x12` / `0x03` | `ble/mod.rs` → `toggle_preferred` |
| Shortcut binding `harbor_control` | `actions.rs` → same preferred toggle |
| Transcription routing | `actions.rs` — Harbor active → submit_transcript (mode phrases first) |
| Settings (General) | preferred mode dropdown + pair URI + shortcut |
| Overlay window | label `harbor-control` |

**Same-Mac default:** Handy calls `POST /v1/pair/local` on
`http://127.0.0.1:7780` (loopback only on the Harbor side). No QR paste is
required when Terminal Harbor is running on this Mac. Harbor Control and
Settings → General auto-try this path; manual `harbor://pair` URI remains as
fallback.

**Speech context:** While Harbor Control is active, Handy fetches
`GET /v1/workspaces` and feeds public labels (directory basenames, agents,
Codex/Claude aliases) into Whisper `initial_prompt` and post-STT custom-word
correction. Saying a sidebar directory name switches that workspace when the
match is unique.

Derived device secret is stored in macOS Keychain service
`ai.handy.terminal-harbor`. Settings keep `harbor_server_id`,
`harbor_client_id`, `harbor_base_url`, and `preferred_control_mode`.

## Harbor side

API **1.7.0** `POST /v1/voice/intent` in:

1. `terminal-harbor-mobile/openapi/harbor-mobile.yaml`
2. `terminal-harbor/wezterm-gui/src/harbor_mobile.rs`
3. Handy client above

See Harbor mobile `docs/voice-control.md` and desktop `docs/mobile-bridge.md`.

## Desktop Control vs Harbor

| | Desktop Control | Harbor Control |
|--|-----------------|----------------|
| Activation | Preferred = Desktop + BLE/shortcut; voice「デスクトップ操作」 | Preferred = Harbor + BLE/shortcut; voice「ハーバー」 |
| Execution | Local Ollama tools → frontmost app | Harbor HTTP |
| Paste | May inject keys | Suppressed |

Modes are mutually exclusive. Voice switches update `preferred_control_mode`
so the next double-tap follows the last mode you asked for.

## Verification

- Pair URI → Keychain secret → signed `/v1/voice/intent`
- Harbor mode: “クロードの方” / “コーデックス” switches and focuses; ambiguous no-op
- Ollama down on Harbor → `model_unavailable`; state unchanged
- Harbor 中「デスクトップ操作」→ Desktop Control + preferred=desktop
- Desktop 中「ハーバー」→ Harbor + preferred=harbor
- Normal STT still pastes when neither mode is active
