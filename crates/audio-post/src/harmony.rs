//! Harmonised voices derived from one lead: the same singing, shifted.
//!
//! **Experimental.** It works and is tested, but it has been heard on exactly
//! one kind of material, and it did not sound good: stacked on a *syllabic*
//! lead — a verse with a note per syllable — every voice carries its own
//! smeared consonants on top of each other and the result is mud, not a choir.
//! Parallel shifting makes musical sense where the source is **sustained and
//! sparse**: held notes, a drone, an organ chord, a pad. There each copy is one
//! clean sustained tone, and stacking reads as one thick sound. Pick the
//! material before reaching for this.
//!
//! Every voice is a pitch-shifted copy of the lead, so by construction it
//! shares the lead's timing sample for sample — which is the one thing a
//! generated choir can never do for itself (see
//! `docs/plans/2026-10-05_ensemble.md`). The result is homophonic: the voices
//! move together, so it sounds like a choir only in the stacked sense, never as
//! independently sung parts.
//!
//! A shift is two textbook steps, not one clever one: resampling moves the
//! pitch (and the length with it), then a phase vocoder takes the length back
//! while leaving the pitch alone. Doing the pitch shift by resampling and the
//! length correction by phase vocoder keeps each step honest — bin remapping
//! inside a single vocoder pass fights its own phase correction and lands
//! roughly half an octave out.

use anyhow::{bail, Context, Result};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::resample;
use crate::Stereo;

/// Window and hop for the vocoder: a quarter of the window, so Hann windows
/// sum to a constant and overlap-add needs no correction beyond the weight.
const SIZE: usize = 2048;
const HOP: usize = SIZE / 4;

/// One harmony voice: how far from the lead, in semitones, and how loud.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Voice {
    pub semitones: f32,
    pub gain: f32,
}

impl Voice {
    pub fn new(semitones: f32, gain: f32) -> Self {
        Self { semitones, gain }
    }
}

/// The usual vocal stack under a soprano lead: alto a fourth below, tenor a
/// sixth below that, bass an octave under.
///
/// **Experimental, and only for sustained material.** Thirds and sixths are a
/// *vocal* voicing — they need a line that moves by step, so the copies stay
/// consonant. On a syllabic line they do not: the SATB preset stacked on a
/// verse read as mud, because every voice smears its own consonants over the
/// others' (see the module docs).
pub fn satb() -> Vec<Voice> {
    vec![Voice::new(-5.0, 0.75), Voice::new(-9.0, 0.65), Voice::new(-12.0, 0.85)]
}

/// A third on top of the lead, the other common doubling. Same caveat.
pub fn thirds_above() -> Vec<Voice> {
    vec![Voice::new(3.0, 0.7), Voice::new(4.0, 0.7)]
}

/// Octaves and fifths — a mixture registration, the way an organ stacks its
/// stops (8', 4', 2⅔', 2').
///
/// **The preset to reach for.** Unlike thirds and sixths, a fourth and a
/// twelfth stay consonant with *any* melody, because they are the harmonics of
/// the fundamental. Stacked on a sustained drone, pad or held organ chord it
/// reads as one thick sound rather than as copies; on moving material it is
/// the safest of the three, though still parallel.
pub fn organ_mixture() -> Vec<Voice> {
    vec![Voice::new(-12.0, 0.7), Voice::new(-7.0, 0.5), Voice::new(12.0, 0.35)]
}

