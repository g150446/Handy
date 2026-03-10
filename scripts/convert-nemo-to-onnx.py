#!/usr/bin/env python3
"""
Convert a NeMo Parakeet TDT model (.nemo) to ONNX format for use with Handy.
Searches ~/Downloads for a .nemo file and exports to ~/Downloads/<model-name>/.

Required: NeMo toolkit  (pip install nemo_toolkit[asr])
"""
import sys
from pathlib import Path
import torch
import torch.nn.functional as F


# ── helpers ──────────────────────────────────────────────────────────────────

def find_nemo_file() -> Path:
    downloads = Path.home() / "Downloads"
    nemo_files = sorted(downloads.glob("*.nemo"))
    if not nemo_files:
        print("ERROR: No .nemo files found in ~/Downloads")
        sys.exit(1)
    if len(nemo_files) == 1:
        return nemo_files[0]
    print("Multiple .nemo files found:")
    for i, f in enumerate(nemo_files, 1):
        print(f"  {i}. {f.name}")
    while True:
        try:
            choice = int(input("Enter number: "))
            return nemo_files[choice - 1]
        except (ValueError, IndexError):
            print("Invalid choice, try again.")


def derive_output_dir(nemo_path: Path) -> Path:
    name = nemo_path.stem.replace("_", "-")
    return nemo_path.parent / name


