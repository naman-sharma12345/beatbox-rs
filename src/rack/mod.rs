//! The rack: a typed, serializable view of a project's signal graph, kept
//! apart from the DSP that plays it.
//!
//! - **State** ([`model`]): [`RackState`] is derived from the
//!   [`Project`](crate::project::Project), which stays the one stored source
//!   of truth (the project file format does not change, so every old
//!   project loads and renders exactly as before). The rack lists every
//!   channel strip (tracks, buses, the master), each strip's devices (the
//!   instrument, then the insert chain in processing order), every routing
//!   edge (output, aux send, sidechain key) and every parameter with a
//!   stable id, its range and unit, whether it is automatable and which
//!   automation lanes drive it.
//! - **Runtime** ([`compile`]): [`compile::compile`] turns the state into a
//!   [`compile::ProcessPlan`], the order the renderer processes strips in
//!   (tracks, then buses after every bus that feeds them, then the master)
//!   with each node's inputs and active devices. The renderer takes its bus
//!   order from here.
//!
//! - **Commands** ([`cmd`]): `rack_get` reads the rack, `rack_apply` edits
//!   it with a list of commands (add / remove / move device, set_param,
//!   bypass, route, send, add / remove bus) as one transaction (validated
//!   whole on a staged copy, then applied), answering failures with typed
//!   errors: a stable code plus that code's own fields (ParamOutOfRange
//!   {param_id, val, min, max}, StaleRevision {expected, actual}, ...). The
//!   studio GUI and the MCP server use this same surface.
//!
//! Design notes are clean-room: the general idea of a modular device rack
//! (devices on a chain, routing between strips, parameters as addressable
//! objects with ranges and automation) is public, common DAW knowledge; no
//! other project's code was read or copied.

pub mod cmd;
pub mod compile;
pub mod model;

pub use compile::{compile, ProcessPlan};
pub use model::{Device, DeviceKind, Param, ParamMismatch, RackErrorCode, RackState, Route, Strip, StripKind};

#[cfg(test)]
mod tests {
    use super::compile::{bus_order, compile};
    use super::model::*;
    use crate::engine::Engine;
    use crate::project::{Bus, Project};
    use serde_json::json;

    fn eng(tag: &str) -> Engine {
        let d = std::env::temp_dir().join(format!("beatbox_rack_tests_{tag}"));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("new_project", &json!({"name": "rack", "bpm": 120})).unwrap();
        e
    }

