use beatbox::{instruments::Instrument, Engine};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    engine: Engine,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "bb_slice_range_{}",
            beatbox::creative::fresh_seed()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("loop.wav");
        let data = vec![0.3; 22050];
        beatbox::render::write_wav(&path, &data, &data).unwrap();
        let mut engine = Engine::new(root.clone());
        engine
            .call("import_sample", &json!({"path": path, "name":"loop"}))
            .unwrap();
        Self { root, engine }
    }
    fn call(&mut self, args: Value) -> anyhow::Result<Value> {
        self.engine.call("slice_sample", &args)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn overflow_fails_before_changing_an_existing_track_or_pattern() {
    let mut f = Fixture::new();
    f.call(json!({"sample":"loop","equal":4,"root":36}))
        .unwrap();
    let before = f.engine.project.clone();
    let revision = f.engine.revision;
    let history = f.engine.undo_depth();
    for (root, equal) in [(126, 4), (127, 2), (100, 64)] {
        let error = f
            .call(json!({"sample":"loop","root":root,"equal":equal}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceed the playable note range"), "{error}");
        assert_eq!(before, f.engine.project);
        assert_eq!(revision, f.engine.revision);
        assert_eq!(history, f.engine.undo_depth());
    }
}
#[test]
fn highest_valid_root_maps_every_slice_to_a_distinct_note() {
    let mut f = Fixture::new();
    let result = f
        .call(json!({"sample":"loop","equal":4,"root":124}))
        .unwrap();
    assert_eq!(result["slices"], 4);
    assert_eq!(result["notes_written"], 4);
    let notes = &f.engine.project.patterns[0].clips["loop_slices"];
    assert_eq!(
        notes.iter().map(|n| n.pitch).collect::<Vec<_>>(),
        vec![124, 125, 126, 127]
    );
    let Instrument::Sampler(s) = &f.engine.project.tracks[0].instrument else {
        panic!("sampler")
    };
    assert_eq!(s.root, 124);
    assert_eq!(s.slices.len(), 4);
    let names = result["map"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["note"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 4);
}
#[test]
fn default_root_still_maps_the_full_sixty_four_slice_limit() {
    let mut f = Fixture::new();
    let result = f
        .call(json!({"sample":"loop","equal":64,"max_slices":64,"write_pattern":false}))
        .unwrap();
    assert_eq!(result["slices"], 64);
    assert_eq!(result["notes_written"], 0);
    let names = result["map"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["note"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 64);
}

#[test]
fn no_pattern_option_does_not_allow_an_unplayable_map() {
    let mut f = Fixture::new();
    let before = f.engine.project.clone();
    let err = f
        .call(json!({"sample":"loop","equal":4,"root":"G9","write_pattern":false}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("choose root 124 or lower"), "{err}");
    assert_eq!(before, f.engine.project);
}
#[test]
fn valid_high_note_slice_map_survives_undo_and_redo() {
    let mut f = Fixture::new();
    let before = f.engine.project.clone();
    f.call(json!({"sample":"loop","equal":4,"root":124}))
        .unwrap();
    let after = f.engine.project.clone();
    assert!(f.engine.undo());
    assert_eq!(before, f.engine.project);
    assert!(f.engine.redo());
    assert_eq!(after, f.engine.project);
}

#[test]
fn boundary_map_preserves_source_and_audio_after_actual_reopen() {
    let mut f = Fixture::new();
    let source = f.root.join("loop.wav");
    let bytes = fs::read(&source).unwrap();
    f.call(json!({"sample":"loop","equal":4,"root":124}))
        .unwrap();
    let project = f.engine.project.clone();
    let rendered = f.engine.render_stems_all(&project).unwrap();
    assert!(rendered.left.iter().any(|v| v.abs() > 0.01));
    let path = f.root.join("reopen.json");
    f.engine
        .call("save_project", &json!({"path":path}))
        .unwrap();
    let mut reopened = Engine::new(f.root.clone());
    reopened
        .call("load_project", &json!({"path":path}))
        .unwrap();
    assert_eq!(reopened.project, project);
    let again = reopened
        .render_stems_all(&reopened.project.clone())
        .unwrap();
    assert_eq!(rendered.left, again.left);
    assert_eq!(rendered.right, again.right);
    assert_eq!(bytes, fs::read(source).unwrap());
}

#[test]
fn every_note_at_the_top_boundary_plays_its_own_slice() {
    let mut f = Fixture::new();
    let path = f.root.join("distinct.wav");
    let audio: Vec<f32> = (0..22048).map(|i| 0.1 * (1 + i / 5512) as f32).collect();
    beatbox::render::write_wav(&path, &audio, &audio).unwrap();
    f.engine
        .call("import_sample", &json!({"path":path,"name":"distinct"}))
        .unwrap();
    f.call(json!({"sample":"distinct","equal":4,"root":124,"write_pattern":false}))
        .unwrap();
    let instrument = &f.engine.project.tracks[0].instrument;
    let Instrument::Sampler(params) = instrument else {
        panic!("sampler")
    };
    let gain = params.gain;
    for k in 0..4 {
        let audio = beatbox::instruments::render_note(
            instrument,
            (124 + k) as f32,
            1.0,
            1.0,
            &f.engine.bank,
            0,
        );
        assert_eq!(audio.len(), 5512);
        let mid = audio[1000];
        assert!(
            (mid - gain * 0.1 * (1 + k) as f32).abs() < 0.0001,
            "slice {k}: {mid}"
        );
    }
    assert!(
        beatbox::instruments::render_note(instrument, 123.0, 1.0, 1.0, &f.engine.bank, 0)
            .is_empty()
    );
}
