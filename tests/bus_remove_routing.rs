use beatbox::Engine;
use serde_json::json;

fn rig() -> Engine {
    let root = std::env::temp_dir().join(format!(
        "bb_bus_remove_routing_{}",
        beatbox::creative::fresh_seed()
    ));
    let mut e = Engine::new(root);
    for name in ["drums", "bass", "mix", "other", "keep"] {
        e.call("add_bus", &json!({"name": name})).unwrap();
    }
    for (bus, destination) in [("drums", "mix"), ("bass", "mix"), ("keep", "other")] {
        e.call(
            "route_bus",
            &json!({"bus": bus, "destination": destination}),
        )
        .unwrap();
    }
    e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
        .unwrap();
    e.call("route_track", &json!({"track": "kick", "bus": "drums"}))
        .unwrap();
    e
}

#[test]
fn deleted_bus_routes_do_not_revive_when_name_is_reused() {
    let mut e = rig();
    e.call("remove_bus", &json!({"name": "mix"})).unwrap();
    e.call("add_bus", &json!({"name": "mix"})).unwrap();
    for name in ["drums", "bass"] {
        let i = e.project.bus_index(name).unwrap();
        assert_eq!(
            e.project.buses[i].output, None,
            "deleted bus routing must not attach to a replacement with the same name"
        );
    }
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn removal_preserves_other_routes_and_round_trips_history_and_disk() {
    let mut e = rig();
    let before = e.project.clone();
    let result = e.call("remove_bus", &json!({"name": "MIX"})).unwrap();
    assert_eq!(result["rerouted_buses_to_master"], json!(["drums", "bass"]));
    assert_eq!(result["rerouted_to_master"], json!([]));
    for name in ["drums", "bass"] {
        assert_eq!(
            e.project.buses[e.project.bus_index(name).unwrap()].output,
            None
        );
    }
    let keep = e.project.bus_index("keep").unwrap();
    let original_keep = before.bus_index("keep").unwrap();
    assert_eq!(e.project.buses[keep], before.buses[original_keep]);
    assert_eq!(e.project.tracks, before.tracks);
    let after = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, after);
    e.call("save_project", &json!({"path": "bus-routing.beatbox.json"}))
        .unwrap();
    e.call("new_project", &json!({})).unwrap();
    e.call("load_project", &json!({"path": "bus-routing.beatbox.json"}))
        .unwrap();
    assert_eq!(e.project, after);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn removal_resolves_stored_bus_id_and_case_insensitive_name_routes() {
    let mut e = rig();
    let id = e.project.buses[e.project.bus_index("mix").unwrap()]
        .id
        .clone();
    // Loaded project files can use the same aliases accepted by bus_index.
    let drums = e.project.bus_index("drums").unwrap();
    let bass = e.project.bus_index("bass").unwrap();
    e.project.buses[drums].output = Some(id.clone());
    e.project.buses[bass].output = Some("MiX".into());
    let result = e.call("remove_bus", &json!({"name": id})).unwrap();
    assert_eq!(result["rerouted_buses_to_master"], json!(["drums", "bass"]));
    assert_eq!(e.project.buses[drums].output, None);
    assert_eq!(e.project.buses[bass].output, None);
    let _ = std::fs::remove_dir_all(&e.workdir);
}

#[test]
fn missing_bus_removal_leaves_routing_unchanged() {
    let mut e = rig();
    let before = e.project.clone();
    assert!(e.call("remove_bus", &json!({"name": "missing"})).is_err());
    assert_eq!(e.project, before);
    let _ = std::fs::remove_dir_all(&e.workdir);
}
