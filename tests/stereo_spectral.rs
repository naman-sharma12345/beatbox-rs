use beatbox::{
    audio_edit::{self, SpectralRegion},
    samples, Engine,
};
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
            "bb_spectral_channels_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.wav");
        let left: Vec<f32> = (0..22050)
            .map(|i| (i as f32 * 0.04).sin() * 0.3 + (i as f32 * 0.7).sin() * 0.1)
            .collect();
        let right: Vec<f32> = left
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if opposed {
                    -v
                } else {
                    (i as f32 * 0.7).sin() * 0.2
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
    fn edit(&mut self, mode: &str) -> (Value, Vec<f32>, Vec<f32>) {
        let r=self.e.call("spectral_edit_sample",&json!({"sample":"wide","start_s":0.1,"end_s":0.4,"low_hz":3000,"high_hz":6000,"gain_db":-24,"mode":mode})).unwrap();
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
    fn expected(&self, mode: &str) -> (Vec<f32>, Vec<f32>) {
        let reg = SpectralRegion {
            t0: 0.1,
            t1: 0.4,
            lo_hz: 3000.,
            hi_hz: 6000.,
            gain_db: -24.,
            outside: mode == "outside",
            soft_ms: 10.,
            soft_oct: 0.1,
        };
        (
            audio_edit::spectral_edit(&self.left, 44100., &reg).0,
            audio_edit::spectral_edit(&self.right, 44100., &reg).0,
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
fn opposed_channels_do_not_cancel_before_spectral_edit() {
    let mut f = Fixture::new(true);
    let (_, l, r) = f.edit("band");
    assert!(l.iter().any(|v| v.abs() > 0.2));
    assert!(l.iter().zip(&r).all(|(l, r)| (l + r).abs() < 2. / 32768.));
    let (a, b) = f.expected("band");
    close(&l, &a);
    close(&r, &b);
}
#[test]
fn separate_channels_match_same_region_for_band_and_outside_modes() {
    for mode in ["band", "outside"] {
        let mut f = Fixture::new(false);
        let (_, l, r) = f.edit(mode);
        let (a, b) = f.expected(mode);
        close(&l, &a);
        close(&r, &b);
        assert_ne!(l, r);
    }
}
#[test]
fn energy_change_uses_both_channels() {
    let mut f = Fixture::new(false);
    let (v, _, _) = f.edit("band");
    let (l, r) = f.expected("band");
    let energy = |l: &[f32], r: &[f32]| l.iter().chain(r).map(|v| (*v as f64).powi(2)).sum::<f64>();
    let change = (10. * (energy(&l, &r) / energy(&f.left, &f.right)).log10() * 10.).round() / 10.;
    assert!((v["changed_db"].as_f64().unwrap() - change).abs() < 0.00001);
}
#[test]
fn repeated_outputs_undo_redo_and_actual_reopen_preserve_stereo() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let (v, l, r) = f.edit("band");
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
    let (second, _, _) = f.edit("outside");
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
fn invalid_region_does_not_change_project_or_create_an_output() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let history = f.e.undo_depth();
    let revision = f.e.revision;
    let files = || {
        fs::read_dir(f.root.join("samples"))
            .map(|i| i.count())
            .unwrap_or(0)
    };
    let count = files();
    assert!(f
        .e
        .call(
            "spectral_edit_sample",
            &json!({"sample":"wide","start_s":0.4,"end_s":0.1})
        )
        .is_err());
    assert_eq!(before, f.e.project);
    assert_eq!(history, f.e.undo_depth());
    assert_eq!(revision, f.e.revision);
    assert_eq!(count, files());
}

#[test]
fn mono_equivalent_input_keeps_previous_processing_math() {
    let mut f = Fixture::new(false);
    beatbox::render::write_wav(&f.path, &f.left, &f.left).unwrap();
    f.bytes = fs::read(&f.path).unwrap();
    let (l, r) = samples::decode_stereo(&f.path).unwrap();
    f.left = l;
    f.right = r;
    let (v, l, r) = f.edit("band");
    let (a, b) = f.expected("band");
    close(&l, &a);
    close(&r, &b);
    assert_eq!(l, r);
    let reg = SpectralRegion {
        t0: 0.1,
        t1: 0.4,
        lo_hz: 3000.,
        hi_hz: 6000.,
        gain_db: -24.,
        outside: false,
        soft_ms: 10.,
        soft_oct: 0.1,
    };
    let (_, change) = audio_edit::spectral_edit(&f.left, 44100., &reg);
    assert!(
        (v["changed_db"].as_f64().unwrap() - ((change * 10.).round() / 10.) as f64).abs() < 0.00001
    );
}
#[test]
fn processing_one_active_channel_does_not_leak_into_the_silent_channel() {
    let mut f = Fixture::new(false);
    let silence = vec![0.; f.left.len()];
    beatbox::render::write_wav(&f.path, &silence, &f.left).unwrap();
    f.bytes = fs::read(&f.path).unwrap();
    let (l, r) = samples::decode_stereo(&f.path).unwrap();
    f.left = l;
    f.right = r;
    let (_, l, r) = f.edit("band");
    assert!(l.iter().all(|v| v.abs() < 0.00001));
    assert!(r.iter().any(|v| v.abs() > 0.2));
    let (a, b) = f.expected("band");
    close(&l, &a);
    close(&r, &b);
}

#[test]
fn relative_source_and_case_insensitive_name_keep_original_stereo() {
    let mut f = Fixture::new(true);
    f.e.project.samples[0].path = "wide.wav".into();
    let result =
        f.e.call(
            "spectral_edit_sample",
            &json!({"sample":"WIDE","low_hz":3000,"high_hz":6000}),
        )
        .unwrap();
    let info =
        f.e.project
            .samples
            .iter()
            .find(|s| s.name == result["sample"].as_str().unwrap())
            .unwrap();
    let (l, r) = samples::decode_stereo(std::path::Path::new(&info.path)).unwrap();
    assert!(l.iter().any(|v| v.abs() > 0.2));
    assert!(l.iter().zip(&r).all(|(l, r)| (l + r).abs() < 2. / 32768.));
    assert_eq!(f.bytes, fs::read(&f.path).unwrap());
}
#[test]
fn missing_or_corrupt_original_returns_error_without_registering_output() {
    let mut f = Fixture::new(false);
    let before = f.e.project.clone();
    let revision = f.e.revision;
    let history = f.e.undo_depth();
    fs::remove_file(&f.path).unwrap();
    for corrupt in [false, true] {
        if corrupt {
            fs::write(&f.path, b"not an audio file").unwrap();
        }
        let err = f.e.call("spectral_edit_sample", &json!({"sample":"wide"}));
        assert!(err.is_err());
        assert_eq!(before, f.e.project);
        assert_eq!(revision, f.e.revision);
        assert_eq!(history, f.e.undo_depth());
    }
}
