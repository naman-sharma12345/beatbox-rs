//! Rack state: strips, devices, parameters and routes, derived from a
//! project (see the module docs in `rack/mod.rs`).

use crate::project::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of the [`RackState`] JSON shape.
pub const RACK_SCHEMA: u32 = 1;

/// The whole rack of a project.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RackState {
    pub schema: u32,
    /// Tracks (declaration order), then buses (declaration order), then the master.
    pub strips: Vec<Strip>,
    /// Every edge between strips: outputs, aux sends and sidechain keys.
    pub routes: Vec<Route>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StripKind {
    Track,
    Bus,
    Master,
}

/// One channel strip: a track, a bus or the master.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Strip {
    /// The object's stable project id ("trk3", "bus1") or "master".
    pub id: String,
    pub kind: StripKind,
    pub name: String,
    /// The instrument (tracks only), then every insert in processing order.
    pub devices: Vec<Device>,
    /// Fader-section parameters: volume, pan (not on the master) and one
    /// `send.<bus>` level per aux send.
    pub mixer: Vec<Param>,
    pub mute: bool,
    pub solo: bool,
    /// The strip id this strip's output goes to (None for the master).
    pub output: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Instrument,
    Effect,
}

/// One device on a strip.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Device {
    /// `<strip id>/<effect id>` for an insert ("trk3/reverb1"), or
    /// `<strip id>/instrument` for a track's instrument. Effect ids are
    /// unique within their chain and survive reordering.
    pub id: String,
    pub kind: DeviceKind,
    /// The effect type ("reverb") or the instrument kind ("subtractive").
    pub type_name: String,
    /// Position in the insert chain (None for the instrument).
    pub slot: Option<usize>,
    pub bypassed: bool,
    pub params: Vec<Param>,
}

/// One parameter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Param {
    /// `<strip id>:<path>`, unique in the rack, e.g. "trk3:fx.1.mix",
    /// "bus1:volume", "trk2:instrument.cutoff".
    pub id: String,
    /// The strip-relative path every tool and automation lane uses
    /// (`set_parameters`, `add_automation`): "fx.1.mix", "volume".
    pub path: String,
    /// number | bool | enum | string | array
    pub kind: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    /// The value a fresh device (or strip) starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Display unit ("Hz", "dB", "ms", "0..1").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// How a knob or an automation curve maps onto the range: "linear", or
    /// "log" (equal travel per octave: frequencies). Numbers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub automatable: bool,
    /// Ids of the automation lanes that drive this parameter (the hook an
    /// automation editor or a model attaches to).
    #[serde(default)]
    pub lanes: Vec<String>,
}

/// Every way a rack edit can be refused. Serialized as the variant name
/// ("ParamOutOfRange"); these strings are stable: a client may match on
/// them, so a variant is never renamed or reused for another meaning.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RackErrorCode {
    /// No strip (track, bus, master) by that id or name.
    NodeNotFound,
    /// No device by that id.
    DeviceNotFound,
    /// No parameter by that id (or device + name).
    ParamNotFound,
    /// No effect type by that name.
    DeviceTypeNotFound,
    /// The device is an instrument; only inserts move, go or bypass.
    NotAnInsert,
    /// A number outside the parameter's range.
    ParamOutOfRange,
    /// A value of the wrong JSON type for the parameter.
    ParamWrongType,
    /// A string that is not one of the parameter's options.
    ParamNotAnOption,
    /// A route the graph does not allow (into a track, out of the master).
    InvalidRouting,
    /// A route that would feed a bus back into itself.
    RoutingCycle,
    /// A send that does not go from a track to a bus.
    InvalidSend,
    /// A chain position past its end.
    PositionOutOfRange,
    /// A command that does not parse, or asks for something no strip has.
    InvalidCommand,
    /// The project changed since the revision the caller read.
    StaleRevision,
    /// The engine refused the edit; its words are in the message.
    EngineRejected,
}