    /// kick + bass (sidechained to kick, reverb send), drums bus -> beat bus,
    /// a filter sweep lane, a bypassed insert on the master.
    pub(crate) fn studio(tag: &str) -> Engine {
        let mut e = eng(tag);
        for (n, p) in [("kick", "kick"), ("bass", "808")] {
            e.call("add_track", &json!({"name": n, "preset": p})).unwrap();
        }
        e.call("add_bus", &json!({"name": "drums", "preset": "none"})).unwrap();
        e.call("add_bus", &json!({"name": "beat", "preset": "none"})).unwrap();
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"})).unwrap();
        e.call("route_track", &json!({"track": "kick", "bus": "drums"})).unwrap();
        e.call("route_bus", &json!({"bus": "drums", "destination": "beat"})).unwrap();
        e.call("set_send", &json!({"track": "bass", "bus": "verb", "db": -12})).unwrap();
        e.call("add_effect", &json!({"track": "bass", "type": "filter", "params": {"cutoff": 800}})).unwrap();
        e.call("add_effect", &json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": 0.7}})).unwrap();
        e.call("add_automation", &json!({"track": "bass", "param": "fx.0.cutoff", "points": [[0, 200], [8, 4000]]})).unwrap();
        e.call("add_effect", &json!({"track": "master", "type": "limiter"})).unwrap();
        e.call("add_effect", &json!({"track": "master", "type": "width"})).unwrap();
        e.call("bypass_effect", &json!({"track": "master", "index": 3, "on": true})).unwrap();
        e
    }

    #[test]
    fn rack_lists_strips_devices_routes_and_params() {
        let e = studio("model");
        let before = e.project.clone();
        let r = RackState::from_project(&e.project);
        assert_eq!(e.project, before, "deriving the rack must not touch the project");
        let kinds: Vec<StripKind> = r.strips.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![StripKind::Track, StripKind::Track, StripKind::Bus, StripKind::Bus, StripKind::Bus, StripKind::Master]);
        let bass = r.strip("bass").unwrap();
        assert_eq!(bass.id, e.project.tracks[1].id);
        assert_eq!(bass.devices[0].kind, DeviceKind::Instrument);
        assert_eq!(bass.devices[0].id, format!("{}/instrument", bass.id));
        assert_eq!(bass.devices[1].type_name, "filter");
        assert_eq!(bass.devices[1].slot, Some(0));
        // the cutoff knob: range, unit, log scale, and the lane that drives it
        let (_, cut) = r.param(&format!("{}:fx.0.cutoff", bass.id)).unwrap();
        assert_eq!((cut.min, cut.max, cut.scale.as_deref(), cut.automatable), (Some(20.0), Some(20000.0), Some("log"), true));
        assert_eq!(cut.default, Some(json!(2000.0)));
        assert_eq!(cut.value, json!(800.0));
        // the spec drives validation and widgets
        assert_eq!(cut.json_schema()["maximum"], json!(20000.0));
        assert_eq!(cut.check(&json!(25000.0)).unwrap_err().code, RackErrorCode::ParamOutOfRange);
        assert_eq!(cut.check(&json!("loud")).unwrap_err().code, RackErrorCode::ParamWrongType);
        assert_eq!(RackErrorCode::ParamOutOfRange.as_str(), "ParamOutOfRange");
        assert!(cut.check(&json!(440.0)).is_ok());
        assert_eq!(cut.unit.as_deref(), Some("Hz"));
        assert_eq!(cut.lanes, vec![e.project.automation[0].id.clone()]);
        assert!(!cut.lanes[0].is_empty());
        // the enum: filter mode with its options
        let (_, mode) = r.param(&format!("{}:fx.0.mode", bass.id)).unwrap();
        assert_eq!(mode.kind, "enum");
        assert!(mode.options.as_ref().unwrap().contains(&"highpass".to_string()));
        assert_eq!(mode.check(&json!("wobble")).unwrap_err().code, RackErrorCode::ParamNotAnOption);
        assert_eq!(mode.json_schema()["type"], "string");
        assert!(mode.scale.is_none());
        // send level on the fader section
        assert!(bass.mixer.iter().any(|m| m.path == "send.verb" && m.value == json!(-12.0)));
        // routes: outputs, the send, the bus-into-bus insert, the sidechain key
        let kick = r.strip("kick").unwrap();
        let drums = r.strip("drums").unwrap();
        let beat = r.strip("beat").unwrap();
        assert!(r.routes.contains(&Route::Output { from: kick.id.clone(), to: drums.id.clone() }));
        assert!(r.routes.contains(&Route::Output { from: drums.id.clone(), to: beat.id.clone() }));
        assert!(r.routes.contains(&Route::Output { from: beat.id.clone(), to: "master".into() }));
        assert!(r.routes.iter().any(|x| matches!(x, Route::Send { from, to, db, .. } if *from == bass.id && *to == r.strip("verb").unwrap().id && *db == -12.0)));
        assert!(r.routes.iter().any(|x| matches!(x, Route::Sidechain { from, to, device } if *from == kick.id && *to == bass.id && device.ends_with("sidechain1"))));
        // master: bypass state, no pan
        let m = r.strip("master").unwrap();
        // new_project's compressor + limiter, then the added limiter and the bypassed width
        assert_eq!(m.devices.iter().map(|d| d.type_name.as_str()).collect::<Vec<_>>(), vec!["compressor", "limiter", "limiter", "width"]);
        assert_eq!(m.devices.iter().map(|d| d.bypassed).collect::<Vec<_>>(), vec![false, false, false, true]);
        assert_ne!(m.devices[1].id, m.devices[2].id, "two limiters, two device ids");
        assert!(m.mixer.iter().all(|x| x.path != "pan"));
        // every parameter id is unique; none of them is the effect id or bypass flag
        let ids = r.param_ids();
        let mut u = ids.clone();
        u.sort_unstable();
        u.dedup();
        assert_eq!(u.len(), ids.len());
        assert!(ids.iter().all(|i| !i.ends_with(":id") && !i.ends_with(".bypass") && !i.ends_with(".id")));
        // the state is plain data: JSON round trip is lossless
        let v = serde_json::to_value(&r).unwrap();
        let back: RackState = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn device_ids_survive_reordering() {
        let mut e = studio("reorder");
        let r0 = RackState::from_project(&e.project);
        let ids0: Vec<String> = r0.strip("bass").unwrap().devices.iter().map(|d| d.id.clone()).collect();
        e.call("reorder_effects", &json!({"track": "bass", "from": 1, "to": 0})).unwrap();
        let r1 = RackState::from_project(&e.project);
        let d1 = &r1.strip("bass").unwrap().devices;
        assert_eq!(d1[1].id, ids0[2]);
        assert_eq!(d1[2].id, ids0[1]);
        assert_eq!(d1[2].slot, Some(1));
        // the lane followed its effect, so the hook is on the new path
        let (_, cut) = r1.param(&format!("{}:fx.1.cutoff", r1.strip("bass").unwrap().id)).unwrap();
        assert_eq!(cut.lanes.len(), 1);
    }

    /// The pre-rack bus order, kept verbatim as the reference.
    fn old_bus_order(p: &Project) -> Vec<usize> {
        let n = p.buses.len();
        let out_of = |i: usize| p.buses[i].output.as_deref().and_then(|o| p.bus_index(o).ok());
        let mut indeg = vec![0usize; n];
        for i in 0..n {
            if let Some(d) = out_of(i) {
                if d != i {
                    indeg[d] += 1;
                }
            }
        }
        let mut order = Vec::with_capacity(n);
        let mut done = vec![false; n];
        while let Some(i) = (0..n).find(|&i| !done[i] && indeg[i] == 0) {
            done[i] = true;
            order.push(i);
            if let Some(d) = out_of(i) {
                if d != i {
                    indeg[d] -= 1;
                }
            }
        }
        order.extend((0..n).filter(|&i| !done[i]));
        order
    }

    #[test]
    fn compiled_bus_order_matches_the_old_renderer_on_random_graphs() {
        let mut s: u64 = 0x9e3779b97f4a7c15;
        let mut rnd = |m: u64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s % m
        };
        let mut cycles_seen = 0;
        for _ in 0..400 {
            let mut p = Project::new("g", 120.0);
            let n = 1 + rnd(7) as usize;
            for i in 0..n {
                p.buses.push(Bus::new(&format!("b{i}")));
            }
            for i in 0..n {
                p.buses[i].output = match rnd(n as u64 + 2) as usize {
                    k if k < n => Some(format!("b{k}")),
                    k if k == n => Some("master".into()),
                    _ => None,
                };
            }
            assert_eq!(bus_order(&p), old_bus_order(&p));
            assert_eq!(crate::render::bus_order(&p), old_bus_order(&p));
            let plan = compile(&p);
            assert_eq!(plan.bus_indices(), old_bus_order(&p));
            if !plan.cycles.is_empty() {
                cycles_seen += 1;
            }
        }
        assert!(cycles_seen > 0, "the generator should produce some cycles");
    }

