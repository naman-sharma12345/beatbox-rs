use beatbox::Engine;
use serde_json::json;
#[test]
fn removed_send_controls_cannot_drive_recreated_send() {
    let root = std::env::temp_dir().join(format!(
        "bb_send_controls_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root.clone());
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call("add_bus", &json!({"name":"verb"})).unwrap();
    e.call("set_send", &json!({"track":"keys","bus":"verb","db":-12.}))
        .unwrap();
    e.call(
        "create_automation_clip",
        &json!({"name":"wet","track":"keys","param":"send.verb","points":[[0.,-30.],[4.,-12.]]}),
    )
    .unwrap();
    e.call(
        "set_send",
        &json!({"track":"keys","bus":"verb","remove":true}),
    )
    .unwrap();
    e.call("set_send", &json!({"track":"keys","bus":"verb","db":-6.}))
        .unwrap();
    assert!(
        e.project.automation_clips.is_empty(),
        "old clip targets new send"
    );
    let _ = std::fs::remove_dir_all(root);
}
fn rig() -> Engine {
    let root = std::env::temp_dir().join(format!(
        "bb_send_cleanup_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root);
    for t in ["keys", "keep"] {
        e.call("add_track", &json!({"name":t,"preset":"piano"}))
            .unwrap();
    }
    for b in ["verb", "other"] {
        e.call("add_bus", &json!({"name":b})).unwrap();
    }
    for (t, b, n) in [
        ("keys", "verb", "gone"),
        ("keep", "verb", "keep_owner"),
        ("keys", "other", "keep_bus"),
    ] {
        e.call("set_send", &json!({"track":t,"bus":b,"db":-12.}))
            .unwrap();
        e.call(
            "add_automation",
            &json!({"track":t,"param":format!("send.{b}"),"points":[[0.,-30.],[4.,-12.]]}),
        )
        .unwrap();
        e.call(
            "create_automation_clip",
            &json!({"name":n,"track":t,"param":format!("send.{b}"),"points":[[0.,-30.],[4.,-12.]]}),
        )
        .unwrap();
    }
    e
}
#[test]
fn remove_send_preserves_other_owner_and_bus_undo_reopen() {
    let mut e = rig();
    let before = e.project.clone();
    e.call(
        "set_send",
        &json!({"track":"KEYS","bus":"VERB","remove":true}),
    )
    .unwrap();
    assert_eq!(e.project.automation.len(), 2);
    assert_eq!(e.project.automation_clips.len(), 2);
    assert_eq!(e.project.automation_clips[0].name, "keep_owner");
    assert_eq!(e.project.automation_clips[1].name, "keep_bus");
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path":"sends.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path":"sends.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn removed_bus_clears_send_clips_from_all_owners() {
    let mut e = rig();
    e.call("remove_bus", &json!({"name":"verb"})).unwrap();
    assert_eq!(e.project.automation.len(), 1);
    assert_eq!(e.project.automation_clips.len(), 1);
    assert_eq!(e.project.automation_clips[0].name, "keep_bus");
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn routing_to_bus_clears_redundant_send_controls() {
    let mut e = rig();
    e.call("route_track", &json!({"track":"keys","bus":"verb"}))
        .unwrap();
    assert_eq!(e.project.automation_clips.len(), 2);
    assert!(e.project.automation_clips.iter().all(|c| c.name != "gone"));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn owner_id_clips_and_regular_send_updates() {
    let mut e = rig();
    let id = e.project.tracks[0].id.clone();
    e.project.automation_clips[0].target = id.clone();
    e.call("set_send", &json!({"track":id,"bus":"verb","db":-6.}))
        .unwrap();
    assert_eq!(e.project.automation_clips.len(), 3);
    e.call(
        "set_send",
        &json!({"track":"keys","bus":"verb","remove":true}),
    )
    .unwrap();
    assert_eq!(e.project.automation_clips.len(), 2);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn invalid_removal_is_atomic() {
    let mut e = rig();
    let before = e.project.clone();
    assert!(e
        .call(
            "set_send",
            &json!({"track":"missing","bus":"verb","remove":true})
        )
        .is_err());
    assert_eq!(e.project, before);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
