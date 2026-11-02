# External Model Cache Discovery

Handy can reuse speech-to-text models already downloaded by other apps, without copying them into Handy’s own `models` directory.

## Supported sources

On startup, Handy scans these locations (when they exist):

| Source | Typical path (macOS) |
|--------|----------------------|
| OpenWhispr | `~/.cache/openwhispr/whisper-models/` (and `models/`) |
| Meetily | `~/Library/Application Support/com.meetily.ai/models/` |

GGUF models in the Hugging Face Hub cache (`~/.cache/huggingface/hub/`) are discovered separately by Handy's built-in HF cache scan.

Windows / Linux use the equivalent user cache and app-data paths (`%USERPROFILE%`, `%APPDATA%`, `~/.config`, `~/.local/share`).

## Compatible formats

Only engines Handy already loads are accepted:

| Kind | Requirement |
|------|-------------|
| Whisper (whisper.cpp) | Single `.bin` file whose magic is GGML (`ggml` / `ggmf` / `ggjt` / LE variants such as `lmgg`) |

Parakeet ONNX directories are no longer loaded: since the transcribe.cpp migration every model runs as GGML/GGUF.

### Not supported

These are **not** loaded:

- faster-whisper / CTranslate2 checkpoints (e.g. `kotoba-whisper-v2.0-faster` `model.bin`)
- Transformers / safetensors Whisper weights (e.g. `kotoba-whisper-v2.2`)
- Parakeet ONNX directories
- PyTorch-only or speaker-diarization models (pyannote, etc.)

Convert or re-download a true whisper.cpp GGML `.bin` if you need those weights in Handy.

## How discovery works

1. Built-in catalog models are registered as usual.
2. Custom models under Handy’s `{app_data}/models/` are discovered.
3. External roots are walked up to three directories deep.
4. Each candidate is handled as follows:
   - **Filename matches a built-in entry** (e.g. `ggml-large-v3-turbo.bin` → Turbo) → mark that catalog model as downloaded and set `local_path` to the absolute path; UI shows an **External** badge.
   - **No catalog match** → register as a custom external model (`is_custom` + `is_external`).
5. If the same name already exists under Handy’s `models/` directory, the **local copy wins** and no external binding is applied.

## Runtime behavior

| Operation | Behavior |
|-----------|----------|
| Load / transcribe | Prefer `{app_data}/models/{filename}`; else use `local_path` |
| Download | Still downloads into Handy’s `models/` dir; after download, local file takes priority |
| Delete | Removes files **only** under Handy’s `models/`. External cache files are never deleted—Handy only unbinds them |
| Restart | External roots are scanned again; unbound catalog models may reappear if the cache file is still present |

## UI

- External models show an **External** badge (EN) / **外部** (JA).
- Description text indicates the model comes from an on-device external cache.

## Implementation

- Backend: `src-tauri/src/managers/model.rs`
  - `ModelInfo.local_path` / `ModelInfo.is_external`
  - `discover_external_models`, `register_external_bin`, `is_ggml_whisper_bin`
  - `get_model_path`, `update_download_status`, `delete_model`
- Frontend: `ModelInfo` in `src/bindings.ts`, badges in `ModelDropdown`, i18n keys

## Examples on a typical Mac

| Cache file | Result in Handy |
|------------|-----------------|
| OpenWhispr `ggml-large-v3-turbo.bin` | Built-in **Whisper Turbo** available (external) |
| Meetily `ggml-medium-q5_0.bin` | Custom external Whisper model |
| Meetily `parakeet/parakeet-tdt-0.6b-v3-int8/` | Not loaded (ONNX) |
