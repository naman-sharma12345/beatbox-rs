use beatbox::Engine;
use serde_json::json;
#[test]
fn deleting_source_does_not_bind_sidechain_to_new_track_with_same_name() {
    let root = std::env::temp_dir().join(format!(
        "source_ref_audit_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root.clone());
    for t in ["kick", "bass"] {
        e.call("add_track", &json!({"name":t,"preset":"piano"}))
            .unwrap();
    }
    e.call(
        "add_effect",
        &json!({"track":"bass","type":"sidechain","params":{"source":"kick","amount":0.5}}),
    )
    .unwrap();
    e.call("remove_track", &json!({"track":"kick"})).unwrap();
    e.call("add_track", &json!({"name":"kick","preset":"piano"}))
        .unwrap();
    let effects = serde_json::to_value(&e.project.tracks[0].effects).unwrap();
    let _ = std::fs::remove_dir_all(root);
    assert!(
        !effects
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["source"] == "kick"
                && f.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)),
        "old sidechain source now points at replacement track: {effects}"
    );
}

fn rig() -> Engine {
    let root = std::env::temp_dir().join(format!(
        "source_cleanup_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root);
    for t in ["kick", "keys", "keep"] {
        e.call("add_track", &json!({"name":t,"preset":"piano"}))
            .unwrap();
    }
    e.call("add_bus", &json!({"name":"verb"})).unwrap();
    e.project.master_effects.clear();
    e.project.buses[0].effects.clear();
    for owner in ["keys", "verb", "master"] {
        for (kind, params) in [
            ("gain", json!({"db":-3.})),
            ("sidechain", json!({"source":"kick","amount":0.5})),
            ("dynamic_eq", json!({"source":"kick"})),
            ("gain", json!({"db":-6.})),
            ("sidechain", json!({"source":"keep","amount":0.25})),
        ] {
            e.call(
                "add_effect",
                &json!({"track":owner,"type":kind,"params":params}),
            )
            .unwrap();
        }
        for (index, param) in [(1, "amount"), (2, "range_db"), (3, "db"), (4, "amount")] {
            e.call("add_automation",&json!({"track":owner,"param":format!("fx.{index}.{param}"),"points":[[0.,0.],[4.,0.]]})).unwrap();
            e.call("create_automation_clip",&json!({"name":format!("{owner}{index}"),"track":owner,"param":format!("fx.{index}.{param}"),"points":[[0.,0.],[4.,0.]]})).unwrap();
        }
    }
    e
}
#[test]
fn source_cleanup_reindexes_survivors_all_owners_and_undo_reopen() {
    let mut e = rig();
    let before = e.project.clone();
    e.call("remove_track", &json!({"track":"KICK"})).unwrap();
    for chain in [
        &e.project.tracks[0].effects,
        &e.project.buses[0].effects,
        &e.project.master_effects,
    ] {
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].type_name(), "gain");
        assert_eq!(chain[1].type_name(), "gain");
        assert_eq!(chain[2].type_name(), "sidechain");
    }
    assert_eq!(e.project.automation.len(), 6);
    assert_eq!(e.project.automation_clips.len(), 6);
    for lane in &e.project.automation {
        assert!(lane.param == "fx.1.db" || lane.param == "fx.2.amount");
    }
    for clip in &e.project.automation_clips {
        assert!(clip.param == "fx.1.db" || clip.param == "fx.2.amount");
    }
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path":"sources.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path":"sources.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn source_id_alias_saved_chains_macro_controls_and_unrelated_effects() {
    let mut e = rig();
    let id = e.project.tracks[0].id.clone();
    let owner = e.project.tracks[1].id.clone();
    if let beatbox::fx::Effect::Sidechain(ref mut sc) = e.project.tracks[1].effects[1] {
        sc.source = id.to_uppercase();
    }
    e.project
        .fx_chains
        .insert("saved".into(), e.project.tracks[1].effects.clone());
    e.call("create_macro",&json!({"name":"keep_mix","targets":[{"track":"keys","param":"fx.1.amount","min":0.,"max":1.},{"track":"keys","param":"fx.3.db","min":-12.,"max":0.}]})).unwrap();
    e.project.macros[0].targets[1].track = owner.clone();
    e.project.automation[0].target = owner;
    e.call("remove_track", &json!({"track":id})).unwrap();
    assert_eq!(e.project.fx_chains["saved"].len(), 3);
    assert_eq!(e.project.macros[0].targets.len(), 1);
    assert_eq!(e.project.macros[0].targets[0].param, "fx.1.db");
    assert!(e
        .project
        .automation
        .iter()
        .all(|l| l.param != "fx.1.amount" && l.param != "fx.2.range_db"));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn invalid_source_removal_is_atomic() {
    let mut e = rig();
    let before = e.project.clone();
    assert!(e.call("remove_track", &json!({"track":"missing"})).is_err());
    assert_eq!(e.project, before);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn unkeyed_dynamic_eq_is_preserved() {
    let mut e = rig();
    e.call(
        "add_effect",
        &json!({"track":"keys","type":"dynamic_eq","params":{"source":""}}),
    )
    .unwrap();
    let fx = e.project.tracks[1].effects.last().unwrap().clone();
    e.call("remove_track", &json!({"track":"kick"})).unwrap();
    assert_eq!(e.project.tracks[0].effects.last(), Some(&fx));
    let _ = std::fs::remove_dir_all(&e.workdir);
}
