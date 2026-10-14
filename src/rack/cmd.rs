//! The rack command API: one surface over the rack state that the studio
//! GUI and the MCP server both drive (`rack_get`, `rack_apply`).
//!
//! A command names strips, devices and parameters by their rack ids
//! ([`super::model`]), or by names where that is natural ("bass",
//! "master"). `rack_apply` runs a list of commands as one transaction: all
//! of them or none, one undo step, one revision. Each command is carried
//! out by the engine's existing tools (add_effect, move_effect,
//! set_parameters, route_track, ...), so validation, stable ids,
//! automation-lane upkeep and change lists are the same whichever surface
//! made the edit.

use super::model::{DeviceKind, Param, ParamMismatch, RackErrorCode, RackState, StripKind};
use crate::engine::Engine;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// What went wrong, with the context each kind of failure needs. The
/// variant name is the stable code (serialized as `"code": "<Variant>"`,
/// see [`RackErrorCode`]); its fields sit beside it in the same object, so
/// an MCP client matches on `code` and reads the fields it names.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "code")]
pub enum RackErrorKind {
    /// No strip (track, bus, master) by that id or name.
    NodeNotFound { node_id: String },
    /// No device by that id. `node_id` is the strip part of the id.
    DeviceNotFound { node_id: String, device_id: String },
    /// No parameter by that id (or device + name) on the strip.
    ParamNotFound { node_id: String, param_id: String },
    /// A number outside the parameter's range.
    ParamOutOfRange { param_id: String, val: f64, min: f64, max: f64 },
    /// A value of the wrong JSON type: `expected` and `received` are type
    /// names ("number", "string", "bool", "array", "object", "null").
    ParamWrongType { param_id: String, expected: String, received: String },
    /// A string that is not one of an enum parameter's options.
    ParamNotAnOption { param_id: String, received: String, options: Vec<String> },
    /// No effect type by that name.
    DeviceTypeNotFound { type_name: String, options: Vec<String> },
    /// The device is an instrument; only inserts move, go or bypass.
    NotAnInsert { node_id: String, device_id: String },
    /// A route the graph does not allow (into a track, out of the master).
    InvalidRouting { source: String, target: String, reason: String },
    /// A route that would feed a bus back into itself: the strip ids
    /// around the loop, starting and ending at the strip being routed.
    RoutingCycle { cycle_path: Vec<String> },
    /// A send that does not go from a track to a bus.
    InvalidSend { source: String, target: String, reason: String },
    /// A chain position past its end (`min..=max` are the valid ones).
    PositionOutOfRange { node_id: String, position: usize, min: usize, max: usize },
    /// The project changed since the revision the caller read.
    StaleRevision { expected: u64, actual: u64 },
    /// A command that does not parse, or asks for something the strip
    /// does not have (`field` is the offending field, when there is one).
    InvalidCommand {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        reason: String,
    },
    /// An engine tool refused the edit; `reason` carries its words, with
    /// the strip / device / parameter the command named.
    EngineRejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        param_id: Option<String>,
        reason: String,
    },
}

impl RackErrorKind {
    /// The stable code of this kind.
    pub fn code(&self) -> RackErrorCode {
        match self {
            RackErrorKind::NodeNotFound { .. } => RackErrorCode::NodeNotFound,
            RackErrorKind::DeviceNotFound { .. } => RackErrorCode::DeviceNotFound,
            RackErrorKind::ParamNotFound { .. } => RackErrorCode::ParamNotFound,
            RackErrorKind::ParamOutOfRange { .. } => RackErrorCode::ParamOutOfRange,
            RackErrorKind::ParamWrongType { .. } => RackErrorCode::ParamWrongType,
            RackErrorKind::ParamNotAnOption { .. } => RackErrorCode::ParamNotAnOption,
            RackErrorKind::DeviceTypeNotFound { .. } => RackErrorCode::DeviceTypeNotFound,
            RackErrorKind::NotAnInsert { .. } => RackErrorCode::NotAnInsert,
            RackErrorKind::InvalidRouting { .. } => RackErrorCode::InvalidRouting,
            RackErrorKind::RoutingCycle { .. } => RackErrorCode::RoutingCycle,
            RackErrorKind::InvalidSend { .. } => RackErrorCode::InvalidSend,
            RackErrorKind::PositionOutOfRange { .. } => RackErrorCode::PositionOutOfRange,
            RackErrorKind::StaleRevision { .. } => RackErrorCode::StaleRevision,
            RackErrorKind::InvalidCommand { .. } => RackErrorCode::InvalidCommand,
            RackErrorKind::EngineRejected { .. } => RackErrorCode::EngineRejected,
        }
    }
}

/// A machine-readable rack_apply failure. Serialized flat:
/// `{"code": "ParamOutOfRange", "param_id": "trk2:fx.1.cutoff", "val": 50000,
/// "min": 20, "max": 20000, "path": "commands[1].value", "index": 1,
/// "message": "...", "accepts": {"unit": "Hz", "scale": "log"}}`. MCP
/// clients read `code` and the kind's fields and correct the call; people
/// read `message`. Its Display is `{"rack_error": {...}}`, so it survives
/// every error channel (an MCP tool error's text, the studio's error line)
/// intact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RackError {
    /// The code and its per-kind fields.
    #[serde(flatten)]
    pub kind: RackErrorKind,
    /// Where in the call: "commands[2].param", "expected_revision".
    pub path: String,
    pub message: String,
    /// The command's index in `commands` (absent for call-level errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// A hint for the retry: the ids that exist ({one_of: [...]}), the
    /// unit and scale of a range, the parameter's JSON schema, ...
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub accepts: Value,
}

impl RackError {
    pub fn code(&self) -> RackErrorCode {
        self.kind.code()
    }
    fn accepts(mut self, v: Value) -> Self {
        self.accepts = v;
        self
    }
}

impl std::fmt::Display for RackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::to_string(&json!({"rack_error": self})).unwrap_or_else(|_| self.message.clone()))
    }
}

impl std::error::Error for RackError {}

