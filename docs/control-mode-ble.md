# Control Mode BLE修正メモとUSBシリアルテスト手順

## 概要

Atom Echo S3R を BLE で `Handy` に接続した状態で、control mode を終了した直後の最初の single-click が効かない不具合を修正した。

今回のデバッグでは、`Handy` 側の BLE イベント解釈と、`voice-bridge-ble` 側ファームウェアの実際の通知仕様にズレがあることが分かった。あわせて、USB シリアル経由で single-click / double-click をエミュレートできるようにし、実機再現を安定して行えるようにした。

---

## 症状

修正前は次のような症状があった。

- control mode に入った直後、single-click していないのに録音がすぐ終わることがある
- control mode 終了後、最初の single-click が無反応になる
- control mode の window に transcription や Groq の返答が表示されないことがある

---

## 根本原因

### 1. `0x01` / `0x02` を「物理ボタンの press / release」と思い込んでいた

`Handy` 側は長い間、BLE の event を次のように解釈していた。

- `0x01` = ボタン press
- `0x02` = ボタン release
- `0x03` = double-click

しかし、実機と USB シリアルエミュレータで確認すると、control mode の app-initiated recording 中は事情が異なっていた。

- `0x03` は control mode 切り替え
- app が `0x01` を device に送って録音開始した直後、device は `recording_started ACK` として `0x01` を返す
- control mode 中の最初の stop は、firmware 側の状態によっては `0x02` 単体で返る

つまり control mode entry 直後の `0x01` は「ユーザーの stop click」ではなく、「device が app の start command を受け付けた通知」だった。

この ACK を実クリックとして処理すると、control mode に入った直後に勝手に stop してしまう。

### 2. control mode stop 時の BLE サンプル回収が遅れていた

もうひとつの問題は `AudioRecordingManager::stop_recording()` 側にあった。

修正前は app-initiated BLE stop のときに、

1. 別の async task を spawn
2. その task の中で `stop_recording_command().await`
3. 呼び出し元は blocking timeout で待つ

という形になっていた。

このため、タイミングによっては transcription が先に `0 samples` で始まり、少し後になってから本当の BLE 音声サンプルが回収されていた。

その結果、

- control mode window に transcription が出ない
- Groq の返答も出ない
- 空文字のまま処理が進む

という見え方になっていた。

---

## 修正内容

### Handy 側

主に以下を修正した。

#### 1. control mode entry 直後の `recording_started ACK (0x01)` を無視

`src-tauri/src/ble/mod.rs`

- control mode に入って app が録音開始した直後の `0x01` を、ユーザー操作ではなく ACK として 1 回だけ無視するフラグを追加
- これにより、control mode に入った瞬間に勝手に stop しなくなった

#### 2. control mode 中の最初の stop を `0x02` 単体でも受理

`src-tauri/src/ble/mod.rs`

- control mode 中は、`device_button_active == false` でも、録音継続中の `0x02` を stop acknowledgment として扱うようにした
- これにより、最初の single-click で確実に録音停止へ進めるようになった

#### 3. BLE stop を nested spawn せず、その場で `await`

`src-tauri/src/managers/audio.rs`

- app-initiated BLE stop は `stop_recording_command().await` を直接待つように変更
- サンプル回収完了前に transcription が始まる race を解消

#### 4. control mode の返答表示を強化

`src-tauri/src/control.rs`

- Groq の plain-text reply を受けたとき、control window を再表示・再フォーカス
- auto-exit の猶予を 2 秒から 4 秒に延長
- 応答本文を `handy.log` に出力するようにした

---

## 修正後の期待動作

修正後は次の流れになる。

1. double-click で control mode に入る
2. control mode 用録音が自動開始される
3. この直後の `0x01` ACK は無視する
4. ユーザーが single-click すると録音停止
5. transcription 結果が control mode window に表示される
6. Groq の返答が control mode window に表示される
7. 数秒後に auto-exit
8. auto-exit 後の最初の single-click で通常録音が始まる

---

## USBシリアルで single-click / double-click をエミュレートする方法

### 前提

- `voice-bridge-ble` 側の最新ファームウェアが Atom Echo S3R に書き込まれていること
- USB 接続したデバイスのシリアルポートが見えていること
- この Mac では `/dev/cu.usbmodem1101` を使用した

今回の実装では、USB Serial/JTAG 経由で click を送れる。

### 利用できるコマンド

| コマンド | 意味 |
|---|---|
| `c` / `C` / `1` | single-click をエミュレート |
| `d` / `D` / `2` | double-click をエミュレート |
| `r` / `R` | 録音開始コマンド |
| `s` / `S` | 録音停止コマンド |
| `h` / `H` | ヘルプ表示 |

### シリアルモニタ例

```bash
python3 - <<'PY'
import serial

ser = serial.Serial('/dev/cu.usbmodem1101', 115200, timeout=0.1)
ser.write(b'h')
print(ser.read(4096).decode('utf-8', errors='replace'))
ser.close()
PY
```

### 手動テストの基本シーケンス

#### control mode に入る

```text
d
```

#### control mode 中の録音を止める

```text
c
```

#### control mode 終了後に通常録音を始める

```text
c
```

#### 通常録音を止める

```text
c
```

---

## 実際に確認できた回帰テストシーケンス

USB シリアルで次の順に送ると、control mode まわりの主要回帰を確認できる。

1. `d`  
   control mode に入る

2. `c`  
   control mode 用録音を stop

3. auto-exit を待つ

4. `c`  
   通常録音が start することを確認

5. `c`  
   通常録音が stop することを確認

このとき `handy.log` では概ね次の順になる。

- `BLE event: toggle control mode`
- `BLE event: ignoring control-mode recording_started ACK`
- `BLE event: control-mode stop acknowledged without press event`
- `BLE stop_recording_command returned ... samples`
- `Transcription result: ...`
- `Received Groq response for control mode ...`
- `Auto-exiting control mode ...`
- `BLE event: device button pressed – start recording`
- `BLE event: device button released – stop recording`

---

## 関連ファイル

### Handy

- `src-tauri/src/ble/mod.rs`
- `src-tauri/src/managers/audio.rs`
- `src-tauri/src/control.rs`
- `src-tauri/src/actions.rs`

### voice-bridge-ble

- `atom_echo_s3r/main.c`
- `atom_echo_s3r/CMakeLists.txt`

---

## 補足

`Handy` と `voice-bridge-ble` は別リポジトリとして扱うこと。修正や commit / push はそれぞれのリポジトリルートで別々に行う。
