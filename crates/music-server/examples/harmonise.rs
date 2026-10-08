//! Sends one track through the harmoniser and writes the result as WAV.
//! Dev-only: an example, so it is never part of the service.
//!
//!   cargo run -p music-server --example harmonise -- in.mp3 out.wav [preset]
//!
//! Presets: `mixture` (octaves and fifths — the default, and the one to reach
//! for), `satb`, `thirds`. This lives here rather than in `audio-post` because
//! it needs the studio's own decoder to read an mp3.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).cloned().unwrap_or_else(|| "/tmp/opencode/harmony-demo/in.mp3".into());
    let output = args.get(2).cloned().unwrap_or_else(|| "/tmp/opencode/harmony-demo/out.wav".into());
    let preset = args.get(3).cloned().unwrap_or_else(|| "mixture".into());

    let path = std::path::Path::new(&input);
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("mp3");
    let bytes = std::fs::read(path)?;
    let lead = music_server::audio_pcm::decode_stereo_bytes(bytes, extension)?;
    println!("decoded {} frames at {} Hz", lead.frames(), lead.rate);

    let voices = match preset.as_str() {
        "satb" => audio_post::harmony::satb(),
        "thirds" => audio_post::harmony::thirds_above(),
        "mixture" => audio_post::harmony::organ_mixture(),
        other => anyhow::bail!("unknown preset {other}: mixture, satb or thirds"),
    };
    println!("preset {preset}: {} added voices", voices.len());

    let mut stacked = audio_post::harmony::harmonize(&lead, &voices, 1.0)?;
    println!("stacked {} frames", stacked.frames());
    let before = stacked.peak();
    stacked.keep_below(0.99);
    let energy: f64 = stacked.left.iter().map(|s| (*s as f64).powi(2)).sum();
    println!(
        "peak {before:.3} -> {:.3}, rms {:.4}",
        stacked.peak(),
        (energy / stacked.left.len().max(1) as f64).sqrt()
    );

    write_wav(&output, &stacked.left, &stacked.right, lead.rate)?;
    println!("wrote {output}");
    Ok(())
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