use anyhow::Result;
use hound::{WavSpec, WavWriter};
use log::debug;
use std::io::Cursor;
use std::path::Path;

fn wav_spec() -> WavSpec {
    WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn write_samples<W: std::io::Write + std::io::Seek>(
    writer: &mut WavWriter<W>,
    samples: &[f32],
) -> Result<()> {
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let sample_i16 = (clamped * i16::MAX as f32) as i16;
        writer.write_sample(sample_i16)?;
    }
    Ok(())
}

pub fn encode_wav_bytes(samples: &[f32]) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    {
        let mut writer = WavWriter::new(Cursor::new(&mut buffer), wav_spec())?;
        write_samples(&mut writer, samples)?;
        writer.finalize()?;
    }
    Ok(buffer)
}

pub async fn save_wav_file<P: AsRef<Path>>(file_path: P, samples: &[f32]) -> Result<()> {
    let mut writer = WavWriter::create(file_path.as_ref(), wav_spec())?;
    write_samples(&mut writer, samples)?;
    writer.finalize()?;
    debug!("Saved WAV file: {:?}", file_path.as_ref());
    Ok(())
}
