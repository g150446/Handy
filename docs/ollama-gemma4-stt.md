# Ollama Gemma 4 Speech-to-Text

Handy はローカル STT（Whisper / Parakeet など）に加え、**Ollama 上の Gemma 4** を音声認識エンジンとして使える。

音声入力があるのは **E2B と E4B だけ**。12B / 26B / 31B は Text + Image のみなのでカタログに出さない。

| Handy id | Ollama タグ | サイズ | 備考 |
|----------|-------------|--------|------|
| `gemma4-e2b` | `gemma4:e2b` | 7.2 GB | 軽量・高速 |
| `gemma4-e4b` | `gemma4:e4b` | 9.6 GB | `gemma4:latest` 相当。認識はこちらが強い |

関連コード: `src-tauri/src/ollama_stt.rs`

---

## 前提

- [Ollama](https://ollama.com) が起動している（既定 `http://localhost:11434`）
- モデルは Handy の「ダウンロード」か、手元で `ollama pull` する

```bash
ollama pull gemma4:e2b
ollama pull gemma4:e4b
```

Handy は Ollama のモデルファイルをコピーしない。`GET /api/tags` で有無を見て `is_downloaded` にする。Handy の削除ボタンは出さない（`ollama rm` もしない）。

---

## 設定

| 項目 | 内容 |
|------|------|
| UI | 設定 → モデル → **Ollama URL** |
| 設定キー | `ollama_base_url` |
| 既定 | `http://localhost:11434` |
| 環境変数 | `OLLAMA_HOST` があればそれを優先（スキームなし可。`/v1` は除く） |
| コマンド | `change_ollama_base_url_setting` |

後処理 LLM の「Ollama / Custom」（`http://localhost:11434/v1`）とは別。STT はネイティブ `/api/chat` を使う。

言語セレクタと「英語に翻訳」は Gemma 4 選択時も有効。プロンプトに載せるだけ（Whisper の `language` パラメータではない）。

---

## 処理フロー

```
録音停止（16 kHz mono f32）
  └── TranscriptionManager::transcribe
        └── encode_wav_bytes
        └── POST {ollama}/api/chat
              model: gemma4:e2b | gemma4:e4b
              messages[0].images: [base64 WAV]
              think: false
              options.temperature: 0
              keep_alive: model_unload_timeout に合わせる
        └── 本文のみ返す
        └── custom words / filler filter（他エンジンと同じ）
```

Ollama の公式 chat スキーマに `audios` はまだない。マルチモーダル添付は `messages[].images` に載せる（CLI の `ollama run gemma4:e4b ./clip.wav ...` と同じ枠）。

thinking は必ずオフ。Gemma 4 は thinking 付きだと遅延とノイズが増える。

---

## ダウンロード（pull）

設定のダウンロードは `POST /api/pull`（ストリーム）。進捗は既存の `model-download-progress` に載せる。

すでに `ollama list` にあれば pull はスキップして完了扱い。

Unload timeout:

| Handy | Ollama `keep_alive` |
|-------|---------------------|
| Never | `-1` |
| Immediately | `0` |
| 2–15 min / 1 h / debug 5s | `"2m"` など |

手動 unload は空 messages + `keep_alive: 0`。

---

## ファイル構成

| ファイル | 役割 |
|----------|------|
| `src-tauri/src/ollama_stt.rs` | URL 正規化、tags / pull / chat、プロンプト |
| `src-tauri/src/managers/model.rs` | `EngineType::Ollama`、カタログ、pull、tags 同期 |
| `src-tauri/src/managers/transcription.rs` | load / transcribe / unload |
| `src-tauri/src/audio_toolkit/audio/utils.rs` | `encode_wav_bytes` |
| `src-tauri/src/settings.rs` | `ollama_base_url` |
| `src/components/settings/models/ModelsSettings.tsx` | Ollama URL 欄 |

---

## トラブルシュート

| 症状 | 確認 |
|------|------|
| ダウンロード失敗 / 「Ollama is not reachable」 | `ollama serve`、URL、ファイアウォール |
| 選択できない | モデル画面を開き直す（tags 再取得）。または `ollama list` に `gemma4:e2b` / `gemma4:e4b` |
| 書き起こしが遅い・前置きが多い | thinking が効いていないか。Handy は `think: false` を送る |
| 12B などを使いたい | 音声エンコーダがない。E2B / E4B を使う |
