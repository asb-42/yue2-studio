//! Processing a finished track: noise reduction, the Spectral Lifter, vocal
//! naturalising, the experimental harmoniser, the user's own VST3 plugins, a
//! remix of the stems and mastering to a reference, in that order.
//!
//! The stages read the track's own file, or a single stem of it, and a remix
//! puts the stems back together afterwards. That is the arrangement the
//! harmoniser needs: shifted copies of one voice under the rest, rather than
//! shifted copies of everything at once.
//!
//! A run never touches the track. It leaves a preview beside the library, to be
//! heard against the original and then kept as a version or thrown away; a kept
//! version plays in place of the original, which stays on disk and can be
//! chosen again at any time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use audio_post::{denoise, harmony, lifter, mastering, naturalize, Stereo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which of a song's own audio the stages run on.
///
/// This is the choice that decides whether a stage can work at all. The
/// harmoniser is the plain case: on `Mix` it shifts every instrument at once
/// and shifts the voice's formants with them, which is why the stems exist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The track's stored file: everything at once.
    #[default]
    Mix,
    /// One stem of a separated track, by name. Only what the stages touch
    /// changes, and only this stem is what the remix puts back.
    Stem(String),
}

impl Source {
    /// The stem's name, when the stages were given one.
    pub fn stem(&self) -> Option<&str> {
        match self {
            Self::Mix => None,
            Self::Stem(name) => Some(name),
        }
    }
}

/// Putting the stems back together after the stages: the processed audio
/// stands in for the stem it came from, the others keep their level.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemixSettings {
    /// One level per stem, 0 to 2. A stem left out keeps its own level of 1,
    /// which makes an untouched remix come back as the mix it came from.
    pub levels: BTreeMap<String, f32>,
    /// Hold the sum under the peak. On by default, because a stack that
    /// clips throws away the headroom the stage above it worked for.
    pub limit: bool,
}

impl Default for RemixSettings {
    fn default() -> Self {
        Self { levels: BTreeMap::new(), limit: true }
    }
}

/// What to do to a track. A stage left out is skipped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessRequest {
    /// Which audio the stages read: the whole track, or one of its stems.
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub denoise: Option<denoise::DenoiseSettings>,
    #[serde(default)]
    pub lifter: Option<lifter::LifterSettings>,
    #[serde(default)]
    pub naturalize: Option<naturalize::NaturalizeSettings>,
    /// Experimental: pitch-shifted copies of the track under the lead, so the
    /// voices share its timing exactly. Sustained material only.
    #[serde(default)]
    pub harmonize: Option<harmony::HarmonizeSettings>,
    /// VST3 plugins, run in order before mastering.
    #[serde(default)]
    pub vst: Option<Vec<crate::vst::VstSlot>>,
    /// Sum the stems back together once the stages have run. Needs a stem as
    /// the source: remixing a whole mix would double it.
    #[serde(default)]
    pub remix: Option<RemixSettings>,
    #[serde(default)]
    pub master: Option<MasterSource>,
}

/// The reference a track is mastered to: another song of the library, or a
/// file uploaded for the purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MasterSource {
    Song { song_id: String },
    Upload { upload_id: String },
}

impl ProcessRequest {
    pub fn stages(&self) -> Vec<&'static str> {
        let mut stages = Vec::new();
        if self.denoise.is_some() {
            stages.push("denoise");
        }
        if self.lifter.is_some() {
            stages.push("lifter");
        }
        if self.naturalize.is_some() {
            stages.push("naturalize");
        }
        // a registration of "none" would do nothing, so it is not a stage
        if self.harmonize.as_ref().is_some_and(|settings| {
            settings.preset.eq_ignore_ascii_case("custom") || !settings.voices().is_ok_and(|voices| voices.is_empty())
        }) {
            stages.push("harmonize");
        }
        if self.vst.as_ref().is_some_and(|chain| chain.iter().any(|slot| slot.enabled)) {
            stages.push("vst");
        }
        if self.remix.is_some() {
            stages.push("remix");
        }
        if self.master.is_some() {
            stages.push("master");
        }
        stages
    }
}

