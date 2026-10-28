use beatbox::Engine;
use serde_json::json;
fn setup(owner: &str) -> Engine {
    let root = std::env::temp_dir().join(format!("bb_fx_move_{}", beatbox::creative::fresh_seed()));
    let mut e = Engine::new(root);
    e.project.master_effects.clear();
    e.call("add_track", &json!({"name":"pad","preset":"warm_pad"}))
        .unwrap();
    if owner == "bus" {
        e.call("add_bus", &json!({"name":"bus"})).unwrap();
    }
    for kind in ["reverb", "delay", "reverb"] {
        e.call("add_effect", &json!({"track":owner,"type":kind}))
            .unwrap();
    }
    for i in 0..3 {
        let param = format!("fx.{i}.mix");
        e.call(
            "add_automation",
            &json!({"track":owner,"param":param,"points":[[0.,0.2],[4.,0.8]]}),
        )
        .unwrap();
        e.call("create_automation_clip", &json!({"name":format!("clip{i}"),"track":owner,"param":param,"points":[[0.,0.2],[4.,0.8]]})).unwrap();
        e.call("create_macro", &json!({"name":format!("knob{i}"),"targets":[{"track":owner,"param":param,"min":0.2,"max":0.8}]})).unwrap();
    }
    e.call(
        "add_automation",
        &json!({"track":"pad","param":"volume","points":[[0.,-6.],[4.,0.]]}),
    )
    .unwrap();
    e
}
#[allow(dead_code)]
fn check(e: &Engine, positions: &[usize]) {
    for (old, new) in positions.iter().enumerate() {
        let param = format!("fx.{new}.mix");
        assert_eq!(e.project.automation[old].param, param);
        assert_eq!(e.project.automation_clips[old].param, param);
        assert_eq!(e.project.macros[old].targets[0].param, param);
    }
    assert_eq!(e.project.automation[3].param, "volume");
}
fn cleanup(e: &Engine) {
    let _ = std::fs::remove_dir_all(&e.workdir);
}
#[test]
fn removal_drops_deleted_controls_and_shifts_survivors() {
    let mut e = setup("pad");
    e.call("remove_effect", &json!({"track":"pad","index":0}))
        .unwrap();
    assert_eq!(e.project.automation[0].param, "fx.0.mix");
    assert_eq!(
        e.project.automation_clips.len(),
        2,
        "deleted effect's clip must not retarget replacement"
    );
    assert_eq!(e.project.automation_clips[0].param, "fx.0.mix");
    assert!(
        e.project.macros[0].targets.is_empty(),
        "deleted effect macro target must be dropped"
    );
    assert_eq!(e.project.macros[1].targets[0].param, "fx.0.mix");
    cleanup(&e);
}
#[test]
fn every_removal_position_undo_redo_and_reopen() {
    for removed in 0..3 {
        let mut e = setup("pad");
        let before = e.project.clone();
        e.call("remove_effect", &json!({"track":"pad","index":removed}))
            .unwrap();
        for (new, old) in (0..3).filter(|i| *i != removed).enumerate() {
            assert_eq!(e.project.automation_clips[new].name, format!("clip{old}"));
            assert_eq!(
                e.project.automation_clips[new].param,
                format!("fx.{new}.mix")
            );
            assert_eq!(
                e.project.macros[old].targets[0].param,
                format!("fx.{new}.mix")
            );
        }
        assert!(e.project.macros[removed].targets.is_empty());
        let after = e.project.clone();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(e.project, after);
        e.call("save_project", &json!({"path":"removed.beatbox.json"}))
            .unwrap();
        e.call("new_project", &json!({})).unwrap();
        e.call("load_project", &json!({"path":"removed.beatbox.json"}))
            .unwrap();
        assert_eq!(e.project, after);
        cleanup(&e);
    }
}
#[test]
fn stable_effect_id_bus_master_and_case_removal() {
    for owner in ["pad", "bus", "master"] {
        let mut e = setup(owner);
        let result = e.call("get_effects", &json!({"track":owner})).unwrap();
        let id = result[0]["id"].clone();
        assert!(id.is_string(), "{result}");
        e.call(
            "remove_effect",
            &json!({"track":owner.to_uppercase(),"index":id}),
        )
        .unwrap();
        assert_eq!(e.project.automation_clips.len(), 2);
        assert_eq!(e.project.automation_clips[0].param, "fx.0.mix");
        assert!(e.project.macros[0].targets.is_empty());
        cleanup(&e);
    }
}
#[test]
fn replacing_sound_effects_clears_old_effect_controls_only() {
    let mut e = setup("pad");
    e.call(
        "create_automation_clip",
        &json!({"name":"volume","track":"pad","param":"volume","points":[[0.,-12.],[4.,-6.]]}),
    )
    .unwrap();
    e.call(
        "create_macro",
        &json!({"name":"volume","targets":[{"track":"pad","param":"volume","min":-12.,"max":-6.}]}),
    )
    .unwrap();
    e.call(
        "design_sound",
        &json!({"track":"pad","description":"warm dusty keys","replace_effects":true}),
    )
    .unwrap();
    assert!(e.project.automation.iter().all(|l| l.param == "volume"));
    assert_eq!(e.project.automation_clips.len(), 1);
    assert_eq!(e.project.automation_clips[0].param, "volume");
    for m in &e.project.macros[..3] {
        assert!(m.targets.is_empty());
    }
    assert_eq!(e.project.macros[3].targets[0].param, "volume");
    cleanup(&e);
}
#[test]
fn unrelated_and_stable_owner_clip_refs() {
    let mut e = setup("pad");
    let id = e.project.tracks[0].id.clone();
    e.project.automation_clips[0].target = id.clone();
    e.project.automation_clips[1].target = id.clone();
    let unrelated = beatbox::project::MacroTarget {
        track: "other".into(),
        param: "fx.0.mix".into(),
        min: 0.,
        max: 1.,
        curve: 1.,
    };
    e.project.macros[0].targets.push(unrelated.clone());
    e.call("remove_effect", &json!({"track":id,"index":0}))
        .unwrap();
    assert_eq!(e.project.automation_clips.len(), 2);
    assert_eq!(e.project.automation_clips[0].param, "fx.0.mix");
    assert_eq!(e.project.macros[0].targets, vec![unrelated]);
    cleanup(&e);
}
#[test]
fn invalid_removal_changes_nothing() {
    let mut e = setup("pad");
    let before = e.project.clone();
    assert!(e
        .call("remove_effect", &json!({"track":"pad","index":99}))
        .is_err());
    assert_eq!(e.project, before);
    cleanup(&e);
}
