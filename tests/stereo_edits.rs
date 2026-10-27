use beatbox::{audio_edit, samples, Engine};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    bytes: Vec<u8>,
    left: Vec<f32>,
    right: Vec<f32>,
    engine: Engine,
}
impl Fixture {
    fn new(left: Vec<f32>, right: Vec<f32>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_stereo_edit_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("wide.wav");
        beatbox::render::write_wav(&source, &left, &right).unwrap();
        let bytes = fs::read(&source).unwrap();
        let (left, right) = samples::decode_stereo(&source).unwrap();
        let mut engine = Engine::new(root.clone());
        engine
            .call("import_sample", &json!({"path":source,"name":"wide"}))
            .unwrap();
        Self {
            root,
            source,
            bytes,
            left,
            right,
            engine,
        }
    }
    fn edit(&mut self, mut args: Value) -> (Value, Vec<f32>, Vec<f32>) {
        args["sample"] = json!("wide");
        let r = self.engine.call("edit_sample", &args).unwrap();
        let sample = self
            .engine
            .project
            .samples
            .iter()
            .find(|s| s.name == r["sample"])
            .unwrap();
        let (l, rch) = samples::decode_stereo(std::path::Path::new(&sample.path)).unwrap();
        assert_eq!(fs::read(&self.source).unwrap(), self.bytes);
        (r, l, rch)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    let error = actual
        .iter()
        .zip(expected)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(
        error <= 2.0 / 32768.0,
        "maximum error {error} exceeds two PCM16 quantization steps"
    );
}
fn opposed() -> Fixture {
    let l: Vec<f32> = (0..8820).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
    let r = l.iter().map(|v| -v).collect();
    Fixture::new(l, r)
}
#[test]
fn reverse_keeps_opposite_polarity_channels_instead_of_cancelling() {
    let mut f = opposed();
    let mut l = f.left.clone();
    let mut r = f.right.clone();
    l.reverse();
    r.reverse();
    let (_, a, b) = f.edit(json!({"reverse":true}));
    close(&a, &l);
    close(&b, &r);
    assert!(a.iter().any(|v| v.abs() > 0.3));
}
#[test]
fn region_reverse_fades_and_gain_keep_channel_alignment() {
    let l: Vec<f32> = (0..8820).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
    let r: Vec<f32> = (0..8820).map(|i| (i as f32 * 0.11).cos() * 0.1).collect();
    let mut f = Fixture::new(l, r);
    let start = (0.02 * 44100.0) as usize;
    let len = (0.1 * 44100.0) as usize;
    let mut l = f.left[start..start + len].to_vec();
    let mut r = f.right[start..start + len].to_vec();
    for c in [&mut l, &mut r] {
        c.reverse();
        audio_edit::fade(c, 4.0, 8.0);
        c.iter_mut().for_each(|v| *v *= 10f32.powf(-6.0 / 20.0));
    }
    let(_,a,b)=f.edit(json!({"start_s":0.02,"length_s":0.1,"reverse":true,"fade_in_ms":4.0,"fade_out_ms":8.0,"gain_db":-6.0}));
    close(&a, &l);
    close(&b, &r);
}
#[test]
fn trim_uses_either_channel_without_phase_cancellation() {
    let mut l = vec![0.0; 8820];
    let mut r = l.clone();
    r[1000..2000].fill(0.3);
    l[4000..5000].fill(0.4);
    r[4000..5000].fill(-0.4);
    let mut f = Fixture::new(l, r);
    let mut envelope: Vec<f32> = f
        .left
        .iter()
        .zip(&f.right)
        .map(|(l, r)| l.abs().max(r.abs()))
        .collect();
    let (start, tail) = audio_edit::trim(&mut envelope, -50.0);
    let end = f.left.len() - tail;
    let l = f.left[start..end].to_vec();
    let r = f.right[start..end].to_vec();
    let (_, a, b) = f.edit(json!({"trim_silence_db":-50.0}));
    close(&a, &l);
    close(&b, &r);
}
#[test]
fn peak_normalization_links_channels_and_clamps_positive_targets() {
    let mut f = Fixture::new(vec![0.1; 4410], vec![-0.4; 4410]);
    let peak = f
        .left
        .iter()
        .chain(&f.right)
        .fold(0.0f32, |p, v| p.max(v.abs()));
    let gain = 1.0 / peak;
    let l: Vec<f32> = f.left.iter().map(|v| v * gain).collect();
    let r: Vec<f32> = f.right.iter().map(|v| v * gain).collect();
    let (_, a, b) = f.edit(json!({"normalize_db":3.0}));
    close(&a, &l);
    close(&b, &r);
}
#[test]
fn highpass_does_not_leak_between_channels() {
    let mut f = Fixture::new(vec![0.0; 4410], vec![0.3; 4410]);
    let mut filter = beatbox::dsp::Biquad::new(beatbox::dsp::BiquadKind::LowCut, 120.0, 0.707, 0.0);
    let r: Vec<f32> = f.right.iter().map(|v| filter.process(*v)).collect();
    let l = f.left.clone();
    let (_, a, b) = f.edit(json!({"highpass_hz":120.0}));
    close(&a, &l);
    close(&b, &r);
}
#[test]
fn stereo_edits_keep_unique_files_and_survive_undo_redo_reopen() {
    let mut f = opposed();
    let prior = f.engine.project.clone();
    let (first, a, b) = f.edit(json!({"reverse":true}));
    let first_info = f
        .engine
        .project
        .samples
        .iter()
        .find(|s| s.name == first["sample"])
        .unwrap()
        .clone();
    let first_bytes = fs::read(&first_info.path).unwrap();
    let (second, _, _) = f.edit(json!({"gain_db":-6.0}));
    assert_ne!(first["sample"], second["sample"]);
    assert_eq!(fs::read(&first_info.path).unwrap(), first_bytes);
    let complete = f.engine.project.clone();
    f.engine.call("undo", &json!({})).unwrap();
    assert_eq!(f.engine.project.samples.len(), prior.samples.len() + 1);
    f.engine.call("redo", &json!({})).unwrap();
    assert_eq!(f.engine.project, complete);
    let path = f.root.join("reopen.json");
    f.engine
        .call("save_project", &json!({"path":path}))
        .unwrap();
    let mut reopened = Engine::new(f.root.clone());
    reopened
        .call("load_project", &json!({"path":path}))
        .unwrap();
    assert_eq!(reopened.project, complete);
    let (l, r) = samples::decode_stereo(std::path::Path::new(&first_info.path)).unwrap();
    assert_eq!(a, l);
    assert_eq!(b, r);
}

#[test]
fn loudness_normalization_uses_stereo_measurement_and_shared_gain() {
    let mut f = opposed();
    let before = beatbox::analysis::loudness(&f.left, &f.right).integrated_lufs;
    assert!(before > -70.0);
    let gain = beatbox::dsp::db_to_gain(-20.0 - before);
    let l: Vec<f32> = f.left.iter().map(|v| v * gain).collect();
    let r: Vec<f32> = f.right.iter().map(|v| v * gain).collect();
    let (_, a, b) = f.edit(json!({"normalize_lufs": -20.0}));
    close(&a, &l);
    close(&b, &r);
}

#[test]
fn empty_edit_is_rejected_without_registering_a_sample() {
    let mut f = opposed();
    let before = f.engine.project.clone();
    assert!(f
        .engine
        .call("edit_sample", &json!({"sample":"wide","start_s":1000.0}))
        .is_err());
    assert_eq!(f.engine.project, before);
    assert_eq!(fs::read(&f.source).unwrap(), f.bytes);
}

#[test]
fn relative_source_path_resolves_in_project_workdir() {
    let mut f = opposed();
    f.engine.project.samples[0].path = "wide.wav".into();
    let (_, a, b) = f.edit(json!({"reverse":true}));
    let left: Vec<f32> = f.left.iter().rev().copied().collect();
    let right: Vec<f32> = f.right.iter().rev().copied().collect();
    close(&a, &left);
    close(&b, &right);
}

#[test]
fn mono_equivalent_source_keeps_previous_edit_math() {
    let x: Vec<f32> = (0..8820).map(|i| (i as f32 * 0.03).sin() * 0.1).collect();
    let mut f = Fixture::new(x.clone(), x);
    let mut expected = f.left.clone();
    expected.reverse();
    audio_edit::fade(&mut expected, 3.0, 6.0);
    audio_edit::normalize(&mut expected, -2.0);
    let (_, a, b) =
        f.edit(json!({"reverse":true,"fade_in_ms":3.0,"fade_out_ms":6.0,"normalize_db":-2.0}));
    close(&a, &expected);
    close(&b, &expected);
}

#[test]
fn edit_does_not_replace_an_existing_unregistered_output_file() {
    let mut f = opposed();
    fs::create_dir_all(f.engine.samples_dir()).unwrap();
    let existing = f.engine.samples_dir().join("wide_edit.wav");
    fs::write(&existing, b"keep this unregistered recording").unwrap();
    let (result, _, _) = f.edit(json!({"reverse":true}));
    assert_eq!(result["sample"], "wide_edit_2");
    assert_eq!(
        fs::read(&existing).unwrap(),
        b"keep this unregistered recording"
    );
}

#[test]
fn stereo_source_at_48khz_is_resampled_without_merging_channels() {
    let mut f = opposed();
    let frames = 4800u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&48000u32.to_le_bytes());
    wav.extend_from_slice(&192000u32.to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(frames * 4).to_le_bytes());
    for i in 0..frames {
        let v = ((i as f32 * 0.06).sin() * 10000.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
        wav.extend_from_slice(&(-v).to_le_bytes());
    }
    fs::write(&f.source, &wav).unwrap();
    f.bytes = wav;
    let (mut left, mut right) = samples::decode_stereo(&f.source).unwrap();
    assert_eq!(left.len(), 4410);
    left.reverse();
    right.reverse();
    let (_, a, b) = f.edit(json!({"reverse":true}));
    close(&a, &left);
    close(&b, &right);
}

#[test]
fn unavailable_or_corrupt_source_does_not_change_project_or_outputs() {
    let mut f = opposed();
    let before = f.engine.project.clone();
    fs::remove_file(&f.source).unwrap();
    assert!(f
        .engine
        .call("edit_sample", &json!({"sample":"wide","reverse":true}))
        .is_err());
    assert_eq!(f.engine.project, before);
    fs::write(&f.source, b"not an audio file").unwrap();
    assert!(f
        .engine
        .call("edit_sample", &json!({"sample":"wide","reverse":true}))
        .is_err());
    assert_eq!(f.engine.project, before);
    assert!(!f.engine.samples_dir().join("wide_edit.wav").exists());
}
