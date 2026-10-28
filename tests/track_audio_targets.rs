use beatbox::Engine;
use serde_json::json;
#[test]
fn deleted_track_audio_does_not_reappear_on_name_reuse() {
    let root = std::env::temp_dir().join(format!(
        "audio_owner_audit_{}",
        beatbox::creative::fresh_seed()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tone.wav");
    let l: Vec<f32> = (0..22050).map(|i| (i as f32 * 0.07).sin() * 0.25).collect();
    beatbox::render::write_wav(&path, &l, &l).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"tone"}))
        .unwrap();
    e.call("add_track", &json!({"name":"voice","preset":"piano"}))
        .unwrap();
    e.call(
        "add_audio_clip",
        &json!({"track":"voice","sample":"tone","start_beat":0.}),
    )
    .unwrap();
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call("add_track", &json!({"name":"voice","preset":"piano"}))
        .unwrap();
    let left = e.project.audio_clips.len();
    let _ = std::fs::remove_dir_all(root);
    assert_eq!(left, 0, "audio from deleted track attached to replacement");
}

fn rig() -> (Engine, Vec<u8>) {
    let root =
        std::env::temp_dir().join(format!("audio_cleanup_{}", beatbox::creative::fresh_seed()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tone.wav");
    let l: Vec<f32> = (0..22050).map(|i| (i as f32 * 0.07).sin() * 0.25).collect();
    beatbox::render::write_wav(&path, &l, &l).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut e = Engine::new(root);
    e.call("import_sample", &json!({"path":path,"name":"tone"}))
        .unwrap();
    for t in ["voice", "keep"] {
        e.call("add_track", &json!({"name":t,"preset":"piano"}))
            .unwrap();
        for at in [0., 2.] {
            e.call(
                "add_audio_clip",
                &json!({"track":t,"sample":"tone","start_beat":at}),
            )
            .unwrap();
        }
    }
    (e, bytes)
}
#[test]
fn removes_only_owner_clips_with_undo_redo_actual_reopen() {
    let (mut e, bytes) = rig();
    let before = e.project.clone();
    let keep: Vec<_> = e
        .project
        .audio_clips
        .iter()
        .filter(|c| c.track == "keep")
        .cloned()
        .collect();
    let samples = e.project.samples.clone();
    let result = e.call("remove_track", &json!({"track":"VOICE"})).unwrap();
    assert_eq!(result["audio_clips_removed"], 2);
    assert_eq!(e.project.audio_clips, keep);
    assert_eq!(e.project.samples, samples);
    assert_eq!(std::fs::read(e.workdir.join("tone.wav")).unwrap(), bytes);
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path":"audio-owner.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path":"audio-owner.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    assert_eq!(std::fs::read(e.workdir.join("tone.wav")).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn removes_stable_owner_aliases_and_keeps_others() {
    let (mut e, _) = rig();
    let id = e.project.tracks[0].id.clone();
    e.project.audio_clips[0].track = id.to_uppercase();
    e.project.audio_clips[1].track = "VoIcE".into();
    let keep = e.project.audio_clips[2..].to_vec();
    e.call("remove_track", &json!({"track":id})).unwrap();
    assert_eq!(e.project.audio_clips, keep);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn renamed_then_removed_owner_clears_clips() {
    let (mut e, _) = rig();
    e.call("rename_track", &json!({"track":"voice","name":"take"}))
        .unwrap();
    e.call("remove_track", &json!({"track":"take"})).unwrap();
    assert!(e.project.audio_clips.iter().all(|c| c.track == "keep"));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn invalid_removal_preserves_clips_and_source() {
    let (mut e, bytes) = rig();
    let before = e.project.clone();
    assert!(e.call("remove_track", &json!({"track":"missing"})).is_err());
    assert_eq!(e.project, before);
    assert_eq!(std::fs::read(e.workdir.join("tone.wav")).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
