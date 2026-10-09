//! Choir alignment ("conductor"), Phases 1–2 of
//! `docs/plans/2026-10-10_choir.md`: correspondence between a take's heard
//! note onsets and the score's own metric grid, and residuals. No audio is
//! touched here.
//!
//! The design is grid-first, not take-to-take: two takes of *different* parts
//! share rhythm, not pitch, so matching takes to each other runs on onset
//! patterns with no absolute reference. The score's grid is exact and common
//! to all takes.
//!
//! Three measured traps shape every choice below (choir plan §3):
//!
//! - Anchors must describe the *voice*, so callers pass vocals-stem note
//!   lists, never full-mix transcriptions (a mix transcribes as chords).
//! - Pitch matching is octave-agnostic: takes were heard a consistent octave
//!   off their written part, so only pitch *class* is compared.
//! - Repeated identical notes are ambiguous. Ambiguity is reported, never
//!   guessed through.
//!
//! A take that does not converge is reported, never silently "fixed".

use crate::midi::Note as MidiNote;
use crate::score::abc::{Score, VOCAL};

/// One written note onset, in seconds from the score's own tempo.
#[derive(Debug, Clone)]
pub struct GridSlot {
    /// Seconds from the take start the score puts this onset at.
    pub time: f64,
    /// Written pitch class, 0–11. Octave-agnostic on purpose.
    pub pitch_class: i32,
    /// Written length in seconds, for telling repeated notes apart.
    pub duration: f64,
    /// Which bar of the score this onset opens in.
    pub bar: usize,
}

/// One heard onset: the notes a transcriber started together.
#[derive(Debug, Clone)]
pub struct ObservedOnset {
    pub time: f64,
    /// Every pitch class starting here, sorted and deduplicated.
    pub pitch_classes: Vec<i32>,
    /// Longest note starting here, in seconds.
    pub duration: f64,
}

/// A bar range of the score that is fitted as one piece, e.g. verse 0–4,
/// heard in the take between two observed times.
///
/// The observed window is load-bearing, not decorative: a global assignment
/// lets a late chorus entry match its bars to verse-tail onsets, because
/// hymn sections share pitch material and the time cost saturates. The
/// window comes from the word clocks (the chorus's first word in the take's
/// LRC), which is why the manifest carries anchors beside the notes.
#[derive(Debug, Clone)]
pub struct Section {
    pub label: String,
    pub bar_from: usize,
    pub bar_to: usize,
    /// Heard seconds: only onsets in [obs_from, obs_to) are assigned here.
    pub obs_from: f64,
    pub obs_to: f64,
}

/// How far apart two heard starts still count as one onset. MuScriptor onset
/// jitter is tens of ms; 50 ms clusters a chord without merging sung steps
/// (the fixture's shortest step is ~160 ms at 90 BPM).
pub const CLUSTER_MS: f64 = 50.0;

/// Time tolerance inside the match cost: this far apart costs one unit.
pub const MATCH_TOL_S: f64 = 0.5;

/// Cost of leaving a written slot unmatched (a deletion) or a heard onset
/// unused (an insertion). Below the worst match cost, so genuinely sung
/// notes match rather than skip; above a decent match, so phrasing
/// differences do not force absurd pairs.
pub const SKIP_COST: f64 = 2.0;

/// Extra cost when no pitch class agrees. Time and duration still decide when
/// every candidate disagrees (repeated identical notes: reported downstream).
pub const PITCH_MISMATCH_COST: f64 = 1.5;

/// A residual beyond this is an outlier, not phrasing: dropped and refitted
/// once, and counted in the report.
pub const OUTLIER_S: f64 = 0.15;

/// Beyond this speed a stretch would degrade rather than fix (choir plan §5):
/// the section is left unaligned and the report says so.
pub const MAX_SPEED: f64 = 1.5;

/// Least pairs a section fit needs. Two points make a line; one point is an
/// offset guess, which this module does not do.
pub const MIN_PAIRS: usize = 2;

/// The score's Vocal notes as a grid in seconds. Uses the studio's own
/// parser output, so the grid is exactly what the engine sang from:
/// quarters × 60/BPM.
pub fn grid_from_score(score: &Score) -> Vec<GridSlot> {
    let voice = &score.voices[VOCAL];
    let quarter = 60.0 / score.bpm.max(1) as f64;
    voice
        .notes
        .iter()
        .map(|note| {
            let bar = voice
                .bars
                .iter()
                .rposition(|bar| bar.start <= note.start)
                .unwrap_or(0);
            GridSlot {
                time: note.start.to_f64() * quarter,
                pitch_class: ((note.pitch % 12) + 12) % 12,
                duration: (note.duration.to_f64() * quarter).max(0.01),
                bar,
            }
        })
        .collect()
}

