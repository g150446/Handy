# HarnessNode + Handy 統合ガイド

Seeed XIAO nRF52840 Sense に書き込んだ `nordic-main` ファームウェア（BLE デバイス名: `HarnessNode`）と Handy アプリの連携について説明します。

---

## 概要

HarnessNode は腕のジェスチャーを IMU（LSM6DS3TR-C）で検出し、BLE 経由で Handy へ録音開始・停止を通知するデバイスです。ユーザーはボタンを押さずに、腕を水平から持ち上げるジェスチャーだけでプッシュトゥトーク（PTT）録音を開始できます。

ジェスチャー判別はファームウェア側で完結しており、Handy は BLE イベント（`0x01` / `0x02`）を受け取るだけです。

---

## BLE 認識

Handy の BLE マネージャー（`src-tauri/src/ble/mod.rs`）の `is_known_ble_device()` は名前に `HarnessNode` / `XIAOVoice` / `AtomEchoS3R` を含むデバイスをスキャン一覧に出す。`HarnessNode-Plus2` / `HarnessNode-PlusSE` もこれに含まれる。

スキャンは GATT 接続しない。接続は設定画面の **Connect** だけ。起動時の自動接続はしない。Connect 成功後にリンクが切れたときだけ、同じ PeripheralId へ再接続する（名前フォールバックはしない）。

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

### XIAO nRF52840 Sense（`HarnessNode`）

1. `nordic-main` を書き込む（`nordic-main/build_and_flash.sh`）。
2. 起動すると `HarnessNode` として広告する。
3. Handy の BLE 設定で Scan → 選択 → Connect。

### M5StickC Plus SE（`HarnessNode-PlusSE`）

起動直後は広告しない。

1. Stick の **BtnA 短押し**（画面が `ADV`）。
2. Handy で音声ソース BLE → Scan → `HarnessNode-PlusSE` → Connect。
3. Stick が `connected` になり、相手 MAC を出す。

Handy を再起動したら、また Scan → Connect が必要（起動時自動接続なし）。

詳細は harness-node リポの `docs/stickc_plus_se_guide.md`。

---

## 注意事項 / 制限

- **文字起こしには Mac マイクを使用**: 録音は Mac のマイクロフォンが主系統です。BLE 経由の PCM オーディオ（`recording_samples`）も `device_button_active` が true の間は蓄積されますが、Whisper / Parakeet への入力は Mac マイク録音が主体です。BLE PCM の利用方法は実装の状態によります。
- **ジェスチャー精度**: ジェスチャーしきい値（`GESTURE_ACTIVE_Z_MIN/MAX`, `GESTURE_SETTLE_Z_MIN`, `GESTURE_WINDOW_MS`）はファームウェアにハードコードされています。誤検知が多い場合はファームウェアを再ビルドして調整してください。
- **OTA アップデート**: ファームウェアの更新は BLE OTA で行えます（`mac_client/ota_updater.py --device HarnessNode ../nordic-main/ota_update.bin`）。Handy と HarnessNode が同時に接続している状態では OTA は実行しないでください。
- **旧ファームウェア（nrf52-voice / VoiceBridge52）との互換性なし**: `nrf52-voice` の BLE プロトコルとは異なります。Handy は `HarnessNode` のデバイス名で認識します。

## voice-harness-android との関係

Android アプリ（`voice-harness-android`）の「優先接続」トグル（Mac Handy / Android）は、**HarnessNode 上の dual-client primary をどちらが取るか**を切り替えるものです。Android が音声やイベントを Handy へネットワーク中継する機能はありません。

| 目的 | 必要な接続 |
|------|------------|
| Handy で Mac に文字を貼る | **Mac ↔ HarnessNode の直接 BLE**（現状どおり） |
| Android だけで ASR / LLM / TTS | Android ↔ HarnessNode のみ（Handy 不要） |
| 両方接続して切替 | 両方 BLE 接続 + Android 側トグルで primary を選択 |

Handy を使う場合は、本ドキュメントの「接続セットアップ」に従い Mac から `HarnessNode` へ直接接続してください。詳細な dual-connection 設計は harness-node 側の `docs/ble_dual_connection_audio_lessons.md` を参照してください。
