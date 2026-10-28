use beatbox::Engine;
use serde_json::json;
#[test]
fn removed_track_controls_do_not_revive_on_recreated_name() {
    let root = std::env::temp_dir().join(format!(
        "bb_removed_owner_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root.clone());
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    e.call(
        "create_automation_clip",
        &json!({"name":"fade","track":"keys","param":"volume","points":[[0.,-24.],[4.,-12.]]}),
    )
    .unwrap();
    e.call(
        "create_macro",
        &json!({"name":"old","targets":[{"track":"keys","param":"volume","min":-24.,"max":-12.}]}),
    )
    .unwrap();
    e.call("remove_track", &json!({"track":"keys"})).unwrap();
    e.call("add_track", &json!({"name":"keys","preset":"piano"}))
        .unwrap();
    assert!(
        e.project.automation_clips.is_empty(),
        "deleted track clip still addresses replacement"
    );
    assert!(
        e.project.macros[0].targets.is_empty(),
        "deleted track macro still addresses replacement"
    );
    let _ = std::fs::remove_dir_all(root);
}
fn rig(bus: bool) -> Engine {
    let root = std::env::temp_dir().join(format!(
        "bb_owner_controls_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root);
    e.call("add_track", &json!({"name":"keep","preset":"piano"}))
        .unwrap();
    if bus {
        e.call("add_bus", &json!({"name":"gone"})).unwrap();
    } else {
        e.call("add_track", &json!({"name":"gone","preset":"piano"}))
            .unwrap();
    }
    for owner in ["gone", "keep"] {
        e.call(
            "add_automation",
            &json!({"track":owner,"param":"volume","points":[[0.,-18.],[4.,-6.]]}),
        )
        .unwrap();
        e.call(
            "create_automation_clip",
            &json!({"name":owner,"track":owner,"param":"volume","points":[[0.,-18.],[4.,-6.]]}),
        )
        .unwrap();
    }
    e.call("create_macro",&json!({"name":"mixed","targets":[{"track":"gone","param":"volume","min":-18.,"max":-6.},{"track":"keep","param":"volume","min":-12.,"max":-6.}]})).unwrap();
    e
}
#[test]
fn track_and_bus_cleanup_undo_redo_reopen() {
    for bus in [false, true] {
        let mut e = rig(bus);
        let before = e.project.clone();
        let (tool, a) = if bus {
            ("remove_bus", json!({"name":"GONE"}))
        } else {
            ("remove_track", json!({"track":"GONE"}))
        };
        e.call(tool, &a).unwrap();
        assert_eq!(e.project.automation.len(), 1);
        assert_eq!(e.project.automation[0].target, "keep");
        assert_eq!(e.project.automation_clips.len(), 1);
        assert_eq!(e.project.automation_clips[0].target, "keep");
        assert_eq!(e.project.macros[0].targets.len(), 1);
        assert_eq!(e.project.macros[0].targets[0].track, "keep");
        let after = e.project.clone();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(e.project, after);
        e.call("save_project", &json!({"path":"owners.beatbox.json"}))
            .unwrap();
        e.call("new_project", &json!({})).unwrap();
        e.call("load_project", &json!({"path":"owners.beatbox.json"}))
            .unwrap();
        assert_eq!(e.project, after);
        let _ = std::fs::remove_dir_all(&e.workdir);
    }
}
#[test]
fn stable_owner_aliases_and_renamed_owner_are_cleaned() {
    for bus in [false, true] {
        let mut e = rig(bus);
        if !bus {
            e.call("rename_track", &json!({"track":"gone","name":"renamed"}))
                .unwrap();
        }
        let id = if bus {
            e.project.buses[0].id.clone()
        } else {
            e.project.tracks[1].id.clone()
        };
        e.project.automation[0].target = id.clone();
        e.project.automation_clips[0].target = id.clone();
        e.project.macros[0].targets[0].track = id.clone();
        let (tool, a) = if bus {
            ("remove_bus", json!({"name":id}))
        } else {
            ("remove_track", json!({"track":id}))
        };
        e.call(tool, &a).unwrap();
        assert_eq!(e.project.automation.len(), 1);
        assert_eq!(e.project.automation_clips.len(), 1);
        assert_eq!(e.project.macros[0].targets.len(), 1);
        let _ = std::fs::remove_dir_all(&e.workdir);
    }
}
#[test]
fn recreated_bus_does_not_inherit_deleted_controls() {
    let mut e = rig(true);
    e.call("remove_bus", &json!({"name":"gone"})).unwrap();
    e.call("add_bus", &json!({"name":"gone"})).unwrap();
    let before = e.project.buses[0].volume_db;
    e.call("set_macro", &json!({"name":"mixed","value":1.}))
        .unwrap();
    assert_eq!(e.project.buses[0].volume_db, before);
    assert!(e
        .project
        .automation_clips
        .iter()
        .all(|c| c.target != "gone"));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn invalid_channel_removal_is_atomic() {
    for bus in [false, true] {
        let mut e = rig(bus);
        let before = e.project.clone();
        let (tool, a) = if bus {
            ("remove_bus", json!({"name":"missing"}))
        } else {
            ("remove_track", json!({"track":"missing"}))
        };
        assert!(e.call(tool, &a).is_err());
        assert_eq!(e.project, before);
        let _ = std::fs::remove_dir_all(&e.workdir);
    }
}