impl RackErrorCode {
    /// The stable string ("ParamOutOfRange").
    pub fn as_str(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default()
    }
}

/// Why a value does not fit a parameter: a machine-readable code, a
/// message, and what would have been accepted.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamMismatch {
    /// ParamOutOfRange | ParamWrongType | ParamNotAnOption
    pub code: RackErrorCode,
    pub message: String,
    pub expected: Value,
}

impl Param {
    /// The parameter as a JSON Schema: what an MCP client validates a value
    /// against and what a GUI builds its widget from (number + range +
    /// scale -> knob or slider, enum -> dropdown, bool -> toggle).
    pub fn json_schema(&self) -> Value {
        let mut s = match self.kind.as_str() {
            "number" => {
                let mut m = serde_json::json!({"type": "number"});
                if let Some(x) = self.min {
                    m["minimum"] = serde_json::json!(x);
                }
                if let Some(x) = self.max {
                    m["maximum"] = serde_json::json!(x);
                }
                if let Some(sc) = &self.scale {
                    m["x-scale"] = serde_json::json!(sc);
                }
                m
            }
            "bool" => serde_json::json!({"type": "boolean"}),
            "enum" => {
                serde_json::json!({"type": "string", "enum": self.options.clone().unwrap_or_default()})
            }
            "array" => serde_json::json!({"type": "array"}),
            _ => serde_json::json!({"type": "string"}),
        };
        if let Some(u) = &self.unit {
            s["x-unit"] = serde_json::json!(u);
        }
        if let Some(d) = &self.default {
            s["default"] = d.clone();
        }
        s
    }

