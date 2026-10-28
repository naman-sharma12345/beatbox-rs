use beatbox::{audio_edit, samples, Engine};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
    left: Vec<f32>,
    right: Vec<f32>,
    e: Engine,
}
impl Fixture {
    fn new(opposed: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_stretch_channels_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.wav");
        let left: Vec<f32> = (0..22050)
            .map(|i| (i as f32 * 0.04).sin() * 0.3 + (i as f32 * 0.7).sin() * 0.02)
            .collect();
        let right: Vec<f32> = left
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if opposed {
                    -v
                } else {
                    (i as f32 * 0.11).sin() * 0.2
                }
            })
            .collect();
        beatbox::render::write_wav(&path, &left, &right).unwrap();
        let bytes = fs::read(&path).unwrap();
        let (left, right) = samples::decode_stereo(&path).unwrap();
        let mut e = Engine::new(root.clone());
        e.call("import_sample", &json!({"path":path,"name":"wide"}))
            .unwrap();
        Self {
            root,
            path,
            bytes,
            left,
            right,
            e,
        }
    }
    fn edit(&mut self) -> (Value, Vec<f32>, Vec<f32>) {
        let r = self
            .e
            .call(
                "stretch_sample",
                &json!({"sample":"wide","factor":1.3,"semitones":2.0}),
            )
            .unwrap();
        let info = self
            .e
            .project
            .samples
            .iter()
            .find(|s| s.name == r["sample"].as_str().unwrap())
            .unwrap();
        let (l, rr) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
        assert_eq!(self.bytes, fs::read(&self.path).unwrap());
        (r, l, rr)
    }
    fn expected(&self) -> (Vec<f32>, Vec<f32>) {
        (self.expected_pair().0, self.expected_pair().1)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 2. / 32768.));
}
#[test]
fn opposed_channels_survive_stretch_and_pitch() {
    let mut f = Fixture::new(true);
    let (_, l, r) = f.edit();
    assert!(l.iter().any(|v| v.abs() > 0.1));
    assert!(l.iter().zip(&r).all(|(l, r)| (l + r).abs() < 2. / 32768.));
    let (a, b) = f.expected();
    close(&l, &a);
    close(&r, &b);
}
#[test]
fn separate_channels_match_shared_stereo_processing() {
    let mut f = Fixture::new(false);
    let (_, l, r) = f.edit();
    let (a, b) = f.expected();
    close(&l, &a);
    close(&r, &b);
    assert_ne!(l, r);
}
#[test]
fn repeated_outputs_undo_redo_and_actual_reopen_preserve_stereo() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let (v, l, r) = f.edit();
    let name = v["sample"].as_str().unwrap();
    let path =
        f.e.project
            .samples
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .path
            .clone();
    let bytes = fs::read(&path).unwrap();
    let after = f.e.project.clone();
    assert!(f.e.undo());
    assert_eq!(before, f.e.project);
    assert!(f.e.redo());
    assert_eq!(after, f.e.project);
    let (second, _, _) = f.edit();
    assert_ne!(v["sample"], second["sample"]);
    assert_eq!(bytes, fs::read(path).unwrap());
    let save = f.root.join("reopen.json");
    f.e.call("save_project", &json!({"path":save})).unwrap();
    let mut reopened = Engine::new(f.root.clone());
    reopened
        .call("load_project", &json!({"path":save}))
        .unwrap();
    assert_eq!(f.e.project, reopened.project);
    let info = reopened
        .project
        .samples
        .iter()
        .find(|s| s.name == name)
        .unwrap();
    let (a, b) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
    assert_eq!(l, a);
    assert_eq!(r, b);
}
#[test]
fn one_silent_channel_stays_silent() {
    let mut f = Fixture::new(false);
    beatbox::render::write_wav(&f.path, &vec![0.; f.left.len()], &f.left).unwrap();
    f.bytes = fs::read(&f.path).unwrap();
    let (l, r) = samples::decode_stereo(&f.path).unwrap();
    f.left = l;
    f.right = r;
    let (_, l, r) = f.edit();
    assert!(l.iter().all(|v| v.abs() < 0.00001));
    assert!(r.iter().any(|v| v.abs() > 0.1));
}
#[test]
fn missing_or_corrupt_source_does_not_mutate_project() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let rev = f.e.revision;
    let hist = f.e.undo_depth();
    fs::write(&f.path, b"not audio").unwrap();
    assert!(f
        .e
        .call("stretch_sample", &json!({"sample":"wide","factor":1.3}))
        .is_err());
    fs::remove_file(&f.path).unwrap();
    assert!(f
        .e
        .call("stretch_sample", &json!({"sample":"wide","factor":1.3}))
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
    assert_eq!(hist, f.e.undo_depth());
}

impl Fixture {
    fn expected_pair(&self) -> (Vec<f32>, Vec<f32>) {
        let (l, r) = audio_edit::time_stretch_stereo(&self.left, &self.right, 1.3);
        audio_edit::pitch_shift_stereo(&l, &r, 2.)
    }
}
#[test]
fn mono_equivalent_stretch_keeps_previous_math() {
    let x: Vec<f32> = (0..22050).map(|i| (i as f32 * 0.04).sin() * 0.3).collect();
    for factor in [0.25, 0.8, 1., 1.3, 4.] {
        let expected = audio_edit::time_stretch(&x, factor);
        let (l, r) = audio_edit::time_stretch_stereo(&x, &x, factor);
        close(&l, &expected);
        close(&r, &expected);
    }
}
#[test]
fn shared_alignment_preserves_channel_ratio() {
    let l: Vec<f32> = (0..22050).map(|i| (i as f32 * 0.04).sin() * 0.3).collect();
    let r: Vec<f32> = l.iter().map(|v| v * 0.4).collect();
    for factor in [0.7, 1.4] {
        let (a, b) = audio_edit::time_stretch_stereo(&l, &r, factor);
        assert_eq!(a.len(), (l.len() as f32 * factor) as usize);
        assert!(a.iter().zip(&b).all(|(a, b)| (b - a * 0.4).abs() < 1e-6));
    }
}
#[test]
fn short_and_identity_stretch_match_each_channel_resampling() {
    let l = [0.1, 0.2, -0.3];
    let r = [-0.2, 0.1, 0.4];
    for factor in [0.5, 1., 2.] {
        let (a, b) = audio_edit::time_stretch_stereo(&l, &r, factor);
        close(&a, &audio_edit::time_stretch(&l, factor));
        close(&b, &audio_edit::time_stretch(&r, factor));
    }
}

#[test]
fn relative_case_insensitive_source_and_explicit_tempo_ratio() {
    let mut f = Fixture::new(false);
    f.e.project.samples[0].path = "wide.wav".into();
    let v =
        f.e.call(
            "stretch_sample",
            &json!({"sample":"WIDE","source_bpm":90.,"target_bpm":120.}),
        )
        .unwrap();
    let info =
        f.e.project
            .samples
            .iter()
            .find(|s| s.name == v["sample"].as_str().unwrap())
            .unwrap();
    let (l, r) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
    let (a, b) = audio_edit::time_stretch_stereo(&f.left, &f.right, 0.75);
    close(&l, &a);
    close(&r, &b);
    assert_eq!(f.bytes, fs::read(&f.path).unwrap());
}
#[test]
fn no_op_request_is_rejected_without_an_output() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let rev = f.e.revision;
    assert!(f
        .e
        .call("stretch_sample", &json!({"sample":"wide"}))
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
}
