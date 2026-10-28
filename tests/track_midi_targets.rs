use beatbox::Engine;
use serde_json::json;
#[test]
fn removed_owner_midi_link_does_not_control_replacement() {
    let mut e = Engine::new(std::env::temp_dir());
    e.call("add_track", &json!({"name":"voice","preset":"piano"}))
        .unwrap();
    e.call(
        "midi_input",
        &json!({"action":"link","control":"cc 7","target":"volume:voice"}),
    )
    .unwrap();
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call(
        "add_track",
        &json!({"name":"voice","preset":"piano","volume_db":-12.}),
    )
    .unwrap();
    e.call("midi_event", &json!({"messages":[[176,7,127]]}))
        .unwrap();
    assert_eq!(
        e.project.tracks[0].volume_db, -12.,
        "deleted track MIDI control retargeted replacement"
    );
}
fn rig() -> Engine {
    let mut e = Engine::new(
        std::env::temp_dir().join(format!("midi_owner_{}", beatbox::creative::fresh_seed())),
    );
    for n in ["voice", "keep"] {
        e.call(
            "add_track",
            &json!({"name":n,"preset":"piano","volume_db":-12.}),
        )
        .unwrap();
    }
    for (c, t) in [
        (7, "volume:voice"),
        (10, "pan:voice"),
        (11, "volume:keep"),
        (12, "volume:master"),
    ] {
        e.call(
            "midi_input",
            &json!({"action":"link","control":format!("cc {c}"),"target":t}),
        )
        .unwrap();
    }
    e
}
#[test]
fn rename_follows_identity_and_display_and_unlink_keep_names() {
    let mut e = rig();
    e.call("rename_track", &json!({"track":"voice","name":"take"}))
        .unwrap();
    let s = e.call("midi_input", &json!({"action":"status"})).unwrap();
    assert_eq!(s["links"][0]["target"], "volume:take");
    e.call("midi_event", &json!({"messages":[[176,7,127],[176,10,0]]}))
        .unwrap();
    assert_eq!(e.project.tracks[0].volume_db, 6.);
    assert_eq!(e.project.tracks[0].pan, -1.);
    e.call(
        "midi_input",
        &json!({"action":"unlink","target":"volume:take"}),
    )
    .unwrap();
    assert_eq!(e.midi.links.len(), 3);
}
#[test]
fn deleted_bindings_survive_undo_without_targeting_replacement() {
    let mut e = rig();
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call("undo", &json!({})).unwrap();
    e.call("midi_event", &json!({"messages":[[176,7,127]]}))
        .unwrap();
    assert_eq!(e.project.tracks[0].volume_db, 6.);
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call("redo", &json!({})).unwrap();
    e.call(
        "add_track",
        &json!({"name":"voice","preset":"piano","volume_db":-12.}),
    )
    .unwrap();
    e.call(
        "midi_event",
        &json!({"messages":[[176,7,127],[176,10,0],[176,11,127],[176,12,127]]}),
    )
    .unwrap();
    assert_eq!(e.project.tracks[1].volume_db, -12.);
    assert_eq!(e.project.tracks[1].pan, 0.);
    assert_eq!(e.project.tracks[0].volume_db, 6.);
    assert_eq!(e.project.master_volume_db, 6.);
}
#[test]
fn pending_learn_and_saved_setup_cannot_attach_to_replacement() {
    let mut e = rig();
    e.call(
        "midi_input",
        &json!({"action":"learn","target":"volume:voice"}),
    )
    .unwrap();
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call(
        "add_track",
        &json!({"name":"voice","preset":"piano","volume_db":-12.}),
    )
    .unwrap();
    e.call("midi_event", &json!({"messages":[[176,20,127],[176,20,0]]}))
        .unwrap();
    assert_eq!(e.project.tracks[1].volume_db, -12.);
    e.call("save_project", &json!({"path":"midi.json"}))
        .unwrap();
    e.call("load_project", &json!({"path":"midi.json"}))
        .unwrap();
    e.call(
        "midi_event",
        &json!({"messages":[[176,7,127],[176,20,127],[176,11,127]]}),
    )
    .unwrap();
    assert_eq!(e.project.tracks[1].volume_db, -12.);
    assert_eq!(e.project.tracks[0].volume_db, 6.);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn live_bindings_roundtrip_and_legacy_name_bindings_upgrade() {
    let mut e = rig();
    std::fs::create_dir_all(&e.workdir).unwrap();
    e.call("save_project", &json!({"path":"live.json"}))
        .unwrap();
    let mut p = e.project.clone();
    p.midi.as_mut().unwrap().links[0].target = "volume:voice".into();
    std::fs::write(
        e.workdir.join("legacy.json"),
        serde_json::to_string(&p).unwrap(),
    )
    .unwrap();
    for file in ["live.json", "legacy.json"] {
        let mut fresh = Engine::new(e.workdir.clone());
        fresh.call("load_project", &json!({"path":file})).unwrap();
        fresh
            .call("rename_track", &json!({"track":"voice","name":"take"}))
            .unwrap();
        fresh
            .call("midi_event", &json!({"messages":[[176,7,127]]}))
            .unwrap();
        assert_eq!(fresh.project.tracks[0].volume_db, 6.);
    }
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn deletion_undo_redo_does_not_retarget_surviving_link() {
    let mut e = rig();
    e.call("remove_track", &json!({"track":"voice"})).unwrap();
    e.call("undo", &json!({})).unwrap();
    e.call("redo", &json!({})).unwrap();
    e.call(
        "add_track",
        &json!({"name":"voice","preset":"piano","volume_db":-12.}),
    )
    .unwrap();
    e.call("midi_event", &json!({"messages":[[176,7,127]]}))
        .unwrap();
    assert_eq!(e.project.tracks[1].volume_db, -12.);
}