fn err(kind: RackErrorKind, field: &str, message: String) -> RackError {
    RackError { kind, path: field.into(), message, index: None, accepts: Value::Null }
}

fn fail<T>(e: RackError) -> Result<T> {
    Err(e.into())
}

/// The JSON type name of a value ("number", "string", ...).
fn json_type(v: &Value) -> String {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .into()
}

/// A value that does not fit a parameter -> the kind for it.
fn mismatch(p: &Param, m: ParamMismatch, v: &Value, field: &str) -> RackError {
    let kind = match m.code {
        RackErrorCode::ParamOutOfRange => RackErrorKind::ParamOutOfRange {
            param_id: p.id.clone(),
            val: v.as_f64().unwrap_or(f64::NAN),
            min: p.min.map(f64::from).unwrap_or(f64::NEG_INFINITY),
            max: p.max.map(f64::from).unwrap_or(f64::INFINITY),
        },
        RackErrorCode::ParamNotAnOption => RackErrorKind::ParamNotAnOption {
            param_id: p.id.clone(),
            received: v.as_str().unwrap_or_default().to_string(),
            options: p.options.clone().unwrap_or_default(),
        },
        _ => RackErrorKind::ParamWrongType { param_id: p.id.clone(), expected: p.kind.clone(), received: json_type(v) },
    };
    let accepts = match m.code {
        RackErrorCode::ParamOutOfRange => json!({"unit": p.unit, "scale": p.scale}),
        RackErrorCode::ParamNotAnOption => Value::Null,
        _ => m.expected,
    };
    err(kind, field, m.message).accepts(accepts)
}

/// The strip part of a device id ("trk2/reverb1" -> "trk2").
fn node_of_device(id: &str) -> String {
    id.split('/').next().unwrap_or_default().to_string()
}

/// The strip part of a parameter id ("trk2:fx.0.mix" -> "trk2").
fn node_of_param(id: &str) -> String {
    id.split(':').next().unwrap_or_default().to_string()
}

/// The RackError inside an error message (for clients and tests).
pub fn parse_error(text: &str) -> Option<RackError> {
    let start = text.find("{\"rack_error\"")?;
    // the object may be followed by more text (an error chain)
    let v: Value = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>().next()?.ok()?;
    serde_json::from_value(v["rack_error"].clone()).ok()
}

/// One rack edit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RackCommand {
    /// Insert an effect on a strip; `position` (0-based) defaults to the end.
    AddDevice {
        strip: String,
        #[serde(rename = "type")]
        type_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<usize>,
    },
    /// Remove an insert ("trk3/reverb1").
    RemoveDevice { device: String },
    /// Move an insert to `position` in its chain.
    MoveDevice { device: String, position: usize },
    /// Bypass (true), enable (false) or toggle (omitted) an insert.
    Bypass {
        device: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on: Option<bool>,
    },
    /// Set one parameter: `param` is a rack id ("trk3:fx.0.mix"), or give
    /// `device` + `name` ("trk3/reverb1" + "mix", which survives reordering).
    SetParam {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        param: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        value: Value,
    },
    /// Mute / solo a strip.
    SetStrip {
        strip: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mute: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        solo: Option<bool>,
    },
    /// Send a track's or bus's output to a bus or "master".
    Route { strip: String, to: String },
    /// Create, change or (remove: true) delete an aux send.
    Send {
        strip: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pre_fader: Option<bool>,
        #[serde(default)]
        remove: bool,
    },
    /// Create a bus (preset: none by default: an empty strip).
    AddBus {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    RemoveBus { strip: String },
}

impl RackCommand {
    pub fn op(&self) -> &'static str {
        match self {
            RackCommand::AddDevice { .. } => "add_device",
            RackCommand::RemoveDevice { .. } => "remove_device",
            RackCommand::MoveDevice { .. } => "move_device",
            RackCommand::Bypass { .. } => "bypass",
            RackCommand::SetParam { .. } => "set_param",
            RackCommand::SetStrip { .. } => "set_strip",
            RackCommand::Route { .. } => "route",
            RackCommand::Send { .. } => "send",
            RackCommand::AddBus { .. } => "add_bus",
            RackCommand::RemoveBus { .. } => "remove_bus",
        }
    }
}

/// A strip key (rack id or name) -> the name the engine tools take.
fn strip_name(r: &RackState, key: &str, field: &str) -> Result<(String, StripKind)> {
    let s = r.strip(key).ok_or_else(|| {
        err(RackErrorKind::NodeNotFound { node_id: key.to_string() }, field, format!("no strip '{key}'"))
            .accepts(json!({"one_of": r.strips.iter().map(|s| json!({"id": s.id, "name": s.name})).collect::<Vec<_>>()}))
    })?;
    Ok((s.name.clone(), s.kind))
}

/// The rack id of a strip key (the key itself when nothing matches).
fn sid_of(r: &RackState, key: &str) -> String {
    r.strip(key).map(|s| s.id.clone()).unwrap_or_else(|| key.to_string())
}

fn device_ids(r: &RackState) -> Value {
    json!({"one_of": r.strips.iter().flat_map(|s| s.devices.iter().map(|d| d.id.clone())).collect::<Vec<_>>()})
}

fn device_not_found(r: &RackState, device: &str, field: &str) -> RackError {
    err(RackErrorKind::DeviceNotFound { node_id: node_of_device(device), device_id: device.to_string() }, field, format!("no device '{device}'")).accepts(device_ids(r))
}

/// An insert by device id -> (strip name, effect id, slot).
fn insert(r: &RackState, device: &str) -> Result<(String, String, usize)> {
    let (s, d) = r.device(device).ok_or_else(|| device_not_found(r, device, "device"))?;
    if d.kind != DeviceKind::Effect {
        return fail(err(RackErrorKind::NotAnInsert { node_id: s.id.clone(), device_id: device.to_string() }, "device", format!("'{device}' is an instrument; only inserts can be moved, removed or bypassed")).accepts(json!({"kind": "effect"})));
    }
    let slot = d.slot.unwrap_or(0);
    let fx = d.id.rsplit('/').next().unwrap_or_default().to_string();
    Ok((s.name.clone(), fx, slot))
}

