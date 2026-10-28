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
fn moving_effect_keeps_all_controls_on_same_effect_and_undo_redo() {
    let mut e = setup("pad");
    let before = e.project.clone();
    e.call(
        "move_effect",
        &json!({"track":"pad","effect":0,"position":"last"}),
    )
    .unwrap();
    check(&e, &[2, 0, 1]);
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path":"mapped.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path":"mapped.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    cleanup(&e);
}
#[test]
fn moves_first_numeric_before_after_and_noop() {
    for (args, positions) in [
        (json!({"effect":2,"position":"first"}), [1, 2, 0]),
        (json!({"effect":0,"position":1}), [1, 0, 2]),
        (json!({"effect":2,"before":0}), [1, 2, 0]),
        (json!({"effect":0,"after":1}), [2, 0, 1]),
        (json!({"effect":1,"position":1}), [0, 1, 2]),
    ] {
        let mut e = setup("pad");
        let mut args = args;
        args["track"] = json!("pad");
        e.call("move_effect", &args).unwrap();
        check(&e, &positions);
        cleanup(&e);
    }
}
#[test]
fn inserts_keep_all_original_targets() {
    for (extra, positions) in [
        (json!({"position":"first"}), [1, 2, 3]),
        (json!({"position":"last"}), [0, 1, 2]),
        (json!({"position":1}), [0, 2, 3]),
        (json!({"before":1}), [0, 2, 3]),
        (json!({"after":1}), [0, 1, 3]),
    ] {
        let mut e = setup("pad");
        let mut a = json!({"track":"pad","type":"delay"});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        e.call("place_fx", &a).unwrap();
        check(&e, &positions);
        cleanup(&e);
    }
}
#[test]
fn explicit_reorder_preserves_clips_and_macros_too() {
    let mut e = setup("pad");
    e.call("reorder_effects", &json!({"track":"pad","order":[2,0,1]}))
        .unwrap();
    check(&e, &[1, 2, 0]);
    cleanup(&e);
}
#[test]
fn bus_master_case_and_stable_owner_id() {
    for owner in ["bus", "master", "pad"] {
        let mut e = setup(owner);
        let key = if owner == "pad" {
            e.project.tracks[0].id.clone()
        } else {
            owner.to_uppercase()
        };
        if owner == "pad" {
            for c in &mut e.project.automation_clips {
                c.target = key.clone();
            }
        }
        e.call(
            "move_effect",
            &json!({"track":key,"effect":0,"position":"last"}),
        )
        .unwrap();
        check(&e, &[2, 0, 1]);
        cleanup(&e);
    }
}
#[test]
fn invalid_edits_leave_chain_and_controls_unchanged() {
    for (tool, args) in [
        (
            "move_effect",
            json!({"track":"pad","effect":0,"before":"missing"}),
        ),
        (
            "move_effect",
            json!({"track":"pad","effect":0,"position":"bad"}),
        ),
        (
            "place_fx",
            json!({"track":"pad","type":"delay","before":"missing"}),
        ),
        ("reorder_effects", json!({"track":"pad","order":[0,0,1]})),
    ] {
        let mut e = setup("pad");
        let before = e.project.clone();
        assert!(e.call(tool, &args).is_err());
        assert_eq!(e.project, before);
        cleanup(&e);
    }
}
#[test]
fn unrelated_owner_and_nonpositional_paths_unchanged() {
    let mut e = setup("pad");
    e.call("add_bus", &json!({"name":"untouched"})).unwrap();
    e.call("add_effect", &json!({"track":"untouched","type":"delay"}))
        .unwrap();
    e.call(
        "add_automation",
        &json!({"track":"untouched","param":"fx.0.mix","points":[[0.,0.1],[4.,0.4]]}),
    )
    .unwrap();
    let unrelated = e.project.automation.last().unwrap().clone();
    let clip = e.project.automation_clips[0].clone();
    e.project
        .automation_clips
        .push(beatbox::project::AutomationClip {
            param: "fx.reverb1.mix".into(),
            ..clip
        });
    e.call(
        "move_effect",
        &json!({"track":"pad","effect":0,"position":"last"}),
    )
    .unwrap();
    assert_eq!(e.project.automation.last().unwrap(), &unrelated);
    assert_eq!(
        e.project.automation_clips.last().unwrap().param,
        "fx.reverb1.mix"
    );
    cleanup(&e);
}
#[test]
fn controls_apply_to_original_effect_after_move_and_restamp() {
    let mut e = setup("pad");
    let id = e.project.tracks[0].effects[0].id().to_string();
    e.call(
        "move_effect",
        &json!({"track":"pad","effect":id,"position":"last"}),
    )
    .unwrap();
    e.call("set_macro", &json!({"name":"knob0","value":0.9}))
        .unwrap();
    let moved = serde_json::to_value(&e.project.tracks[0].effects[2]).unwrap();
    let other = serde_json::to_value(&e.project.tracks[0].effects[0]).unwrap();
    assert!((moved["mix"].as_f64().unwrap() - 0.74).abs() < 1e-5);
    assert!((other["mix"].as_f64().unwrap() - 0.5).abs() < 1e-5);
    assert_eq!(e.project.tracks[0].effects[2].id(), id);
    e.call(
        "place_automation_clip",
        &json!({"name":"clip0","at_beat":0}),
    )
    .unwrap();
    assert_eq!(e.project.automation_clips[0].param, "fx.2.mix");
    assert_eq!(e.project.automation[0].param, "fx.2.mix");
    cleanup(&e);
}
