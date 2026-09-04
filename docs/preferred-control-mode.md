# Preferred Control Mode — Desktop vs Harbor

Handy には **2 つのコントロール面**がある。どちらも通常入力（貼り付け）とは別経路で、**同時には 1 つだけ**有効。

| | **Desktop Control（デスクトップ操作）** | **Harbor Control** |
|--|------------------------------------------|--------------------|
| 目的 | 直前の貼り付け操作・Enter・モデル切替など | Terminal Harbor のワークスペース切替 |
| 実装 | `src-tauri/src/control.rs` + ローカル Ollama | `src-tauri/src/harbor_control.rs` → Harbor HTTP |
| 既定 LLM | Ollama `lfm2.5:latest`（`custom` / `:11434`） | Harbor 側 Ollama（bridge） |
| ウィンドウ | label `control` | label `harbor-control` |
| 貼り付け | ツール実行時はキー注入あり得る | なし（intent のみ） |

共通ロジック: **`src-tauri/src/preferred_control.rs`**

関連ドキュメント:

- Desktop 詳細: [`control-mode.md`](./control-mode.md)
- Harbor 詳細: [`harbor-control-architecture.md`](./harbor-control-architecture.md)
- BLE イベント履歴メモ: [`control-mode-ble.md`](./control-mode-ble.md)

---

## 優先モード設定（永続）

| 項目 | 内容 |
|------|------|
| 設定キー | `preferred_control_mode` |
| 値 | `harbor`（既定） / `desktop` |
| UI | 設定 → 一般 → **優先コントロールモード** |
| 保存 | Tauri store（再起動後も維持） |
| コマンド | `change_preferred_control_mode_setting` |

**この設定が決めるもの**

- HarnessNode stick の **ダブルタップ**（BLE `0x12` / legacy `0x03`）
- ショートカット binding **`harbor_control`**（表示名: 優先コントロールモード）

いずれも `preferred_control::toggle_preferred` を呼び、優先側を ON/OFF する。OFF 時は「通常入力モード」オーバーレイを表示。

---

## 起動・終了の経路

```
通常入力
   │
   ├─ ダブルタップ / 優先ショートカット
   │     → preferred が Harbor なら Harbor トグル
   │     → preferred が Desktop なら Desktop トグル
   │
   ├─ 音声（どちらかのコントロール中）
   │     → フレーズマッチで他方 or 通常へ（優先設定も更新）
   │
   └─ 相互排他
         Harbor ON → Desktop OFF
         Desktop ON → Harbor OFF
```

---

## 音声でのモード切替

Harbor サーバーは Handy のモードを変えられない。**Handy 側で STT 後にローカル判定**する。

| 意図 | 例（JA） | 例（EN） | 動作 |
|------|----------|----------|------|
| → Harbor | ハーバー / ハーバーモード / ターミナルハーバー | harbor / harbor mode | `preferred=harbor` + Harbor 開始 |
| → Desktop | デスクトップ / デスクトップ操作 / デスクトップモード | desktop / desktop control | `preferred=desktop` + Desktop 開始 |
| → 通常 | 通常入力 / 通常モード | normal mode / exit control | 両方 OFF（優先設定は変更しない） |

実装:

1. **フレーズマッチ**（LLM 不要）— `preferred_control::match_mode_switch_intent`
   - Harbor 中: `harbor_control::submit_transcript` の HTTP 前
   - Desktop 中: `control::submit_prompt` の LLM 前
2. **Desktop の LLM tools**（フォールバック）
   - `switch_to_harbor_control`
   - `switch_to_normal_input`

音声で Harbor ⇔ Desktop を切り替えると **`preferred_control_mode` も更新**される（次回のダブルタップ先が追従）。

---

## STT ルーティング（`actions.rs`）

```
if Harbor active  → harbor_control::submit_transcript  （貼り付けなし）
else if Desktop active → control::submit_voice_prompt （貼り付けなし）
else              → 通常 paste
```

---

## ファイル索引

| 役割 | パス |
|------|------|
| 優先モード・フレーズ・トグル | `src-tauri/src/preferred_control.rs` |
| Desktop Control | `src-tauri/src/control.rs` |
| Harbor Control | `src-tauri/src/harbor_control.rs` |
| STT 振り分け / ショートカット action | `src-tauri/src/actions.rs` |
| BLE ダブルタップ | `src-tauri/src/ble/mod.rs` |
| 設定フィールド | `src-tauri/src/settings.rs` → `PreferredControlMode` |
| 設定 UI | `src/components/settings/PreferredControlMode.tsx` |
| Desktop ウィンドウ | `src/components/conversation/ConversationWindow.tsx` |
| Harbor ウィンドウ | `src/components/conversation/HarborControlWindow.tsx` |

---

## 手動確認チェックリスト

1. 設定で Harbor → 再起動 → ダブルタップで Harbor ウィンドウ
2. 設定で デスクトップ操作 → 再起動 → ダブルタップで Desktop ウィンドウ
3. Harbor 中に「デスクトップ操作」→ Desktop に切替 + 設定が desktop に
4. Desktop 中に「ハーバーモード」→ Harbor に切替 + 設定が harbor に
5. 「通常入力」→ 通常モード + オーバーレイ
6. 優先ショートカットが BLE と同じ面をトグルすること