/// The run in progress or the last one, as the interface polls it.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessRun {
    /// Tells a finishing worker whether its run is still the current one.
    pub id: String,
    pub song_id: String,
    pub stages: Vec<&'static str>,
    /// The stage working now, once started.
    pub stage: Option<&'static str>,
    pub done: bool,
    pub error: Option<String>,
    /// The preview's file name inside the processing folder, when ready.
    #[serde(skip)]
    pub preview: Option<String>,
    pub preview_ready: bool,
    pub request: ProcessRequest,
}

impl ProcessRun {
    /// The files only this run owns: its preview and an uploaded reference.
    pub fn leftovers(&self, media: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self.preview.iter().filter_map(|name| workspace_file(media, name)).collect();
        if let Some(MasterSource::Upload { upload_id }) = &self.request.master {
            files.extend(workspace_file(media, upload_id));
        }
        files
    }
}

/// Empties the workspace: previews and references live for one sitting.
pub fn clear_workspace(media: &Path) {
    let Ok(entries) = std::fs::read_dir(workspace(media)) else { return };
    for entry in entries.flatten() {
        if entry.path().is_file() {
            if let Err(error) = std::fs::remove_file(entry.path()) {
                eprintln!("[ERROR] processing: remove {}: {error}", entry.path().display());
            }
        }
    }
}

/// Where previews and uploaded references wait, inside the media folder.
pub fn workspace(media: &Path) -> PathBuf {
    media.join("processing")
}

/// A name that is a plain file of the workspace, never a path out of it.
pub fn workspace_file(media: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') {
        return None;
    }
    let path = workspace(media).join(name);
    path.is_file().then_some(path)
}

/// The audio a run works on: the file the stages read, and the stems it may
/// be put back into. The library resolves both, so this module never has to
/// know what a song is.
#[derive(Debug, Clone)]
pub struct RunSource {
    /// The file the stages read: the track's own, or one of its stems.
    pub path: PathBuf,
    /// Which stem `path` is, when the stages were given one.
    pub stem: Option<String>,
    /// Every stem of the track on disk, in the separator's order.
    pub stems: Vec<(String, PathBuf)>,
}

/// Sums the stems back together: the processed audio in place of the stem it
/// came from, the rest at their own levels, and the whole under the peak.
///
/// An untouched remix is the mix the stems were separated from, which is what
/// makes the levels worth trusting: a stem turned down is heard as turned down.
pub fn remix(source: &RunSource, processed: &Stereo, settings: &RemixSettings) -> Result<Stereo> {
    let stem = source
        .stem
        .as_deref()
        .context("the stages must run on one stem: a remix of a whole mix would double it")?;
    if source.stems.is_empty() {
        bail!("this track has no stems to put back together; separate it first");
    }

    let mut parts: Vec<(Stereo, f32)> = Vec::with_capacity(source.stems.len());
    for (name, path) in &source.stems {
        let level = settings.levels.get(name).copied().unwrap_or(1.0);
        let audio = if name == stem { processed.clone() } else { crate::audio_pcm::decode_stereo(path)? };
        parts.push((audio, level));
    }

    // The separator writes one rate for all of its stems; anything else is
    // brought up to it rather than letting the sum play at two speeds.
    let rate = parts.iter().map(|(audio, _)| audio.rate).max().unwrap_or(processed.rate);
    let frames = parts.iter().map(|(audio, _)| audio.frames()).max().unwrap_or(0);
    let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
    for (audio, level) in &mut parts {
        let level = *level;
        let audio = if audio.rate == rate { audio } else { &mut audio.resampled(rate)? };
        // a stem a fraction of a frame longer would otherwise clip its tail
        for (index, (l, r)) in audio.left.iter().zip(&audio.right).enumerate() {
            left[index] += l * level;
            right[index] += r * level;
        }
    }

    let mut mixed = Stereo::new(left, right, rate);
    if settings.limit {
        mixed.keep_below(0.99);
    }
    Ok(mixed)
}