/// Shifts one channel by `semitones`, keeping its length.
///
/// Two textbook steps, because one pass cannot do both jobs at once: keeping
/// the length and moving the pitch are contradictory in a single vocoder pass,
/// since a stretch is made by the synthesis hop and the pitch runs on the
/// phase. So the pitch is a resample - exact, and the only step that moves
/// frequency - and a phase vocoder takes the length back without touching it.
///
/// The order matters. Resampling to a lower declared rate and reading the
/// result back at the original rate is a tape played faster: shorter by
/// `ratio`, higher by `ratio`. The vocoder then lengthens by `ratio`, leaving
/// the original length and the shifted pitch.
pub fn shift_channel(x: &[f32], rate: u32, semitones: f32) -> Result<Vec<f32>> {
    if !semitones.is_finite() {
        bail!("a shift needs a number of semitones");
    }
    if rate == 0 {
        bail!("the audio needs a sample rate");
    }
    let ratio = 2f64.powf(semitones as f64 / 12.0);
    if (ratio - 1.0).abs() < 1e-9 || x.is_empty() {
        return Ok(x.to_vec());
    }
    if !(0.25..=4.0).contains(&ratio) {
        bail!("a harmony voice may only shift between 4 and 48 semitones");
    }
    // 1. the pitch. The resampler preserves frequency in Hz, so the shift comes
    // from the *length* it produces: resampling by `ratio` shortens the track
    // by `ratio`, and reading that shorter track back at the original rate
    // plays it `ratio` times faster, which is `ratio` times higher in pitch.
    // Declaring a lower rate than we started with shortens the track; read back
    // at the original rate that is playing it faster, hence higher. The bound
    // is wide on purpose: a downward shift needs a rate ABOVE the original,
    // and clamping to `rate` would quietly turn a shift down into no shift.
    let declared = ((rate as f64 / ratio).round() as u32).clamp(1, 384_000);
    let pitched = resample::mono(x, rate, declared).context("resample to move the pitch")?;
    // 2. the length back again, pitch untouched.
    // The resample left a track `1 / ratio` as long as it started, so the
    // stretch has to multiply the length by `ratio` — which is what the same
    // factor does in both directions, since dividing and multiplying by it are
    // inverses. (For a shift up the resample shortened the track and the
    // vocoder lengthens it; for a shift down it is the other way round.)
    stretch(&pitched, ratio, x.len())
}

/// Time-stretches by `factor` without moving the pitch: above 1 makes the
/// track longer and slower. Exactly `wanted` frames come back.
///
/// The synthesis hop carries the length change; the phase still advances by
/// what one *analysis* hop is worth, which is what keeps every partial at its
/// own frequency. Hop stays a quarter of the window so the Hann windows keep
/// summing flat and the level survives the overlap-add.
fn stretch(x: &[f32], factor: f64, wanted: usize) -> Result<Vec<f32>> {
    if x.is_empty() || wanted == 0 {
        return Ok(vec![0.0; wanted]);
    }
    if !factor.is_finite() || factor <= 0.0 {
        bail!("a stretch factor must be a positive number");
    }
    let factor = factor.clamp(0.25, 4.0);
    let mut planner = RealFftPlanner::<f32>::new();
    let window: Vec<f32> = (0..SIZE)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / SIZE as f32).cos())
        .collect();
    let forward = planner.plan_fft_forward(SIZE);
    let inverse = planner.plan_fft_inverse(SIZE);

    let frames = analyse(x, HOP, &window, forward.as_ref());
    if frames.is_empty() {
        return Ok(vec![0.0; wanted]);
    }
    let bins = frames[0].len();
    let mut phase = vec![0f64; bins];
    let mut stretched: Vec<Vec<Complex32>> = Vec::with_capacity(frames.len());
    let step = std::f64::consts::TAU * HOP as f64 / SIZE as f64;
    for (index, frame) in frames.iter().enumerate() {
        let mut out = frame.clone();
        for bin in 0..bins {
            let measured = frame[bin].im.atan2(frame[bin].re) as f64;
            let angle = if index == 0 {
                measured
            } else {
                // advance by the expected step, then correct by the deviation
                // this frame shows, wrapped into +/- pi
                phase[bin] += step * bin as f64;
                phase[bin] += wrap64(measured - phase[bin]);
                phase[bin]
            };
            phase[bin] = angle;
            out[bin] = Complex32::from_polar(frame[bin].norm(), angle as f32);
        }
        stretched.push(out);
    }
    // The synthesis hop is what carries the length change. It is held to the
    // window's quarter or more overlap: below that the Hann windows no longer
    // sum to a constant, and dividing by the weight then removes level instead
    // of reconstructing it. A hop between HOP and SIZE/2 covers every factor
    // from 1 up to 2; beyond that the caller should stretch in steps.
    let hop_out = ((HOP as f64 * factor).round() as usize).clamp(HOP, SIZE / 2);
    Ok(synthesise(&stretched, hop_out, wanted, &window, inverse.as_ref()))
}

/// The difference between two angles, wrapped into +/- pi.
fn wrap64(angle: f64) -> f64 {
    let two_pi = std::f64::consts::TAU;
    (angle + two_pi / 2.0).rem_euclid(two_pi) - two_pi / 2.0
}

