//! Harmonised voices derived from one lead: the same singing, shifted.
//!
//! **Experimental.** It works and is tested, but it has been heard on exactly
//! two kinds of material, and only one of them was good:
//!
//! - **A syllabic lead** — a verse with a note per syllable — came out as mud.
//!   Every voice carries its own smeared consonants over the others'. Parallel
//!   shifting wants **sustained and sparse** material: held notes, a drone, an
//!   organ chord, a pad.
//! - **A whole mix** came out dissonant and out of tune. Every instrument in
//!   it is transposed and stacked, the vocal's formants shift with it, and the
//!   peak runs into the limiter. The same registration on the *separated
//!   stems* is clean and needs no gain reduction at all.
//!
//! So: separate first, stack the stem, and mix the stems back. This is a
//! texture tool for one sustained part — it adds thickness, never new music.
//!
//! Every voice is a pitch-shifted copy of the lead, so by construction it
//! shares the lead's timing sample for sample — which is the one thing a
//! generated choir can never do for itself (see
//! `docs/plans/2026-10-05_ensemble.md`). The result is homophonic: the voices
//! move together, so it sounds like a choir only in the stacked sense, never as
//! independently sung parts.
//!
//! A shift is two textbook steps, not one clever one: resampling moves the
//! pitch (and the length with it), then a time-scaler takes the length back
//! while leaving the pitch alone. Keeping the two steps apart keeps each
//! honest — bin remapping inside a single spectral pass fights its own phase
//! correction and lands roughly half an octave out.
//!
//! The time-scaler is **WSOLA**, not a phase vocoder, and the reason is
//! directional. A shift *up* shortens the track and the length is added back,
//! which any vocoder manages. A shift *down* leaves the track longer, so the
//! length has to be taken out — and a phase vocoder cannot compress: its
//! synthesis hop would drop below a quarter of the window, where the Hann
//! windows no longer sum flat. Measured on this crate's own vocoder: half the
//! level and 15% of pitch at a factor of 0.5. Clamping the hop to keep the
//! level, which is what this did before, is worse in a way you can hear: the
//! voice then plays in slow motion, so an octave below arrives as a chorus at
//! half speed and the wrong words under it. WSOLA has no hop to clamp, because
//! it moves the *join* rather than the phase: it takes the next piece of the
//! track from further along and crossfades it over the last one, which is a
//! cut rather than a rate change, and a cut cannot drift.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::resample;
use crate::Stereo;

/// Window for the time-scaler and its output hop, a quarter of the window:
/// two pieces overlap by three quarters and the Hann windows cover every
/// output sample the same number of times.
const SIZE: usize = 2048;
const HOP: usize = SIZE / 4;

/// One harmony voice: how far from the lead, in semitones, and how loud.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// A named registration, for callers that hold a name rather than a list:
/// `mixture`, `satb`, `thirds`, or `none` for the lead alone.
pub fn preset(name: &str) -> Option<Vec<Voice>> {
    match name.trim().to_ascii_lowercase().as_str() {
        "none" | "" => Some(Vec::new()),
        "mixture" | "organ" | "organ_mixture" => Some(organ_mixture()),
        "satb" | "choir" => Some(satb()),
        "thirds" | "thirds_above" => Some(thirds_above()),
        _ => None,
    }
}

/// What the processing stage is given: a registration by name, or the voices
/// themselves, and how loud the lead stays.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HarmonizeSettings {
    /// `mixture` (default), `satb`, `thirds`, `none`, or `custom`.
    pub preset: String,
    /// Used when `preset` is `custom`: semitones from the lead and a level.
    pub voices: Vec<Voice>,
    /// How loud the lead itself stays.
    pub lead_gain: f32,
    /// Put the result under the peak, rather than let the stack clip.
    pub limit: bool,
}

impl Default for HarmonizeSettings {
    fn default() -> Self {
        Self {
            preset: "mixture".into(),
            voices: Vec::new(),
            lead_gain: 1.0,
            limit: true,
        }
    }
}