    /// Check a value against the parameter (type, range, options).
    pub fn check(&self, v: &Value) -> Result<(), ParamMismatch> {
        let wrong = |what: &str| ParamMismatch {
            code: RackErrorCode::ParamWrongType,
            message: format!("{} takes {what}, got {v}", self.id),
            expected: self.json_schema(),
        };
        match self.kind.as_str() {
            "number" => {
                let x = v.as_f64().ok_or_else(|| wrong("a number"))? as f32;
                let lo = self.min.unwrap_or(f32::NEG_INFINITY);
                let hi = self.max.unwrap_or(f32::INFINITY);
                if !x.is_finite() || x < lo || x > hi {
                    return Err(ParamMismatch {
                        code: RackErrorCode::ParamOutOfRange,
                        message: format!(
                            "{} = {x} is outside {lo}..{hi}{}",
                            self.id,
                            self.unit
                                .as_deref()
                                .map(|u| format!(" {u}"))
                                .unwrap_or_default()
                        ),
                        expected: serde_json::json!({"min": lo, "max": hi, "unit": self.unit}),
                    });
                }
            }
            "bool" => {
                v.as_bool().ok_or_else(|| wrong("true or false"))?;
            }
            "enum" => {
                let x = v.as_str().ok_or_else(|| wrong("one of its options"))?;
                let opts = self.options.clone().unwrap_or_default();
                if !opts.iter().any(|o| o.eq_ignore_ascii_case(x)) {
                    return Err(ParamMismatch {
                        code: RackErrorCode::ParamNotAnOption,
                        message: format!("{} has no option '{x}'", self.id),
                        expected: serde_json::json!({"options": opts}),
                    });
                }
            }
            "array" => {
                v.as_array().ok_or_else(|| wrong("an array"))?;
            }
            _ => {
                v.as_str().ok_or_else(|| wrong("a string"))?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    /// The strip's whole post-fader signal goes to `to`.
    Output { from: String, to: String },
    /// An aux send (`id` is the send's stable id).
    Send {
        id: String,
        from: String,
        to: String,
        db: f32,
        pre_fader: bool,
    },
    /// A device on `to` listens to `from` as its key (a sidechain).
    Sidechain {
        from: String,
        to: String,
        device: String,
    },
}

impl Route {
    pub fn from(&self) -> &str {
        match self {
            Route::Output { from, .. }
            | Route::Send { from, .. }
            | Route::Sidechain { from, .. } => from,
        }
    }
    pub fn to(&self) -> &str {
        match self {
            Route::Output { to, .. } | Route::Send { to, .. } | Route::Sidechain { to, .. } => to,
        }
    }
}

fn strip_id(id: &str, kind: &str, name: &str) -> String {
    if id.is_empty() {
        format!("{kind}:{}", name.to_lowercase())
    } else {
        id.to_string()
    }
}

/// Strip id of a track or bus name ("master" for the master or an unknown
/// name, which the renderer also sends to the master).
fn resolve_strip(p: &Project, key: Option<&str>) -> String {
    match key {
        None => "master".into(),
        Some(k) if k.eq_ignore_ascii_case("master") => "master".into(),
        Some(k) => {
            if let Ok(i) = p.bus_index(k) {
                strip_id(&p.buses[i].id, "bus", &p.buses[i].name)
            } else if let Ok(i) = p.track_index(k) {
                strip_id(&p.tracks[i].id, "track", &p.tracks[i].name)
            } else {
                "master".into()
            }
        }
    }
}

/// Every string under a "source" key in a JSON value (a sidechain or keyed
/// dynamic EQ listening to a track).
fn sources(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "source" {
                    if let Value::String(s) = x {
                        if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
                            out.push(s.clone());
                        }
                    }
                }
                sources(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| sources(x, out)),
        _ => {}
    }
}

fn param_from_desc(
    strip: &str,
    owner: &str,
    prefix: &str,
    d: &Value,
    defaults: &[Value],
    p: &Project,
) -> Option<Param> {
    let rel = d["param"].as_str()?;
    if rel == "id" || rel == "bypass" {
        return None;
    }
    let path = if prefix.is_empty() {
        rel.to_string()
    } else {
        format!("{prefix}.{rel}")
    };
    let kind = d["type"].as_str().unwrap_or("number").to_string();
    let f = |k: &str| d.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
    let value = d.get("value").cloned().unwrap_or(Value::Null);
    let default = defaults
        .iter()
        .find(|x| x["param"].as_str() == Some(rel))
        .and_then(|x| x.get("value").cloned());
    let number = kind == "number";
    // the range always holds the current and the default value
    let widen = |r: Option<f32>, lo: bool| {
        r.map(|mut b| {
            for v in [value.as_f64(), default.as_ref().and_then(|x| x.as_f64())]
                .into_iter()
                .flatten()
            {
                b = if lo { b.min(v as f32) } else { b.max(v as f32) };
            }
            b
        })
    };
    Some(Param {
        id: format!("{strip}:{path}"),
        lanes: lanes_of(p, owner, &path),
        min: widen(f("min"), true),
        max: widen(f("max"), false),
        value,
        default,
        unit: d
            .get("unit")
            .and_then(|x| x.as_str())
            .filter(|u| !u.is_empty())
            .map(String::from),
        scale: number.then(|| {
            if d.get("log_scale")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                "log".to_string()
            } else {
                "linear".to_string()
            }
        }),
        kind,
        options: d.get("options").and_then(|x| x.as_array()).map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        }),
        automatable: d
            .get("automatable")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        path,
    })
}

fn lanes_of(p: &Project, owner: &str, path: &str) -> Vec<String> {
    let canon = crate::automation::canonical(path).unwrap_or_else(|| path.to_string());
    p.automation
        .iter()
        .filter(|l| {
            l.target.eq_ignore_ascii_case(owner)
                && crate::automation::canonical(&l.param)
                    .unwrap_or_else(|| l.param.clone())
                    .eq_ignore_ascii_case(&canon)
        })
        .map(|l| l.id.clone())
        .collect()
}

