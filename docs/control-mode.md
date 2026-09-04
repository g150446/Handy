# Desktop Control（デスクトップ操作）— 実装・保守ガイド

## 概要

**Desktop Control** は、音声 + ローカル LLM の function calling でデスクトップを操作する機能。画面右上にコントロールウィンドウ（label `control`）が表示される。

旧称: Control Mode / Groq Control Mode（現在の既定 LLM は Ollama）。

| 項目 | 値 |
|------|-----|
| Provider | `custom`（`http://localhost:11434/v1`） |
| Default model | `lfm2.5:latest`（`post_process_models["custom"]`） |
| API キー | 不要（ローカル Ollama） |
| 起動 | `preferred_control_mode = desktop` のとき BLE ダブルタップ / 優先ショートカット。Harbor 中の音声「デスクトップ操作」でも可 |
| Harbor へ | 音声「ハーバー」または tool `switch_to_harbor_control`（優先設定も更新） |

**モード全体（Harbor との関係・優先設定）:** [`preferred-control-mode.md`](./preferred-control-mode.md)

---

## ファイル構成

| ファイル | 役割 |
|---|---|
| `src-tauri/src/control.rs` | Desktop Control の本体（tools / 実行 / ウィンドウ） |
| `src-tauri/src/preferred_control.rs` | 優先モード・音声フレーズ・BLE/ショートカット共通トグル |
| `src-tauri/src/managers/model.rs` | `ModelInfo`・`get_available_models()` |
| `src-tauri/src/managers/transcription.rs` | `TranscriptionManager::load_model()` |
| `src-tauri/src/commands/models.rs` | `set_active_model` Tauri コマンド（参照実装） |
| `src/components/conversation/ConversationWindow.tsx` | UI |
| 設定 UI（Ollama モデル） | `src/components/settings/openrouter/OpenRouterSettings.tsx`（provider `custom`） |

---

## ツール一覧（LLM に提供される function）

| ツール名 | トリガー例 | 実装 |
|---|---|---|
| `undo_last_input` | "取り消して", "undo" | `execute_undo_last_input()` |
| `send_enter_key` | "エンターを押して", "press enter" | `execute_enter_key_action()` |
| `replace_input` | "英語に直して", "fix typo" | `execute_replace_input()` |
| `select_transcription_model` | "Whisperに切り替えて", "switch model" | `execute_select_model_action()` |
| `switch_to_harbor_control` | "ハーバー", "harbor mode" | `preferred_control::activate_harbor` |
| `switch_to_normal_input` | "通常入力", "normal mode" | `preferred_control::activate_normal` |

モード切替は LLM の前に **ローカルフレーズマッチ**も行う（確実・低遅延）。

### `select_transcription_model` の動的生成ルール

- **ダウンロード済みモデルのみ**を `enum` に列挙する（`is_downloaded && !is_downloading`）
- ダウンロード済みモデルが 0 件の場合はツール自体を追加しない
- システムプロンプトにモデル一覧・現在のモデル・使用トリガー例を追記する

---

## `execute_select_model_action()` の実装詳細

```
src-tauri/src/control.rs — execute_select_model_action()
```

### 処理フロー

```
1. ModelManager::get_available_models() でモデル一覧を取得
2. model_id と is_downloaded を検証（失敗 → エラーメッセージ返却）
3. TranscriptionManager の Arc を clone
4. tokio::task::spawn_blocking で load_model() を別スレッドで実行
5. 成功 → settings.selected_model を更新して write_settings()
6. 結果メッセージを返す（[モデルを 'X' に切り替えました]）
```

### なぜ `spawn_blocking` が必要か

`TranscriptionManager::load_model()` は同期ブロッキング関数であり、内部でディスク I/O と GPU/Vulkan の初期化を行う（Windows では数十秒かかることがある）。

Desktop Control の音声処理パイプラインは `tauri::async_runtime::spawn` の中で動作しており、`load_model()` を直接呼ぶと **Tokio のワーカースレッドをブロック**する。Windows では macOS（Metal）と異なり Vulkan/DirectML の初期化が大幅に遅く、ランタイムが応答できなくなりモデルが実際には切り替わらない症状が発生した。

`spawn_blocking` を使うことで専用のブロッキングスレッドプールで実行され、Tokio ランタイムへの影響を排除できる。

---

## システムプロンプト

`build_control_system_prompt()` が生成。内容には:

- ツール利用方針
- 最後の貼り付けテキスト（あれば）
- ダウンロード済み転写モデル一覧
- Harbor / 通常入力への切替フレーズ

---

## 自動終了

アクション成功後 roughly 4 秒で Desktop Control を自動終了する（`schedule_auto_exit`）。  
**モード切替 tools**（Harbor / 通常）では auto-exit をスケジュールしない。

---

## ターミナル向けキー操作

WezTerm 等の GPU 端末では通常の Cmd+Z が効かないことがある。Enter / undo / replace はフロント最前面アプリ名を見て Ctrl+U 等に分岐する。

詳細: [`control-mode-terminal-enter-key.md`](./control-mode-terminal-enter-key.md)

---

## BLE との関係

BLE ダブルタップは **優先モード**をトグルする（常に Desktop ではない）。  
過去の control-mode 専用 BLE デバッグメモ: [`control-mode-ble.md`](./control-mode-ble.md)