impl Voice {
    /// The names a caller may use, for an error message that helps.
    pub const PRESETS: [&'static str; 4] = ["mixture", "satb", "thirds", "none"];
}

impl HarmonizeSettings {
    /// The voices these settings ask for, refusing a name it does not know.
    pub fn voices(&self) -> Result<Vec<Voice>> {
        if self.preset.eq_ignore_ascii_case("custom") {
            if self.voices.is_empty() {
                bail!("preset custom needs at least one voice");
            }
            return Ok(self.voices.clone());
        }
        preset(&self.preset)
            .ok_or_else(|| anyhow::anyhow!("unknown preset {}: try one of {}", self.preset, Voice::PRESETS.join(", ")))
    }

    /// Runs the stack over `lead`.
    pub fn apply(&self, lead: &Stereo) -> Result<Stereo> {
        let voices = self.voices()?;
        let mut stacked = harmonize(lead, &voices, self.lead_gain)?;
        if self.limit {
            stacked.keep_below(0.99);
        }
        Ok(stacked)
    }
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
    let declared = declared_rate(rate, ratio);
    let pitched = resample::mono(x, rate, declared).context("resample to move the pitch")?;
    // 2. the length back again, pitch untouched.
    // The resample left a track `1 / ratio` as long as it started, so the
    // stretch has to multiply the length by `ratio` — which is what the same
    // factor does in both directions, since dividing and multiplying by it are
    // inverses. (For a shift up the resample shortened the track and the
    // vocoder lengthens it; for a shift down it is the other way round.)
    stretch(&pitched, ratio, x.len())
}

/// The sample rate to declare for a shift of `ratio`.
///
/// The resampler works from two *whole* rates and takes its FFT sizes from
/// what they have in common, so it is exact when the two share a large factor
/// and wrong when they share none: at three semitones up, 40363 against 48000
/// have nothing in common and the result came back at two thirds of the level
/// with a frequency nowhere near the one asked for. Rounding `rate / ratio` to
/// an integer is what produced such a pair.
///
/// So the rate is chosen as the nearest one that keeps a common factor, which
/// is the nearest rational `p / q` with `q` a divisor of the original rate: at
/// `q` the declared rate is exactly `rate / q * p`, and the pair reduces to
/// `q : p`, both small. The cost is the error of that approximation, and the
/// search stops at the first divisor good enough to be under a tenth of a
/// percent — about one and a half cents, well under what an ear separates.
fn declared_rate(rate: u32, ratio: f64) -> u32 {
    const GOOD_ENOUGH: f64 = 0.001;
    let ceiling = 384_000u32;
    // The declared rate is the original divided by the ratio, so it is `ratio`
    // inverted that has to be approximated: a shift down declares a *higher*
    // rate, which is what makes the track longer for the time-scaler to trim.
    let target = 1.0 / ratio;
    let mut best = ((rate as f64 * target).round() as u32).clamp(1, ceiling);
    let mut best_error = f64::INFINITY;
    for q in 1..=8_192u32 {
        if rate % q != 0 {
            continue;
        }
        let p = (target * q as f64).round();
        if p < 1.0 || p > 4.0 * q as f64 {
            continue;
        }
        let declared = (rate / q) as u64 * p as u64;
        if declared < 1 || declared > ceiling as u64 {
            continue;
        }
        let error = ((p / q as f64) - target).abs() / target;
        if error < best_error {
            best_error = error;
            best = declared as u32;
            if error <= GOOD_ENOUGH {
                break;
            }
        }
    }
    best
}

/// Time-stretches by `factor` without moving the pitch: above 1 makes the
/// track longer and slower. Exactly `wanted` frames come back.
///
/// This is WSOLA, waveform-similarity overlap-add, and it is here for one
/// reason: a pitch shift *down* leaves the track longer than it started, so
/// the length has to be taken back out, and a phase vocoder cannot compress.
/// Its synthesis hop would have to fall below a quarter of the window, where
/// the Hann windows stop summing flat and the reconstruction loses both level
/// and pitch — measured here as half the level and 15% of pitch. Clamping the
/// hop instead, as this did before, quietly left every downward shift playing
/// in slow motion, an octave below sounding like a tape at half speed.
///
/// WSOLA compresses by taking the next piece of the track from further along
/// and crossfading it over the previous one, so each piece keeps the pitch it
/// came with and the crossfade hides the join. It needs no phase estimate, so
/// there is nothing in it to drift.
fn stretch(x: &[f32], factor: f64, wanted: usize) -> Result<Vec<f32>> {
    if x.is_empty() || wanted == 0 {
        return Ok(vec![0.0; wanted]);
    }
    if !factor.is_finite() || factor <= 0.0 {
        bail!("a stretch factor must be a positive number");
    }
    let factor = factor.clamp(0.25, 4.0);
    let window: Vec<f32> = (0..SIZE)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / SIZE as f32).cos())
        .collect();
    // How far the next piece is nominally taken from: a hop in the OUTPUT is
    // fixed, and the factor divides it to say how far apart the pieces sit in
    // the input. Below 1 the pieces come from further apart, which is the
    // compression the pitch shifter needs.
    let hop_out = HOP;
    let hop_in = hop_out as f64 / factor;
    // How far a piece may be slid to find the join that fits best, in samples.
    let search = (SIZE / 8) as isize;
    let last = (x.len() as isize - SIZE as isize).max(0);

    let frames = wanted / hop_out + 2;
    let span = frames * hop_out + SIZE;
    let mut out = vec![0.0f32; span];
    let mut weight = vec![0.0f32; span];
    // The part of the last piece the next one will overlap: the next piece is
    // chosen to continue exactly this, which is what keeps the seams silent.
    let mut continuation: Vec<f32> = Vec::new();

    for frame in 0..frames {
        let nominal = ((frame as f64 * hop_in).round() as isize).clamp(0, last);
        let chosen = if continuation.is_empty() {
            nominal
        } else {
            let mut best = nominal;
            let mut score = f32::MIN;
            for candidate in (nominal - search).max(0)..=(nominal + search).min(last) {
                let start = candidate as usize;
                let dot: f32 = x[start..start + continuation.len()]
                    .iter()
                    .zip(&continuation)
                    .map(|(a, b)| a * b)
                    .sum();
                if dot > score {
                    score = dot;
                    best = candidate;
                }
            }
            best
        };
        for i in 0..SIZE {
            let source = chosen as usize + i;
            let sample = if source < x.len() { x[source] } else { 0.0 };
            let at = frame * hop_out + i;
            if at < span {
                // The window goes on squared, because there is no analysis
                // window to square it with: WSOLA reads the track straight
                // through. Weighting once and dividing by the summed square
                // would divide the level by 2/1.5 and every voice would come
                // out a third louder than the lead.
                let w = window[i] * window[i];
                out[at] += sample * w;
                weight[at] += w;
            }
        }
        let end = (chosen as usize + SIZE).min(x.len());
        if end > chosen as usize + hop_out {
            continuation.clear();
            continuation.extend_from_slice(&x[chosen as usize + hop_out..end]);
        } else {
            continuation.clear();
        }
    }

    // the first window is a fade-in, so it goes; the weight divides what is left
    let begin = SIZE.min(span);
    let mut result: Vec<f32> = (begin..span.min(begin + wanted))
        .map(|at| if weight[at] > 1e-8 { out[at] / weight[at] } else { 0.0 })
        .collect();
    result.resize(wanted, 0.0);
    Ok(result)
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

    /// A shifted voice has to sit at the same MOMENT as the lead. A hop
    /// clamped to a quarter of the window left every downward shift playing in
    /// slow motion, which an octave below sounds like: a tape at half speed,
    /// carrying the first half of the track where the second half should be.
    #[test]
    fn a_shifted_voice_keeps_the_time_of_the_lead() {
        let rate = 48_000u32;
        let second = rate as usize;
        // one second of a clear pitch, then two seconds of near silence
        let mut x = vec![0.0f32; second * 3];
        for (i, slot) in x[..second].iter_mut().enumerate() {
            *slot = (i as f32 * 2.0 * std::f32::consts::TAU * 220.0 / rate as f32).sin() * 0.2;
        }
        for slot in x[second..].iter_mut() {
            *slot = 1.0 / 32768.0;
        }
        let rms = |audio: &[f32], from: usize, to: usize| {
            let window = &audio[from..to];
            (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt()
        };
        for semitones in [-12.0f32, -7.0, 12.0] {
            let out = shift_channel(&x, rate, semitones).expect("shift");
            assert_eq!(out.len(), x.len(), "{semitones} semitones keeps the length");
            let (lead, silence) = (rms(&out, 0, second), rms(&out, second * 2, second * 3));
            assert!(lead > 0.05, "{semitones} semitones: the tone must still be there, rms {lead}");
            assert!(
                silence < lead / 20.0,
                "{semitones} semitones: the voice drifted into the silence, rms {silence} against {lead}"
            );
        }
    }

    /// The length has to come back with the level it went out with: the point
    /// of the resample is that it is the only step that touches the pitch.
    #[test]
    fn a_shifted_voice_keeps_the_level_of_the_lead() {
        let rate = 48_000u32;
        let x = tone(220.0, 0.8, rate);
        let lead = (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        for semitones in [-12.0f32, -9.0, -5.0, -1.0, 3.0, 12.0] {
            let out = shift_channel(&x, rate, semitones).expect("shift");
            // only the middle, where the first and last windows do not reach
            let from = out.len() / 4;
            let to = out.len() * 3 / 4;
            let got = (out[from..to].iter().map(|s| s * s).sum::<f32>() / (to - from) as f32).sqrt();
            assert!(
                (got / lead - 1.0).abs() < 0.08,
                "{semitones} semitones changed the level by {}%: {got} against {lead}",
                100.0 * (got / lead - 1.0)
            );
        }
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
    fn the_named_registrations_are_reachable_by_name() {
        assert!(preset("mixture").is_some_and(|v| v.len() == 3));
        assert!(preset("SATB").is_some_and(|v| v.len() == 3));
        assert!(preset(" Thirds ").is_some_and(|v| v.len() == 2));
        assert!(preset("none").is_some_and(|v| v.is_empty()));
        assert!(preset("barbershop").is_none(), "an unknown name must not resolve");
    }

    #[test]
    fn settings_carry_a_registration_to_the_audio() {
        let lead = Stereo::new(tone(220.0, 0.3, 48_000), tone(220.0, 0.3, 48_000), 48_000);
        let settings = HarmonizeSettings { preset: "mixture".into(), ..Default::default() };
        let stacked = settings.apply(&lead).expect("mixture");
        assert_eq!(stacked.frames(), lead.frames());
        assert!(stacked.peak() <= 0.99, "limited to {}", stacked.peak());
        let alone = HarmonizeSettings { preset: "none".into(), ..Default::default() }
            .apply(&lead).expect("none");
        let worst = alone.left.iter().zip(&lead.left).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "worst difference {worst}");
    }

    #[test]
    fn custom_voices_are_used_as_given() {
        let lead = Stereo::new(tone(220.0, 0.2, 48_000), tone(220.0, 0.2, 48_000), 48_000);
        let settings = HarmonizeSettings {
            preset: "custom".into(),
            voices: vec![Voice::new(7.0, 0.5)],
            lead_gain: 1.0,
            limit: false,
        };
        let stacked = settings.apply(&lead).expect("custom");
        assert_eq!(stacked.frames(), lead.frames());
    }

    #[test]
    fn rubbish_settings_are_refused_with_a_reason() {
        let lead = Stereo::new(vec![0.0; 64], vec![0.0; 64], 48_000);
        assert!(harmonize(&lead, &[Voice::new(f32::NAN, 1.0)], 1.0).is_err());
        assert!(harmonize(&lead, &[Voice::new(3.0, f32::INFINITY)], 1.0).is_err());
        assert!(harmonize(&lead, &[], f32::NAN).is_err());
        assert!(HarmonizeSettings { preset: "barbershop".into(), ..Default::default() }
            .apply(&lead).is_err());
        assert!(HarmonizeSettings { preset: "custom".into(), ..Default::default() }
            .apply(&lead).is_err());
    }
}






