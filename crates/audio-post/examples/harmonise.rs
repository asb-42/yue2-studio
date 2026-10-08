//! Sends one vocal stem through the harmoniser and writes the result as WAV.
//! Dev-only: an example, so it is never part of the service.
//!
//!   cargo run -p audio-post --example harmonise -- in.wav out.wav [preset]
//!
//! Input is 16-bit wav; for an mp3, run the music-server example of the
//! same name, which decodes with the studio's own decoder.
//!
//! Presets: `mixture` (octaves and fifths, the default and the one to reach
//! for), `satb`, `thirds`.

use audio_post::Stereo;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).cloned().unwrap_or_else(|| "/tmp/opencode/harmony-demo/in.wav".into());
    let output = args.get(2).cloned().unwrap_or_else(|| "/tmp/opencode/harmony-demo/out.wav".into());
    let bytes = std::fs::read(&input)?;
    let (left, right, rate) = read_wav(&bytes)?;
    println!("read {} frames at {rate} Hz", left.len());

    let preset = args.get(3).cloned().unwrap_or_else(|| "mixture".into());
    let voices = match preset.as_str() {
        "satb" => audio_post::harmony::satb(),
        "thirds" => audio_post::harmony::thirds_above(),
        "mixture" => audio_post::harmony::organ_mixture(),
        other => anyhow::bail!("unknown preset {other}: mixture, satb or thirds"),
    };
    println!("preset {preset}: {} added voices", voices.len());

    let lead = Stereo::new(left, right, rate);
    let stacked = audio_post::harmony::harmonize(&lead, &voices, 1.0)?;
    println!("stacked {} frames (lead had {})", stacked.frames(), lead.frames());

    let peak = stacked.left.iter().fold(0f32, |p, s| p.max(s.abs()));
    let energy: f64 = stacked.left.iter().map(|s| (*s as f64).powi(2)).sum();
    println!("peak {peak:.3} rms {:.4}", (energy / stacked.left.len() as f64).sqrt());

    let mut stacked = stacked;
    stacked.keep_below(0.99);
    write_wav(&output, &stacked.left, &stacked.right, rate)?;
    println!("wrote {output}");
    Ok(())
}

fn read_wav(bytes: &[u8]) -> anyhow::Result<(Vec<f32>, Vec<f32>, u32)> {
    // 16-bit PCM wav, straight from the byte layout: no decode needed and no
    // dependency for the common case.
    let mut pos = 12;
    let mut channels = 2usize;
    let mut rate = 44_100u32;
    let mut samples: Option<Vec<f32>> = None;
    while pos + 8 <= bytes.len() {
        let id: [u8; 4] = bytes[pos..pos + 4].try_into()?;
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into()?) as usize;
        let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
        match &id {
            b"fmt " => {
                let tag = u16::from_le_bytes(body[0..2].try_into()?);
                anyhow::ensure!(tag == 1, "only PCM wav (format 1), got {tag}");
                channels = u16::from_le_bytes(body[2..4].try_into()?) as usize;
                rate = u32::from_le_bytes(body[4..8].try_into()?);
                let bits = u16::from_le_bytes(body[14..16].try_into()?);
                anyhow::ensure!(bits == 16, "only 16-bit wav, got {bits}");
            }
            b"data" => {
                samples = Some(
                    body.chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                        .collect(),
                );
            }
            _ => {}
        }
        pos += 8 + size + (size & 1);
    }
    let samples = samples.ok_or_else(|| anyhow::anyhow!("no data chunk"))?;
    anyhow::ensure!(channels >= 1, "a wav needs at least one channel");
    let left: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    let right = if channels >= 2 {
        samples.iter().skip(1).step_by(channels).copied().collect()
    } else {
        left.clone()
    };
    Ok((left, right, rate))
}

fn write_wav(path: &str, left: &[f32], right: &[f32], rate: u32) -> anyhow::Result<()> {
    use std::io::Write;
    let frames = left.len().min(right.len());
    let mut data = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        for s in [left[i], right[i]] {
            data.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    out.write_all(b"RIFF")?;
    out.write_all(&((36 + data.len() as u32).to_le_bytes()))?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * 4).to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&(data.len() as u32).to_le_bytes())?;
    out.write_all(&data)?;
    std::fs::write(path, out.into_inner())?;
    Ok(())
}