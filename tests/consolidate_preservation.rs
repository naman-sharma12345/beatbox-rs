use beatbox::Engine;
use serde_json::json;

fn fixture() -> (Engine, std::path::PathBuf, Vec<u8>) {
    let mut e = Engine::new(
        std::env::temp_dir().join(format!("bb236_{}", beatbox::creative::fresh_seed())),
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
fn repeated_consolidation_keeps_prior_wav_and_undo_audio_reopen() {
    let (mut e, source, original) = fixture();
    let first = e
        .call(
            "consolidate_clips",
            &json!({"track":"voice","name":"rendered"}),
        )
        .unwrap();
    let file = first["file"].as_str().unwrap().to_string();
    let bytes = std::fs::read(&file).unwrap();
    let prior = e.project.clone();
    let audio = e.mix().unwrap();
    let second = e
        .call(
            "consolidate_clips",
            &json!({"track":"voice","name":"rendered","to_beat":0.2}),
        )
        .unwrap();
    assert_eq!(second["sample"], "rendered_2");
    assert_ne!(second["file"], first["file"]);
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(
        e.project.audio_clips.iter().find(|c| c.track == "double"),
        prior.audio_clips.iter().find(|c| c.track == "double")
    );
    let final_project = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, prior);
    let undone = e.mix().unwrap();
    assert_eq!(audio.left, undone.left);
    assert_eq!(audio.right, undone.right);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, final_project);
    let current_audio = e.mix().unwrap();
    let save = e.workdir.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let mut loaded = Engine::new(e.workdir.clone());
    loaded.call("load_project", &json!({"path":save})).unwrap();
    assert_eq!(loaded.project, final_project);
    let reopened = loaded.mix().unwrap();
    assert_eq!(current_audio.left, reopened.left);
    assert_eq!(current_audio.right, reopened.right);
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
}

#[test]
fn case_collision_and_unregistered_destination_remain_intact() {
    let (mut e, source, original) = fixture();
    e.call("import_sample", &json!({"path":source,"name":"RENDERED"}))
        .unwrap();
    let dir = e.workdir.join("consolidated");
    std::fs::create_dir_all(&dir).unwrap();
    let reserved = dir.join("rendered_2.wav");
    std::fs::write(&reserved, b"existing unregistered file").unwrap();
    let out = e
        .call(
            "consolidate_clips",
            &json!({"track":"voice","name":"rendered"}),
        )
        .unwrap();
    assert_eq!(out["sample"], "rendered_3");
    assert_eq!(
        std::fs::read(reserved).unwrap(),
        b"existing unregistered file"
    );
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn no_matching_clips_does_not_write_or_mutate() {
    let (mut e, source, original) = fixture();
    let project = e.project.clone();
    assert!(e
        .call(
            "consolidate_clips",
            &json!({"track":"voice","ids":["missing"]})
        )
        .is_err());
    assert_eq!(e.project, project);
    assert!(!e.workdir.join("consolidated").exists());
    assert_eq!(std::fs::read(source).unwrap(), original);
}