fn chain_ids(e: &Engine, owner: &str) -> Vec<String> {
    crate::tools_studio::chain_of(&e.project, owner).iter().map(|f| f.id().to_string()).collect()
}

/// Apply one command. Returns what it did (with the new device id for
/// add_device). Runs inside the caller's transaction.
pub fn apply_one(e: &mut Engine, c: &RackCommand, source: &str) -> Result<Value> {
    let r = RackState::from_project(&e.project);
    let call = |e: &mut Engine, t: &str, a: Value| e.call_nested(t, &a, source);
    match c {
        RackCommand::AddDevice { strip, type_name, params, position } => {
            let (owner, _) = strip_name(&r, strip, "strip")?;
            let sid = r.strip(strip).map(|x| x.id.clone()).unwrap_or_default();
            let Some(fresh) = crate::fx::Effect::from_type(type_name) else {
                return fail(err(
                    RackErrorKind::DeviceTypeNotFound { type_name: type_name.clone(), options: crate::fx::EFFECT_TYPES.iter().map(|t| t.0.to_string()).collect() },
                    "type",
                    format!("no effect type '{type_name}'"),
                ));
            };
            let before = chain_ids(e, &owner);
            if let Some(pos) = position {
                if *pos > before.len() {
                    return fail(err(RackErrorKind::PositionOutOfRange { node_id: sid.clone(), position: *pos, min: 0, max: before.len() }, "position", format!("position {pos} is past the end of '{owner}' ({} inserts)", before.len())));
                }
            }
            if let Some(Value::Object(pm)) = params {
                // each given parameter against the type's spec (defaults)
                let mut probe = crate::project::Project::new("probe", 120.0);
                probe.master_effects = vec![fresh];
                let spec = RackState::from_project(&probe);
                let dev = &spec.strips.last().map(|s| s.devices.clone()).unwrap_or_default();
                for (k, v) in pm {
                    if let Some(p) = dev.first().and_then(|d| d.params.iter().find(|p| p.path == format!("fx.0.{k}"))) {
                        p.check(v).map_err(|m| {
                            let mut x = mismatch(p, m, v, &format!("params.{k}"));
                            x.message = x.message.replace("master:fx.0.", "");
                            // the spec came from a probe on the master: name the new device's param by its strip
                            match &mut x.kind {
                                RackErrorKind::ParamOutOfRange { param_id, .. } | RackErrorKind::ParamWrongType { param_id, .. } | RackErrorKind::ParamNotAnOption { param_id, .. } => *param_id = format!("{sid}:fx.{}.{k}", position.unwrap_or(before.len())),
                                _ => {}
                            }
                            x
                        })?;
                    }
                }
            } else if params.as_ref().is_some_and(|p| !p.is_null()) {
                let reason = format!("params must be an object {{name: value}}, got {}", json_type(params.as_ref().unwrap_or(&Value::Null)));
                return fail(err(RackErrorKind::InvalidCommand { node_id: Some(sid.clone()), field: Some("params".into()), reason: reason.clone() }, "params", reason).accepts(json!({"type": "object"})));
            }
            let mut a = json!({"track": owner, "type": type_name});
            if let Some(p) = params {
                a["params"] = p.clone();
            }
            call(e, "add_effect", a)?;
            let after = chain_ids(e, &owner);
            let new = after.iter().find(|i| !before.contains(i)).cloned().ok_or_else(|| anyhow!("add_effect added nothing"))?;
            if let Some(pos) = position {
                if *pos + 1 < after.len() {
                    // reorder_effects keeps automation lanes on their effects
                    call(e, "reorder_effects", json!({"track": owner, "from": after.len() - 1, "to": *pos}))?;
                }
            }
            let sid = RackState::from_project(&e.project).strip(&owner).map(|s| s.id.clone()).unwrap_or_default();
            Ok(json!({"op": c.op(), "device": format!("{sid}/{new}")}))
        }
        RackCommand::RemoveDevice { device } => {
            let (owner, fx, _) = insert(&r, device)?;
            call(e, "remove_effect", json!({"track": owner, "index": fx}))?;
            Ok(json!({"op": c.op(), "device": device}))
        }
        RackCommand::MoveDevice { device, position } => {
            let (owner, _, slot) = insert(&r, device)?;
            let n = chain_ids(e, &owner).len();
            if *position >= n {
                return fail(err(RackErrorKind::PositionOutOfRange { node_id: node_of_device(device), position: *position, min: 0, max: n.saturating_sub(1) }, "position", format!("position {position} is past the end of '{owner}' ({n} inserts)")).accepts(json!({"device": device})));
            }
            if slot != *position {
                // reorder_effects keeps automation lanes on their effects
                call(e, "reorder_effects", json!({"track": owner, "from": slot, "to": *position}))?;
            }
            Ok(json!({"op": c.op(), "device": device, "position": position}))
        }
        RackCommand::Bypass { device, on } => {
            let (owner, fx, _) = insert(&r, device)?;
            let mut a = json!({"track": owner, "index": fx});
            if let Some(b) = on {
                a["on"] = json!(b);
            }
            call(e, "bypass_effect", a)?;
            Ok(json!({"op": c.op(), "device": device}))
        }
        RackCommand::SetParam { param, device, name, value } => {
            let (field, spec) = match (param, device, name) {
                (Some(pid), _, _) => {
                    let found = r.param(pid).ok_or_else(|| {
                        err(RackErrorKind::ParamNotFound { node_id: node_of_param(pid), param_id: pid.clone() }, "param", format!("no parameter '{pid}'"))
                            .accepts(json!({"hint": "rack_get lists every parameter id, e.g. trk2:fx.0.mix"}))
                    })?;
                    ("param", found)
                }
                (None, Some(dev), Some(n)) => {
                    let (s, d) = r.device(dev).ok_or_else(|| device_not_found(&r, dev, "device"))?;
                    let prefix = match d.slot {
                        Some(i) => format!("fx.{i}."),
                        None => "instrument.".into(),
                    };
                    let want = format!("{prefix}{n}");
                    let p = d.params.iter().find(|p| p.path.eq_ignore_ascii_case(&want)).ok_or_else(|| {
                        err(RackErrorKind::ParamNotFound { node_id: s.id.clone(), param_id: format!("{}:{want}", s.id) }, "name", format!("{dev} has no parameter '{n}'"))
                            .accepts(json!({"device": dev, "one_of": d.params.iter().map(|p| p.path.trim_start_matches(prefix.as_str()).to_string()).collect::<Vec<_>>()}))
                    })?;
                    ("name", (s, p))
                }
                _ => return fail(err(RackErrorKind::InvalidCommand { node_id: None, field: Some("param".into()), reason: "set_param needs `param` (a rack parameter id) or `device` + `name`".into() }, "param", "set_param needs `param` (a rack parameter id) or `device` + `name`".into()).accepts(json!({"one_of": ["param", "device+name"]}))),
            };
            let (s, p) = spec;
            let _ = field;
            p.check(value).map_err(|m| mismatch(p, m, value, "value"))?;
            let (owner, path) = (s.name.clone(), p.path.clone());
            if let Some(bus) = path.strip_prefix("send.") {
                let db = value.as_f64().ok_or_else(|| anyhow!("a send level is a number in dB"))?;
                call(e, "set_send", json!({"track": owner, "bus": bus, "db": db}))?;
            } else if owner.eq_ignore_ascii_case("master") && path == "volume" {
                let db = value.as_f64().ok_or_else(|| anyhow!("volume is a number in dB"))?;
                call(e, "set_mixer", json!({"track": "master", "volume_db": db}))?;
            } else {
                let mut ch = Map::new();
                ch.insert(path.clone(), value.clone());
                call(e, "set_parameters", json!({"track": owner, "changes": ch}))?;
            }
            Ok(json!({"op": c.op(), "strip": owner, "path": path, "value": value}))
        }
        RackCommand::SetStrip { strip, mute, solo } => {
            let (owner, kind) = strip_name(&r, strip, "strip")?;
            if kind == StripKind::Master {
                return fail(err(RackErrorKind::InvalidCommand { node_id: Some("master".into()), field: Some("strip".into()), reason: "the master has no mute or solo".into() }, "strip", "the master has no mute or solo".into()).accepts(json!({"kind": ["track", "bus"]})));
            }
            if solo.is_some() && kind == StripKind::Bus {
                return fail(err(RackErrorKind::InvalidCommand { node_id: Some(sid_of(&r, &owner)), field: Some("solo".into()), reason: "buses have no solo; solo a track".into() }, "solo", "buses have no solo; solo a track".into()).accepts(json!({"kind": "track"})));
            }
            let mut a = json!({"track": owner});
            if let Some(m) = mute {
                a["mute"] = json!(m);
            }
            if let Some(s) = solo {
                a["solo"] = json!(s);
            }
            call(e, "set_mixer", a)?;
            Ok(json!({"op": c.op(), "strip": owner}))
        }
        RackCommand::Route { strip, to } => {
            let (owner, kind) = strip_name(&r, strip, "strip")?;
            let (dest, dkind) = strip_name(&r, to, "to")?;
            let buses = || json!({"one_of": r.strips.iter().filter(|s| s.kind != StripKind::Track).map(|s| s.id.clone()).collect::<Vec<_>>()});
            if dkind == StripKind::Track {
                let reason = format!("'{dest}' is a track; route to a bus or the master");
                return fail(err(RackErrorKind::InvalidRouting { source: sid_of(&r, &owner), target: sid_of(&r, &dest), reason: reason.clone() }, "to", reason).accepts(buses()));
            }
            match kind {
                StripKind::Track => call(e, "route_track", json!({"track": owner, "bus": dest}))?,
                StripKind::Bus => {
                    // a bus may not feed itself, directly or through others
                    if dkind == StripKind::Bus {
                        let mut cur = Some(dest.clone());
                        let mut hops = 0;
                        // the loop as strip ids: owner -> dest -> ... -> owner
                        let mut cycle = vec![sid_of(&r, &owner)];
                        while let Some(c) = cur {
                            cycle.push(sid_of(&r, &c));
                            if c.eq_ignore_ascii_case(&owner) {
                                return fail(err(RackErrorKind::RoutingCycle { cycle_path: cycle }, "to", format!("routing '{owner}' into '{dest}' makes a loop ('{dest}' already reaches '{owner}')")).accepts(json!({"not": [sid_of(&r, &dest)]})));
                            }
                            hops += 1;
                            cur = if hops > r.strips.len() { None } else { e.project.bus_index(&c).ok().and_then(|i| e.project.buses[i].output.clone()) };
                        }
                    }
                    call(e, "route_bus", json!({"bus": owner, "destination": dest}))?
                }
                StripKind::Master => {
                    return fail(err(RackErrorKind::InvalidRouting { source: "master".into(), target: sid_of(&r, &dest), reason: "the master is the end of the graph".into() }, "strip", "the master is the end of the graph".into()).accepts(json!({"kind": ["track", "bus"]})))
                }
            };
            Ok(json!({"op": c.op(), "strip": owner, "to": dest}))
        }
        RackCommand::Send { strip, to, db, pre_fader, remove } => {
            let (owner, kind) = strip_name(&r, strip, "strip")?;
            if kind != StripKind::Track {
                let reason = format!("sends start on a track ('{owner}' is not one)");
                return fail(err(RackErrorKind::InvalidSend { source: sid_of(&r, &owner), target: sid_of(&r, to), reason: reason.clone() }, "strip", reason).accepts(json!({"kind": "track"})));
            }
            let (bus, bkind) = strip_name(&r, to, "to")?;
            if bkind != StripKind::Bus {
                let reason = format!("a send goes to a bus ('{bus}' is not one)");
                return fail(err(RackErrorKind::InvalidSend { source: sid_of(&r, &owner), target: sid_of(&r, &bus), reason: reason.clone() }, "to", reason).accepts(json!({"kind": "bus"})));
            }
            if let Some(d) = db {
                if !(-60.0..=12.0).contains(d) {
                    return fail(err(RackErrorKind::ParamOutOfRange { param_id: format!("{}:send.{}", sid_of(&r, &owner), bus.to_lowercase()), val: *d as f64, min: -60.0, max: 12.0 }, "db", format!("send level {d} dB is outside -60..12 dB")).accepts(json!({"unit": "dB"})));
                }
            }
            let mut a = json!({"track": owner, "bus": bus});
            if let Some(d) = db {
                a["db"] = json!(d);
            }
            if let Some(p) = pre_fader {
                a["pre_fader"] = json!(p);
            }
            if *remove {
                a["remove"] = json!(true);
            }
            call(e, "set_send", a)?;
            Ok(json!({"op": c.op(), "strip": owner, "to": bus}))
        }
        RackCommand::AddBus { name, preset } => {
            call(e, "add_bus", json!({"name": name, "preset": preset.clone().unwrap_or_else(|| "none".into())}))?;
            let sid = RackState::from_project(&e.project).strip(name).map(|s| s.id.clone()).unwrap_or_default();
            Ok(json!({"op": c.op(), "strip": sid}))
        }
        RackCommand::RemoveBus { strip } => {
            let (name, kind) = strip_name(&r, strip, "strip")?;
            if kind != StripKind::Bus {
                let reason = format!("'{name}' is not a bus");
                return fail(err(RackErrorKind::InvalidCommand { node_id: Some(sid_of(&r, &name)), field: Some("strip".into()), reason: reason.clone() }, "strip", reason).accepts(json!({"kind": "bus"})));
            }
            call(e, "remove_bus", json!({"name": name}))?;
            Ok(json!({"op": c.op(), "strip": strip}))
        }
    }
}

