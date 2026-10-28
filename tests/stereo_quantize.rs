use beatbox::{audio_edit, samples, Engine};
use serde_json::json;
use std::{fs, path::PathBuf};
fn hits() -> Vec<f32> {
    let n = 88200;
    let mut x = vec![0.; n];
    let mut rng = beatbox::dsp::Rng::new(7);
    for (k, off) in [441isize, -1323, 662, 0].iter().enumerate() {
        let start = (k * 22050) as isize + off;
        for j in 0..2000 {
            let i = start + j;
            if i >= 0 && (i as usize) < n {
                x[i as usize] = rng.bipolar() * (-(j as f32) / 300.).exp() * 0.8;
            }
        }
    }
    x
}
struct Fixture {
    root: PathBuf,
    e: Engine,
    l: Vec<f32>,
    r: Vec<f32>,
    source: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_quantize_stereo_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let l = hits();
        let r: Vec<f32> = l.iter().map(|v| -v).collect();
        let p = root.join("hits.wav");
        beatbox::render::write_wav(&p, &l, &r).unwrap();
        let source = fs::read(&p).unwrap();
        let (l, r) = samples::decode_stereo(&p).unwrap();
        let mut e = Engine::new(root.clone());
        e.call("import_sample", &json!({"path":p,"name":"hits"}))
            .unwrap();
        Self {
            root,
            e,
            l,
            r,
            source,
        }
    }
    fn edit(&mut self) -> (serde_json::Value, Vec<f32>, Vec<f32>) {
        let v = self
            .e
            .call(
                "quantize_sample",
                &json!({"sample":"hits","bpm":120,"grid":"1/4"}),
            )
            .unwrap();
        let p = &self
            .e
            .project
            .samples
            .iter()
            .find(|s| s.name == v["sample"].as_str().unwrap())
            .unwrap()
            .path;
        let (l, r) = samples::decode_stereo(std::path::Path::new(p)).unwrap();
        assert_eq!(self.source, fs::read(self.root.join("hits.wav")).unwrap());
        (v, l, r)
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
fn opposed_hits_keep_channels_and_shared_grid() {
    let mut f = Fixture::new();
    let (v, l, r) = f.edit();
    assert_eq!(v["hits"], 4);
    assert!(v["moved"].as_u64().unwrap() > 0);
    assert_eq!(l.len(), f.l.len());
    assert!(l.iter().any(|v| v.abs() > 0.3));
    assert!(l.iter().zip(&r).all(|(a, b)| (a + b).abs() < 2. / 32768.));
    let (a, b, _, _) = audio_edit::quantize_onsets_stereo(&f.l, &f.r, 22050, 1.);
    close(&l, &a);
    close(&r, &b);
}
#[test]
fn mono_equivalent_math_and_vote_counts_match_old_helper() {
    let x = hits();
    for amount in [0., 0.4, 1.] {
        let (a, h, m) = audio_edit::quantize_onsets(&x, 22050, amount);
        let (l, r, hh, mm) = audio_edit::quantize_onsets_stereo(&x, &x, 22050, amount);
        close(&a, &l);
        close(&a, &r);
        assert_eq!((h, m), (hh, mm));
    }
}
#[test]
fn proportional_channels_do_not_drift() {
    let l = hits();
    let r: Vec<f32> = l.iter().map(|v| v * 0.4).collect();
    let (a, b, h, _) = audio_edit::quantize_onsets_stereo(&l, &r, 22050, 1.);
    assert_eq!(h, 4);
    assert!(a.iter().zip(&b).all(|(a, b)| (b - a * 0.4).abs() < 1e-6));
}
#[test]
fn right_only_hits_do_not_leak() {
    let mut f = Fixture::new();
    let p = f.root.join("hits.wav");
    beatbox::render::write_wav(&p, &vec![0.; f.l.len()], &f.l).unwrap();
    f.source = fs::read(&p).unwrap();
    let (v, l, r) = f.edit();
    assert_eq!(v["hits"], 4);
    assert!(l.iter().all(|v| v.abs() < 1e-6));
    assert!(r.iter().any(|v| v.abs() > 0.3));
}
#[test]
fn repeat_undo_redo_and_actual_reopen_preserve_source_and_output() {
    let mut f = Fixture::new();
    let before = f.e.project.clone();
    let (v, l, r) = f.edit();
    let after = f.e.project.clone();
    assert!(f.e.undo());
    assert_eq!(before, f.e.project);
    assert!(f.e.redo());
    assert_eq!(after, f.e.project);
    let (v2, _, _) = f.edit();
    assert_ne!(v["sample"], v2["sample"]);
    let save = f.root.join("project.json");
    f.e.call("save_project", &json!({"path":save})).unwrap();
    let mut e = Engine::new(f.root.clone());
    e.call("load_project", &json!({"path":save})).unwrap();
    assert_eq!(e.project, f.e.project);
    let p = &e
        .project
        .samples
        .iter()
        .find(|s| s.name == v["sample"].as_str().unwrap())
        .unwrap()
        .path;
    let (a, b) = samples::decode_stereo(std::path::Path::new(p)).unwrap();
    assert_eq!(l, a);
    assert_eq!(r, b);
}
#[test]
fn relative_case_insensitive_input_works() {
    let mut f = Fixture::new();
    f.e.project.samples[0].path = "hits.wav".into();
    let v =
        f.e.call(
            "quantize_sample",
            &json!({"sample":"HITS","bpm":120,"grid":"1/4"}),
        )
        .unwrap();
    assert_eq!(v["hits"], 4);
}
#[test]
fn short_silent_corrupt_and_invalid_grid_do_not_mutate() {
    let mut f = Fixture::new();
    let before = f.e.project.clone();
    let rev = f.e.revision;
    let hist = f.e.undo_depth();
    assert!(f
        .e
        .call("quantize_sample", &json!({"sample":"hits","grid":"1/3"}))
        .is_err());
    let p = f.root.join("hits.wav");
    for n in [32, 88200] {
        beatbox::render::write_wav(&p, &vec![0.; n], &vec![0.; n]).unwrap();
        assert!(f
            .e
            .call("quantize_sample", &json!({"sample":"hits"}))
            .is_err());
    }
    fs::write(&p, b"not audio").unwrap();
    assert!(f
        .e
        .call("quantize_sample", &json!({"sample":"hits"}))
        .is_err());
    fs::remove_file(&p).unwrap();
    assert!(f
        .e
        .call("quantize_sample", &json!({"sample":"hits"}))
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
    assert_eq!(hist, f.e.undo_depth());
}