/// Clusters a vocals-stem note list into heard onsets. Notes starting within
/// `CLUSTER_MS` belong together (a chord, or one onset heard twice); the
/// cluster time is the earliest start.
pub fn distinct_onsets(notes: &[MidiNote]) -> Vec<ObservedOnset> {
    let mut sorted: Vec<&MidiNote> = notes.iter().collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut onsets: Vec<ObservedOnset> = Vec::new();
    for note in sorted {
        let pitch_class = (note.pitch % 12) as i32;
        let duration = (note.end - note.start).max(0.01);
        match onsets.last_mut() {
            Some(last) if (note.start - last.time) * 1000.0 <= CLUSTER_MS => {
                if !last.pitch_classes.contains(&pitch_class) {
                    last.pitch_classes.push(pitch_class);
                    last.pitch_classes.sort();
                }
                last.duration = last.duration.max(duration);
            }
            _ => onsets.push(ObservedOnset { time: note.start, pitch_classes: vec![pitch_class], duration }),
        }
    }
    onsets
}

/// Cost of hearing `obs` where the score wrote `grid`. Time, then duration,
/// then pitch class — in that order of trust, because the whole point is
/// that takes drift in time while singing the written pitches.
fn match_cost(grid: &GridSlot, obs: &ObservedOnset) -> f64 {
    let time = ((obs.time - grid.time).abs() / MATCH_TOL_S).min(2.0);
    let duration = (obs.duration / grid.duration).ln().abs().min(1.0);
    let pitch = if obs.pitch_classes.contains(&grid.pitch_class) { 0.0 } else { PITCH_MISMATCH_COST };
    time + 0.5 * duration + pitch
}

