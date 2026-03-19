# モデルダウンロード不具合修正 — 保守ガイド

## 概要

Parakeet V3 のダウンロードボタンを押すと UI が「0% Downloading」のまま進まない問題を調査・修正した際の記録。
根本原因は `modelStore.ts` のステート未クリーンアップだったが、あわせて `model.rs` の不要 HTTP リクエストと重複宣言も修正した。

---

## 発見した不具合一覧

### Bug 1 — `downloadProgress` がエラー時に残留する（主因）

**ファイル:** `src/stores/modelStore.ts`

`commands.downloadModel()` が失敗した場合（ネットワークエラー・HTTP エラー）、`downloadingModels[modelId]` は削除されていたが `downloadProgress[modelId]` と `downloadStats[modelId]` が削除されていなかった。

`getModelDisplayText()` は `Object.values(downloadProgress).length > 0` を見て「0% Downloading」を表示するため、ダウンロードが失敗した後も UI がその表示のまま固まり続けていた。

**修正箇所:** `downloadModel` 内の error result パスと catch ブロックの両方に `delete` を追加。

```typescript
// 修正前（どちらのパスも同じ問題があった）
set(produce((state) => {
  delete state.downloadingModels[modelId];
  // downloadProgress / downloadStats が残ったまま
}));

// 修正後
set(produce((state) => {
  delete state.downloadingModels[modelId];
  delete state.downloadProgress[modelId];  // 追加
  delete state.downloadStats[modelId];     // 追加
}));
```

---

### Bug 2 — `skip_download` パスで不要な GET リクエストを発行

**ファイル:** `src-tauri/src/managers/model.rs`

416 Range Not Satisfiable レスポンスによって `skip_download = true` になった場合（前回ダウンロード済み・抽出未完了）、ストリームの型合わせのために新たな GET リクエストを発行していた。このストリームは直後の `if !skip_download { ... }` ガードによって一切消費されないため、完全に無駄なリクエストだった。

```rust
// 修正前: 478 MB の GET リクエストを無駄に発行
(total_size, total_size, client.get(&url).send().await?.bytes_stream())

// 修正後: 空ストリームで代替（リクエストなし）
(total_size, total_size, futures_util::stream::empty().boxed())
```

3つの分岐すべてで `.boxed()` を付与し型を統一している。

---

### Bug 3 — `resume_from` の重複宣言（デッドコード）

**ファイル:** `src-tauri/src/managers/model.rs`

`let mut resume_from` が2回宣言されており、1回目は直後に2回目で即シャドウされ使われることがなかった。1回目の宣言（8行）を削除。

---

### Bug 4 — 不確定状態（indeterminate）でも「0%」が表示される（UX）

**ファイル:** `src/components/model-selector/ModelSelector.tsx`
**ファイル:** `src/i18n/locales/*/translation.json`（全17言語）

HTTP リクエストが飛んでいる間（`is_indeterminate: true`）は `progress.percentage` が 0 のため「0% Downloading」と表示されていた。この状態では「Downloading...」と表示するよう変更。

```typescript
// 修正後
if (progress.is_indeterminate) {
  return t("modelSelector.downloadingGeneric");
}
```

翻訳キー `modelSelector.downloadingGeneric` を全17言語ファイルに追加した。

---

## 修正ファイル一覧

| ファイル | 変更内容 |
|---|---|
| `src/stores/modelStore.ts` | Bug 1: エラー時に `downloadProgress` / `downloadStats` を削除 |
| `src-tauri/src/managers/model.rs` | Bug 2: 不要 GET を空ストリームに置換、Bug 3: 重複宣言を削除 |
| `src/components/model-selector/ModelSelector.tsx` | Bug 4: indeterminate 中は "Downloading..." を表示 |
| `src/i18n/locales/*/translation.json` | Bug 4: `downloadingGeneric` キーを全17言語に追加 |

---

## 再現手順（テスト時の参考）

1. モデル設定でダウンロードボタンを押す
2. ダウンロード中にネットワークを切断する
3. UI が「0% Downloading」に固まらずリセットされることを確認
4. 再接続後に再度ダウンロードできることを確認

---

## 関連する既知の問題

**Parakeet Japanese (`parakeet-tdt-ctc-0.6b-ja`) のダウンロード URL が 404**

- 設定 URL: `https://blob.handy.computer/parakeet-ja-int8.tar.gz`
- 調査日: 2026-03-19 時点で 404 Not Found
- このアプリはフォークであり、オリジナル作者が指定した URL のため正しい URL は不明
- 対応が必要になった際はアップストリームリポジトリで URL を確認すること