/// Run the parsed commands in order; the first refusal comes back as a
/// RackError naming the command (`commands[i]...`, index i).
fn run_batch(e: &mut Engine, parsed: &[RackCommand], source: &str) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(parsed.len());
    for (i, c) in parsed.iter().enumerate() {
        match apply_one(e, c, source) {
            Ok(r) => out.push(r),
            Err(x) => {
                let mut re = match x.downcast_ref::<RackError>() {
                    Some(r) => r.clone(),
                    None => {
                        // the engine tool said no: pass its words through,
                        // with the strip / device / parameter the command named
                        let r = RackState::from_project(&e.project);
                        let (mut node_id, mut device_id, mut param_id) = (None, None, None);
                        match c {
                            RackCommand::RemoveDevice { device } | RackCommand::MoveDevice { device, .. } | RackCommand::Bypass { device, .. } => {
                                node_id = Some(node_of_device(device));
                                device_id = Some(device.clone());
                            }
                            RackCommand::SetParam { param, device, .. } => {
                                node_id = param.as_deref().map(node_of_param).or_else(|| device.as_deref().map(node_of_device));
                                param_id = param.clone();
                                device_id = device.clone();
                            }
                            RackCommand::AddDevice { strip, .. } | RackCommand::SetStrip { strip, .. } | RackCommand::Route { strip, .. } | RackCommand::Send { strip, .. } | RackCommand::RemoveBus { strip } => {
                                node_id = Some(sid_of(&r, strip))
                            }
                            RackCommand::AddBus { .. } => {}
                        }
                        err(RackErrorKind::EngineRejected { node_id, device_id, param_id, reason: format!("{x:#}") }, "", format!("{x:#}"))
                    }
                };
                re.path = if re.path.is_empty() { format!("commands[{i}]") } else { format!("commands[{i}].{}", re.path) };
                re.index = Some(i);
                re.message = format!("command {i} ({}): {}. Nothing was changed.", c.op(), re.message);
                return fail(re);
            }
        }
    }
    Ok(out)
}

