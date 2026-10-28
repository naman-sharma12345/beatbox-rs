//! Routing-only regressions. No rendering, audio devices or GUI startup.
use beatbox::automation::AutomationLane;
use beatbox::project::{AutomationClip, Macro, MacroTarget};
use beatbox::Engine;
use serde_json::{json, Value};

fn control(e: &mut Engine, owner: &str, param: &str, name: &str) {
    e.project.automation.push(AutomationLane::new(owner, param));
    e.project.automation_clips.push(AutomationClip {
        id: String::new(),
        name: name.into(),
        target: owner.into(),
        param: param.into(),
        length_beats: 4.0,
        points: vec![],
        placements: vec![0.0],
    });
    e.project.macros[0].targets.push(MacroTarget {
        track: owner.into(),
        param: param.into(),
        min: -18.0,
        max: -6.0,
        curve: 1.0,
    });
}

fn rig() -> Engine {
    let mut e = Engine::new(std::env::temp_dir().join(format!(
        "bb_bus_preview_{}",
        beatbox::creative::fresh_seed()
    )));
    for name in ["lead", "kick", "snare", "keep"] {
        e.call("add_track", &json!({"name": name, "preset": "kick"}))
            .unwrap();
    }
    for name in ["mix", "drums", "bass", "other", "keepbus"] {
        e.call("add_bus", &json!({"name": name})).unwrap();
    }
    for (bus, destination) in [("drums", "mix"), ("bass", "mix"), ("keepbus", "other")] {
        e.call(
            "route_bus",
            &json!({"bus": bus, "destination": destination}),
        )
        .unwrap();
    }
    e.call("route_track", &json!({"track": "lead", "bus": "mix"}))
        .unwrap();
    e.call("route_track", &json!({"track": "kick", "bus": "drums"}))
        .unwrap();
    for (track, bus) in [
        ("kick", "mix"),
        ("kick", "other"),
        ("snare", "mix"),
        ("keep", "other"),
    ] {
        e.call(
            "set_send",
            &json!({"track": track, "bus": bus, "db": -12.0}),
        )
        .unwrap();
    }
    let id = e.project.buses[0].id.clone();
    let kick = e.project.tracks[1].id.to_uppercase();
    let snare_send = e.project.tracks[2].sends[0].id.to_uppercase();
    // Model aliases from a loaded file, including stable IDs in stored routes.
    e.project.tracks[0].output = Some(id.to_uppercase());
    e.project.buses[1].output = Some("MiX".into());
    e.project.buses[2].output = Some(id.clone());
    e.project.tracks[1].sends[0].bus = "MiX".into();
    e.project.tracks[2].sends[0].bus = id.to_uppercase();
    e.project.macros.push(Macro {
        name: "mixed".into(),
        value: 0.5,
        targets: vec![],
    });
    control(&mut e, &id.to_uppercase(), "volume", "bus control");
    control(
        &mut e,
        &kick,
        &format!("sends.{}", id.to_uppercase()),
        "bus ID send",
    );
    control(
        &mut e,
        "SnArE",
        &format!("send.{snare_send}"),
        "send ID control",
    );
    control(&mut e, "lead", "volume", "keep volume");
    control(&mut e, "kick", "send.other", "keep send");
    // A send ID belongs to its track, not every owner with a matching param.
    control(
        &mut e,
        "keep",
        &format!("send.{snare_send}"),
        "keep unrelated owner",
    );
    beatbox::ids::ensure_ids(&mut e.project, None);
    e
}

fn preview(e: &mut Engine) -> Value {
    e.call("preview_remove_bus", &json!({"name": "mix"}))
        .unwrap()
}

