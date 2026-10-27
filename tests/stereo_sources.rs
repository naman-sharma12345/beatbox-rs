use beatbox::Engine;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
    engine: Engine,
}
impl Fixture {
    fn new(opposed: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_stereo_sources_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.wav");
        let l: Vec<f32> = (0..22050).map(|i| (i as f32 * 0.07).sin() * 0.3).collect();
        let r: Vec<f32> = if opposed {
            l.iter().map(|v| -v).collect()
        } else {
            (0..22050).map(|i| (i as f32 * 0.13).cos() * 0.12).collect()
        };
        beatbox::render::write_wav(&path, &l, &r).unwrap();
        let bytes = fs::read(&path).unwrap();
        let mut engine = Engine::new(root.clone());
        engine
            .call("import_sample", &json!({"path":path,"name":"wide"}))
            .unwrap();
        Self {
            root,
            path,
            bytes,
            engine,
        }
    }
    fn parity(&mut self, tool: &str, args: Value) -> (Value, Value) {
        let project = self.engine.project.clone();
        let revision = self.engine.revision;
        let mut sample = args.clone();
        sample["sample"] = json!("wide");
        let a = self.engine.call(tool, &sample).unwrap();
        let mut path = args;
        path["path"] = json!(self.path);
        let b = self.engine.call(tool, &path).unwrap();
        assert_eq!(self.engine.project, project);
        assert_eq!(self.engine.revision, revision);
        assert_eq!(fs::read(&self.path).unwrap(), self.bytes);
        (a, b)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn opposed_source_analysis_matches_file_and_keeps_nonzero_peak() {
    let mut f = Fixture::new(true);
    let (a, b) = f.parity("analyze_audio", json!({}));
    for field in [
        "stereo_correlation",
        "true_peak_dbtp",
        "integrated_lufs",
        "side_to_mid_db",
        "seconds",
    ] {
        assert_eq!(a[field], b[field], "{field}");
    }
    assert_eq!(a["stereo_correlation"], -1.0);
    assert!(a["true_peak_dbtp"].as_f64().unwrap() > -20.0);
}
#[test]
fn distinct_stereo_source_analysis_matches_file_metrics() {
    let mut f = Fixture::new(false);
    let (a, b) = f.parity("analyze_audio", json!({}));
    for field in [
        "stereo_correlation",
        "true_peak_dbtp",
        "integrated_lufs",
        "side_to_mid_db",
        "spectral_centroid_hz",
        "bands",
    ] {
        assert_eq!(a[field], b[field], "{field}");
    }
}
#[test]
fn limiter_measurement_sees_same_stereo_source_as_file() {
    let mut f = Fixture::new(true);
    let (a, b) = f.parity("true_peak_limit", json!({}));
    for field in [
        "true_peak_before_dbtp",
        "true_peak_after_dbtp",
        "integrated_lufs_before",
        "integrated_lufs_after",
    ] {
        assert_eq!(a[field], b[field], "{field}");
    }
    assert!(a["true_peak_before_dbtp"].as_f64().unwrap() > -20.0);
}
#[test]
fn registered_source_resolves_relative_path_and_case() {
    let mut f = Fixture::new(true);
    f.engine.project.samples[0].path = "wide.wav".into();
    let a = f
        .engine
        .call("analyze_audio", &json!({"sample":"WIDE"}))
        .unwrap();
    let b = f
        .engine
        .call("analyze_audio", &json!({"path":f.path}))
        .unwrap();
    assert_eq!(a["stereo_correlation"], b["stereo_correlation"]);
    assert_eq!(a["true_peak_dbtp"], b["true_peak_dbtp"]);
}
#[test]
fn unavailable_source_returns_error_without_project_mutation() {
    let mut f = Fixture::new(true);
    let before = f.engine.project.clone();
    let revision = f.engine.revision;
    fs::remove_file(&f.path).unwrap();
    assert!(f
        .engine
        .call("analyze_audio", &json!({"sample":"wide"}))
        .is_err());
    assert_eq!(f.engine.project, before);
    assert_eq!(f.engine.revision, revision);
}