/// Monotone assignment of heard onsets to written slots, with insertions and
/// deletions. Returns the matched pairs as (grid index, onset index).
pub fn assign(grid: &[GridSlot], obs: &[ObservedOnset]) -> Vec<(usize, usize)> {
    let (n, m) = (grid.len(), obs.len());
    let mut cost = vec![vec![f64::INFINITY; m + 1]; n + 1];
    let mut step = vec![vec![0u8; m + 1]; n + 1];
    cost[0][0] = 0.0;
    for i in 1..=n {
        cost[i][0] = i as f64 * SKIP_COST;
        step[i][0] = 1;
    }
    for j in 1..=m {
        cost[0][j] = j as f64 * SKIP_COST;
        step[0][j] = 2;
    }
    for i in 1..=n {
        for j in 1..=m {
            let pair = cost[i - 1][j - 1] + match_cost(&grid[i - 1], &obs[j - 1]);
            let skip_grid = cost[i - 1][j] + SKIP_COST;
            let skip_obs = cost[i][j - 1] + SKIP_COST;
            if pair <= skip_grid && pair <= skip_obs {
                cost[i][j] = pair;
            } else if skip_grid <= skip_obs {
                cost[i][j] = skip_grid;
                step[i][j] = 1;
            } else {
                cost[i][j] = skip_obs;
                step[i][j] = 2;
            }
        }
    }
    let (mut i, mut j) = (n, m);
    let mut pairs = Vec::new();
    while i > 0 || j > 0 {
        match step[i][j] {
            0 => {
                pairs.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    pairs.reverse();
    pairs
}

/// A section fitted as observed = offset + speed × grid, by least squares,
/// with one round of outlier rejection. `None` when the section cannot be
/// fitted honestly: too few pairs, no spread in grid times, or a speed the
/// stretcher could not honour.
pub fn fit_section(grid: &[f64], obs: &[f64]) -> Option<SectionFit> {
    if grid.len() != obs.len() || grid.len() < MIN_PAIRS {
        return None;
    }
    let fit = |grid: &[f64], obs: &[f64]| {
        let n = grid.len() as f64;
        let (mg, mo) = (grid.iter().sum::<f64>() / n, obs.iter().sum::<f64>() / n);
        let var = grid.iter().map(|g| (g - mg).powi(2)).sum::<f64>();
        if var < 1e-9 {
            return (mo - mg, 1.0);
        }
        let cov = grid.iter().zip(obs.iter()).map(|(g, o)| (g - mg) * (o - mo)).sum::<f64>();
        let speed = cov / var;
        (mo - speed * mg, speed)
    };
    let (mut offset, mut speed) = fit(grid, obs);
    let mut kept: Vec<bool> = vec![true; grid.len()];
    let residuals: Vec<f64> = grid.iter().zip(obs.iter()).map(|(g, o)| o - (offset + speed * g)).collect();
    if residuals.iter().any(|r| r.abs() > OUTLIER_S) {
        for (keep, r) in kept.iter_mut().zip(&residuals) {
            *keep = r.abs() <= OUTLIER_S;
        }
        let (g2, o2): (Vec<f64>, Vec<f64>) = grid
            .iter()
            .zip(obs.iter())
            .zip(kept.iter())
            .filter(|(_, keep)| **keep)
            .map(|((g, o), _)| (*g, *o))
            .unzip();
        if g2.len() < MIN_PAIRS {
            return None;
        }
        (offset, speed) = fit(&g2, &o2);
    }
    if !(1.0 / MAX_SPEED..=MAX_SPEED).contains(&speed) {
        return None;
    }
    let dropped = kept.iter().filter(|keep| !**keep).count();
    Some(SectionFit { offset, speed, dropped })
}

/// What one section's fit says: the warp and how many pairs refused it.
#[derive(Debug, Clone)]
pub struct SectionFit {
    /// Seconds: observed ≈ offset + speed × grid.
    pub offset: f64,
    pub speed: f64,
    /// Pairs dropped as outliers, refitted without.
    pub dropped: usize,
}

/// One section's verdict, fitted or skipped with the reason.
#[derive(Debug, Clone)]
pub struct SectionReport {
    pub label: String,
    pub pairs: usize,
    pub fit: Option<SectionFit>,
    /// Residuals in ms of the pairs the fit kept, in score order.
    pub residuals_ms: Vec<f64>,
    pub skip_reason: Option<String>,
}

/// The whole take: per-section verdicts and the residuals the gate reads.
#[derive(Debug, Clone)]
pub struct TakeReport {
    pub sections: Vec<SectionReport>,
    pub matched: usize,
    pub grid_slots: usize,
    pub onsets: usize,
}

impl TakeReport {
    /// Every kept residual in ms, across fitted sections.
    pub fn residuals_ms(&self) -> Vec<f64> {
        self.sections.iter().flat_map(|section| section.residuals_ms.iter().copied()).collect()
    }

    pub fn median_ms(&self) -> Option<f64> {
        median(&mut self.residuals_ms())
    }

    pub fn p95_ms(&self) -> Option<f64> {
        percentile(&mut self.residuals_ms(), 95.0)
    }

    /// The choir plan's acceptance: median ≤ 20 ms, p95 ≤ 40 ms.
    pub fn meets_gate(&self) -> bool {
        matches!((self.median_ms(), self.p95_ms()), (Some(med), Some(p95)) if med <= 20.0 && p95 <= 40.0)
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 { values[mid] } else { (values[mid - 1] + values[mid]) / 2.0 })
}

fn percentile(values: &mut [f64], pct: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let rank = ((pct / 100.0 * values.len() as f64).ceil() as usize).max(1).min(values.len());
    Some(values[rank - 1])
}

/// Aligns one take's heard onsets to the score grid, section by section.
/// Each section's grid slots are assigned only to onsets heard inside that
/// section's observed window, then fitted locally — so verse pacing never
/// pays for chorus drift, and a late chorus entry cannot match its bars to
/// verse-tail onsets (which share pitch material and would otherwise win on
/// saturated time cost).
pub fn align_take(grid: &[GridSlot], sections: &[Section], obs: &[ObservedOnset]) -> TakeReport {
    let mut matched_total = 0;
    let mut reports = Vec::new();
    for section in sections {
        let section_grid: Vec<(usize, &GridSlot)> = grid
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.bar >= section.bar_from && slot.bar < section.bar_to)
            .collect();
        let window: Vec<(usize, &ObservedOnset)> = obs
            .iter()
            .enumerate()
            .filter(|(_, onset)| onset.time >= section.obs_from && onset.time < section.obs_to)
            .collect();
        let grid_slice: Vec<GridSlot> = section_grid.iter().map(|(_, slot)| (*slot).clone()).collect();
        let obs_slice: Vec<ObservedOnset> = window.iter().map(|(_, onset)| (*onset).clone()).collect();
        let pairs = assign(&grid_slice, &obs_slice);
        matched_total += pairs.len();
        let (grid_times, obs_times): (Vec<f64>, Vec<f64>) = pairs
            .iter()
            .map(|(gi, oi)| (grid_slice[*gi].time, obs_slice[*oi].time))
            .unzip();
        match fit_section(&grid_times, &obs_times) {
            Some(fit) => {
                // pairs the fit dropped are reported by count (fit.dropped),
                // not hidden in the spread: only kept pairs shape residuals
                let kept: Vec<f64> = grid_times
                    .iter()
                    .zip(obs_times.iter())
                    .filter(|(g, o)| (*o - (fit.offset + fit.speed * *g)).abs() <= OUTLIER_S)
                    .map(|(g, o)| (o - (fit.offset + fit.speed * g)) * 1000.0)
                    .collect();
                reports.push(SectionReport {
                    label: section.label.clone(),
                    pairs: pairs.len(),
                    fit: Some(fit),
                    residuals_ms: kept,
                    skip_reason: None,
                });
            }
            None => reports.push(SectionReport {
                label: section.label.clone(),
                pairs: pairs.len(),
                fit: None,
                residuals_ms: Vec::new(),
                skip_reason: Some(if pairs.len() < MIN_PAIRS {
                    format!("only {} pairs, need {}", pairs.len(), MIN_PAIRS)
                } else {
                    "no honest fit (degenerate times or speed beyond 1.5x)".to_string()
                }),
            }),
        }
    }
    TakeReport { sections: reports, matched: matched_total, grid_slots: grid.len(), onsets: obs.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::abc;

    fn slot(time: f64, pitch_class: i32) -> GridSlot {
        GridSlot { time, pitch_class, duration: 0.5, bar: 0 }
    }

    fn heard(time: f64, pitch_class: i32) -> ObservedOnset {
        ObservedOnset { time, pitch_classes: vec![pitch_class], duration: 0.5 }
    }

    #[test]
    fn a_perfect_take_aligns_to_zero_residual() {
        let grid: Vec<GridSlot> = [0.0, 1.0, 2.0, 3.0].iter().map(|t| slot(*t, 0)).collect();
        let obs: Vec<ObservedOnset> = [0.01, 1.02, 1.99, 3.0].iter().map(|t| heard(*t, 0)).collect();
        let sections = vec![Section { label: "all".into(), bar_from: 0, bar_to: 99, obs_from: 0.0, obs_to: f64::INFINITY }];
        let report = align_take(&grid, &sections, &obs);
        assert_eq!(report.matched, 4);
        let fit = report.sections[0].fit.as_ref().expect("a fit");
        assert!((fit.speed - 1.0).abs() < 0.05, "speed {}", fit.speed);
        assert!(report.median_ms().expect("residuals") < 20.0);
        assert!(report.meets_gate(), "a near-perfect take passes the gate");
    }

    #[test]
    fn a_drifted_take_recovers_offset_and_speed() {
        // the tenor's shape: late entry, slower pace
        let grid: Vec<GridSlot> = (0..8).map(|i| slot(i as f64, i % 12)).collect();
        let obs: Vec<ObservedOnset> = (0..8).map(|i| heard(4.2 + 1.05 * i as f64, i % 12)).collect();
        let sections = vec![Section { label: "all".into(), bar_from: 0, bar_to: 99, obs_from: 0.0, obs_to: f64::INFINITY }];
        let report = align_take(&grid, &sections, &obs);
        let fit = report.sections[0].fit.as_ref().expect("a fit");
        assert!((fit.offset - 4.2).abs() < 0.05, "offset {}", fit.offset);
        assert!((fit.speed - 1.05).abs() < 0.02, "speed {}", fit.speed);
        assert!(report.median_ms().expect("residuals") < 5.0);
    }

    #[test]
    fn an_octave_displacement_still_matches() {
        // heard a consistent octave below the written part (choir plan §3)
        let grid: Vec<GridSlot> = [0.0, 1.0, 2.0].iter().map(|t| slot(*t, 4)).collect();
        let obs: Vec<ObservedOnset> = [0.05, 1.03, 2.02]
            .iter()
            .map(|t| ObservedOnset { time: *t, pitch_classes: vec![4], duration: 0.5 })
            .collect();
        // pitch class 4 == 4 whatever the octave; matching must not care
        let pairs = assign(&grid, &obs);
        assert_eq!(pairs.len(), 3, "octave displacement must not break matching");
    }

    #[test]
    fn insertions_and_deletions_are_skipped_not_forced() {
        // an extra onset right after a real one: one of the two is skipped
        let grid: Vec<GridSlot> = [0.0, 1.0, 2.0].iter().map(|t| slot(*t, 0)).collect();
        let obs: Vec<ObservedOnset> = [0.0, 1.0, 1.05, 2.0].iter().map(|t| heard(*t, 0)).collect();
        let pairs = assign(&grid, &obs);
        assert_eq!(pairs.len(), 3, "the doubled onset is skipped: {pairs:?}");
        assert!(pairs.contains(&(0, 0)) && pairs.contains(&(2, 3)), "ends anchor: {pairs:?}");
        // a missing onset: the written slot is skipped, the rest still anchor
        let obs2: Vec<ObservedOnset> = [0.0, 2.0].iter().map(|t| heard(*t, 0)).collect();
        let pairs2 = assign(&grid, &obs2);
        assert_eq!(pairs2, vec![(0, 0), (2, 1)], "the sung notes anchor: {pairs2:?}");
    }

    #[test]
    fn an_empty_take_is_reported_not_panicking() {
        let grid: Vec<GridSlot> = vec![slot(0.0, 0)];
        let sections = vec![Section { label: "all".into(), bar_from: 0, bar_to: 99, obs_from: 0.0, obs_to: f64::INFINITY }];
        let report = align_take(&grid, &sections, &[]);
        assert_eq!(report.matched, 0);
        assert!(report.sections[0].fit.is_none());
        assert!(report.sections[0].skip_reason.is_some());
        assert!(!report.meets_gate(), "no anchors must not pass the gate");
    }

    #[test]
    fn a_speed_beyond_the_cap_leaves_the_section_unaligned() {
        // twice as slow: a stretch would ruin it, so refuse instead
        let grid: Vec<GridSlot> = (0..6).map(|i| slot(i as f64, 0)).collect();
        let obs: Vec<ObservedOnset> = (0..6).map(|i| heard(2.0 * i as f64, 0)).collect();
        let sections = vec![Section { label: "all".into(), bar_from: 0, bar_to: 99, obs_from: 0.0, obs_to: f64::INFINITY }];
        let report = align_take(&grid, &sections, &obs);
        assert!(report.sections[0].fit.is_none(), "2x speed must be refused");
        assert!(report.sections[0].skip_reason.as_deref().is_some_and(|r| r.contains("1.5")));
    }

    #[test]
    fn the_grid_comes_from_the_studios_own_parser() {
        let text = "X:1\nT:\nM:4/4\nL:1/32\nQ:1/4=90\nV: Vocal clef=treble name=\"Vocal Melody\" snm=\"Vocal\"\nV: Ins clef=treble name=\"Ins Melody\" snm=\"Inst.\"\nK:C\n% verse\nV: Vocal\n\"C\"E8G8A8G8|A8G8E8D8|\nV: Ins\nZ2|\n";
        let score = abc::parse(text).expect("the score reads");
        let grid = grid_from_score(&score);
        // 8 eighth-notes per bar at L:1/32: 8 units of 1/8 quarter = 1 quarter each
        assert_eq!(grid.len(), 8, "eight written notes");
        let quarter = 60.0 / 90.0;
        assert!((grid[0].time - 0.0).abs() < 1e-9);
        assert!((grid[1].time - quarter).abs() < 1e-9, "second note one quarter later");
        assert!((grid[4].time - 4.0 * quarter).abs() < 1e-9, "second bar starts at 4 quarters");
        assert_eq!(grid[0].pitch_class, 4, "E is pitch class 4");
        assert_eq!(grid[0].bar, 0);
        assert_eq!(grid[4].bar, 1);
    }

    #[test]
    fn onsets_cluster_chords_without_merging_steps() {
        let notes = vec![
            MidiNote { pitch: 60, start: 1.0, end: 1.5, instrument: "voice".into(), velocity: None },
            MidiNote { pitch: 64, start: 1.02, end: 1.5, instrument: "voice".into(), velocity: None },
            MidiNote { pitch: 67, start: 1.2, end: 1.6, instrument: "voice".into(), velocity: None },
        ];
        let onsets = distinct_onsets(&notes);
        assert_eq!(onsets.len(), 2, "chord clusters, step stays: {onsets:?}");
        assert_eq!(onsets[0].pitch_classes, vec![0, 4]);
    }

    /// Phase 2 of the choir plan, as an executable record: the four takes
    /// against their written grids, section by section, with the gate.
    ///
    /// Ignored by default: it needs the Phase-1 manifest (vocals-stem
    /// transcriptions, `scripts/choir-align.py`) and the regenerated part
    /// ABCs, and it measures real renders rather than asserting code
    /// behaviour. Run it with:
    ///
    ///   CHOIR_MANIFEST=workdir/choir-manifest.json CHOIR_SATB_DIR=/tmp/opencode/satb \
    ///     cargo test -p music-server --lib choir_gate -- --ignored --nocapture
    ///
    /// The gate is the plan's: the tenor under 40 ms on note anchors, or the
    /// idea is dropped and this test says so with the full report attached.
    /// (`--nocapture` because the report is the point.)
    #[test]
    #[ignore]
    fn choir_gate_tenor_under_40ms_on_note_anchors() {
        use std::collections::HashMap;

        let manifest_path =
            std::env::var("CHOIR_MANIFEST").unwrap_or_else(|_| "workdir/choir-manifest.json".into());
        let satb_dir = std::env::var("CHOIR_SATB_DIR").unwrap_or_else(|_| "/tmp/opencode/satb".into());
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|_| panic!("run scripts/choir-align.py first: no manifest at {manifest_path}"));
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).expect("the manifest is JSON");
        let sections: Vec<Section> = manifest["sections"]
            .as_array()
            .expect("manifest sections")
            .iter()
            .map(|section| Section {
                label: section["label"].as_str().unwrap_or("?").to_string(),
                bar_from: section["bar_from"].as_u64().unwrap_or(0) as usize,
                bar_to: section["bar_to"].as_u64().unwrap_or(0) as usize,
                // observed windows are per take (each take enters the chorus
                // at its own time), filled in below from take anchors
                obs_from: 0.0,
                obs_to: f64::INFINITY,
            })
            .collect();
        assert!(!sections.is_empty(), "the manifest names no sections");

        let mut gates: HashMap<String, bool> = HashMap::new();
        for take in manifest["takes"].as_array().expect("manifest takes") {
            let part = take["part"].as_str().unwrap_or("?").to_string();
            // each take's observed section windows come from its word clocks
            // (the chorus's first word in its LRC), not from the score
            let anchors: Vec<f64> = take["anchors"]
                .as_array()
                .map(|list| list.iter().map(|time| time.as_f64().unwrap_or(0.0)).collect())
                .unwrap_or_else(|| panic!("take {part} has no anchors: re-run scripts/choir-align.py"));
            assert_eq!(
                anchors.len() + 1,
                sections.len(),
                "take {part}: {} anchors must split {} sections",
                anchors.len(),
                sections.len()
            );
            let mut sections = sections.clone();
            for (index, section) in sections.iter_mut().enumerate() {
                section.obs_from = if index == 0 { 0.0 } else { anchors[index - 1] };
                section.obs_to = anchors.get(index).copied().unwrap_or(f64::INFINITY);
            }
            let abc_path = format!("{satb_dir}/part_{part}.abc");
            let abc = std::fs::read_to_string(&abc_path)
                .unwrap_or_else(|_| panic!("missing regenerated grid {abc_path}"));
            let score = crate::score::abc::parse(&abc).expect("the part ABC reads");
            let grid = grid_from_score(&score);
            let notes: Vec<MidiNote> = take["notes"]
                .as_array()
                .expect("take notes")
                .iter()
                .map(|note| MidiNote {
                    pitch: note["pitch"].as_u64().unwrap_or(0).min(127) as u8,
                    start: note["start"].as_f64().unwrap_or(0.0),
                    end: note["end"].as_f64().unwrap_or(0.0),
                    instrument: note["instrument"].as_str().unwrap_or("voice").to_string(),
                    velocity: None,
                })
                .collect();
            let onsets = distinct_onsets(&notes);
            let report = align_take(&grid, &sections, &onsets);
            println!("=== take {part}: {} grid slots, {} notes, {} onsets, {} matched",
                grid.len(), notes.len(), onsets.len(), report.matched);
            // the worst pairs, so a failing gate can be read: wrong assignment
            // (pitch classes disagree) or genuine phrasing (they agree)?
            // Assigned per section like the report, never globally.
            let mut worst: Vec<(f64, f64, f64, i32, Vec<i32>, bool)> = Vec::new();
            for section in &report.sections {
                let manifest_section = sections.iter().find(|s| s.label == section.label).expect("section");
                let grid_slice: Vec<&GridSlot> = grid
                    .iter()
                    .filter(|slot| slot.bar >= manifest_section.bar_from && slot.bar < manifest_section.bar_to)
                    .collect();
                let obs_slice: Vec<&ObservedOnset> = onsets
                    .iter()
                    .filter(|onset| onset.time >= manifest_section.obs_from && onset.time < manifest_section.obs_to)
                    .collect();
                let (offset, speed, fitted) = section.fit.as_ref()
                    .map(|fit| (fit.offset, fit.speed, true))
                    .unwrap_or((0.0, 1.0, false));
                for (gi, oi) in assign(
                    &grid_slice.iter().map(|s| (*s).clone()).collect::<Vec<_>>(),
                    &obs_slice.iter().map(|o| (*o).clone()).collect::<Vec<_>>(),
                ) {
                    let (slot, onset) = (grid_slice[gi], obs_slice[oi]);
                    let residual = if fitted {
                        (onset.time - (offset + speed * slot.time)) * 1000.0
                    } else {
                        // no fit, no residual: the raw gap, marked as such
                        (onset.time - slot.time) * 1000.0
                    };
                    worst.push((
                        residual,
                        slot.time,
                        onset.time,
                        slot.pitch_class,
                        onset.pitch_classes.clone(),
                        fitted,
                    ));
                }
            }
            worst.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
            for section in &report.sections {
                match &section.fit {
                    Some(fit) => {
                        let mut residuals = section.residuals_ms.clone();
                        residuals.sort_by(|a, b| a.total_cmp(b));
                        println!(
                            "  {:6} pairs {:3} offset {:+.3}s speed {:.4} dropped {} residual_ms median {:.1} p95 {:.1}",
                            section.label,
                            section.pairs,
                            fit.offset,
                            fit.speed,
                            fit.dropped,
                            median(&mut residuals.clone()).unwrap_or(f64::NAN),
                            percentile(&mut residuals, 95.0).unwrap_or(f64::NAN),
                        );
                    }
                    None => println!("  {:6} pairs {:3} SKIPPED: {}",
                        section.label, section.pairs,
                        section.skip_reason.as_deref().unwrap_or("?")),
                }
            }
            println!("  overall median {:.1} ms, p95 {:.1} ms, gate {}",
                report.median_ms().unwrap_or(f64::NAN),
                report.p95_ms().unwrap_or(f64::NAN),
                if report.meets_gate() { "PASS" } else { "FAIL" });
            println!("  worst pairs (residual, grid_t, obs_t, grid_pc vs obs_pcs):");
            for (residual, grid_t, obs_t, grid_pc, obs_pcs, fitted) in worst.iter().take(5) {
                let mark = if *fitted { "ms " } else { "ms UNFITTED" };
                println!("    {residual:+7.1} {mark} grid {grid_t:6.2}s  obs {obs_t:6.2}s  pc {grid_pc} vs {obs_pcs:?}");
            }
            // CHOIR_DUMP=1 prints every pair in score order, for reading an
            // assignment against the word clocks: which bars the take puts where.
            if std::env::var("CHOIR_DUMP").is_ok() {
                let mut all = worst.clone();
                all.sort_by(|a, b| a.1.total_cmp(&b.1));
                for (residual, grid_t, obs_t, grid_pc, obs_pcs, fitted) in &all {
                    let mark = if *fitted { "" } else { " UNFITTED" };
                    println!("    pair grid {grid_t:6.2}s pc {grid_pc:2} -> obs {obs_t:6.2}s {obs_pcs:?} ({residual:+6.1} ms{mark})");
                }
            }
            gates.insert(part, report.meets_gate());
        }
        assert_eq!(gates.len(), 4, "the take set is four renders, not {}", gates.len());
        assert!(
            gates.get("T").copied().unwrap_or(false),
            "GATE FAILED: the tenor is not under 40 ms on note anchors — \
             per docs/plans/2026-10-10_choir.md §6, drop the idea and say so"
        );
    }
}
