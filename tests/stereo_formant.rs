use beatbox::{samples, tools_sound, Engine};
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
            "bb_formant_channels_{}",
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
                "formant_sample",
                &json!({"sample":"wide","shift":1.2,"amount":0.65}),
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
        (
            tools_sound::formant_shift(&self.left, 1.2, 0.65),
            tools_sound::formant_shift(&self.right, 1.2, 0.65),
        )
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
fn opposed_channels_survive_formant_shift() {
    let mut f = Fixture::new(true);
    let (_, l, r) = f.edit();
    assert!(l.iter().any(|v| v.abs() > 0.1));
    assert!(l.iter().zip(&r).all(|(l, r)| (l + r).abs() < 2. / 32768.));
    let (a, b) = f.expected();
    close(&l, &a);
    close(&r, &b);
}
#[test]
fn separate_channels_match_existing_formant_math() {
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
fn too_short_or_missing_source_does_not_mutate_project() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let rev = f.e.revision;
    let hist = f.e.undo_depth();
    beatbox::render::write_wav(&f.path, &[0.1; 32], &[0.2; 32]).unwrap();
    assert!(f
        .e
        .call("formant_sample", &json!({"sample":"wide"}))
        .is_err());
    fs::remove_file(&f.path).unwrap();
    assert!(f
        .e
        .call("formant_sample", &json!({"sample":"wide"}))
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
    assert_eq!(hist, f.e.undo_depth());
}

#[test]
fn zero_amount_preserves_channels() {
    let mut f = Fixture::new(false);
    let v =
        f.e.call(
            "formant_sample",
            &json!({"sample":"wide","shift":1.2,"amount":0.0}),
        )
        .unwrap();
    let info =
        f.e.project
            .samples
            .iter()
            .find(|s| s.name == v["sample"].as_str().unwrap())
            .unwrap();
    let (l, r) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
    close(&l, &f.left);
    close(&r, &f.right);
}
#[test]
fn relative_case_insensitive_source_and_corrupt_errors() {
    let mut f = Fixture::new(false);
    f.e.project.samples[0].path = "wide.wav".into();
    let v =
        f.e.call("formant_sample", &json!({"sample":"WIDE","shift":0.8}))
            .unwrap();
    assert!(v["sample"].as_str().unwrap().contains("formant"));
    fs::write(&f.path, b"not audio").unwrap();
    let before = f.e.project.clone();
    let rev = f.e.revision;
    let hist = f.e.undo_depth();
    assert!(f
        .e
        .call("formant_sample", &json!({"sample":"wide"}))
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
    assert_eq!(hist, f.e.undo_depth());
}
