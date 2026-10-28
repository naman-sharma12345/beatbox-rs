use beatbox::{samples, sc_dsp, Engine};
use serde_json::json;
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
    left: Vec<f32>,
    right: Vec<f32>,
    engine: Engine,
}
impl Fixture {
    fn new(right_only: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_strip_stereo_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.wav");
        let mut l = vec![0.0; 22050];
        let mut r = l.clone();
        if right_only {
            r[1000..5000].fill(0.2);
        } else {
            l[1000..5000].fill(0.3);
            r[1000..5000].fill(-0.3);
        }
        l[12000..19000].fill(0.1);
        r[12000..19000].fill(0.2);
        beatbox::render::write_wav(&path, &l, &r).unwrap();
        let bytes = fs::read(&path).unwrap();
        let (left, right) = samples::decode_stereo(&path).unwrap();
        let mut engine = Engine::new(root.clone());
        engine
            .call("import_sample", &json!({"path":path,"name":"wide"}))
            .unwrap();
        Self {
            root,
            path,
            bytes,
            left,
            right,
            engine,
        }
    }
    fn apply(&mut self) -> (String, Vec<f32>, Vec<f32>) {
        let response = self
            .engine
            .call(
                "strip_silence",
                &json!({"sample":"wide","apply":true,"pad_ms":0,"min_gap_ms":50}),
            )
            .unwrap();
        let n = response["new_sample"].as_str().unwrap().to_string();
        let info = self
            .engine
            .project
            .samples
            .iter()
            .find(|s| s.name == n)
            .unwrap();
        let (l, r) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
        assert_eq!(fs::read(&self.path).unwrap(), self.bytes);
        (n, l, r)
    }
    fn expected(&self) -> (Vec<f32>, Vec<f32>) {
        let a: Vec<f32> = self
            .left
            .iter()
            .zip(&self.right)
            .map(|(l, r)| l.abs().max(r.abs()))
            .collect();
        let ranges = sc_dsp::non_silent_ranges(&a, -48.0, 2205, 0, 0);
        let mut l = vec![];
        let mut r = vec![];
        for (s, t) in ranges {
            l.extend_from_slice(&self.left[s..t]);
            r.extend_from_slice(&self.right[s..t]);
        }
        (l, r)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2.0 / 32768.0));
}
#[test]
fn opposed_regions_survive_and_kept_samples_preserve_both_channels() {
    let mut f = Fixture::new(false);
    let (l, r) = f.expected();
    let (_, a, b) = f.apply();
    close(&a, &l);
    close(&b, &r);
    assert!(a.iter().zip(&b).any(|(l, r)| *l > 0.2 && *r < -0.2));
}
#[test]
fn right_only_activity_is_retained_without_moving_channel_alignment() {
    let mut f = Fixture::new(true);
    let (l, r) = f.expected();
    let (_, a, b) = f.apply();
    close(&a, &l);
    close(&b, &r);
    assert!(a.iter().zip(&b).any(|(l, r)| l.abs() < 0.00001 && *r > 0.1));
}
#[test]
fn detection_matches_file_and_does_not_change_project() {
    let mut f = Fixture::new(false);
    let before = f.engine.project.clone();
    let revision = f.engine.revision;
    let a = f
        .engine
        .call(
            "strip_silence",
            &json!({"sample":"wide","pad_ms":0,"min_gap_ms":50}),
        )
        .unwrap();
    let b = f
        .engine
        .call(
            "strip_silence",
            &json!({"path":f.path,"pad_ms":0,"min_gap_ms":50}),
        )
        .unwrap();
    assert_eq!(a["count"], 2);
    assert_eq!(a["regions_s"], b["regions_s"]);
    assert_eq!(f.engine.project, before);
    assert_eq!(f.engine.revision, revision);
}
#[test]
fn repeated_outputs_and_existing_unregistered_file_are_preserved() {
    let mut f = Fixture::new(false);
    fs::create_dir_all(f.engine.samples_dir()).unwrap();
    let occupied = f.engine.samples_dir().join("wide_stripped.wav");
    fs::write(&occupied, b"keep recording").unwrap();
    let (n, l, r) = f.apply();
    assert_eq!(n, "wide_stripped_2");
    let path = f.engine.samples_dir().join(format!("{n}.wav"));
    let bytes = fs::read(&path).unwrap();
    let (n2, a, b) = f.apply();
    assert_eq!(n2, "wide_stripped_3");
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fs::read(occupied).unwrap(), b"keep recording");
    assert_eq!(l, a);
    assert_eq!(r, b);
}
#[test]
fn undo_redo_and_actual_reopen_keep_stereo_result_and_source() {
    let mut f = Fixture::new(false);
    let before = f.engine.project.clone();
    let (n, a, b) = f.apply();
    let complete = f.engine.project.clone();
    f.engine.call("undo", &json!({})).unwrap();
    assert_eq!(f.engine.project, before);
    f.engine.call("redo", &json!({})).unwrap();
    assert_eq!(f.engine.project, complete);
    let path = f.root.join("saved.json");
    f.engine
        .call("save_project", &json!({"path":path}))
        .unwrap();
    let mut reopened = Engine::new(f.root.clone());
    reopened
        .call("load_project", &json!({"path":path}))
        .unwrap();
    assert_eq!(reopened.project, complete);
    let info = reopened
        .project
        .samples
        .iter()
        .find(|s| s.name == n)
        .unwrap();
    let (l, r) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
    assert_eq!(a, l);
    assert_eq!(b, r);
    assert_eq!(fs::read(&f.path).unwrap(), f.bytes);
}
