use beatbox::Engine;
use serde_json::json;

fn fixture() -> Engine {
    let mut e = Engine::new(
        std::env::temp_dir().join(format!("bb237_{}", beatbox::creative::fresh_seed())),
    );
    e.call("new_project", &json!({"name":"bounce safety"}))
        .unwrap();
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call(
        "add_notes",
        &json!({"track":"keys","notes":[{"start":0,"pitch":60,"len":1}]}),
    )
    .unwrap();
    e
}

#[test]
fn sanitized_track_names_keep_prior_bounce_and_undo_reopen_audio() {
    let mut e = fixture();
    let first = e
        .call(
            "bounce_track",
            &json!({"track":"keys","name":"print one","tail":0}),
        )
        .unwrap();
    let file = first["file"].as_str().unwrap().to_string();
    let bytes = std::fs::read(&file).unwrap();
    let prior = e.project.clone();
    let audio = e.mix().unwrap();
    let second = e
        .call(
            "bounce_track",
            &json!({"track":"keys","name":"print_one","tail":1}),
        )
        .unwrap();
    assert_eq!(second["sample"], "print_one_2");
    assert_eq!(second["track"], "print_one");
    assert_ne!(second["file"], first["file"]);
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let completed = e.project.clone();
    let final_audio = e.mix().unwrap();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, prior);
    let undone = e.mix().unwrap();
    assert_eq!(audio.left, undone.left);
    assert_eq!(audio.right, undone.right);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, completed);
    let save = e.workdir.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let mut loaded = Engine::new(e.workdir.clone());
    loaded.call("load_project", &json!({"path":save})).unwrap();
    assert_eq!(loaded.project, completed);
    let reopened = loaded.mix().unwrap();
    assert_eq!(final_audio.left, reopened.left);
    assert_eq!(final_audio.right, reopened.right);
    assert_eq!(std::fs::read(file).unwrap(), bytes);
}

#[test]
fn registered_case_and_unregistered_file_collisions_are_skipped() {
    let mut e = fixture();
    let root = e.workdir.join("bounces");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("reference.wav");
    beatbox::render::write_wav(&source, &[0.1; 1000], &[0.1; 1000]).unwrap();
    let original = std::fs::read(&source).unwrap();
    e.call("import_sample", &json!({"path":source,"name":"PRINT_ONE"}))
        .unwrap();
    let occupied = root.join("print_one_2.wav");
    std::fs::write(&occupied, b"occupied file").unwrap();
    let out = e
        .call(
            "bounce_track",
            &json!({"track":"keys","name":"print one","tail":0,"mute_original":false}),
        )
        .unwrap();
    assert_eq!(out["sample"], "print_one_3");
    assert!(!e.project.tracks[0].mute);
    assert_eq!(std::fs::read(occupied).unwrap(), b"occupied file");
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn existing_track_name_fails_without_writing_or_changing_project() {
    let mut e = fixture();
    let prior = e.project.clone();
    assert!(e
        .call("bounce_track", &json!({"track":"keys","name":"keys"}))
        .is_err());
    assert_eq!(e.project, prior);
    assert!(!e.workdir.join("bounces").exists());
}