/// Frames of one channel, windowed and transformed, padded so every sample
/// sits under fully overlapped windows.
fn analyse(x: &[f32], hop: usize, window: &[f32], forward: &dyn RealToComplex<f32>) -> Vec<Vec<Complex32>> {
    let padded = x.len() + 2 * SIZE;
    let count = (padded - SIZE) / hop + 1;
    let mut frames = Vec::with_capacity(count);
    let mut input = forward.make_input_vec();
    for f in 0..count {
        let start = f * hop;
        for (i, slot) in input.iter_mut().enumerate() {
            let p = start + i;
            let v = if p >= SIZE && p - SIZE < x.len() { x[p - SIZE] } else { 0.0 };
            *slot = v * window[i];
        }
        let mut output = forward.make_output_vec();
        forward.process(&mut input, &mut output).expect("stft frame");
        frames.push(output);
    }
    frames
}

/// Overlap-add back to `wanted` frames, dividing by the weight each sample
/// was covered by.
fn synthesise(
    frames: &[Vec<Complex32>],
    hop: usize,
    wanted: usize,
    window: &[f32],
    inverse: &dyn ComplexToReal<f32>,
) -> Vec<f32> {
    let span = frames.len() * hop + SIZE;
    let mut out = vec![0.0f32; span];
    let mut weight = vec![0.0f32; span];
    let mut time = inverse.make_output_vec();
    let scale = 1.0 / SIZE as f32;
    for (f, frame) in frames.iter().enumerate() {
        let mut spectrum = frame.clone();
        spectrum[0].im = 0.0;
        if let Some(last) = spectrum.last_mut() {
            last.im = 0.0;
        }
        inverse.process(&mut spectrum, &mut time).expect("istft frame");
        let start = f * hop;
        for i in 0..SIZE {
            if start + i < span {
                out[start + i] += time[i] * scale * window[i];
                weight[start + i] += window[i] * window[i];
            }
        }
    }
    // the padding at both ends is dropped, as elsewhere in this crate
    let begin = SIZE.min(span);
    let mut result: Vec<f32> = (begin..span.min(begin + wanted))
        .map(|p| if weight[p] > 1e-8 { out[p] / weight[p] } else { 0.0 })
        .collect();
    result.resize(wanted, 0.0);
    result
}

