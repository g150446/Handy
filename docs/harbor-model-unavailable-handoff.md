# Harbor Control 意図解析（OpenRouter）

Harbor Control のワークスペース切替は **Handy STT + Harbor 側 OpenRouter**。ローカル Ollama は使わない。

## 経路

```text
Handy STT
  → HMAC POST /v1/voice/intent
  → Harbor handle_voice_intent
       ├─ resolve_voice_workspace(transcript)   // ローカル照合（フォールバック）
       └─ infer_voice_intent
            POST https://openrouter.ai/api/v1/chat/completions
            model: deepseek/deepseek-v4-flash-0731
            timeout: 10s（Handy→Harbor HTTP は 20s）
```

成功時は候補リストの directory / name / id を `target` にし、一意なら `executed`。  
失敗 + ローカル照合も失敗 → `model_unavailable`（文言に HTTP 状態・キー未設定・timeout などを含む）。

## API キー（Harbor / Handy 共通の解決順）

1. プロセス環境変数 `OPENROUTER_API_KEY`
2. macOS `launchctl getenv OPENROUTER_API_KEY`
3. `~/.zshrc` の `export OPENROUTER_API_KEY=...`
4. アプリ設定（Harbor: `settings-v1.json` の `openrouter_api_key` / Handy: Settings → Desktop）

キー・transcript・モデル応答はログに出さない。OpenRouter 以外の URL へは送らない。

## 実装

| 場所 | 役割 |
| ---- | ---- |
| Harbor `wezterm-gui/src/harbor_mobile.rs` | intent / OpenRouter POST（`Content-Length` 必須） |
| Harbor `wezterm-gui/src/harbor_settings.rs` | URL / model / timeout / キー解決 |
| Handy `src-tauri/src/harbor_control.rs` | pair / voice client / outcome → status |
| Handy `src/components/settings/openrouter/OpenRouterSettings.tsx` | Desktop Control のキーとモデル |

関連: [`harbor-control-architecture.md`](./harbor-control-architecture.md) · Harbor `docs/mobile-bridge.md`