#[test]
fn preview_is_nonmutating_and_aliases_have_identical_exact_impact() {
    let mut e = rig();
    e.call("set_tempo", &json!({"bpm": 111})).unwrap();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(
        e.undo_depth().1,
        1,
        "the preview must preserve pending redo"
    );
    std::fs::create_dir_all(&e.workdir).unwrap();
    let marker = e.workdir.join("unchanged.txt");
    std::fs::write(&marker, "preview must not write files").unwrap();
    let before = e.project.clone();
    let revision = e.revision;
    let depth = e.undo_depth();
    let history = e.history();
    let snapshots = e.snapshots.len();
    let files = std::fs::read_dir(&e.workdir)
        .map(|x| x.count())
        .unwrap_or(0);
    let p = preview(&mut e);
    for alias in [
        "MiX".to_string(),
        before.buses[0].id.clone(),
        before.buses[0].id.to_uppercase(),
    ] {
        assert_eq!(
            e.call("preview_remove_bus", &json!({"name": alias}))
                .unwrap(),
            p
        );
    }
    let impact = &p["impact"];
    assert_eq!(
        impact["track_outputs"],
        json!([{"id": before.tracks[0].id, "name": "lead"}])
    );
    assert_eq!(
        impact["bus_outputs"],
        json!([
            {"id": before.buses[1].id, "name": "drums"},
            {"id": before.buses[2].id, "name": "bass"}
        ])
    );
    assert_eq!(
        impact["sends"],
        json!([
            {"id": before.tracks[1].sends[0].id, "track_id": before.tracks[1].id, "track": "kick"},
            {"id": before.tracks[2].sends[0].id, "track_id": before.tracks[2].id, "track": "snare"}
        ])
    );
    for key in ["automation", "automation_clips", "macro_targets"] {
        assert_eq!(impact[key].as_array().unwrap().len(), 3, "{key}: {impact}");
    }
    assert_eq!(
        impact["automation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].clone())
            .collect::<Vec<_>>(),
        before.automation[..3]
            .iter()
            .map(|l| json!(l.id))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        impact["automation_clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].clone())
            .collect::<Vec<_>>(),
        before.automation_clips[..3]
            .iter()
            .map(|c| json!(c.id))
            .collect::<Vec<_>>()
    );
    assert_eq!(e.project, before);
    assert_eq!(e.revision, revision);
    assert_eq!(e.undo_depth(), depth);
    assert_eq!(e.history(), history);
    assert_eq!(e.snapshots.len(), snapshots);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "preview must not write files"
    );
    assert_eq!(
        std::fs::read_dir(&e.workdir)
            .map(|x| x.count())
            .unwrap_or(0),
        files
    );
    // Normal read-call logging is permitted; project/history/files are not.
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn preview_commit_agree_and_deletion_is_one_undo_step_with_reopen() {
    let mut e = rig();
    let before = e.project.clone();
    let depth = e.undo_depth().0;
    let p = preview(&mut e);
    let result = e
        .call(
            "remove_bus",
            &json!({"name": p["bus_id"], "expected_revision": p["revision"]}),
        )
        .unwrap();
    assert_eq!(result["impact"], p["impact"]);
    assert_eq!(result["removed"], "mix");
    assert_eq!(result["rerouted_to_master"], json!(["lead"]));
    assert_eq!(result["rerouted_buses_to_master"], json!(["drums", "bass"]));
    assert_eq!(e.undo_depth().0, depth + 1);
    assert_eq!(e.revision, p["revision"].as_u64().unwrap() + 1);
    assert_eq!(e.project.tracks[0].output, None);
    assert_eq!(e.project.buses[0].output, None);
    assert_eq!(e.project.buses[1].output, None);
    assert_eq!(e.project.tracks[1].sends, before.tracks[1].sends[1..]);
    assert!(e.project.tracks[2].sends.is_empty());
    assert_eq!(e.project.tracks[3], before.tracks[3]);
    assert_eq!(e.project.buses[2..], before.buses[3..]);
    assert_eq!(e.project.automation, before.automation[3..]);
    assert_eq!(e.project.automation_clips, before.automation_clips[3..]);
    assert_eq!(e.project.macros[0].targets, before.macros[0].targets[3..]);
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path": "routing.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path": "routing.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    e.call("add_bus", &json!({"name": "mix"})).unwrap();
    assert_ne!(e.project.buses.last().unwrap().id, before.buses[0].id);
    assert_eq!(e.project.buses[0].output, None);
    assert_eq!(e.project.buses[1].output, None);
    assert_eq!(e.project.tracks[0].output, None);
    assert_eq!(e.project.automation, after.automation);
    assert_eq!(e.project.macros, after.macros);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn stale_preview_rejects_without_project_or_history_changes_then_refreshes() {
    let mut e = rig();
    let p = preview(&mut e);
    e.call("route_track", &json!({"track": "keep", "bus": "mix"}))
        .unwrap();
    let before = e.project.clone();
    let depth = e.undo_depth();
    let history = e.history();
    let revision = e.revision;
    let err = e
        .call(
            "remove_bus",
            &json!({"name": p["bus_id"], "expected_revision": p["revision"]}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("StaleRevision"), "{err:#}");
    assert_eq!(e.project, before);
    assert_eq!(e.undo_depth(), depth);
    assert_eq!(e.history(), history);
    assert_eq!(e.revision, revision);
    let fresh = preview(&mut e);
    assert_eq!(
        fresh["impact"]["track_outputs"].as_array().unwrap().len(),
        2
    );
    let result = e
        .call(
            "remove_bus",
            &json!({"name": fresh["bus_id"], "expected_revision": fresh["revision"]}),
        )
        .unwrap();
    assert_eq!(result["impact"], fresh["impact"]);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn deleted_and_reused_name_cannot_accept_the_old_preview_or_id() {
    let mut e = rig();
    let p = preview(&mut e);
    e.call("remove_bus", &json!({"name": "mix"})).unwrap();
    e.call("add_bus", &json!({"name": "mix"})).unwrap();
    let before = e.project.clone();
    assert!(e
        .call(
            "remove_bus",
            &json!({"name": p["bus_id"], "expected_revision": p["revision"]})
        )
        .is_err());
    assert!(e
        .call("preview_remove_bus", &json!({"name": p["bus_id"]}))
        .is_err());
    assert_eq!(e.project, before);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn mixer_category_and_rack_removal_share_the_same_dependency_cleanup() {
    let mut direct = rig();
    let help = direct
        .call(
            "mixer",
            &json!({"action": "help", "params": {"action": "preview_remove_bus"}}),
        )
        .unwrap();
    assert_eq!(help["params_schema"]["required"], json!(["name"]));
    let description = direct
        .call("describe_tool", &json!({"name": "preview_remove_bus"}))
        .unwrap();
    assert_eq!(description["changes_project"], false);
    let mut category = Engine::new(direct.workdir.join("category"));
    let mut rack = Engine::new(direct.workdir.join("rack"));
    category.replace_project(direct.project.clone());
    rack.replace_project(direct.project.clone());
    let p = preview(&mut direct);
    let cp = category
        .call(
            "mixer",
            &json!({"action": "preview_remove_bus", "name": "mix"}),
        )
        .unwrap();
    assert_eq!(cp["impact"], p["impact"]);
    direct
        .call("remove_bus", &json!({"name": p["bus_id"]}))
        .unwrap();
    let result = category.call("mixer", &json!({"action": "remove_bus", "name": cp["bus_id"], "expected_revision": cp["revision"]})).unwrap();
    assert_eq!(result["impact"], p["impact"]);
    let depth = rack.undo_depth().0;
    rack.call("rack_apply", &json!({"expected_revision": rack.revision, "commands": [{"op": "remove_bus", "strip": p["bus_id"]}]})).unwrap();
    assert_eq!(rack.undo_depth().0, depth + 1);
    assert_eq!(category.project, direct.project);
    assert_eq!(rack.project, direct.project);
    let _ = std::fs::remove_dir_all(&direct.workdir);
}

#[test]
fn missing_owner_and_malformed_revision_do_not_change_the_project() {
    let mut e = rig();
    let before = e.project.clone();
    let revision = e.revision;
    let depth = e.undo_depth();
    assert!(e
        .call("preview_remove_bus", &json!({"name": "missing"}))
        .is_err());
    assert!(e.call("remove_bus", &json!({"name": "missing"})).is_err());
    for invalid in [json!(-1), json!("not a revision"), json!(1.5), Value::Null] {
        assert!(e
            .call(
                "remove_bus",
                &json!({"name": "mix", "expected_revision": invalid})
            )
            .is_err());
    }
    assert_eq!(e.project, before);
    assert_eq!(e.revision, revision);
    assert_eq!(e.undo_depth(), depth);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

fn send_id_collision_rig(uppercase: bool, incoming: bool) -> (Engine, String) {
    let mut e = rig();
    let id = e.project.tracks[3].sends[0].id.clone();
    let alias = if uppercase { id.to_uppercase() } else { id };
    // Accepted API: bus names are not reserved against send IDs.
    e.call("add_bus", &json!({"name": alias})).unwrap();
    if incoming {
        e.call(
            "set_send",
            &json!({"track": "keep", "bus": alias, "db": -9}),
        )
        .unwrap();
    }
    control(&mut e, "KeEp", &format!("send.{alias}"), "collision");
    beatbox::ids::ensure_ids(&mut e.project, None);
    (e, alias)
}

#[test]
fn bus_named_after_surviving_send_id_preserves_all_controls_and_undo() {
    for uppercase in [false, true] {
        let (mut e, alias) = send_id_collision_rig(uppercase, false);
        let before = e.project.clone();
        let depth = e.undo_depth().0;
        let p = e
            .call("preview_remove_bus", &json!({"name": alias}))
            .unwrap();
        assert_eq!(
            p["impact"],
            json!({
                "track_outputs": [], "bus_outputs": [], "sends": [],
                "automation": [], "automation_clips": [], "macro_targets": []
            })
        );
        let result = e
            .call(
                "remove_bus",
                &json!({
                    "name": p["bus_id"], "expected_revision": p["revision"]
                }),
            )
            .unwrap();
        assert_eq!(result["impact"], p["impact"]);
        assert_eq!(e.project.tracks, before.tracks);
        assert_eq!(e.project.automation, before.automation);
        assert_eq!(e.project.automation_clips, before.automation_clips);
        assert_eq!(e.project.macros, before.macros);
        assert_eq!(e.undo_depth().0, depth + 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
    }
}

#[test]
fn ambiguous_removed_and_surviving_send_alias_rejects_each_control_kind() {
    for uppercase in [false, true] {
        for kind in ["lane", "clip", "macro"] {
            let (mut e, alias) = send_id_collision_rig(uppercase, true);
            // Isolate each newly added control type so every walker is tested.
            if kind != "lane" {
                e.project.automation.pop();
            }
            if kind != "clip" {
                e.project.automation_clips.pop();
            }
            if kind != "macro" {
                e.project.macros[0].targets.pop();
            }
            let before = e.project.clone();
            let depth = e.undo_depth();
            let revision = e.revision;
            for action in ["preview_remove_bus", "remove_bus"] {
                let err = e.call(action, &json!({"name": alias})).unwrap_err();
                assert!(
                    err.to_string().contains("AmbiguousBusRemoval"),
                    "{kind}: {err:#}"
                );
                assert_eq!(e.project, before);
                assert_eq!(e.undo_depth(), depth);
                assert_eq!(e.revision, revision);
            }
        }
    }
}

#[test]
fn removed_bus_id_colliding_with_live_track_name_rejects_owner_cleanup() {
    let mut e = rig();
    let id = e.project.buses[0].id.clone();
    // Loaded projects can have cross-namespace name/ID collisions. The
    // renderer recognizes the track name, while cleanup also recognizes IDs.
    e.project.tracks[3].name = id.to_uppercase();
    control(&mut e, &id.to_uppercase(), "volume", "live track owner");
    beatbox::ids::ensure_ids(&mut e.project, None);
    let before = e.project.clone();
    let depth = e.undo_depth();
    for action in ["preview_remove_bus", "remove_bus"] {
        let err = e.call(action, &json!({"name": "mix"})).unwrap_err();
        assert!(err.to_string().contains("AmbiguousBusRemoval"), "{err:#}");
        assert_eq!(e.project, before);
        assert_eq!(e.undo_depth(), depth);
    }
}

#[test]
fn actual_bus_resolver_preserves_route_when_bus_name_collides_with_removed_id() {
    let mut e = rig();
    let alias = e.project.buses[0].id.to_uppercase();
    // bus_index accepts names case-insensitively but IDs case-sensitively.
    // Thus this route resolves to keepbus, not the bus with the lowercase ID.
    e.project.buses[4].name = alias.clone();
    // Keep the removed bus's own controls unambiguous in this route test.
    // The separate owner-collision test covers rejection of the ID alias.
    e.project.automation[0].target = "mix".into();
    e.project.automation_clips[0].target = "mix".into();
    e.project.macros[0].targets[0].track = "mix".into();
    e.project.tracks[3].output = Some(alias.clone());
    e.project.tracks[3].sends[0].bus = alias.clone();
    control(&mut e, "keep", &format!("send.{alias}"), "live bus name");
    beatbox::ids::ensure_ids(&mut e.project, None);
    assert_eq!(e.project.bus_index(&alias).unwrap(), 4);
    let before = e.project.clone();
    let p = preview(&mut e);
    assert!(!p["impact"]["track_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "keep"));
    assert!(!p["impact"]["sends"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["track"] == "keep"));
    e.call("remove_bus", &json!({"name": "mix"})).unwrap();
    assert_eq!(e.project.tracks[3], before.tracks[3]);
    assert!(e
        .project
        .automation
        .contains(before.automation.last().unwrap()));
    assert!(e
        .project
        .automation_clips
        .contains(before.automation_clips.last().unwrap()));
    assert!(e.project.macros[0]
        .targets
        .contains(before.macros[0].targets.last().unwrap()));
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
}