fn number_param(
    strip: &str,
    owner: &str,
    path: &str,
    value: f32,
    range: (f32, f32),
    default: f32,
    unit: &str,
    p: &Project,
) -> Param {
    Param {
        id: format!("{strip}:{path}"),
        path: path.to_string(),
        kind: "number".into(),
        value: serde_json::json!(value),
        min: Some(range.0.min(value)),
        max: Some(range.1.max(value)),
        default: Some(serde_json::json!(default)),
        unit: Some(unit.into()),
        scale: Some("linear".into()),
        options: None,
        automatable: true,
        lanes: lanes_of(p, owner, path),
    }
}

fn devices_of(
    p: &Project,
    strip: &str,
    owner: &str,
    instrument: Option<&crate::instruments::Instrument>,
    chain: &[crate::fx::Effect],
) -> Vec<Device> {
    let mut out = Vec::new();
    if let Some(inst) = instrument {
        let kind = inst.kind_name();
        let mut desc = Vec::new();
        if let Ok(v) = serde_json::to_value(inst) {
            crate::tools_sound::describe_value(kind, "", &v, &mut desc);
        }
        let mut defaults = Vec::new();
        if let Ok(fresh) = serde_json::from_value::<crate::instruments::Instrument>(
            serde_json::json!({"type": kind}),
        ) {
            if let Ok(v) = serde_json::to_value(&fresh) {
                crate::tools_sound::describe_value(kind, "", &v, &mut defaults);
            }
        }
        out.push(Device {
            id: format!("{strip}/instrument"),
            kind: DeviceKind::Instrument,
            type_name: kind.to_string(),
            slot: None,
            bypassed: false,
            params: desc
                .iter()
                .filter_map(|d| param_from_desc(strip, owner, "instrument", d, &defaults, p))
                .collect(),
        });
    }
    for (i, fx) in chain.iter().enumerate() {
        let kind = fx.type_name();
        let mut desc = Vec::new();
        if let Ok(v) = serde_json::to_value(fx) {
            crate::tools_sound::describe_value(&kind, "", &v, &mut desc);
        }
        let mut defaults = Vec::new();
        if let Some(fresh) = crate::fx::Effect::from_type(&kind) {
            if let Ok(v) = serde_json::to_value(&fresh) {
                crate::tools_sound::describe_value(&kind, "", &v, &mut defaults);
            }
        }
        let fid = if fx.id().is_empty() {
            format!("{kind}@{i}")
        } else {
            fx.id().to_string()
        };
        out.push(Device {
            id: format!("{strip}/{fid}"),
            kind: DeviceKind::Effect,
            type_name: kind,
            slot: Some(i),
            bypassed: fx.bypassed(),
            params: desc
                .iter()
                .filter_map(|d| param_from_desc(strip, owner, &format!("fx.{i}"), d, &defaults, p))
                .collect(),
        });
    }
    out
}

