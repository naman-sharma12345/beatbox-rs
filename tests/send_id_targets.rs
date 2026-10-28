use beatbox::Engine;
use serde_json::json;
#[test]
fn send_id_survives_level_update() {
    let root =
        std::env::temp_dir().join(format!("send_id_audit_{}", beatbox::creative::fresh_seed()));
    let mut e = Engine::new(root.clone());
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call("add_bus", &json!({"name":"verb"})).unwrap();
    e.call("set_send", &json!({"track":"keys","bus":"verb","db":-12.}))
        .unwrap();
    let id = e.project.tracks[0].sends[0].id.clone();
    e.call("create_automation_clip",&json!({"name":"wet","track":"keys","param":format!("send.{id}"),"points":[[0.,-30.],[4.,-12.]]})).unwrap();
    e.call("set_send", &json!({"track":"keys","bus":"verb","db":-6.}))
        .unwrap();
    let new_id = e.project.tracks[0].sends[0].id.clone();
    let _ = std::fs::remove_dir_all(root);
    assert_eq!(id, new_id, "ordinary send update changed stable identity");
}
#[test]
fn send_id_target_cleared_on_removal() {
    let root = std::env::temp_dir().join(format!(
        "send_id_remove_audit_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root.clone());
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call("add_bus", &json!({"name":"verb"})).unwrap();
    e.call("set_send", &json!({"track":"keys","bus":"verb","db":-12.}))
        .unwrap();
    let id = e.project.tracks[0].sends[0].id.clone();
    e.call("create_automation_clip",&json!({"name":"wet","track":"keys","param":format!("send.{id}"),"points":[[0.,-30.],[4.,-12.]]})).unwrap();
    e.call(
        "set_send",
        &json!({"track":"keys","bus":"verb","remove":true}),
    )
    .unwrap();
    let left = e.project.automation_clips.len();
    let _ = std::fs::remove_dir_all(root);
    assert_eq!(left, 0, "send-ID automation clip left dangling");
}

fn rig() -> Engine {
    let root =
        std::env::temp_dir().join(format!("send_id_rig_{}", beatbox::creative::fresh_seed()));
    let mut e = Engine::new(root);
    for t in ["keys", "keep"] {
        e.call("add_track", &json!({"name":t,"preset":"piano"}))
            .unwrap();
    }
    for b in ["verb", "other"] {
        e.call("add_bus", &json!({"name":b})).unwrap();
    }
    for (t, b) in [("keys", "verb"), ("keys", "other"), ("keep", "verb")] {
        e.call("set_send", &json!({"track":t,"bus":b,"db":-12.}))
            .unwrap();
        let ti = e.project.track_index(t).unwrap();
        let id = e.project.tracks[ti]
            .sends
            .iter()
            .find(|s| s.bus == b)
            .unwrap()
            .id
            .clone();
        for param in [format!("send.{b}"), format!("send.{id}")] {
            e.call(
                "add_automation",
                &json!({"track":t,"param":param,"points":[[0.,-30.],[4.,-12.]]}),
            )
            .unwrap();
            e.call("create_automation_clip",&json!({"name":format!("{t}_{param}"),"track":t,"param":param,"points":[[0.,-30.],[4.,-12.]]})).unwrap();
        }
    }
    e
}
#[test]
fn id_alias_remove_preserves_others_and_undo_reopen() {
    let mut e = rig();
    let before = e.project.clone();
    let owner = e.project.tracks[0].id.clone();
    for c in &mut e.project.automation_clips {
        if c.target == "keys" {
            c.target = owner.to_uppercase();
        }
    }
    e.call(
        "set_send",
        &json!({"track":"KEYS","bus":"VERB","remove":true}),
    )
    .unwrap();
    assert_eq!(e.project.automation.len(), 4);
    assert_eq!(e.project.automation_clips.len(), 4);
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project.tracks, before.tracks);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path":"send-ids.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path":"send-ids.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn bus_remove_clears_send_id_aliases() {
    let mut e = rig();
    e.call("remove_bus", &json!({"name":"verb"})).unwrap();
    assert_eq!(e.project.automation.len(), 2);
    assert_eq!(e.project.automation_clips.len(), 2);
    assert!(e
        .project
        .automation_clips
        .iter()
        .all(|c| c.name.starts_with("keys_send.other")
            || c.param == format!("send.{}", e.project.tracks[0].sends[0].id)));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn route_remove_clears_send_id_aliases() {
    let mut e = rig();
    e.call("route_track", &json!({"track":"keys","bus":"verb"}))
        .unwrap();
    assert_eq!(e.project.automation.len(), 4);
    assert_eq!(e.project.automation_clips.len(), 4);
    assert!(e
        .project
        .automation_clips
        .iter()
        .all(|c| !c.name.starts_with("keys_send.verb")));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn invalid_send_update_preserves_ids_and_controls() {
    let mut e = rig();
    let before = e.project.clone();
    assert!(e
        .call(
            "set_send",
            &json!({"track":["keys","missing"],"bus":"verb","remove":true})
        )
        .is_err());
    assert_eq!(e.project, before);
    assert!(e
        .call(
            "route_track",
            &json!({"track":["keys","missing"],"bus":"verb"})
        )
        .is_err());
    assert_eq!(e.project, before);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
