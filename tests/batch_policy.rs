//! Batch policy and Project-only rollback. No render, GUI or audio/MIDI device.
//! New regressions are intentionally unexecuted in the bounded safety worktree.
use beatbox::engine::TransportCmd;
use beatbox::tools_categories::category_of;
use beatbox::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "beatbox-batch-policy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Never reuse another test's (or a stale process's) files.
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rig(dir: &TempDir) -> Engine {
    let mut disk = Engine::new(dir.0.clone());
    disk.call("new_project", &json!({"name":"disk-song","bpm":89}))
        .unwrap();
    disk.midi.quantize = 4.0;
    disk.call("snapshot", &json!({"name":"disk-checkpoint"}))
        .unwrap();
    disk.call("save_project", &json!({"path":"other.json"}))
        .unwrap();
    std::fs::write(dir.0.join("sentinel"), b"never overwrite").unwrap();
    let mut e = Engine::new(dir.0.clone());
    e.call("set_tempo", &json!({"bpm":130})).unwrap();
    e.call("set_tempo", &json!({"bpm":140})).unwrap();
    e.call("undo", &json!({})).unwrap(); // both undo and redo have entries
    e.call("snapshot", &json!({"name":"live-checkpoint"}))
        .unwrap();
    e.midi.quantize = 0.25;
    e.transport.pending.push(TransportCmd::Play);
    e
}

fn state(e: &Engine) -> Value {
    // Logs intentionally excluded: they are audit evidence, not transaction state.
    let snapshots: Vec<_> = e
        .snapshots
        .iter()
        .map(|(n, s)| json!({"name":n,"project":s.project,"note":s.note,"revision":s.revision}))
        .collect();
    json!({
        "project": e.project, "history": e.history(), "depth": e.undo_depth(),
        "revision":e.revision,"generation":e.project_load_generation,
        "snapshots":snapshots,"midi":beatbox::midi_in::snapshot(e),
        "midi_record":e.midi.record,"midi_learn":e.midi.learn,
        "transport":format!("{:?}", e.transport),
    })
}