impl RackState {
    /// The rack of a project. Pure: reads the project, changes nothing.
    pub fn from_project(p: &Project) -> RackState {
        let mut strips = Vec::new();
        let mut routes = Vec::new();
        let vol = (-60.0, 12.0);
        let pan = (-1.0, 1.0);
        for t in &p.tracks {
            let sid = strip_id(&t.id, "track", &t.name);
            let mut mixer = vec![
                number_param(&sid, &t.name, "volume", t.volume_db, vol, 0.0, "dB", p),
                number_param(&sid, &t.name, "pan", t.pan, pan, 0.0, "-1..1", p),
            ];
            for s in &t.sends {
                let path = format!("send.{}", s.bus.to_lowercase());
                mixer.push(number_param(
                    &sid, &t.name, &path, s.db, vol, -12.0, "dB", p,
                ));
                routes.push(Route::Send {
                    id: s.id.clone(),
                    from: sid.clone(),
                    to: resolve_strip(p, Some(&s.bus)),
                    db: s.db,
                    pre_fader: s.pre_fader,
                });
            }
            let output = resolve_strip(p, t.output.as_deref());
            routes.push(Route::Output {
                from: sid.clone(),
                to: output.clone(),
            });
            strips.push(Strip {
                devices: devices_of(p, &sid, &t.name, Some(&t.instrument), &t.effects),
                id: sid,
                kind: StripKind::Track,
                name: t.name.clone(),
                mixer,
                mute: t.mute,
                solo: t.solo,
                output: Some(output),
            });
        }
        for b in &p.buses {
            let sid = strip_id(&b.id, "bus", &b.name);
            let output = resolve_strip(p, b.output.as_deref());
            routes.push(Route::Output {
                from: sid.clone(),
                to: output.clone(),
            });
            strips.push(Strip {
                devices: devices_of(p, &sid, &b.name, None, &b.effects),
                mixer: vec![
                    number_param(&sid, &b.name, "volume", b.volume_db, vol, 0.0, "dB", p),
                    number_param(&sid, &b.name, "pan", b.pan, pan, 0.0, "-1..1", p),
                ],
                id: sid,
                kind: StripKind::Bus,
                name: b.name.clone(),
                mute: b.mute,
                solo: false,
                output: Some(output),
            });
        }
        strips.push(Strip {
            id: "master".into(),
            kind: StripKind::Master,
            name: "master".into(),
            devices: devices_of(p, "master", "master", None, &p.master_effects),
            mixer: vec![number_param(
                "master",
                "master",
                "volume",
                p.master_volume_db,
                vol,
                0.0,
                "dB",
                p,
            )],
            mute: false,
            solo: false,
            output: None,
        });
        // sidechain keys: a device listening to another track
        let mut keyed = Vec::new();
        let nt = p.tracks.len();
        for (si, s) in strips.iter().enumerate() {
            let chain: &[crate::fx::Effect] = match s.kind {
                StripKind::Master => &p.master_effects,
                StripKind::Track => &p.tracks[si].effects,
                StripKind::Bus => &p.buses[si - nt].effects,
            };
            for d in &s.devices {
                let Some(fx) = d.slot.and_then(|i| chain.get(i)) else {
                    continue;
                };
                let mut src = Vec::new();
                if let Ok(v) = serde_json::to_value(fx) {
                    sources(&v, &mut src);
                }
                for name in src {
                    if let Ok(ti) = p.track_index(&name) {
                        keyed.push(Route::Sidechain {
                            from: strip_id(&p.tracks[ti].id, "track", &p.tracks[ti].name),
                            to: s.id.clone(),
                            device: d.id.clone(),
                        });
                    }
                }
            }
        }
        routes.extend(keyed);
        RackState {
            schema: RACK_SCHEMA,
            strips,
            routes,
        }
    }

    pub fn strip(&self, key: &str) -> Option<&Strip> {
        self.strips
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(key))
            .or_else(|| {
                self.strips
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(key))
            })
    }

    pub fn device(&self, id: &str) -> Option<(&Strip, &Device)> {
        self.strips.iter().find_map(|s| {
            s.devices
                .iter()
                .find(|d| d.id.eq_ignore_ascii_case(id))
                .map(|d| (s, d))
        })
    }

    /// A parameter by its rack id ("trk3:fx.1.mix").
    pub fn param(&self, id: &str) -> Option<(&Strip, &Param)> {
        self.strips.iter().find_map(|s| {
            s.mixer
                .iter()
                .chain(s.devices.iter().flat_map(|d| d.params.iter()))
                .find(|x| x.id.eq_ignore_ascii_case(id))
                .map(|x| (s, x))
        })
    }

    /// Every parameter id in the rack (for uniqueness checks and listings).
    pub fn param_ids(&self) -> Vec<&str> {
        self.strips
            .iter()
            .flat_map(|s| {
                s.mixer
                    .iter()
                    .chain(s.devices.iter().flat_map(|d| d.params.iter()))
            })
            .map(|x| x.id.as_str())
            .collect()
    }
}
