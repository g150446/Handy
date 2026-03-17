# Control Mode — 実装・保守ガイド

## 概要

Control Mode（コントロールモード）は、音声 + Groq LLM のファンクションコールでデスクトップを操作する機能。BLE デバイスのダブルクリックで起動し、画面右上にコントロールウィンドウが表示される。

---

## ファイル構成

| ファイル | 役割 |
|---|---|
| `src-tauri/src/control.rs` | Control Mode の全ロジック（唯一の変更対象） |
| `src-tauri/src/managers/model.rs` | `ModelInfo` 構造体・`get_available_models()` |
| `src-tauri/src/managers/transcription.rs` | `TranscriptionManager::load_model()` |
| `src-tauri/src/commands/models.rs` | `set_active_model` Tauri コマンド（参照実装） |

---

## ツール一覧（LLM に提供される function）

| ツール名 | トリガー例 | 実装関数 |
|---|---|---|
| `undo_last_input` | "取り消して", "undo" | `execute_undo_last_input()` |
| `send_enter_key` | "エンターを押して", "press enter" | `execute_enter_key_action()` |
| `replace_input` | "英語に直して", "fix typo" | `execute_replace_input()` |
| `select_transcription_model` | "Whisperに切り替えて", "switch model" | `execute_select_model_action()` |

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

`TranscriptionManager::load_model()` は同期ブロッキング関数であり、内部でディスク I/O とGPU/Vulkan の初期化を行う（Windows では数十秒かかることがある）。

Control Mode の音声処理パイプラインは `tauri::async_runtime::spawn` の中で動作しており、`load_model()` を直接呼ぶと **Tokio のワーカースレッドをブロック**する。Windows では macOS（Metal）と異なり Vulkan/DirectML の初期化が大幅に遅く、ランタイムが応答できなくなりモデルが実際には切り替わらない症状が発生した。

`spawn_blocking` を使うことで専用のブロッキングスレッドプールで実行され、Tokio ランタイムへの影響を排除できる。

> **注意**: `commands/models.rs` の `set_active_model` Tauri コマンドは直接 `load_model()` を呼んでいるが、そちらはフロントエンドから独立した async タスクとして起動されるため問題が起きにくい。Control Mode はパイプラインの末尾で呼ばれるため条件が異なる。

### エラーハンドリング

| ケース | 返却メッセージ |
|---|---|
| ModelManager が見つからない | `[エラー: モデルマネージャーが利用できません]` |
| TranscriptionManager が見つからない | `[エラー: 文字起こしマネージャーが利用できません]` |
| モデルが未ダウンロード / 不在 | `[エラー: モデル 'X' は利用できません]` |
| load_model() が Err を返した | `[エラー: モデルの読み込みに失敗しました: <詳細>]` |
| load_model() がパニック | `[エラー: モデル読み込みスレッドがパニックしました: <詳細>]` |

エラーはすべてコントロールウィンドウのメッセージとして表示され、ログにも `log::error!` で記録される。

---

## システムプロンプトの構造

`build_control_system_prompt()` がシグネチャ:

```rust
fn build_control_system_prompt(
    last_pasted: Option<&str>,
    downloaded_models: &[&ModelInfo],
    current_model: &str,
) -> String
```

モデルが存在する場合、以下のセクションをプロンプト末尾に追記する:

```
## Transcription Models
The following speech recognition models are installed:
- ID: `parakeet-tdt-0.6b-v3` | Name: Parakeet V3 | Languages: en, ja, ...
- ID: `whisper-large-v3-turbo` | Name: Whisper Turbo | Languages: multilingual
Current model: `parakeet-tdt-0.6b-v3`

Use `select_transcription_model` when the user asks to:
- switch / change / use a different model
- use a specific language model (e.g. "日本語のモデル", "English only")
- モデルを変える / 切り替える / 〜に変更して
```

---

## ツールの追加方法（将来の拡張）

`submit_prompt()` 内の `tools` ベクタに要素を追加し、`match name.as_str()` に対応するアームを追加する。

### ステップ

1. `tools.push((name, description, Option<serde_json::Value>))` でツールを追加
2. `match name.as_str()` に `"tool_name" => { ... }` を追加
3. アクション実行後、`inner.messages` に結果を push して `inner.is_sending = false` にする
4. `emit_state_changed()` と `schedule_auto_exit()` を呼ぶ

既存の `"send_enter_key"` ブロックを最小の参照実装として使うこと。

---

## 自動終了の動作

`schedule_auto_exit(app_handle, session_id)` がアクション完了後に呼ばれ、4 秒後にコントロールモードを自動終了する。セッション ID が変わっていた場合（ユーザーが手動で終了した場合）は終了をスキップする。

---

---

## Windows での `load_model()` ブロック問題（修正済み）

### 症状

Windows でコントロールモードの音声によるモデル切り替えを行うと、LLM が `select_transcription_model` ツールを呼び出して成功メッセージ（`[モデルを 'X' に切り替えました]`）が表示されるにもかかわらず、実際の STT モデルが切り替わらなかった。

### 原因

`TranscriptionManager::load_model()` は同期ブロッキング関数。コントロールモードの音声処理パイプラインは `tauri::async_runtime::spawn` の内側で動作しており、`load_model()` を直接 `.await` なしで呼ぶと Tokio のワーカースレッドをブロックする。

Windows では macOS（Metal）と異なり Vulkan/DirectML の GPU 初期化が数秒〜数十秒かかる。このためランタイムが応答できなくなり、モデルのロードが完了しない（または emit() のディスパッチが詰まる）状態になった。

`set_active_model` Tauri コマンド（UI からの切り替え）は独立した async タスクで起動されるため同症状が出にくかった。

### 修正

`execute_select_model_action()` 内で `tokio::task::spawn_blocking` を使用し、`load_model()` を専用のブロッキングスレッドプールで実行するよう変更した。

```rust
let tm_arc = transcription_manager.inner().clone();
let model_id_for_load = model_id.clone();
let load_result = tokio::task::spawn_blocking(move || tm_arc.load_model(&model_id_for_load))
    .await;
```

`State<Arc<T>>` から `.inner().clone()` で `Arc<T>` を取得し、`'static` な所有権を持った状態で `spawn_blocking` に渡す。

---

## 関連する既知の制約

- **モデルの並行ロード競合**: `initiate_model_load()` がバックグラウンドで旧モデルをロード中に `execute_select_model_action` が新モデルをロードする競合が理論上あり得る。実際には音声処理パイプラインの順序により発生しにくいが、将来的には `is_loading` フラグで排他制御を強化することを検討。
- **アイドルタイマーによる即時アンロード**: `model_unload_timeout` が設定されている場合、モデル切り替え後に次の transcribe() が実行されるまでの間にアイドルタイマーがモデルをアンロードすることがある。その後 `initiate_model_load()` が新しい `settings.selected_model` から正しくロードするため、動作上の問題はない（ただし初回 transcribe のレイテンシは増える）。