    #[test]
    fn plan_orders_tracks_buses_master_and_skips_bypassed_devices() {
        let e = studio("plan");
        let plan = compile(&e.project);
        let r = RackState::from_project(&e.project);
        let kinds: Vec<StripKind> = plan.nodes.iter().map(|n| n.kind).collect();
        assert_eq!(kinds, vec![StripKind::Track, StripKind::Track, StripKind::Bus, StripKind::Bus, StripKind::Bus, StripKind::Master]);
        let pos = |name: &str| plan.nodes.iter().position(|n| n.strip == r.strip(name).unwrap().id).unwrap();
        assert!(pos("drums") < pos("beat"));
        assert!(plan.cycles.is_empty());
        let master = plan.nodes.last().unwrap();
        assert_eq!(master.active_devices.len(), 3, "the bypassed width is not processed");
        assert!(master.inputs.contains(&r.strip("beat").unwrap().id));
        let bass = &plan.nodes[1];
        assert_eq!(bass.keys, vec![r.strip("kick").unwrap().id.clone()]);
        let verb = &plan.nodes[pos("verb")];
        assert_eq!(verb.inputs, vec![r.strip("bass").unwrap().id.clone()]);
    }

    #[test]
    fn a_pre_id_project_file_gets_a_rack() {
        // schema 0: no ids anywhere, as written before stable ids
        let old = json!({
            "name": "old", "bpm": 90.0, "key_root": "C", "scale": "minor",
            "tracks": [{"name": "lead", "instrument": {"type": "synth"}, "effects": [{"type": "reverb"}, {"type": "delay"}], "sends": [{"bus": "fx", "db": -6.0}]}],
            "patterns": [{"name": "a", "bars": 1, "clips": {}}],
            "buses": [{"name": "fx", "effects": [{"type": "chorus"}]}]
        });
        let mut p: Project = serde_json::from_value(old).unwrap();
        p.ensure_fx_ids();
        let r = RackState::from_project(&p);
        let lead = r.strip("lead").unwrap();
        assert!(!lead.id.contains(':'), "ids were migrated: {}", lead.id);
        assert_eq!(lead.devices.iter().map(|d| d.type_name.as_str()).collect::<Vec<_>>(), vec!["synth", "reverb", "delay"]);
        assert!(r.routes.iter().any(|x| matches!(x, Route::Send { to, .. } if *to == r.strip("fx").unwrap().id)));
        let ids = r.param_ids();
        let mut u = ids.clone();
        u.sort_unstable();
        u.dedup();
        assert_eq!(u.len(), ids.len());
    }
}
