use beatbox::Engine;
use serde_json::json;

fn fixture(label: &str) -> (Engine, std::path::PathBuf, Vec<u8>) {
    let root = std::env::temp_dir().join(format!(
        "bb234_{}_{}",
        label,
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root);
    e.call("new_project", &json!({"name":"preservation"}))
        .unwrap();
    let path = e.samples_dir().join("take.wav");
    std::fs::create_dir_all(e.samples_dir()).unwrap();
    let x: Vec<f32> = (0..13230)
        .map(|i| ((i as f32 * 0.05).sin() + 0.2 * (i as f32 * 1.1).sin()) * 0.1)
        .collect();
    beatbox::render::write_wav(&path, &x, &x).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    e.call("import_sample", &json!({"path":path,"name":"take"}))
        .unwrap();
    for t in ["voice", "double"] {
        e.call("add_track", &json!({"name":t,"preset":"pad"}))
            .unwrap();
        e.call(
            "add_audio_clip",
            &json!({"sample":"take","track":t,"start_beat":0}),
        )
        .unwrap();
    }
    (e, path, bytes)
}

#[test]
fn every_shared_processor_keeps_original_and_previous_candidate_files() {
    for tool in [
        "eq_7band",
        "eq_1band",
        "expander_gate",
        "de_ess",
        "channel_strip",
        "vocal_clean_chain",
        "match_vocal_tone",
    ] {
        let (mut e, path, bytes) = fixture(tool);
        let clips = e.project.audio_clips.clone();
        let args = json!({"sample":"take","new_name":"take","score":false});
        // Only tone matching accepts score; the other processing schemas reject it.
        let args = if tool == "match_vocal_tone" {
            args
        } else {
            json!({"sample":"take","new_name":"take"})
        };
        let out = e.call(tool, &args).unwrap();
        assert_eq!(out["sample"], "take_2", "{tool}");
        assert_eq!(out["clips_switched"], 0);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{tool}");
        assert_eq!(e.project.audio_clips, clips);
        let first = e.samples_dir().join("take_2.wav");
        let candidate = std::fs::read(&first).unwrap();
        let out = e.call(tool, &args).unwrap();
        assert_eq!(out["sample"], "take_3");
        assert_eq!(std::fs::read(&first).unwrap(), candidate);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn occupied_file_and_case_collision_skip_names_and_track_replacement_stays_scoped() {
    let (mut e, path, bytes) = fixture("scoped");
    let reserved = e.samples_dir().join("take_2.wav");
    std::fs::write(&reserved, b"unregistered existing file").unwrap();
    let out = e
        .call(
            "eq_1band",
            &json!({"sample":"take","new_name":"take","track":"voice","output_gain":-12}),
        )
        .unwrap();
    assert_eq!(out["sample"], "take_3");
    assert_eq!(out["clips_switched"], 1);
    assert_eq!(e.project.audio_clips[0].sample, "take_3");
    assert_eq!(e.project.audio_clips[1].sample, "take");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::read(&reserved).unwrap(),
        b"unregistered existing file"
    );
    let out = e
        .call("eq_1band", &json!({"sample":"take","new_name":"TAKE"}))
        .unwrap();
    assert_ne!(out["sample"], "TAKE");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn processed_candidate_survives_undo_redo_and_actual_save_reopen() {
    let (mut e, path, bytes) = fixture("roundtrip");
    let original = e.project.clone();
    let before = beatbox::render::render(&e.project, &e.bank, &Default::default()).unwrap();
    e.call(
        "eq_1band",
        &json!({"sample":"take","new_name":"take","output_gain":-12}),
    )
    .unwrap();
    let after = beatbox::render::render(&e.project, &e.bank, &Default::default()).unwrap();
    assert_eq!(before.left, after.left);
    assert_eq!(before.right, after.right);
    let candidate = e.samples_dir().join("take_2.wav");
    let candidate_bytes = std::fs::read(&candidate).unwrap();
    let changed = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(&e.project).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(&e.project).unwrap(),
        serde_json::to_value(&changed).unwrap()
    );
    assert_eq!(std::fs::read(&candidate).unwrap(), candidate_bytes);
    let save = e.workdir.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let saved = serde_json::to_value(&e.project).unwrap();
    let mut reopened = Engine::new(e.workdir.join("reopened"));
    reopened
        .call("load_project", &json!({"path":save}))
        .unwrap();
    assert_eq!(serde_json::to_value(&reopened.project).unwrap(), saved);
    let render =
        beatbox::render::render(&reopened.project, &reopened.bank, &Default::default()).unwrap();
    assert_eq!(render.left, before.left);
    assert_eq!(render.right, before.right);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