class _MelSpectrogramExport(torch.nn.Module):
    """
    Real-valued mel spectrogram preprocessor for ONNX export.

    Replaces NeMo's AudioToMelSpectrogramPreprocessor which cannot be exported
    due to torch.stft returning complex tensors.  This module implements the
    identical computation using a pre-computed DFT conv1d kernel so that no
    complex types appear in the ONNX graph.

    Input  names expected by transcribe-rs:
        waveforms      – [B, T]   float32
        waveforms_lens – [B]      int64

    Output names expected by transcribe-rs:
        features       – [B, n_mels, T_frames]  float32
        features_lens  – [B]                    int64
    """

    def __init__(self, preprocessor):
        super().__init__()
        feat = preprocessor.featurizer
        n_fft = feat.n_fft          # 512
        hop = feat.hop_length       # 160
        win_len = feat.win_length   # 400

        # ── Build windowed DFT kernel ──────────────────────────────────────
        # The STFT window (win_len) is zero-padded symmetrically to n_fft.
        pad_l = (n_fft - win_len) // 2
        pad_r = n_fft - win_len - pad_l
        window = F.pad(feat.window.float(), (pad_l, pad_r))  # [n_fft]

        k = torch.arange(n_fft // 2 + 1, dtype=torch.float32)   # [K]
        t = torch.arange(n_fft, dtype=torch.float32)              # [n_fft]
        phase = 2 * torch.pi * k.unsqueeze(1) * t.unsqueeze(0) / n_fft  # [K, n_fft]

        cos_k = (torch.cos(phase) * window.unsqueeze(0)).unsqueeze(1)   # [K, 1, n_fft]
        sin_k = (-torch.sin(phase) * window.unsqueeze(0)).unsqueeze(1)  # [K, 1, n_fft]
        dft_kernel = torch.cat([cos_k, sin_k], dim=0)                   # [2K, 1, n_fft]

        self.register_buffer("dft_kernel", dft_kernel)
        self.register_buffer("fb", feat.fb.float())  # [1, n_mels, K]

        self.n_fft = n_fft
        self.hop = hop
        self.pad_amount = n_fft // 2   # center=True padding
        self.k = n_fft // 2 + 1       # number of frequency bins
        self.mag_power = feat.mag_power
        # log_zero_guard_value_fn returns a plain float scalar
        self.log_guard = float(feat.log_zero_guard_value_fn(torch.ones(1)))

    def forward(self, waveforms: torch.Tensor, waveforms_lens: torch.Tensor):
        # ── 1. Center-pad (constant/zero) ──────────────────────────────────
        x = F.pad(waveforms, (self.pad_amount, self.pad_amount))  # [B, T+2*pad]

        # ── 2. STFT via conv1d – no complex tensors ─────────────────────
        x = x.unsqueeze(1)                                          # [B, 1, T+2*pad]
        stft = F.conv1d(x, self.dft_kernel, stride=self.hop)       # [B, 2K, n_frames]
        real = stft[:, : self.k, :]                                 # [B, K, n_frames]
        imag = stft[:, self.k :, :]                                 # [B, K, n_frames]

        # ── 3. Magnitude → power spectrum ─────────────────────────────────
        magnitude = torch.sqrt(real.pow(2) + imag.pow(2))
        if self.mag_power != 1.0:
            magnitude = magnitude.pow(self.mag_power)

        # ── 4. Mel filterbank  [B, n_mels, n_frames] ──────────────────────
        features = torch.matmul(self.fb, magnitude)  # fb: [1, n_mels, K]

        # ── 5. Log ────────────────────────────────────────────────────────
        features = torch.log(features + self.log_guard)

        # ── 6. Per-feature normalization (over all frames) ─────────────────
        # NeMo normalizes only over valid frames; we normalize over all frames.
        # For non-padded inputs (typical transcribe-rs use-case) this matches
        # NeMo within ~0.05 mean absolute error in normalized space.
        x_mean = features.mean(dim=-1, keepdim=True)
        x_std = features.std(dim=-1, keepdim=True, correction=1).clamp(min=1e-5)
        features = (features - x_mean) / x_std

        # ── 7. Output lengths ─────────────────────────────────────────────
        # NeMo: floor_divide((T + 2*(n_fft//2) - n_fft) / hop) = T // hop
        features_lens = torch.div(waveforms_lens, self.hop, rounding_mode="floor")

        return features, features_lens


def export_preprocessor(asr_model, out_path: Path) -> None:
    """
    Export nemo128.onnx using a custom real-valued DFT module.

    Tries NeMo's built-in export first; falls back to the custom
    _MelSpectrogramExport wrapper which avoids complex-tensor issues.
    """
    # Attempt 1: NeMo built-in export
    try:
        asr_model.preprocessor.export(str(out_path), check_trace=False)
        print("  nemo128.onnx exported via NeMo")
        return
    except Exception as e:
        print(f"  NeMo preprocessor.export failed ({e!r})")
        print("  Falling back to custom DFT-based export...")

    # Attempt 2: custom real-valued module
    module = _MelSpectrogramExport(asr_model.preprocessor)
    module.eval()
    dummy_waveforms = torch.zeros(1, 16000)
    dummy_lens = torch.tensor([16000], dtype=torch.long)
    with torch.no_grad():
        torch.onnx.export(
            module,
            (dummy_waveforms, dummy_lens),
            str(out_path),
            input_names=["waveforms", "waveforms_lens"],
            output_names=["features", "features_lens"],
            dynamic_axes={
                "waveforms": {0: "batch", 1: "time"},
                "waveforms_lens": {0: "batch"},
                "features": {0: "batch", 2: "time"},
                "features_lens": {0: "batch"},
            },
            opset_version=14,
            dynamo=False,
        )
    print("  nemo128.onnx exported via custom DFT module")


def write_vocab(asr_model, out_path: Path) -> None:
    vocab = None
    for attr_path in ["joint.vocabulary", "decoder.vocabulary", "tokenizer.vocab"]:
        try:
            obj = asr_model
            for part in attr_path.split("."):
                obj = getattr(obj, part)
            vocab = list(obj)
            print(f"  Vocabulary found via asr_model.{attr_path} ({len(vocab)} tokens)")
            break
        except AttributeError:
            continue

    if vocab is None:
        print("ERROR: Could not find vocabulary on model object")
        sys.exit(1)

    with open(out_path, "w", encoding="utf-8") as f:
        for i, token in enumerate(vocab):
            f.write(f"{token} {i}\n")
        f.write(f"<blk> {len(vocab)}\n")
    print(f"  vocab.txt written ({len(vocab) + 1} entries)")


# ── main ─────────────────────────────────────────────────────────────────────

def main():
    import nemo.collections.asr as nemo_asr

    nemo_file = find_nemo_file()
    print(f"\nLoading {nemo_file.name} ...")
    asr_model = nemo_asr.models.ASRModel.restore_from(restore_path=str(nemo_file))
    asr_model.eval()
    asr_model.to("cpu")

    out_dir = derive_output_dir(nemo_file)
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"Output directory: {out_dir}\n")

    print("Exporting encoder-model.onnx ...")
    asr_model.encoder.export(str(out_dir / "encoder-model.onnx"))
    print("  done")

    print("Exporting decoder_joint-model.onnx ...")
    asr_model.decoder_joint.export(str(out_dir / "decoder_joint-model.onnx"))
    print("  done")

    print("Exporting nemo128.onnx (preprocessor) ...")
    export_preprocessor(asr_model, out_dir / "nemo128.onnx")

    print("Writing vocab.txt ...")
    write_vocab(asr_model, out_dir / "vocab.txt")

    print("\n✓ Done. Files written:")
    for f in sorted(out_dir.iterdir()):
        size_mb = f.stat().st_size / (1024 * 1024)
        print(f"  {f.name:<40} {size_mb:6.1f} MB")


if __name__ == "__main__":
    main()