/// Parse, validate and apply a command list (the body of `rack_apply`).
///
/// Two passes. Every command parses first. Pass 1 then runs the whole
/// batch against a staged copy of the project, so each command is checked
/// against the state the commands before it leave (a set_param on a device
/// an earlier add_device made); the copy is thrown away whatever happens,
/// and the engine's log and revision are put back. Only a batch that
/// passes is applied, by pass 2, to the project itself. A refusal in
/// pass 1 leaves the project, the revision and the undo history as they
/// were; pass 2 runs the same commands on the same state, and if the
/// engine still refused one the project is restored as well.
pub fn apply(e: &mut Engine, commands: &Value, source: &str) -> Result<Vec<Value>> {
    let invalid = |field: &str, reason: String| err(RackErrorKind::InvalidCommand { node_id: None, field: Some(field.into()), reason: reason.clone() }, field, reason);
    let list = commands.as_array().ok_or_else(|| invalid("commands", format!("commands must be an array of {{op, ...}}, got {}", json_type(commands))).accepts(json!({"type": "array"})))?;
    if list.is_empty() {
        return fail(invalid("commands", "no commands".into()).accepts(json!({"min_items": 1})));
    }
    // every command parses before anything runs
    let mut parsed = Vec::with_capacity(list.len());
    for (i, raw) in list.iter().enumerate() {
        let c: RackCommand = serde_json::from_value(raw.clone()).map_err(|x| {
            let mut re = err(RackErrorKind::InvalidCommand { node_id: None, field: None, reason: format!("{x}") }, "", format!("{x}")).accepts(json!({"op": OPS}));
            re.path = format!("commands[{i}]");
            re.index = Some(i);
            re
        })?;
        parsed.push(c);
    }
    // pass 1: validate the whole batch on a staged copy
    let live = e.project.clone();
    let (log, revision) = (e.log.clone(), e.revision);
    let checked = run_batch(e, &parsed, source);
    e.project = live;
    e.log = log;
    e.revision = revision;
    checked?;
    // pass 2: mutate
    let before = e.project.clone();
    run_batch(e, &parsed, source).inspect_err(|_| e.project = before)
}

const OPS: [&str; 10] = ["add_device", "remove_device", "move_device", "bypass", "set_param", "set_strip", "route", "send", "add_bus", "remove_bus"];

