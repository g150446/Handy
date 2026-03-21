# XIAOVoice + Handy 統合ガイド

Seeed XIAO nRF52840 Sense に書き込んだ `nrf52-handy` ファームウェア（BLE デバイス名: `XIAOVoice`）と Handy アプリの連携について説明します。

---

## 概要

XIAOVoice は腕のジェスチャーを IMU（LSM6DS3TR-C）で検出し、BLE 経由で Handy へ録音開始・停止を通知するデバイスです。ユーザーはボタンを押さずに、腕を水平から持ち上げるジェスチャーだけでプッシュトゥトーク（PTT）録音を開始できます。

ジェスチャー判別はファームウェア側で完結しており、Handy は BLE イベント（`0x01` / `0x02`）を受け取るだけです。

---

## BLE 認識

Handy の BLE マネージャー（`src-tauri/src/ble/mod.rs`）の `is_known_ble_device()` 関数に `XIAOVoice` が登録されています。BLE スキャン中にこのデバイス名が見つかると、既知デバイスとして自動認識されます。

---

## イベントフロー

### 1. ユーザーがジェスチャーを行う

腕を水平に近い状態から持ち上げて静止させます（ファームウェア内の 3 条件 AND 判定）。

```
ユーザー: 腕を水平近傍から持ち上げて静止
    ↓
ファームウェア:
  - motion_active 検出（z 軸: -3.0 〜 +3.0 m/s²）→ BLE 送信: [0x00][0x55][0x10][z f32 LE]
  - motion_settled 検出（z 軸: ≥ 8.0 m/s²、2000ms 以内）→ BLE 送信: [0x00][0x55][0x11][z f32 LE]
  - 3 条件成立 → recording_requested = true
    ↓
ファームウェア: DMIC 録音開始 + BLE 送信: [0x00][0x55][0x01]（recording_start）
```

### 2. Handy が `0x01`（recording_start）を受信

```
Handy (ble/mod.rs):
  - is_recording = true
  - device_button_active = true
  - send_ble_button_event(true) 呼び出し（プッシュトゥトーク押下）
    ↓
TranscriptionCoordinator:
  - Mac マイクロフォン録音開始
  - BLE PCM パケット蓄積開始（recording_samples に追加）
```

### 3. BLE 音声パケットが到着

```
ファームウェア: [seq][0xAA][PCM data...] を Notify で送信
    ↓
Handy: PCM サンプルを recording_samples に蓄積
       （device_button_active = true の間、継続）
```

### 4. ユーザーが次のジェスチャーを行う（録音停止）

腕を再び動かすと `motion_active` が検出されます。

```
ユーザー: 次の motion_active ジェスチャー
    ↓
ファームウェア: stop_requested = true
  - DMIC 録音停止 + BLE 送信: [0x00][0x55][0x02]（recording_stop）
```

### 5. Handy が `0x02`（recording_stop）を受信

```
Handy (ble/mod.rs):
  - device_button_active = true を確認
  - send_ble_button_event(false) 呼び出し（プッシュトゥトーク解放）
    ↓
TranscriptionCoordinator:
  - Mac マイクロフォン録音停止
  - 音声データを Whisper / Parakeet に渡して文字起こし実行
  - 結果テキストを出力
```

---

## デバッグログの確認

Handy のログ（`tracing` / `log` クレート出力）で以下のエントリを確認できます。

| ログエントリ | 意味 |
|------------|------|
| `motion active z=<value>` | `0x10` イベント受信、z 軸加速度値（info レベル） |
| `motion settled z=<value>` | `0x11` イベント受信、z 軸加速度値（info レベル） |
| `device button pressed` / `send_ble_button_event(true)` に相当するログ | `0x01` 受信、PTT 押下処理 |
| `device button released` / `send_ble_button_event(false)` に相当するログ | `0x02` 受信、PTT 解放処理 |

motion_active / motion_settled の z 値は info レベルで出力されるため、ログレベルを `INFO` 以上に設定していれば確認できます。

---

## 接続セットアップ

1. XIAO nRF52840 Sense に `nrf52-handy` ファームウェアを書き込みます（`nrf52-handy/build_and_flash.sh`）。
2. デバイスを起動すると青色 LED が点滅し、`XIAOVoice` としてアドバタイジングを開始します。
3. Handy アプリの BLE 設定画面でデバイスをスキャンし、`XIAOVoice` を選択してペアリングします。
4. 接続が確立すると XIAO の LED が緑色に変わります。

以降は Handy 起動時に自動で再接続されます（`is_known_ble_device()` により自動認識）。

---

## 注意事項 / 制限

- **文字起こしには Mac マイクを使用**: 録音は Mac のマイクロフォンが主系統です。BLE 経由の PCM オーディオ（`recording_samples`）も `device_button_active` が true の間は蓄積されますが、Whisper / Parakeet への入力は Mac マイク録音が主体です。BLE PCM の利用方法は実装の状態によります。
- **ジェスチャー精度**: ジェスチャーしきい値（`GESTURE_ACTIVE_Z_MIN/MAX`, `GESTURE_SETTLE_Z_MIN`, `GESTURE_WINDOW_MS`）はファームウェアにハードコードされています。誤検知が多い場合はファームウェアを再ビルドして調整してください。
- **OTA アップデート**: ファームウェアの更新は BLE OTA で行えます（`mac_client/ota_updater.py --device XIAOVoice ../nrf52-handy/ota_update.bin`）。Handy と XIAOVoice が同時に接続している状態では OTA は実行しないでください。
- **旧ファームウェア（nrf52-voice / VoiceBridge52）との互換性なし**: `nrf52-voice` の BLE プロトコルとは異なります。Handy は `XIAOVoice` のデバイス名で認識します。
