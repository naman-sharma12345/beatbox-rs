use beatbox::{instruments, Engine};
use serde_json::json;
use std::fs;
#[test]
fn overdividing_tiny_sample_does_not_create_empty_slices() {
    let root = std::env::temp_dir().join(format!(
        "bb_short_slices_{}",
        beatbox::creative::fresh_seed()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("tiny.wav");
    let audio = vec![0.2, 0.4];
    beatbox::render::write_wav(&path, &audio, &audio).unwrap();
    let original = fs::read(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"tiny"}))
        .unwrap();
    let result = e
        .call(
            "slice_sample",
            &json!({"sample":"tiny","equal":64,"max_slices":64,"root":126}),
        )
        .unwrap();
    assert_eq!(result["slices"], 2);
    assert_eq!(result["notes_written"], 2);
    let instruments::Instrument::Sampler(s) = &e.project.tracks[0].instrument else {
        panic!("sampler")
    };
    assert_eq!(s.slices.len(), 2);
    assert!(s.slices[1] > s.slices[0]);
    for pitch in [126., 127.] {
        assert_eq!(
            instruments::render_note(&e.project.tracks[0].instrument, pitch, 1., 1., &e.bank, 0)
                .len(),
            1
        );
    }
    assert_eq!(original, fs::read(path).unwrap());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn one_frame_source_gets_one_real_slice() {
    let root =
        std::env::temp_dir().join(format!("bb_one_slice_{}", beatbox::creative::fresh_seed()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("one.wav");
    beatbox::render::write_wav(&path, &[0.2], &[0.2]).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"one"}))
        .unwrap();
    let result = e
        .call(
            "slice_sample",
            &json!({"sample":"one","equal":4,"root":127}),
        )
        .unwrap();
    assert_eq!(result["slices"], 1);
    assert_eq!(result["notes_written"], 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tiny_slice_map_survives_undo_redo_and_actual_reopen_without_source_changes() {
    let root = std::env::temp_dir().join(format!(
        "bb_tiny_reopen_{}",
        beatbox::creative::fresh_seed()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("tiny.wav");
    beatbox::render::write_wav(&path, &[0.2, 0.4, 0.1], &[0.2, 0.4, 0.1]).unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"tiny"}))
        .unwrap();
    let before = e.project.clone();
    let r = e
        .call(
            "slice_sample",
            &json!({"sample":"tiny","equal":64,"max_slices":64}),
        )
        .unwrap();
    assert_eq!(r["slices"], 3);
    let after = e.project.clone();
    assert!(e.undo());
    assert_eq!(before, e.project);
    assert!(e.redo());
    assert_eq!(after, e.project);
    let save = root.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let mut reopened = Engine::new(root.clone());
    reopened
        .call("load_project", &json!({"path":save}))
        .unwrap();
    assert_eq!(after, reopened.project);
    for pitch in 36..39 {
        let a = instruments::render_note(
            &e.project.tracks[0].instrument,
            pitch as f32,
            1.,
            1.,
            &e.bank,
            0,
        );
        let b = instruments::render_note(
            &reopened.project.tracks[0].instrument,
            pitch as f32,
            1.,
            1.,
            &reopened.bank,
            0,
        );
        assert_eq!(a.len(), 1);
        assert_eq!(a, b);
    }
    assert_eq!(bytes, fs::read(path).unwrap());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn uneven_frame_count_keeps_requested_slices_ordered_and_nonempty() {
    let root = std::env::temp_dir().join(format!(
        "bb_uneven_slices_{}",
        beatbox::creative::fresh_seed()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("uneven.wav");
    let audio = vec![0.2; 7];
    beatbox::render::write_wav(&path, &audio, &audio).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"odd"}))
        .unwrap();
    let r = e
        .call("slice_sample", &json!({"sample":"odd","equal":4}))
        .unwrap();
    assert_eq!(r["slices"], 4);
    let mut total = 0;
    for pitch in 36..40 {
        let a = instruments::render_note(
            &e.project.tracks[0].instrument,
            pitch as f32,
            1.,
            1.,
            &e.bank,
            0,
        );
        assert!(!a.is_empty());
        total += a.len();
    }
    assert_eq!(total, 7);
    fs::remove_dir_all(root).unwrap();
}