fn files(dir: &TempDir) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| {
            let p = entry.unwrap().path();
            let bytes = std::fs::read(&p).unwrap();
            (p.file_name().unwrap().into(), bytes)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn category(name: &str, args: Value) -> (String, Value) {
    (
        category_of(name).unwrap().name.into(),
        json!({"action":name,"params":args}),
    )
}
fn wrapper(name: &str, args: Value) -> (String, Value) {
    ("call_tool".into(), json!({"name":name,"arguments":args}))
}
fn category_wrapper(name: &str, args: Value) -> (String, Value) {
    category("call_tool", json!({"name":name,"arguments":args}))
}
fn aliases(name: &str, args: Value) -> Vec<(String, Value)> {
    let (home, params) = category(name, args.clone());
    let mut flat = args.as_object().unwrap().clone();
    flat.insert("action".into(), json!(name));
    let mut forms = vec![
        (name.into(), args.clone()),
        (home.clone(), Value::Object(flat)),
        (home.clone(), params.clone()),
        ("mixer".into(), json!({"action":name,"params":args})),
        wrapper(name, args.clone()),
        wrapper(&home, params),
        category_wrapper(name, args.clone()),
    ];
    let mut nested: (String, Value) = (name.into(), args);
    for _ in 0..4 {
        nested = category_wrapper(&nested.0, nested.1);
    }
    forms.push(wrapper(
        "mixer",
        json!({"action":name,"params":forms[0].1.clone()}),
    ));
    forms.push(wrapper(&nested.0, nested.1.clone()));
    forms.push(nested);
    forms
}

#[test]
fn all_six_effective_actions_reject_before_any_member_in_both_modes() {
    for (terminal, args) in [
        ("batch", json!({"calls":[]})),
        ("undo", json!({})),
        ("redo", json!({})),
        ("load_project", json!({"path":"other.json"})),
        ("new_project", json!({"name":"must-not-replace"})),
        // No valid production brief: a missed policy must fail this test before
        // it could ever reach expensive production/audio work.
        ("produce_track", json!({"out_dir":"must-not-create"})),
    ] {
        for (name, args) in aliases(terminal, args) {
            for atomic in [true, false] {
                for alternate_keys in [false, true] {
                    let dir = TempDir::new();
                    let mut e = rig(&dir);
                    let before = state(&e);
                    let disk_before = files(&dir);
                    let log_len = e.log.len();
                    let member = if alternate_keys {
                        json!({"name":name,"arguments":args})
                    } else {
                        json!({"tool":name,"args":args})
                    };
                    let error = e
                        .call(
                            "batch",
                            &json!({"atomic":atomic,"calls":[
                                {"tool":"set_tempo","args":{"bpm":151}}, member,
                                {"tool":"set_tempo","args":{"bpm":144,"typo":1}}
                            ]}),
                        )
                        .unwrap_err()
                        .to_string();
                    assert!(
                        error.contains("call 1")
                            && error.contains(terminal)
                            && error.contains("can't run inside a batch"),
                        "{name}: {error}"
                    );
                    assert_eq!(state(&e), before, "{name}: {terminal}, atomic={atomic}");
                    assert_eq!(files(&dir), disk_before);
                    assert_eq!(e.log.len(), log_len + 1);
                    assert_eq!(e.log.last().unwrap().tool, "batch");
                    assert!(!e.log.last().unwrap().ok);
                    assert!(!dir.0.join("must-not-create").exists());
                    // Rejection must not leak policy into a standalone call.
                    e.call("history", &json!({"action":"undo"})).unwrap();
                    e.call("history", &json!({"action":"redo"})).unwrap();
                }
            }
        }
    }
}

#[test]
fn standalone_aliases_answers_routing_and_wrapper_validation() {
    let dir = TempDir::new();
    let mut e = rig(&dir);
    let before = e.project.clone();
    e.call(
        "call_tool",
        &json!({"name":"history","arguments":{"action":"undo"}}),
    )
    .unwrap();
    e.call(
        "call_tool",
        &json!({"name":"history","arguments":{"action":"redo"}}),
    )
    .unwrap();
    assert_eq!(e.project, before);
    e.call("history", &json!({})).unwrap(); // legacy terminal, never a resolution loop
    e.call("transport", &json!({"action":"status"})).unwrap();
    e.call("transport", &json!({"action":"transport","op":"status"}))
        .unwrap();
    // Help about banned actions is an answer, not a forbidden invocation.
    for name in ["undo", "batch", "load_project"] {
        let home = category_of(name).unwrap().name;
        e.call(
            "batch",
            &json!({"calls":[{"tool":home,
            "args":{"action":"help","params":{"action":name}}}]}),
        )
        .unwrap();
    }
    let routed = e
        .call(
            "call_tool",
            &json!({"name":"mixer","arguments":{
        "action":"set_tempo","params":{"bpm":137}}}),
        )
        .unwrap();
    assert!(routed["note"].as_str().unwrap().contains("project"));
    assert_eq!(e.project.bpm, 137.0);
    for bad in [
        json!({}),
        json!({"name":true}),
        json!({"name":"call_tool"}),
        json!({"name":"set_tempo","extra":1}),
        json!({"name":"no-such-tool"}),
        json!({"name":"set_tempo","arguments":{"bpm":140,"typo":1}}),
    ] {
        let before = state(&e);
        let solo = e.call("call_tool", &bad).unwrap_err().to_string();
        let r = e
            .call(
                "batch",
                &json!({"atomic":false,"calls":[
            {"tool":"call_tool","args":bad}]}),
            )
            .unwrap();
        assert_eq!(r["errors"], 1);
        assert_eq!(r["results"][0]["error"], solo);
        assert_eq!(state(&e), before);
    }
    let before_batch = e.project.clone();
    let depth = e.undo_depth().0;
    e.call(
        "call_tool",
        &json!({"name":"help","arguments":{"action":"batch","params":{
            "calls":[{"tool":"set_tempo","args":{"bpm":142}}]
        }}}),
    )
    .unwrap();
    assert_eq!(e.undo_depth().0, depth + 1);
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before_batch);
    e.call(
        "project",
        &json!({"action":"new_project","params":{"name":"new-song"}}),
    )
    .unwrap();
    assert_eq!(e.project.name, "new-song");
    e.call(
        "call_tool",
        &json!({"name":"project","arguments":{
        "action":"load_project","params":{"path":"other.json"}}}),
    )
    .unwrap();
    assert_eq!(e.project.name, "disk-song");
    assert_eq!(e.midi.quantize, 4.0);
    assert_eq!(e.snapshots[0].0, "disk-checkpoint");
}

