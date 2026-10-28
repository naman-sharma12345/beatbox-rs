use beatbox::{audio_edit, samples, Engine};
use serde_json::json;
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    e: Engine,
    reference: Vec<f32>,
    take: Vec<f32>,
    source: Vec<u8>,
}
impl Fixture {
    fn new(delay: isize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_stereo_align_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut reference = vec![0.; 176400];
        for (k, s) in [8000, 30000, 57000, 83000, 119000, 148000]
            .iter()
            .enumerate()
        {
            for j in 0..1800 {
                reference[s + j] =
                    (j as f32 * (0.13 + k as f32 * 0.01)).sin() * (-(j as f32) / 250.).exp() * 0.6;
            }
        }
        let take = if delay >= 0 {
            let mut t = vec![0.; delay as usize];
            t.extend_from_slice(&reference);
            t
        } else {
            reference[(-delay) as usize..].to_vec()
        };
        let mut e = Engine::new(root.clone());
        for (name, data) in [("reference", &reference), ("take", &take)] {
            let p = root.join(format!("{name}.wav"));
            let r: Vec<f32> = data.iter().map(|v| -v).collect();
            beatbox::render::write_wav(&p, data, &r).unwrap();
            e.call("import_sample", &json!({"path":p,"name":name}))
                .unwrap();
        }
        let source = fs::read(root.join("take.wav")).unwrap();
        Self {
            root,
            e,
            reference,
            take,
            source,
        }
    }
    fn align(&mut self) -> (serde_json::Value, Vec<f32>, Vec<f32>) {
        let v = self
            .e
            .call(
                "shift_take",
                &json!({"sample":"take","reference":"reference","max_offset_s":0.3}),
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
        assert_eq!(self.source, fs::read(self.root.join("take.wav")).unwrap());
        (v, l, r)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn opposed_delayed_take_aligns_with_one_shift() {
    let mut f = Fixture::new(4410);
    let (v, l, r) = f.align();
    assert!((v["offset_ms"].as_f64().unwrap() - 100.).abs() < 2.);
    assert!(l.iter().any(|v| v.abs() > 0.1));
    assert!(l.iter().zip(&r).all(|(l, r)| (l + r).abs() < 2. / 32768.));
}
#[test]
fn early_take_gets_shared_silence_prefix() {
    let mut f = Fixture::new(-2205);
    let (v, l, r) = f.align();
    assert!((v["offset_ms"].as_f64().unwrap() + 50.).abs() < 2.);
    assert!(l[..2100].iter().chain(&r[..2100]).all(|v| v.abs() < 1e-6));
    assert_eq!(l.len(), r.len());
}
#[test]
fn stereo_duplicate_onsets_do_not_inflate_votes() {
    let f = Fixture::new(4410);
    let expected = audio_edit::find_offset(&f.reference, &f.take, 0.3);
    let r: Vec<f32> = f.reference.iter().map(|v| -v).collect();
    let t: Vec<f32> = f.take.iter().map(|v| -v).collect();
    let actual = audio_edit::find_offset_stereo(&f.reference, &r, &f.take, &t, 0.3);
    assert_eq!(expected, actual);
}
#[test]
fn right_only_source_can_align_without_leaking_left() {
    let mut f = Fixture::new(4410);
    for (name, data) in [("reference", &f.reference), ("take", &f.take)] {
        beatbox::render::write_wav(
            &f.root.join(format!("{name}.wav")),
            &vec![0.; data.len()],
            data,
        )
        .unwrap();
    }
    f.source = fs::read(f.root.join("take.wav")).unwrap();
    let (_, l, r) = f.align();
    assert!(l.iter().all(|v| v.abs() < 1e-6));
    assert!(r.iter().any(|v| v.abs() > 0.1));
}
#[test]
fn undo_redo_reopen_and_repeated_output_preserve_audio() {
    let mut f = Fixture::new(4410);
    let before = f.e.project.clone();
    let (v, l, r) = f.align();
    let after = f.e.project.clone();
    assert!(f.e.undo());
    assert_eq!(before, f.e.project);
    assert!(f.e.redo());
    assert_eq!(after, f.e.project);
    let (v2, _, _) = f.align();
    assert_ne!(v["sample"], v2["sample"]);
    let save = f.root.join("project.json");
    f.e.call("save_project", &json!({"path":save})).unwrap();
    let mut e = Engine::new(f.root.clone());
    e.call("load_project", &json!({"path":save})).unwrap();
    assert_eq!(f.e.project, e.project);
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
fn unavailable_corrupt_or_silent_reference_does_not_change_project() {
    let mut f = Fixture::new(4410);
    let before = f.e.project.clone();
    let rev = f.e.revision;
    let hist = f.e.undo_depth();
    let p = f.root.join("reference.wav");
    for data in [b"not audio".as_slice(), b"".as_slice()] {
        fs::write(&p, data).unwrap();
        assert!(f
            .e
            .call(
                "shift_take",
                &json!({"sample":"take","reference":"reference"})
            )
            .is_err());
    }
    beatbox::render::write_wav(&p, &vec![0.; 176400], &vec![0.; 176400]).unwrap();
    assert!(f
        .e
        .call(
            "shift_take",
            &json!({"sample":"take","reference":"reference"})
        )
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(rev, f.e.revision);
    assert_eq!(hist, f.e.undo_depth());
}
#[test]
fn relative_paths_and_case_insensitive_names_work() {
    let mut f = Fixture::new(4410);
    for s in &mut f.e.project.samples {
        s.path = format!("{}.wav", s.name);
    }
    let v =
        f.e.call(
            "shift_take",
            &json!({"sample":"TAKE","reference":"REFERENCE","max_offset_s":0.3}),
        )
        .unwrap();
    assert!((v["offset_ms"].as_f64().unwrap() - 100.).abs() < 2.);
}

#[test]
fn rendered_mix_reference_aligns_stereo_take() {
    let root =
        std::env::temp_dir().join(format!("bb_mix_align_{}", beatbox::creative::fresh_seed()));
    let mut e = Engine::new(root.clone());
    e.call("new_project", &json!({"name":"mix reference"}))
        .unwrap();
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call("add_notes",&json!({"track":"keys","notes":[{"start":0.5,"pitch":60,"len":0.15},{"start":1.7,"pitch":67,"len":0.15},{"start":3.1,"pitch":64,"len":0.15},{"start":4.6,"pitch":72,"len":0.15}]})).unwrap();
    let m = e.mix().unwrap();
    let delay = 4410;
    let mut l = vec![0.; delay];
    l.extend_from_slice(&m.left);
    let mut r = vec![0.; delay];
    r.extend_from_slice(&m.right);
    let p = root.join("take.wav");
    beatbox::render::write_wav(&p, &l, &r).unwrap();
    e.call("import_sample", &json!({"path":p,"name":"take"}))
        .unwrap();
    let before = e.project.clone();
    assert!(e
        .call(
            "shift_take",
            &json!({"sample":"take","reference":"mix","max_offset_s":0.3})
        )
        .is_err());
    assert_eq!(before, e.project);
    let v = e
        .call(
            "shift_take",
            &json!({"sample":"take","reference":"mix","max_offset_s":0.3,"min_confidence":0.2}),
        )
        .unwrap();
    assert!((v["offset_ms"].as_f64().unwrap() - 100.).abs() < 2.);
    std::fs::remove_dir_all(root).unwrap();
}