/// Runs the stages on `source`, calling `on_stage` as each begins.
pub fn run(
    source: &RunSource,
    reference: Option<&Path>,
    request: &ProcessRequest,
    vst: Option<&crate::vst::VstHost>,
    on_stage: impl Fn(&'static str),
) -> Result<Stereo> {
    if request.stages().is_empty() {
        bail!("choose at least one kind of processing");
    }
    let mut audio = crate::audio_pcm::decode_stereo(&source.path)?;
    if let Some(settings) = &request.denoise {
        on_stage("denoise");
        audio = denoise::denoise(&audio, settings);
    }
    if let Some(settings) = &request.lifter {
        on_stage("lifter");
        audio = lifter::lift(&audio, settings);
    }
    if let Some(settings) = &request.naturalize {
        on_stage("naturalize");
        audio = naturalize::naturalize(&audio, settings);
    }
    if let Some(settings) = &request.harmonize {
        on_stage("harmonize");
        audio = settings.apply(&audio).with_context(|| "harmonise the track")?;
    }
    if let Some(chain) = request.vst.as_ref().filter(|chain| chain.iter().any(|slot| slot.enabled)) {
        on_stage("vst");
        let host = vst.context("the VST host is not installed")?;
        let work = source.path.parent().context("the track has no folder")?.join("processing");
        audio = host.process(&audio, chain, &work)?;
    }
    if let Some(settings) = &request.remix {
        on_stage("remix");
        audio = remix(source, &audio, settings).context("put the stems back together")?;
    }
    if request.master.is_some() {
        on_stage("master");
        let reference = reference.context("mastering needs a reference track")?;
        let reference = crate::audio_pcm::decode_stereo(reference)?;
        audio = mastering::master(&audio, &reference, &mastering::MasteringConfig::default())?;
    }
    Ok(audio)
}

/// The processing settings a kept version records, for showing and repeating.
pub fn settings_record(request: &ProcessRequest, reference_title: Option<&str>) -> Value {
    let mut value = serde_json::to_value(request).unwrap_or(Value::Null);
    if let (Some(title), Some(object)) = (reference_title, value.as_object_mut()) {
        object.insert("reference_title".into(), Value::String(title.to_string()));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_follow_the_request_in_processing_order() {
        let request = ProcessRequest {
            master: Some(MasterSource::Song { song_id: "x".into() }),
            denoise: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(request.stages(), vec!["denoise", "master"]);
        let parsed: ProcessRequest =
            serde_json::from_value(serde_json::json!({"lifter": {"shimmer_reduction_db": 3.0}, "master": {"type": "upload", "upload_id": "u"}})).unwrap();
        assert_eq!(parsed.stages(), vec!["lifter", "master"]);
        assert_eq!(parsed.lifter.unwrap().shimmer_reduction_db, 3.0);
    }

    /// The harmoniser arrives as one more stage: named registrations resolve, an
/// empty one is not a stage, and an unknown name is refused rather than
/// silently ignored.
#[test]
    fn the_harmoniser_is_a_stage_that_refuses_what_it_cannot_do() {
        let named: ProcessRequest =
            serde_json::from_value(serde_json::json!({ "harmonize": { "preset": "mixture" } })).unwrap();
        assert_eq!(named.stages(), vec!["harmonize"]);
        assert_eq!(named.harmonize.as_ref().expect("stage").preset, "mixture");

        let empty: ProcessRequest =
            serde_json::from_value(serde_json::json!({ "harmonize": { "preset": "none" } })).unwrap();
        assert!(empty.stages().is_empty(), "preset none would do nothing");

        let custom: ProcessRequest = serde_json::from_value(serde_json::json!({
            "harmonize": { "preset": "custom", "voices": [{ "semitones": 7.0, "gain": 0.5 }] }
        }))
        .unwrap();
        assert_eq!(custom.stages(), vec!["harmonize"]);

        let broken = harmony::HarmonizeSettings { preset: "barbershop".into(), ..Default::default() };
        assert!(broken.voices().is_err(), "an unknown registration must be refused");
        let bare = harmony::HarmonizeSettings { preset: "custom".into(), ..Default::default() };
        assert!(bare.voices().is_err(), "custom without voices must be refused");
    }

/// The remix is the stage that makes the rest worth doing: an untouched one
/// has to come back as the mix the stems were separated from, and a level set
/// has to be heard as that level.
#[test]
    fn an_untouched_remix_gives_the_mix_back_and_levels_are_heard() {
        let dir = std::env::temp_dir().join(format!("remix-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).expect("temp folder");
        let tone = |hz: f32, frames: usize| {
            (0..frames)
                .map(|frame| (frame as f32 * 2.0 * std::f32::consts::PI * hz / 48_000.0).sin() * 0.2)
                .collect::<Vec<f32>>()
        };
        // two stems of the same length, so the sum has something to be
        let vocals = dir.join("song-vocals.wav");
        let other = dir.join("song-other.wav");
        crate::audio_pcm::write_wav24(&vocals, &Stereo::new(tone(220.0, 4_800), tone(220.0, 4_800), 48_000)).expect("vocals");
        crate::audio_pcm::write_wav24(&other, &Stereo::new(tone(110.0, 4_800), tone(110.0, 4_800), 48_000)).expect("other");
        let source = RunSource {
            path: vocals.clone(),
            stem: Some("vocals".into()),
            stems: vec![("vocals".into(), vocals), ("other".into(), other)],
        };
        let processed = crate::audio_pcm::decode_stereo(&source.path).expect("read");
        let rest = crate::audio_pcm::decode_stereo(&source.stems[1].1).expect("read other");

        // untouched: the two stems come back as their sum, length and rate kept
        let settings = RemixSettings::default();
        let mixed = remix(&source, &processed, &settings).expect("remix");
        assert_eq!(mixed.frames(), 4_800, "the length of the stems is kept");
        assert_eq!(mixed.rate, 48_000);
        let expected = processed.left.iter().zip(&rest.left).map(|(v, o)| v + o);
        let worst = mixed.left.iter().zip(expected).map(|(m, want)| (m - want).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "the other stem has to be in there too, worst difference {worst}");

        // a stem turned down is heard as turned down, and a muted one is gone
        let mut levels = BTreeMap::new();
        levels.insert("other".to_string(), 0.0);
        let quiet = remix(&source, &processed, &RemixSettings { levels, limit: true }).expect("remix");
        let worst = quiet.left.iter().zip(&processed.left).map(|(m, v)| (m - v).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "only the processed stem is left, worst difference {worst}");

        std::fs::remove_dir_all(&dir).ok();
    }

#[test]
    fn a_remix_of_a_whole_mix_is_refused_rather_than_doubled() {
        let source = RunSource { path: PathBuf::from("mix.wav"), stem: None, stems: vec![("other".into(), PathBuf::from("o.wav"))] };
        let audio = Stereo::new(vec![0.1; 10], vec![0.1; 10], 48_000);
        let problem = remix(&source, &audio, &RemixSettings::default()).expect_err("a mix cannot be put back together");
        assert!(problem.to_string().contains("one stem"), "said why: {problem}");
    }

#[test]
    fn a_remix_needs_stems_on_disk() {
        let source = RunSource { path: PathBuf::from("v.wav"), stem: Some("vocals".into()), stems: Vec::new() };
        let audio = Stereo::new(vec![0.1; 10], vec![0.1; 10], 48_000);
        let problem = remix(&source, &audio, &RemixSettings::default()).expect_err("nothing to sum");
        assert!(problem.to_string().contains("no stems"), "said why: {problem}");
    }

#[test]
    fn a_vst_chain_runs_before_mastering_only_with_a_plugin_on() {
        let chain = |enabled| serde_json::json!([{ "path": "C:/x.vst3", "name": "X", "enabled": enabled }]);
        let on: ProcessRequest = serde_json::from_value(serde_json::json!({ "vst": chain(true), "master": { "type": "upload", "upload_id": "u" } })).unwrap();
        assert_eq!(on.stages(), vec!["vst", "master"]);
        let off: ProcessRequest = serde_json::from_value(serde_json::json!({ "vst": chain(false) })).unwrap();
        assert!(off.stages().is_empty());
    }

    #[test]
    fn workspace_names_cannot_leave_the_folder() {
        let media = std::env::temp_dir();
        assert!(workspace_file(&media, "../library.sqlite").is_none());
        assert!(workspace_file(&media, "a\\b.wav").is_none());
        assert!(workspace_file(&media, "").is_none());
    }
}