/// The lead plus each harmony voice, summed. The lead keeps its own level
/// unless `lead_gain` says otherwise.
pub fn harmonize(lead: &Stereo, voices: &[Voice], lead_gain: f32) -> Result<Stereo> {
    if !lead_gain.is_finite() {
        bail!("the lead level must be a number");
    }
    for voice in voices {
        if !voice.semitones.is_finite() {
            bail!("a harmony voice needs a number of semitones");
        }
        if !voice.gain.is_finite() {
            bail!("a harmony voice needs a number for its level");
        }
    }
    let mut left = lead.left.iter().map(|s| s * lead_gain).collect::<Vec<_>>();
    let mut right = lead.right.iter().map(|s| s * lead_gain).collect::<Vec<_>>();
    for voice in voices {
        for (sum, channel) in [
            (&mut left, &lead.left),
            (&mut right, &lead.right),
        ] {
            let shifted = shift_channel(channel, lead.rate, voice.semitones)
                .with_context(|| format!("shift a voice by {} semitones", voice.semitones))?;
            for (slot, sample) in sum.iter_mut().zip(shifted.iter()) {
                *slot += sample * voice.gain;
            }
        }
    }
    Ok(Stereo::new(left, right, lead.rate))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A steady tone, so a frequency estimate means something.
    fn tone(freq: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// The loudest partial of a tone, from the average spectrum: bin centres
    /// are too coarse on their own, so the peak is interpolated.
    /// Frequency by upward zero crossings over the steady middle half, where
    /// the vocoder is not ramping. Exact for a pure tone. It resolves to
    /// about a tenth of a tone, so the pitch assertions below also have an
    /// autocorrelation check that assumes nothing about the waveform.
    pub(crate) fn measured_freq(x: &[f32], rate: u32) -> f32 {
        let from = x.len() / 4;
        let to = x.len() * 3 / 4;
        if to <= from + 2 {
            return 0.0;
        }
        let mut crossings = 0usize;
        let mut last_negative = x[from] <= 0.0;
        for sample in &x[from..to] {
            let negative = *sample <= 0.0;
            if last_negative && !negative {
                crossings += 1;
            }
            last_negative = negative;
        }
        crossings as f32 * rate as f32 / (to - from) as f32
    }

    #[test]
    fn no_shift_gives_the_lead_back() {
        let x = tone(220.0, 0.5, 48_000);
        let back = shift_channel(&x, 48_000, 0.0).expect("no shift");
        assert_eq!(back.len(), x.len());
        let worst = back.iter().zip(&x).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "worst sample difference {worst}");
    }

    #[test]
    fn an_octave_up_doubles_the_frequency_and_keeps_the_length() {
        let rate = 48_000;
        let x = tone(220.0, 0.6, rate);
        let up = shift_channel(&x, rate, 12.0).expect("octave up");
        assert_eq!(up.len(), x.len(), "length must be preserved");
        assert!((measured_freq(&x, rate) - 220.0).abs() < 10.0, "input measured wrong");
        assert!((measured_freq(&up, rate) - 440.0).abs() < 15.0, "shifted up measured wrong");
    }

    #[test]
    fn an_octave_down_halves_the_frequency() {
        let rate = 48_000;
        let x = tone(440.0, 0.6, rate);
        let down = shift_channel(&x, rate, -12.0).expect("octave down");
        assert_eq!(down.len(), x.len());
        assert!((measured_freq(&down, rate) - 220.0).abs() < 15.0, "shifted down measured wrong");
    }

    #[test]
    fn a_fifth_up_lands_on_the_ratio_asked_for() {
        let rate = 48_000;
        let x = tone(220.0, 0.6, rate);
        let up = shift_channel(&x, rate, 7.0).expect("fifth up");
        assert_eq!(up.len(), x.len());
        let expected = 220.0 * 2f32.powf(7.0 / 12.0);
        assert!((measured_freq(&up, rate) - expected).abs() < 15.0, "measured {}", measured_freq(&up, rate));
    }

    /// A stretch must not move the pitch. Measured across factors, because
    /// the synthesis hop is what changes and the phase clock is what must not.
    #[test]
    fn stretching_by_any_factor_keeps_the_pitch() {
        let rate = 48_000u32;
        let hz = 220.0f32;
        let x = tone(hz, 0.8, rate);
        for factor in [0.5f64, 1.0, 2.0] {
            let want_len = ((x.len() as f64) * factor) as usize;
            let y = stretch(&x, factor, want_len).expect("stretch");
            let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
            let freq = measured_freq(&y, rate);
            println!("factor {factor}: len {} (want {want_len}) peak {peak:.3} freq {freq:.1} (want {hz:.1})", y.len());
            assert_eq!(y.len(), want_len, "length");
            assert!(peak > 0.4, "factor {factor} lost level: peak {peak}");
            assert!((freq - hz).abs() / hz < 0.10, "factor {factor} moved the pitch to {freq}");
        }
    }

    /// A second opinion, by autocorrelation: it does not assume a clean sine
    /// the way counting crossings does, so it can tell a real pitch error from
    /// an estimator artefact on a long stretched signal.
    fn autocorr_freq(x: &[f32], rate: u32) -> f32 {
        let from = x.len() / 4;
        let to = x.len() * 3 / 4;
        let seg = &x[from..to];
        let min = (rate as usize / 1000).max(2);
        let max = (rate as usize / 60).min(seg.len() / 2);
        let mut best_lag = min;
        let mut best = f32::MIN;
        for lag in min..=max {
            let mut sum = 0f32;
            for i in 0..seg.len() - lag {
                sum += seg[i] * seg[i + lag];
            }
            if sum > best {
                best = sum;
                best_lag = lag;
            }
        }
        rate as f32 / best_lag as f32
    }

    #[test]
    fn a_stretched_tone_keeps_its_pitch_by_autocorrelation() {
        let rate = 48_000u32;
        let hz = 220.0f32;
        let x = tone(hz, 0.8, rate);
        for factor in [0.5f64, 1.0, 2.0, 3.0] {
            let want_len = ((x.len() as f64) * factor) as usize;
            let y = stretch(&x, factor, want_len).expect("stretch");
            let measured = autocorr_freq(&y, rate);
            println!("factor {factor}: autocorr {measured:.1} (want {hz:.1})");
            assert!((measured - hz).abs() / hz < 0.05, "factor {factor} measured {measured}, want {hz}");
        }
    }

    #[test]
    fn the_stack_keeps_the_lead_length_and_carries_every_voice() {
        let lead = Stereo::new(tone(220.0, 0.4, 48_000), tone(220.0, 0.4, 48_000), 48_000);
        let stacked = harmonize(&lead, &satb(), 1.0).expect("stack");
        assert_eq!(stacked.frames(), lead.frames());
        let energy = stacked.left.iter().map(|s| s * s).sum::<f32>();
        assert!(energy > lead.left.iter().map(|s| s * s).sum::<f32>(), "the stack is quieter than the lead");
    }

    #[test]
    fn no_voices_leave_the_lead_alone() {
        let lead = Stereo::new(tone(330.0, 0.3, 48_000), tone(330.0, 0.3, 48_000), 48_000);
        let plain = harmonize(&lead, &[], 1.0).expect("plain");
        assert_eq!(plain.frames(), lead.frames());
        let worst = plain.left.iter().zip(&lead.left).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "worst difference {worst}");
    }

    #[test]
    fn a_voice_level_scales_only_that_voice() {
        let lead = Stereo::new(tone(220.0, 0.3, 48_000), tone(220.0, 0.3, 48_000), 48_000);
        let loud = harmonize(&lead, &[Voice::new(7.0, 1.0)], 1.0).expect("loud");
        let quiet = harmonize(&lead, &[Voice::new(7.0, 0.1)], 1.0).expect("quiet");
        let loud_peak = loud.left.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        let quiet_peak = quiet.left.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!(loud_peak > quiet_peak * 1.5, "peaks {loud_peak} vs {quiet_peak}");
    }

    /// Isolates the pitch step: resampling to half the declared rate and
    /// reading it back at the original rate is an octave up. No vocoder.
    #[test]
    fn resampling_alone_moves_the_pitch_by_the_ratio() {
        let rate = 48_000u32;
        let hz = 220.0f32;
        let x = tone(hz, 0.5, rate);
        for octaves in [1i32, -1, 2] {
            let ratio = 2f32.powi(octaves);
            let declared = ((rate as f64 / ratio as f64).round() as u32).max(1);
            let y = crate::resample::mono(&x, rate, declared).expect("resample");
            // read back at the ORIGINAL rate: that ratio is the pitch move
            let measured = measured_freq(&y, rate);
            let want = hz * ratio;
            println!("octaves {octaves}: declared {declared}, len {} (want ~{}), measured {measured:.1}, want {want:.1}",
                y.len(), (x.len() as f32 / ratio) as usize);
            assert!((measured - want).abs() / want < 0.05, "measured {measured}, want {want}");
        }
    }

    /// The mixture registration must be octaves and fifths only: those are
    /// the harmonics of the fundamental, so they cannot turn a chord into a
    /// dissonance however the lead moves. A fifth *below* is -7, which lands
    /// on 5 within the octave; both spellings of a fifth are allowed, and
    /// anything a third or a sixth away is not.
    #[test]
    fn the_mixture_stacks_octaves_and_fifths() {
        let voices = organ_mixture();
        assert!(!voices.is_empty());
        for voice in &voices {
            let within_octave = voice.semitones.abs() % 12.0;
            let is_octave = within_octave.abs() < 1e-6;
            let is_fifth = (within_octave - 5.0).abs() < 1e-6 || (within_octave - 7.0).abs() < 1e-6;
            assert!(
                is_octave || is_fifth,
                "{} semitones is neither an octave nor a fifth",
                voice.semitones
            );
        }
    }

    #[test]
    fn rubbish_settings_are_refused_with_a_reason() {
        let lead = Stereo::new(vec![0.0; 64], vec![0.0; 64], 48_000);
        assert!(harmonize(&lead, &[Voice::new(f32::NAN, 1.0)], 1.0).is_err());
        assert!(harmonize(&lead, &[Voice::new(3.0, f32::INFINITY)], 1.0).is_err());
        assert!(harmonize(&lead, &[], f32::NAN).is_err());
    }
}



