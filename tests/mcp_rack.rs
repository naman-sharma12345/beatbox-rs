//! The MCP server over real stdio: a client process talks newline-delimited
//! JSON-RPC to `beatbox mcp`, reads the rack, edits it with rack_apply
//! (through the mixer category and by the tool's own name), gets structured
//! errors back, and ends with the same rack an in-process engine (the
//! studio's path) produces from the same commands.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

struct Client {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Client {
    fn start(workdir: &std::path::Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_beatbox"))
            .arg("--workdir")
            .arg(workdir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn beatbox mcp");
        let out = BufReader::new(child.stdout.take().unwrap());
        Client { child, out, id: 0 }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v["id"] == json!(self.id) {
                return v;
            }
        }
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn setup() -> Vec<(&'static str, Value)> {
    vec![
        ("new_project", json!({"name": "rack", "bpm": 120})),
        ("add_track", json!({"name": "kick", "preset": "kick"})),
        ("add_track", json!({"name": "bass", "preset": "808"})),
        ("add_bus", json!({"name": "verb", "preset": "reverb"})),
    ]
}

#[test]
fn rack_over_stdio_mcp() {
    let dir = std::env::temp_dir().join(format!("beatbox_mcp_rack_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Client::start(&dir);

    let init = c.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "beatbox", "{init}");
    // the rack is an action of the mixer category in tools/list
    let list = c.rpc("tools/list", json!({}));
    let mixer = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "mixer").cloned().expect("mixer category");
    let actions = mixer["inputSchema"]["properties"]["action"]["enum"].as_array().unwrap();
    assert!(actions.contains(&json!("rack_get")) && actions.contains(&json!("rack_apply")), "{actions:?}");

    for (t, a) in setup() {
        let r = c.call(t, a);
        assert_eq!(r["isError"], false, "{t}: {r}");
    }

    // 1. read the rack through the category
    let got = c.call("mixer", json!({"action": "rack_get", "params": {"plan": true}}));
    assert_eq!(got["isError"], false, "{got}");
    let rack = &got["structuredContent"];
    let strips = rack["strips"].as_array().unwrap();
    assert_eq!(strips.iter().map(|s| s["kind"].as_str().unwrap()).collect::<Vec<_>>(), vec!["track", "track", "bus", "master"]);
    let bass = strips[1]["id"].as_str().unwrap().to_string();
    let rev = rack["revision"].as_u64().unwrap();
    assert_eq!(rack["plan"]["nodes"].as_array().unwrap().len(), 4);
    // every parameter carries a spec a client can validate against
    let p0 = &strips[1]["mixer"][0];
    assert_eq!((p0["path"].as_str(), p0["kind"].as_str(), p0["unit"].as_str()), (Some("volume"), Some("number"), Some("dB")));
    assert!(p0["min"].is_number() && p0["max"].is_number() && p0["default"].is_number(), "{p0}");

    // 2. a batch mutating several params: one transaction, by the tool's own name
    let commands = json!([
        {"op": "add_device", "strip": bass, "type": "filter", "params": {"cutoff": 900.0}},
        {"op": "add_device", "strip": bass, "type": "compressor", "position": 0},
        {"op": "set_param", "device": format!("{bass}/filter1"), "name": "resonance", "value": 0.5},
        {"op": "send", "strip": bass, "to": "verb", "db": -14.0},
        {"op": "set_param", "param": format!("{bass}:volume"), "value": -2.5}
    ]);
    let r = c.call("rack_apply", json!({"expected_revision": rev, "commands": commands}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["applied"], 5);
    assert_eq!(r["structuredContent"]["results"][0]["device"], format!("{bass}/filter1"));
    let batched = c.call("rack_get", json!({}))["structuredContent"].clone();
    assert_eq!(batched["revision"].as_u64().unwrap(), rev + 1, "one revision for the whole batch");
    let bass_after = batched["strips"][1].clone();
    assert_eq!(bass_after["devices"].as_array().unwrap().len(), 3, "{bass_after}");
    assert_eq!(bass_after["mixer"][0]["value"], -2.5);

    // 3. one undo rolls the whole batch back; redo brings it back
    let u = c.call("history", json!({"action": "undo"}));
    assert_eq!(u["isError"], false, "{u}");
    let undone = c.call("rack_get", json!({}))["structuredContent"].clone();
    assert_eq!(undone["strips"], rack["strips"], "a single undo restores every strip");
    assert_eq!(undone["routes"], rack["routes"]);
    let r = c.call("history", json!({"action": "redo"}));
    assert_eq!(r["isError"], false, "{r}");
    let redone = c.call("rack_get", json!({}))["structuredContent"].clone();
    assert_eq!(redone["strips"], batched["strips"]);

    // 4. an out-of-bounds value: a structured rack_error, nothing applied
    let before = c.call("rack_get", json!({}))["structuredContent"].clone();
    let bad = c.call(
        "mixer",
        json!({"action": "rack_apply", "params": {"commands": [
            {"op": "set_param", "param": format!("{bass}:volume"), "value": -1.0},
            {"op": "set_param", "param": format!("{bass}:fx.1.cutoff"), "value": 50000.0}
        ]}}),
    );
    assert_eq!(bad["isError"], true, "{bad}");
    let e = &bad["structuredContent"]["rack_error"];
    assert_eq!(e["code"], "ParamOutOfRange", "{bad}");
    assert_eq!(e["path"], "commands[1].value");
    assert_eq!(e["index"], 1);
    assert_eq!(e["param_id"], format!("{bass}:fx.1.cutoff"));
    assert_eq!((e["val"].as_f64(), e["min"].as_f64(), e["max"].as_f64()), (Some(50000.0), Some(20.0), Some(20000.0)), "{bad}");
    assert!(bad["content"][0]["text"].as_str().unwrap().contains("ParamOutOfRange"));
    let after = c.call("rack_get", json!({}))["structuredContent"].clone();
    assert_eq!(before, after, "a failed rack_apply changes nothing (revision included)");
    let stale = c.call("rack_apply", json!({"expected_revision": rev, "commands": [{"op": "set_strip", "strip": "kick", "mute": true}]}));
    assert_eq!(stale["structuredContent"]["rack_error"]["code"], "StaleRevision", "{stale}");
    assert_eq!(stale["structuredContent"]["rack_error"]["expected"], rev);
    assert_eq!(stale["structuredContent"]["rack_error"]["actual"], after["revision"], "{stale}");
    let cyc = c.call("rack_apply", json!({"commands": [{"op": "route", "strip": bass, "to": bass}]}));
    assert_eq!(cyc["structuredContent"]["rack_error"]["code"], "InvalidRouting", "{cyc}");
    assert_eq!(c.call("rack_get", json!({}))["structuredContent"], after);

    // the in-process engine (the studio's path) gets the same rack from the same commands
    let mut e = beatbox::engine::Engine::new(dir.join("inproc"));
    for (t, a) in setup() {
        e.call_from(t, &a, "you").unwrap();
    }
    e.call_from("rack_apply", &json!({"commands": commands}), "you").unwrap();
    let local = e.call_from("rack_get", &json!({}), "you").unwrap();
    assert_eq!(local["strips"], after["strips"]);
    assert_eq!(local["routes"], after["routes"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The rack_error payload of a failed call (asserting it failed).
fn rack_error(r: &Value) -> Value {
    assert_eq!(r["isError"], true, "expected a failure: {r}");
    let e = r["structuredContent"]["rack_error"].clone();
    assert!(e.is_object(), "no structured rack_error: {r}");
    e
}

#[test]
fn each_rack_error_code_over_stdio_carries_its_own_fields() {
    let dir = std::env::temp_dir().join(format!("beatbox_mcp_rack_errors_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Client::start(&dir);
    c.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    for (t, a) in setup() {
        assert_eq!(c.call(t, a)["isError"], false, "{t}");
    }
    // two buses a -> b (so b -> a would loop) and a filter on the bass
    let rack = c.call("rack_get", json!({}))["structuredContent"].clone();
    let id = |rack: &Value, name: &str| rack["strips"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["id"].as_str().unwrap().to_string();
    let (kick, bass) = (id(&rack, "kick"), id(&rack, "bass"));
    let ok = c.call(
        "rack_apply",
        json!({"commands": [
            {"op": "add_bus", "name": "a"},
            {"op": "add_bus", "name": "b"},
            {"op": "route", "strip": "a", "to": "b"},
            {"op": "add_device", "strip": bass, "type": "filter"}
        ]}),
    );
    assert_eq!(ok["isError"], false, "{ok}");
    let rack = c.call("rack_get", json!({}))["structuredContent"].clone();
    let (a, b) = (id(&rack, "a"), id(&rack, "b"));
    let rev = rack["revision"].as_u64().unwrap();
    let apply = |c: &mut Client, cmd: Value| c.call("rack_apply", json!({"commands": [cmd]}));

    // NodeNotFound {node_id}
    let e = rack_error(&apply(&mut c, json!({"op": "set_strip", "strip": "ghost", "mute": true})));
    assert_eq!((e["code"].as_str(), e["node_id"].as_str(), e["path"].as_str()), (Some("NodeNotFound"), Some("ghost"), Some("commands[0].strip")), "{e}");
    // DeviceNotFound {node_id, device_id}
    let e = rack_error(&apply(&mut c, json!({"op": "remove_device", "device": format!("{bass}/reverb9")})));
    assert_eq!((e["code"].as_str(), e["node_id"].as_str(), e["device_id"].as_str()), (Some("DeviceNotFound"), Some(bass.as_str()), Some(format!("{bass}/reverb9").as_str())), "{e}");
    assert!(e["accepts"]["one_of"].as_array().unwrap().contains(&json!(format!("{bass}/filter1"))), "{e}");
    // ParamNotFound {node_id, param_id}
    let e = rack_error(&apply(&mut c, json!({"op": "set_param", "param": format!("{bass}:fx.0.cutof"), "value": 1.0})));
    assert_eq!((e["code"].as_str(), e["node_id"].as_str(), e["param_id"].as_str()), (Some("ParamNotFound"), Some(bass.as_str()), Some(format!("{bass}:fx.0.cutof").as_str())), "{e}");
    // ParamOutOfRange {param_id, val, min, max}
    let e = rack_error(&apply(&mut c, json!({"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": 5.0})));
    assert_eq!(e["code"], "ParamOutOfRange", "{e}");
    assert_eq!(e["param_id"], format!("{bass}:fx.0.cutoff"));
    assert_eq!((e["val"].as_f64(), e["min"].as_f64(), e["max"].as_f64()), (Some(5.0), Some(20.0), Some(20000.0)), "{e}");
    // ParamWrongType {param_id, expected, received} + the accepts hint (the param's JSON schema)
    let e = rack_error(&apply(&mut c, json!({"op": "set_param", "param": format!("{bass}:volume"), "value": "loud"})));
    assert_eq!((e["code"].as_str(), e["param_id"].as_str()), (Some("ParamWrongType"), Some(format!("{bass}:volume").as_str())), "{e}");
    assert_eq!((e["expected"].as_str(), e["received"].as_str()), (Some("number"), Some("string")), "{e}");
    assert_eq!(e["accepts"]["type"], "number", "{e}");
    assert!(e["accepts"]["minimum"].is_number() && e["accepts"]["maximum"].is_number(), "{e}");
    // InvalidRouting {source, target, reason}
    let e = rack_error(&apply(&mut c, json!({"op": "route", "strip": kick, "to": bass})));
    assert_eq!((e["code"].as_str(), e["source"].as_str(), e["target"].as_str()), (Some("InvalidRouting"), Some(kick.as_str()), Some(bass.as_str())), "{e}");
    assert!(e["reason"].as_str().unwrap().contains("track"), "{e}");
    // RoutingCycle {cycle_path}
    let e = rack_error(&apply(&mut c, json!({"op": "route", "strip": b, "to": a})));
    assert_eq!(e["code"], "RoutingCycle", "{e}");
    assert_eq!(e["cycle_path"], json!([b, a, b]), "{e}");
    // StaleRevision {expected, actual}
    let e = rack_error(&c.call("rack_apply", json!({"expected_revision": rev - 1, "commands": [{"op": "set_strip", "strip": kick, "mute": true}]})));
    assert_eq!((e["code"].as_str(), e["expected"].as_u64(), e["actual"].as_u64()), (Some("StaleRevision"), Some(rev - 1), Some(rev)), "{e}");
    // none of the eight changed anything
    assert_eq!(c.call("rack_get", json!({}))["structuredContent"], rack, "failed calls leave the rack and the revision as they were");

    // a batch with an injected fault (the 4th of 5) applies nothing; the revision stays
    let bad = c.call(
        "rack_apply",
        json!({"commands": [
            {"op": "set_param", "param": format!("{bass}:volume"), "value": -6.0},
            {"op": "add_device", "strip": kick, "type": "chorus"},
            {"op": "send", "strip": bass, "to": "verb", "db": -10.0},
            {"op": "set_param", "param": format!("{bass}:fx.0.resonance"), "value": 9.0},
            {"op": "set_strip", "strip": kick, "mute": true}
        ]}),
    );
    let e = rack_error(&bad);
    assert_eq!((e["code"].as_str(), e["index"].as_u64(), e["path"].as_str()), (Some("ParamOutOfRange"), Some(3), Some("commands[3].value")), "{e}");
    let now = c.call("rack_get", json!({}))["structuredContent"].clone();
    assert_eq!(now, rack, "nothing of the faulty batch was applied");
    assert_eq!(now["revision"].as_u64(), Some(rev), "the revision did not move");

    // one undo reverts exactly the last batch's delta: batch A, then batch B,
    // undo -> the rack as it was after A (A's edits stay)
    let ra = c.call("rack_apply", json!({"expected_revision": rev, "commands": [{"op": "set_param", "param": format!("{bass}:volume"), "value": -5.0}, {"op": "add_device", "strip": kick, "type": "compressor"}]}));
    assert_eq!(ra["isError"], false, "{ra}");
    let after_a = c.call("rack_get", json!({"plan": true}))["structuredContent"].clone();
    let rb = c.call(
        "rack_apply",
        json!({"expected_revision": after_a["revision"], "commands": [
            {"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": 440.0},
            {"op": "move_device", "device": format!("{bass}/filter1"), "position": 0},
            {"op": "send", "strip": kick, "to": "verb", "db": -12.0},
            {"op": "route", "strip": "b", "to": "verb"}
        ]}),
    );
    assert_eq!(rb["isError"], false, "{rb}");
    let after_b = c.call("rack_get", json!({"plan": true}))["structuredContent"].clone();
    assert_ne!(after_b["strips"], after_a["strips"]);
    assert_ne!(after_b["routes"], after_a["routes"]);
    let u = c.call("history", json!({"action": "undo"}));
    assert_eq!(u["isError"], false, "{u}");
    let undone = c.call("rack_get", json!({"plan": true}))["structuredContent"].clone();
    // the undo is itself a new revision; everything else is the rack after A
    for k in ["strips", "routes", "plan", "schema"] {
        assert_eq!(undone[k], after_a[k], "one undo reverts exactly batch B ({k})");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
