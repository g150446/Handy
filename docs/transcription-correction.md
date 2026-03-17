# Transcription Auto Correction — 実装・保守ガイド

## 概要

Auto Correction は、音声認識後のテキストを Groq LLM に送り、STT（音声→テキスト）変換ミスだけを自動修正する機能。通常ショートカットでの録音停止直後、Post-processing（カスタムプロンプト）の前に実行される。

設定: **Settings → Post-processing → Auto Correction** (`transcription_correction_enabled: bool`)

---

## ファイル構成

| ファイル | 役割 |
|---|---|
| `src-tauri/src/actions.rs` | `correct_transcription()`, `build_correction_prompt()` — 唯一の変更対象 |
| `src-tauri/src/settings.rs` | `transcription_correction_enabled`, `custom_words`, `GROQ_PROVIDER_ID` |
| `src-tauri/src/llm_client.rs` | `send_chat_completion_with_schema()` — HTTP 呼び出し |

---

## 処理フロー

```
録音停止
  └── tm.transcribe(samples)
        └── (Chinese variant conversion)
              └── correct_transcription()          ← Auto Correction
                    └── build_correction_prompt()
                    └── Groq API (15s timeout)
                    └── extract_transcription_from_response()
                          └── (変化あれば) final_text 更新
              └── (post_process フラグが true なら)
                    └── post_process_transcription()  ← LLM Post-processing
              └── paste(final_text)
```

Auto Correction は `post_process == false` のとき（通常ショートカット）のみ動作する。
`post_process == true`（Post-processing ショートカット）のときは実行されない。

---

## 主要関数

### `correct_transcription(app, transcription) -> Option<String>`

```
src-tauri/src/actions.rs — correct_transcription()
```

1. `transcription_correction_enabled` を確認（false → None）
2. Groq プロバイダーと API キーを取得（未設定 → None）
3. モデルを取得（設定値 or `CORRECTION_DEFAULT_MODEL`）
4. `build_correction_prompt(&settings.custom_words)` でシステムプロンプトを生成
5. `send_chat_completion_with_schema()` を 15 秒タイムアウト付きで呼び出し
6. `extract_transcription_from_response()` で JSON から `transcription` フィールドを抽出
7. 元テキストと同一なら None（変更なし）、異なれば Some(corrected)

### `build_correction_prompt(custom_words) -> String`

修正ポリシーを定義したシステムプロンプトを返す。custom_words が空でない場合はカスタムワードセクションを末尾に追加する。

### `extract_transcription_from_response(content) -> Option<String>`

LLM レスポンスから `{"transcription": "..."}` を取り出す。直接 JSON parse → `{...}` ブロック抽出の順でフォールバック。

---

## プロンプト設計方針

`build_correction_prompt()` のプロンプトは「置き換えのみ・追加/削除なし」原則に基づいて設計されている。

### 修正してよいこと

| 種別 | 説明 |
|---|---|
| 同音異義語の誤変換 | 「機会」→「機械」など、文脈から判断して置き換える |
| 助詞の誤認識 | 「は」→「が」のように既存の助詞を別の助詞に置き換える（追加はしない） |
| 数字・単位の誤認識 | 例: 「さんびゃく」→「300」 |
| 漢字の文脈誤変換 | 文章全体の文脈から、より適切な漢字・熟語に置き換える |
| 英単語の音声誤変換 | 「愛」→「AI」、「アップ」→「app」など、文脈から英語表記が適切な場合 |

### 絶対にしてはいけないこと

- 語・助詞・句読点を追加する（テキストを長くしない）
- 語を削除する（テキストを短くしない）
- 語順・文構造を変える
- 表現を言い換える・改善する
- 文法を正しくする
- 内容を補足する

迷った場合は修正せず、入力テキストをそのまま返す。

### なぜこの設計か

過去に発生した問題（プロンプト改訂の背景）:

| 問題 | 原因 | 修正内容 |
|---|---|---|
| 語の追加が行われた | 「助詞の欠落」という表現が追加を正当化 | 「欠落」→「誤認識（置き換えのみ）」に変更 |
| 曖昧な改変が行われた | 「文脈に合わない語の誤認識」が範囲広すぎ | 削除 |
| 追加禁止が機能しなかった | 厳守事項に明示的な禁止がなかった | 「語・助詞・句読点を追加しない」を明示 |
| 漢字/英単語の誤変換を見落とした | 文脈考慮の指示がなかった | 「修正の判断基準」セクションを追加 |

---

## カスタムワード

**Settings → Post-processing → Custom Words** でユーザーが登録した固有名詞・専門用語は `settings.custom_words: Vec<String>` に保存される。

`build_correction_prompt()` がプロンプト末尾に以下のセクションとして追加する:

```
## カスタムワード（変換候補）
以下はユーザーが登録した固有名詞・専門用語です。
音声認識テキストに近い発音の誤変換が含まれる場合は、下記の正しい表記に置き換えてください。
語の追加・削除は行わず、置き換えのみ行ってください。
それ以外の箇所は変更しないでください。

- <word1>
- <word2>
```

---

## 設定キー

| キー | 型 | 説明 |
|---|---|---|
| `transcription_correction_enabled` | `bool` | Auto Correction の有効/無効 |
| `custom_words` | `Vec<String>` | カスタムワード一覧 |
| `post_process_models[GROQ_PROVIDER_ID]` | `String` | 使用モデル（空なら `CORRECTION_DEFAULT_MODEL`） |

`CORRECTION_PROVIDER_ID` = `GROQ_PROVIDER_ID`（`settings.rs` 定義）
`CORRECTION_DEFAULT_MODEL` = `"openai/gpt-oss-120b"`（`actions.rs` 定義）

---

## Auto Correction と LLM Post-processing の違い

| | Auto Correction | LLM Post-processing |
|---|---|---|
| トリガー | 通常ショートカット（常時） | Post-processing ショートカットのみ |
| 目的 | STT 変換ミスのみ修正 | 自由なテキスト変換（翻訳・要約など） |
| プロンプト | 固定（`build_correction_prompt()`） | ユーザー定義（Settings で設定） |
| プロバイダー | Groq 固定 | 複数プロバイダー対応 |
| 副作用 | テキストの意味・量を変えない | 自由な変換を許容 |
| 実行順序 | Post-processing より前 | Auto Correction の後 |

どちらも `post_processed_text` / `post_process_prompt` としてヒストリに記録される。

---

## エラーハンドリング

| ケース | 動作 |
|---|---|
| `transcription_correction_enabled == false` | スキップ、None を返す |
| Groq API キー未設定 | warn ログ、None を返す（元テキストをそのまま使用） |
| 15 秒タイムアウト | warn ログ、None を返す |
| API エラー | warn ログ、None を返す |
| JSON パース失敗 | warn ログ、None を返す |
| 修正結果が元テキストと同一 | None を返す（変更なしとして扱う） |

None を返した場合、呼び出し元 (`actions.rs` の `stop()`) は `final_text` を変更せずにそのままペーストする。

---

## 関連する既知の制約

- **Post-processing との排他**: `post_process == true` のショートカットでは Auto Correction は実行されない。両方の効果を得たい場合は通常ショートカット + Post-processing ショートカットを別々に使う必要がある。
- **モデル依存**: プロンプトの効果は使用する Groq モデルに依存する。モデルを変更した場合はプロンプトの動作を再確認すること。
- **タイムアウト固定**: 15 秒のタイムアウトは `correct_transcription()` にハードコードされている。設定からは変更できない。