#[test]
fn wrapper_hop_boundary_is_bounded_and_does_not_poison_next_call() {
    let dir = TempDir::new();
    let mut e = Engine::new(dir.0.clone());
    let mut invocation: (String, Value) = ("set_tempo".into(), json!({"bpm":133}));
    for _ in 0..32 {
        invocation = category_wrapper(&invocation.0, invocation.1);
    }
    e.call(&invocation.0, &invocation.1).unwrap();
    assert_eq!(e.project.bpm, 133.0);
    invocation = category_wrapper(&invocation.0, invocation.1);
    let before = state(&e);
    assert!(e
        .call(&invocation.0, &invocation.1)
        .unwrap_err()
        .to_string()
        .contains("hop limit"));
    assert_eq!(state(&e), before);
    let r = e
        .call(
            "batch",
            &json!({"atomic":false,"calls":[
                {"tool":invocation.0,"args":invocation.1},
                {"tool":"set_tempo","args":{"bpm":134}}
            ]}),
        )
        .unwrap();
    assert_eq!(r["errors"], 1);
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project.bpm, 133.0);
}

#[test]
fn success_is_one_undo_with_runtime_ids_and_seed_rules() {
    let dir = TempDir::new();
    let mut e = Engine::new(dir.0.clone());
    e.call("add_track", &json!({"name":"pad","preset":"warm_pad"}))
        .unwrap();
    let before = e.project.clone();
    let history = e.undo_depth();
    let revision = e.revision;
    let r = e.call("batch", &json!({"calls":[
        {"tool":"set_tempo","args":{"bpm":138}},
        {"tool":"project","args":{"action":"set_tempo","params":{"bpm":139}}},
        {"tool":"call_tool","args":{"name":"add_effect","arguments":{"track":"pad","type":"filter"}}},
        {"tool":"effects","args":{"action":"tweak_effect","params":{"track":"pad","index":"filter1","params":{"cutoff":900}}}},
        {"tool":"help","args":{"action":"call_tool","params":{"name":"generate_drums","arguments":{"style":"trap","seed":4}}}},
        {"tool":"call_tool","args":{"name":"pattern","arguments":{"action":"generate_drums","params":{"style":"trap"}}}}
    ]})).unwrap();
    assert_eq!(e.revision, revision + 1);
    assert_eq!(e.undo_depth(), (history.0 + 1, 0));
    assert_eq!(r["results"][5]["result"]["seed_source"], "fresh");
    let seeds = &e.project.seed_log;
    assert_eq!(seeds[seeds.len() - 2].seed, 4);
    assert_eq!(seeds[seeds.len() - 2].source, "given");
    assert_eq!(seeds.last().unwrap().source, "fresh");
    let fx = serde_json::to_value(&e.project.tracks[0].effects[0]).unwrap();
    assert_eq!(fx["cutoff"], 900.0);
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    e.call("redo", &json!({})).unwrap(); // successful batch policy is gone
}

