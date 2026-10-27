use beatbox::Engine;
use serde_json::json;

fn fixture() -> (Engine, std::path::PathBuf, Vec<u8>) {
    let mut e = Engine::new(
        std::env::temp_dir().join(format!("bb235_{}", beatbox::creative::fresh_seed())),
    );
    e.call("new_project", &json!({"name":"silence safety"}))
        .unwrap();
    std::fs::create_dir_all(e.samples_dir()).unwrap();
    let path = e.samples_dir().join("take.wav");
    let x: Vec<f32> = (0..13230)
        .map(|i| {
            if (2000..9000).contains(&i) {
                (i as f32 * 0.05).sin() * 0.1
            } else {
                0.0
            }
        })
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
fn repeated_trim_preserves_source_candidate_clips_render_and_reopen() {
    let (mut e, source, original) = fixture();
    let before = e.mix().unwrap();
    let first = e
        .call(
            "strip_silence",
            &json!({"sample":"take","apply":true,"pad_ms":0}),
        )
        .unwrap();
    assert_eq!(first["new_sample"], "take_stripped");
    let candidate = e.samples_dir().join("take_stripped.wav");
    let bytes = std::fs::read(&candidate).unwrap();
    let project = e.project.clone();
    let second = e
        .call(
            "strip_silence",
            &json!({"sample":"take","apply":true,"pad_ms":50}),
        )
        .unwrap();
    assert_eq!(second["new_sample"], "take_stripped_2");
    assert_ne!(
        std::fs::read(e.samples_dir().join("take_stripped_2.wav")).unwrap(),
        bytes
    );
    assert_eq!(std::fs::read(&candidate).unwrap(), bytes);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(e.project.audio_clips, project.audio_clips);
    let after = e.mix().unwrap();
    assert_eq!(before.left, after.left);
    assert_eq!(before.right, after.right);
    let completed = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, project);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, completed);
    let save = e.workdir.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let mut loaded = Engine::new(e.workdir.clone());
    loaded.call("load_project", &json!({"path":save})).unwrap();
    assert_eq!(loaded.project, completed);
    let reopened = loaded.mix().unwrap();
    assert_eq!(before.left, reopened.left);
    assert_eq!(before.right, reopened.right);
    assert_eq!(std::fs::read(&candidate).unwrap(), bytes);
}

#[test]
fn occupied_unregistered_and_case_colliding_names_are_not_overwritten() {
    let (mut e, source, original) = fixture();
    e.call(
        "import_sample",
        &json!({"path":source,"name":"TAKE_STRIPPED"}),
    )
    .unwrap();
    let reserved = e.samples_dir().join("take_stripped_2.wav");
    std::fs::write(&reserved, b"unregistered existing file").unwrap();
    let out = e
        .call("strip_silence", &json!({"sample":"take","apply":true}))
        .unwrap();
    assert_eq!(out["new_sample"], "take_stripped_3");
    assert_eq!(
        std::fs::read(&reserved).unwrap(),
        b"unregistered existing file"
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn analysis_only_does_not_create_a_candidate_or_change_project() {
    let (mut e, source, original) = fixture();
    let project = e.project.clone();
    let out = e.call("strip_silence", &json!({"sample":"take"})).unwrap();
    assert!(out.get("new_sample").is_none());
    assert_eq!(e.project, project);
    assert!(!e.samples_dir().join("take_stripped.wav").exists());
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn all_silent_apply_fails_without_files_or_project_changes() {
    let (mut e, source, _) = fixture();
    let silence = vec![0.0; 13230];
    beatbox::render::write_wav(&source, &silence, &silence).unwrap();
    e.call("import_sample", &json!({"path":source,"name":"silent"}))
        .unwrap();
    let project = e.project.clone();
    let files: Vec<_> = std::fs::read_dir(e.samples_dir())
        .unwrap()
        .map(|v| v.unwrap().path())
        .collect();
    let error = e
        .call("strip_silence", &json!({"sample":"silent","apply":true}))
        .unwrap_err();
    assert!(error.to_string().contains("below the threshold"));
    assert_eq!(e.project, project);
    let after: Vec<_> = std::fs::read_dir(e.samples_dir())
        .unwrap()
        .map(|v| v.unwrap().path())
        .collect();
    assert_eq!(files, after);
}