fn rack_json(e: &Engine, strip: Option<&str>, plan: bool) -> Result<Value> {
    let r = RackState::from_project(&e.project);
    let mut v = json!({"revision": e.revision, "schema": r.schema});
    match strip {
        Some(k) => {
            let s = r.strip(k).ok_or_else(|| anyhow!("no strip '{k}'"))?;
            v["strips"] = json!([s]);
            v["routes"] = json!(r.routes.iter().filter(|x| x.from() == s.id || x.to() == s.id).collect::<Vec<_>>());
        }
        None => {
            v["strips"] = serde_json::to_value(&r.strips)?;
            v["routes"] = serde_json::to_value(&r.routes)?;
        }
    }
    if plan {
        v["plan"] = serde_json::to_value(super::compile::compile_state(&e.project, &r))?;
    }
    Ok(v)
}

fn command_schema() -> Value {
    json!({
        "type": "object",
        "description": "{op, ...}. add_device{strip, type, params?, position?} remove_device{device} move_device{device, position} bypass{device, on?} set_param{param | device+name, value} set_strip{strip, mute?, solo?} route{strip, to} send{strip, to, db?, pre_fader?, remove?} add_bus{name, preset?} remove_bus{strip}",
        "properties": {"op": {"type": "string", "enum": ["add_device", "remove_device", "move_device", "bypass", "set_param", "set_strip", "route", "send", "add_bus", "remove_bus"]}},
        "required": ["op"],
        "additionalProperties": true
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "rack_get",
            description: "The rack: every strip (tracks, buses, master) with its devices (instrument, inserts in order) and their parameters (id, value, min/max, unit, log scale, options, automatable, automation lane ids), the fader section, and every route (output, send, sidechain). plan:true adds the processing order. Ids are what rack_apply takes.",
            mutates: false,
            schema: || crate::tools::obj(json!({
                "strip": {"type": "string", "description": "Only this strip (id or name) and its routes"},
                "plan": {"type": "boolean", "description": "Include the compiled processing plan"}
            }), &[]),
            run: |e, a| rack_json(e, a.get("strip").and_then(|v| v.as_str()), a.get("plan").and_then(|v| v.as_bool()).unwrap_or(false)),
        },
        Tool {
            name: "rack_apply",
            description: "Edit the rack with a list of commands, all or nothing (one undo step): add_device, remove_device, move_device, bypass, set_param, set_strip, route, send, add_bus, remove_bus. E.g. [{op:'add_device', strip:'bass', type:'compressor', position:0}, {op:'set_param', param:'trk2:fx.0.ratio', value:4}]. expected_revision fails the call (StaleRevision) if the project changed since you read it. Errors are structured: {rack_error: {code, <the code's fields>, path, index, message, accepts}}, e.g. ParamOutOfRange{param_id, val, min, max}, StaleRevision{expected, actual}, RoutingCycle{cycle_path}. Returns each command's result and the new revision.",
            mutates: true,
            schema: || crate::tools::obj(json!({
                "commands": {"type": "array", "items": command_schema()},
                "expected_revision": {"type": "integer", "description": "The revision rack_get returned; the call fails with StaleRevision if the project has changed since"}
            }), &["commands"]),
            run: |e, a| {
                if let Some(want) = a.get("expected_revision").and_then(|v| v.as_u64()) {
                    if want != e.revision {
                        return fail(err(RackErrorKind::StaleRevision { expected: want, actual: e.revision }, "expected_revision", format!("the project is at revision {} and you read {want}; read rack_get again", e.revision)));
                    }
                }
                let results = apply(e, &a["commands"], "rack")?;
                Ok(json!({"applied": results.len(), "results": results}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rack::RackState;

    fn studio(tag: &str) -> Engine {
        crate::rack::tests::studio(&format!("cmd_{tag}"))
    }

    fn rack_err(r: Result<Value>) -> RackError {
        let e = r.expect_err("expected a rack error");
        parse_error(&format!("{e:#}")).unwrap_or_else(|| panic!("not a structured error: {e:#}"))
    }

    #[test]
    fn apply_is_one_transaction_and_one_undo_step() {
        let mut e = studio("txn");
        let r0 = RackState::from_project(&e.project);
        let bass = r0.strip("bass").unwrap().id.clone();
        let rev0 = e.revision;
        let out = e
            .call(
                "rack_apply",
                &json!({"expected_revision": rev0, "commands": [
                    {"op": "add_device", "strip": bass, "type": "compressor", "params": {"ratio": 4.0}, "position": 0},
                    {"op": "set_param", "device": format!("{bass}/filter1"), "name": "cutoff", "value": 1200.0},
                    {"op": "set_param", "param": format!("{bass}:volume"), "value": -3.0},
                    {"op": "set_param", "param": format!("{bass}:send.verb"), "value": -18.0},
                    {"op": "bypass", "device": format!("{bass}/sidechain1"), "on": true},
                    {"op": "set_strip", "strip": "kick", "mute": true}
                ]}),
            )
            .unwrap();
        assert_eq!(out["applied"], 6, "{out}");
        assert_eq!(out["results"][0]["device"], format!("{bass}/compressor1"));
        assert_eq!(e.revision, rev0 + 1, "one revision");
        let r = RackState::from_project(&e.project);
        let s = r.strip("bass").unwrap();
        assert_eq!(s.devices.iter().map(|d| d.type_name.as_str()).collect::<Vec<_>>(), vec!["bass808", "compressor", "filter", "sidechain"]);
        assert_eq!(r.param(&format!("{bass}:fx.0.ratio")).unwrap().1.value, json!(4.0));
        assert_eq!(r.param(&format!("{bass}:fx.1.cutoff")).unwrap().1.value, json!(1200.0));
        // the filter's sweep lane moved with it (fx.0 -> fx.1)
        assert_eq!(r.param(&format!("{bass}:fx.1.cutoff")).unwrap().1.lanes.len(), 1);
        assert_eq!(e.project.tracks[1].volume_db, -3.0);
        assert_eq!(e.project.tracks[1].sends[0].db, -18.0);
        assert!(s.devices[3].bypassed);
        assert!(r.strip("kick").unwrap().mute);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(RackState::from_project(&e.project), r0, "one undo restores the whole rack");
    }

    #[test]
    fn a_failing_command_changes_nothing_and_says_where() {
        let mut e = studio("atomic");
        let before = e.project.clone();
        let rev = e.revision;
        let bass = RackState::from_project(&e.project).strip("bass").unwrap().id.clone();
        let x = rack_err(e.call(
            "rack_apply",
            &json!({"commands": [
                {"op": "set_param", "param": format!("{bass}:volume"), "value": -6.0},
                {"op": "remove_device", "device": "nope/reverb9"}
            ]}),
        ));
        assert_eq!((x.code(), x.path.as_str(), x.index), (RackErrorCode::DeviceNotFound, "commands[1].device", Some(1)));
        assert_eq!(x.kind, RackErrorKind::DeviceNotFound { node_id: "nope".into(), device_id: "nope/reverb9".into() });
        assert!(x.accepts["one_of"].as_array().unwrap().iter().any(|d| d == &json!(format!("{bass}/filter1"))), "{:?}", x.accepts);
        assert_eq!(e.project, before, "the first command was rolled back too");
        assert_eq!(e.revision, rev);
    }

    #[test]
    fn a_batch_with_one_bad_command_keeps_nothing() {
        let mut e = studio("nopartial");
        let before = e.project.clone();
        let rev = e.revision;
        let (undo0, _) = e.history();
        let bass = RackState::from_project(&e.project).strip("bass").unwrap().id.clone();
        // three good edits (params, a device, a bus), then a routing cycle
        let x = rack_err(e.call(
            "rack_apply",
            &json!({"commands": [
                {"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": 3000.0},
                {"op": "add_device", "strip": "bass", "type": "chorus"},
                {"op": "add_bus", "name": "late"},
                {"op": "route", "strip": "beat", "to": "drums"}
            ]}),
        ));
        assert_eq!((x.code(), x.path.as_str(), x.index), (RackErrorCode::RoutingCycle, "commands[3].to", Some(3)));
        let id = |n: &str| RackState::from_project(&e.project).strip(n).unwrap().id.clone();
        assert_eq!(x.kind, RackErrorKind::RoutingCycle { cycle_path: vec![id("beat"), id("drums"), id("beat")] });
        assert_eq!(e.project, before, "no partial mutation");
        assert_eq!(e.revision, rev, "no revision bump");
        assert_eq!(e.history().0, undo0, "no undo step");
        // an out-of-range value after good ones: the same
        let x = rack_err(e.call(
            "rack_apply",
            &json!({"commands": [
                {"op": "set_param", "param": format!("{bass}:volume"), "value": -1.0},
                {"op": "set_param", "param": format!("{bass}:fx.0.resonance"), "value": 7.5}
            ]}),
        ));
        assert_eq!(x.code(), RackErrorCode::ParamOutOfRange);
        assert_eq!(x.kind, RackErrorKind::ParamOutOfRange { param_id: format!("{bass}:fx.0.resonance"), val: 7.5, min: 0.0, max: 1.0 });
        assert_eq!(e.project, before);
        assert_eq!(e.revision, rev);
        // a command that does not parse stops the batch before anything runs
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_strip", "strip": "kick", "mute": true}, {"op": "set_param", "value": 1, "colour": "red"}]})));
        assert_eq!((x.code(), x.index), (RackErrorCode::InvalidCommand, Some(1)));
        assert_eq!(e.project, before);
    }

    #[test]
    fn two_applies_on_one_base_revision_the_second_is_stale() {
        let mut e = studio("stale");
        let base = e.call("rack_get", &json!({})).unwrap()["revision"].as_u64().unwrap();
        let bass = RackState::from_project(&e.project).strip("bass").unwrap().id.clone();
        // back to back through Engine::call, the way the studio and a remote client both arrive
        e.call_from("rack_apply", &json!({"expected_revision": base, "commands": [{"op": "set_param", "param": format!("{bass}:volume"), "value": -4.0}]}), "you").unwrap();
        assert_eq!(e.revision, base + 1);
        let after_first = e.project.clone();
        let (undo1, _) = e.history();
        let x = rack_err(e.call_from("rack_apply", &json!({"expected_revision": base, "commands": [{"op": "set_param", "param": format!("{bass}:volume"), "value": -9.0}]}), "mcp"));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::StaleRevision, "expected_revision"));
        assert_eq!(x.kind, RackErrorKind::StaleRevision { expected: base, actual: base + 1 });
        assert_eq!(e.project, after_first, "the stale edit is not applied: state intact");
        assert_eq!(e.revision, base + 1, "no revision bump");
        assert_eq!(e.history().0, undo1, "no undo step");
        // re-read, retry: accepted
        e.call("rack_apply", &json!({"expected_revision": base + 1, "commands": [{"op": "set_param", "param": format!("{bass}:volume"), "value": -9.0}]})).unwrap();
        assert_eq!(e.project.tracks[1].volume_db, -9.0);
    }

    #[test]
    fn values_are_checked_against_the_param_spec() {
        let mut e = studio("range");
        let bass = RackState::from_project(&e.project).strip("bass").unwrap().id.clone();
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": 99999.0}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::ParamOutOfRange, "commands[0].value"));
        assert_eq!(x.kind, RackErrorKind::ParamOutOfRange { param_id: format!("{bass}:fx.0.cutoff"), val: 99999.0, min: 20.0, max: 20000.0 });
        assert_eq!(x.accepts["unit"], json!("Hz"));
        // on the wire the code is its stable name
        let wire = format!("{:#}", e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": 99999.0}]})).unwrap_err());
        assert!(wire.contains("\"code\":\"ParamOutOfRange\""), "{wire}");
        let v: Value = serde_json::from_str(&wire[wire.find('{').unwrap()..]).unwrap();
        let w = &v["rack_error"];
        assert_eq!((w["param_id"].as_str(), w["val"].as_f64(), w["min"].as_f64(), w["max"].as_f64()), (Some(format!("{bass}:fx.0.cutoff").as_str()), Some(99999.0), Some(20.0), Some(20000.0)), "{wire}");
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:fx.0.cutoff"), "value": "loud"}]})));
        assert_eq!(x.kind, RackErrorKind::ParamWrongType { param_id: format!("{bass}:fx.0.cutoff"), expected: "number".into(), received: "string".into() });
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:fx.0.mode"), "value": "wobble"}]})));
        assert_eq!(x.code(), RackErrorCode::ParamNotAnOption);
        match &x.kind {
            RackErrorKind::ParamNotAnOption { received, options, .. } => assert!(received == "wobble" && options.iter().any(|o| o == "lowpass"), "{x:?}"),
            k => panic!("{k:?}"),
        }
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:fx.0.cutof"), "value": 1.0}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::ParamNotFound, "commands[0].param"));
        assert_eq!(x.kind, RackErrorKind::ParamNotFound { node_id: bass.clone(), param_id: format!("{bass}:fx.0.cutof") });
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "add_device", "strip": "bass", "type": "filter", "params": {"cutoff": -5.0}}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::ParamOutOfRange, "commands[0].params.cutoff"));
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "add_device", "strip": "bass", "type": "flanger9000"}]})));
        assert_eq!(x.code(), RackErrorCode::DeviceTypeNotFound);
        assert!(matches!(&x.kind, RackErrorKind::DeviceTypeNotFound { type_name, options } if type_name == "flanger9000" && options.iter().any(|o| o == "compressor")), "{x:?}");
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "set_param", "param": format!("{bass}:send.verb"), "value": 40.0}]})));
        assert_eq!(x.code(), RackErrorCode::ParamOutOfRange);
    }

    #[test]
    fn routing_errors_are_structured() {
        let mut e = studio("route");
        // drums -> beat exists; beat -> drums would loop
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "route", "strip": "beat", "to": "drums"}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::RoutingCycle, "commands[0].to"));
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "route", "strip": "kick", "to": "bass"}]})));
        assert!(matches!(&x.kind, RackErrorKind::InvalidRouting { source, target, .. } if *source == RackState::from_project(&e.project).strip("kick").unwrap().id && *target == RackState::from_project(&e.project).strip("bass").unwrap().id), "{x:?}");
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "route", "strip": "ghost", "to": "master"}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::NodeNotFound, "commands[0].strip"));
        assert_eq!(x.kind, RackErrorKind::NodeNotFound { node_id: "ghost".into() });
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "send", "strip": "beat", "to": "verb"}]})));
        assert_eq!(x.code(), RackErrorCode::InvalidSend);
        let x = rack_err(e.call("rack_apply", &json!({"commands": [{"op": "explode"}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::InvalidCommand, "commands[0]"));
        let x = rack_err(e.call("rack_apply", &json!({"expected_revision": 0, "commands": [{"op": "set_strip", "strip": "kick", "mute": true}]})));
        assert_eq!((x.code(), x.path.as_str()), (RackErrorCode::StaleRevision, "expected_revision"));
        assert_eq!(x.kind, RackErrorKind::StaleRevision { expected: 0, actual: e.revision });
        // valid routing through the API: a new bus, kick into it, a send from bass
        e.call(
            "rack_apply",
            &json!({"commands": [
                {"op": "add_bus", "name": "glue"},
                {"op": "route", "strip": "drums", "to": "glue"},
                {"op": "add_device", "strip": "glue", "type": "compressor"},
                {"op": "send", "strip": "kick", "to": "verb", "db": -20.0}
            ]}),
        )
        .unwrap();
        let r = RackState::from_project(&e.project);
        let glue = r.strip("glue").unwrap();
        assert_eq!(glue.devices.len(), 1);
        assert!(r.routes.contains(&crate::rack::Route::Output { from: r.strip("drums").unwrap().id.clone(), to: glue.id.clone() }));
        let plan = crate::rack::compile(&e.project);
        let pos = |n: &str| plan.nodes.iter().position(|x| x.strip == r.strip(n).unwrap().id).unwrap();
        assert!(pos("drums") < pos("glue"));
        // moving and removing devices by id
        let bass = r.strip("bass").unwrap().id.clone();
        e.call("rack_apply", &json!({"commands": [{"op": "move_device", "device": format!("{bass}/sidechain1"), "position": 0}, {"op": "remove_device", "device": format!("{bass}/filter1")}]})).unwrap();
        let r = RackState::from_project(&e.project);
        assert_eq!(r.strip("bass").unwrap().devices.iter().map(|d| d.type_name.as_str()).collect::<Vec<_>>(), vec!["bass808", "sidechain"]);
        // the filter's lane went with the filter
        assert!(e.project.automation.iter().all(|l| !l.param.starts_with("fx.")), "{:?}", e.project.automation);
    }

    #[test]
    fn rack_get_returns_the_state_the_plan_and_the_revision() {
        let mut e = studio("get");
        let v = e.call("rack_get", &json!({"plan": true})).unwrap();
        assert_eq!(v["revision"], json!(e.revision));
        assert_eq!(v["strips"].as_array().unwrap().len(), 6);
        assert_eq!(v["plan"]["nodes"].as_array().unwrap().len(), 6);
        // the same strips, devices and parameter ids as the state (values are
        // float-cleaned on the way out, like every tool result)
        let back: Vec<crate::rack::Strip> = serde_json::from_value(v["strips"].clone()).unwrap();
        let ids = |s: &[crate::rack::Strip]| s.iter().map(|x| (x.id.clone(), x.devices.iter().map(|d| (d.id.clone(), d.params.iter().map(|p| p.id.clone()).collect::<Vec<_>>())).collect::<Vec<_>>())).collect::<Vec<_>>();
        assert_eq!(ids(&back), ids(&RackState::from_project(&e.project).strips));
        let one = e.call("rack_get", &json!({"strip": "bass"})).unwrap();
        assert_eq!(one["strips"].as_array().unwrap().len(), 1);
        assert!(one["routes"].as_array().unwrap().len() >= 2, "{one}");
        assert!(e.call("rack_get", &json!({"strip": "ghost"})).is_err());
    }
}