#[test]
fn partial_errors_stay_ordered_and_all_failed_has_no_undo() {
    let dir = TempDir::new();
    let mut e = Engine::new(dir.0.clone());
    let before = e.project.clone();
    let r = e
        .call(
            "batch",
            &json!({"atomic":false,"calls":[
                {"tool":"set_tempo","args":{"bpm":140}},
                {"tool":"no-such-tool"},
                {"tool":"call_tool","args":{"name":"set_tempo","arguments":{"bpm":144,"typo":1}}},
                {"tool":"project","args":{"action":"set_tempo","bpm":141}}
            ]}),
        )
        .unwrap();
    let ok: Vec<_> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ok"].as_bool().unwrap())
        .collect();
    assert_eq!(ok, vec![true, false, false, true]);
    assert_eq!(e.project.bpm, 141.0);
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, before);
    let saved = state(&e);
    let r = e
        .call(
            "batch",
            &json!({"atomic":false,"calls":[
                {"tool":"no-such-tool"}, {"tool":"set_tempo","args":{"bpm":140,"typo":1}}
            ]}),
        )
        .unwrap();
    assert_eq!(r["applied"], 0);
    assert_eq!(state(&e), saved);
}

#[test]
fn atomic_boundary_keeps_checkpoints_transport_and_written_files() {
    for existing_sidecar in [false, true] {
        let dir = TempDir::new();
        let mut e = Engine::new(dir.0.clone());
        e.midi.quantize = 0.25;
        std::fs::write(dir.0.join("partial.json"), b"old-main").unwrap();
        std::fs::write(dir.0.join("partial.json.snapshots.json"), b"old-side").unwrap();
        if existing_sidecar {
            e.call("snapshot", &json!({"name":"old-checkpoint"}))
                .unwrap();
        }
        let project = e.project.clone();
        let history = e.history();
        let revision = e.revision;
        let error = e
            .call(
                "batch",
                &json!({"calls":[
                    {"tool":"set_tempo","args":{"bpm":151}},
                    {"tool":"save_project","args":{"path":"partial.json"}},
                    {"tool":"snapshot","args":{"name":"outside-transaction"}},
                    {"tool":"transport","args":{"action":"stop"}},
                    {"tool":"set_tempo","args":{"bpm":144,"typo":1}}
                ]}),
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("call 4")
                && error.contains("project edits rolled back")
                && error.contains("external/session effects are not transactional"),
            "{error}"
        );
        assert_eq!(e.project, project); // includes restored project.midi
        assert_eq!(e.history(), history);
        assert_eq!(e.revision, revision);
        let written: Value =
            serde_json::from_slice(&std::fs::read(dir.0.join("partial.json")).unwrap()).unwrap();
        assert_eq!(written["bpm"], 151.0);
        assert_eq!(written["midi"]["quantize"], 0.25);
        let side = dir.0.join("partial.json.snapshots.json");
        assert_eq!(side.exists(), existing_sidecar);
        if existing_sidecar {
            let saved: Value = serde_json::from_slice(&std::fs::read(side).unwrap()).unwrap();
            assert_eq!(saved[0]["name"], "old-checkpoint");
            assert_eq!(saved.as_array().unwrap().len(), 1);
        }
        assert!(e
            .snapshots
            .iter()
            .any(|(n, s)| n == "outside-transaction" && s.project.bpm == 151.0));
        assert_eq!(e.transport.pending, vec![TransportCmd::Stop]);
        e.call("new_project", &json!({"name":"policy-cleared-after-error"}))
            .unwrap();
    }
}

#[test]
fn checkpoint_overwrite_is_not_rewound_by_project_rollback() {
    let dir = TempDir::new();
    let mut e = Engine::new(dir.0.clone());
    e.call("snapshot", &json!({"name":"same"})).unwrap();
    let before = e.project.clone();
    assert!(e
        .call(
            "batch",
            &json!({"calls":[
                {"tool":"set_tempo","args":{"bpm":152}},
                {"tool":"snapshot","args":{"name":"same","note":"intermediate"}},
                {"tool":"set_tempo","args":{"bpm":144,"typo":1}}
            ]})
        )
        .is_err());
    assert_eq!(e.project, before);
    assert_eq!(e.snapshots.len(), 1);
    assert_eq!(e.snapshots[0].1.project.bpm, 152.0);
    assert_eq!(e.snapshots[0].1.note, "intermediate");
    assert_eq!(e.undo_depth(), (0, 0));
    assert_eq!(e.revision, 0);
}
