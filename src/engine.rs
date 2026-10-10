//! The engine owns the project, decoded samples and undo history. Every
//! feature is a named tool called through `Engine::call` — the CLI, the
//! desktop studio and MCP clients all go through this one door.

use crate::analysis;
use crate::project::Project;
use crate::render::{self, Mix, RenderOptions};
use crate::samples::SampleBank;
use crate::tools;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub tool: String,
    pub summary: String,
    pub ok: bool,
    pub source: String,
}

/// A named version of the project (A/B variants).
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub project: Project,
    pub note: String,
    /// Engine revision when the snapshot was taken.
    pub revision: u64,
}

/// A transport command for an attached studio (applied on its next frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransportCmd {
    Play,
    Stop,
    Seek(f32),
}

/// Playback state shared with Beatbox Studio over the live link.
#[derive(Clone, Debug, Default)]
pub struct Transport {
    /// True once a studio GUI is driving this engine.
    pub attached: bool,
    pub playing: bool,
    pub position_s: f32,
    pub pending: Vec<TransportCmd>,
}

/// Full renders kept by project hash.
const RENDER_CACHE: usize = 3;

pub struct Engine {
    pub transport: Transport,
    pub project: Project,
    /// Named versions for A/B comparison, in creation order.
    pub snapshots: Vec<(String, Snapshot)>,
    pub bank: SampleBank,
    undo: Vec<Project>,
    redo: Vec<Project>,
    pub workdir: PathBuf,
    /// Bumped on every successful mutation (the GUI watches this).
    pub revision: u64,
    pub log: Vec<LogEntry>,
    cached_mix: Option<(u64, Arc<Mix>)>,
    /// Full renders keyed by project hash (the same project renders to the
    /// same audio), so a tool that renders a candidate and then applies it
    /// does not make the next listener render it again.
    render_cache: Vec<(String, Arc<Mix>)>,
    /// Pre-fader track audio reused across renders that only move faders or
    /// the master (the producer turns it on for its loop; it costs memory).
    pub track_cache: Option<render::TrackCache>,
    /// The same for the ears' masking stem renders (one hook section).
    pub mask_cache: Option<render::TrackCache>,
}

const MAX_UNDO: usize = 100;

impl Engine {
    pub fn new(workdir: PathBuf) -> Self {
        let mut e = Engine {
            transport: Transport::default(),
            project: Project::default(),
            snapshots: Vec::new(),
            bank: SampleBank::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            workdir,
            revision: 0,
            log: Vec::new(),
            cached_mix: None,
            render_cache: Vec::new(),
            track_cache: None,
            mask_cache: None,
        };
        e.project.ensure_fx_ids();
        e
    }

    pub fn samples_dir(&self) -> PathBuf {
        self.workdir.join("beatbox_samples")
    }

    pub fn renders_dir(&self) -> PathBuf {
        self.workdir.join("renders")
    }

    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            self.workdir.join(p)
        }
    }

    /// Call a tool by name with JSON arguments.
    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        self.call_from(name, args, "cli")
    }

    pub fn call_from(&mut self, name: &str, args: &Value, source: &str) -> Result<Value> {
        let tool = match tools::find(name) {
            Some(t) => t,
            None => return Err(unknown_tool(name)),
        };
        let snapshot = if tool.mutates {
            Some(self.project.clone())
        } else {
            None
        };
        // an omitted seed is fresh only on a top-level call (see dispatch_validated)
        let depth = tools::CallDepth::enter();
        let (result, args) = self.dispatch_validated(tool, name, args, depth.top());
        drop(depth);
        let args = &args;
        let mut result = result;
        match &mut result {
            Ok(v) => {
                if let Some(s) = snapshot {
                    if s != self.project {
                        // every mutation reports its state version and an exact change list
                        if let Value::Object(m) = v {
                            if name != "undo" && name != "redo" && !m.contains_key("changes") {
                                let ch = crate::diff::diff(&s, &self.project);
                                let mut list: Vec<Value> = ch
                                    .iter()
                                    .take(12)
                                    .map(|c| Value::String(crate::diff::describe(c)))
                                    .collect();
                                if ch.len() > 12 {
                                    list.push(Value::String(format!(
                                        "... and {} more",
                                        ch.len() - 12
                                    )));
                                }
                                m.insert("changes".into(), Value::Array(list));
                            }
                        }
                        self.undo.push(s);
                        if self.undo.len() > MAX_UNDO {
                            self.undo.remove(0);
                        }
                        self.redo.clear();
                        self.revision += 1;
                    }
                }
                if tool.mutates {
                    if let Value::Object(m) = v {
                        m.insert("revision".into(), Value::from(self.revision));
                    }
                }
            }
            Err(_) => {
                if let Some(s) = snapshot {
                    self.project = s;
                }
            }
        }
        self.push_log(name, args, result.is_ok(), source);
        result
    }

    /// The one validated way into a tool, shared by top-level calls and by
    /// `batch`: unknown-argument rejection, stable effect-id resolution
    /// (against the project as it is *now*, so a batch can refer to an
    /// effect an earlier call added), panic containment, effect-id upkeep
    /// and float cleaning; on a `top` call (a user's call, or a call inside a
    /// user's batch) an omitted seed is fresh (OS entropy + time) and the
    /// chosen seed is returned, while explicit seeds and seeds a tool passes
    /// to the tools it calls stay deterministic. It does NOT snapshot, roll
    /// back, touch undo/redo, the revision or the log: the caller owns the
    /// transaction. Returns the result and the arguments as the tool saw
    /// them (ids resolved, seed filled in).
    pub(crate) fn dispatch_validated(
        &mut self,
        tool: &tools::Tool,
        name: &str,
        args: &Value,
        top: bool,
    ) -> (Result<Value>, Value) {
        let empty = Value::Object(Default::default());
        let args = if args.is_null() { &empty } else { args };
        if let Err(e) = check_unknown_args(tool, name, args) {
            return (Err(e), args.clone());
        }
        let mut resolved = match resolve_effect_refs(&self.project, args) {
            Ok(a) => a,
            Err(e) => return (Err(e), args.clone()),
        };
        let mut fresh_seed = None;
        if top
            && (tool.schema)()["properties"].get("seed").is_some()
            && args.get("seed").is_none_or(|v| v.is_null())
        {
            let s = crate::creative::fresh_seed();
            if let Value::Object(m) = &mut resolved {
                m.insert("seed".into(), Value::from(s));
            }
            fresh_seed = Some(s);
        }
        if top {
            tools::set_seed_fresh(fresh_seed.is_some());
        }
        // a panicking tool must never take the MCP server down with it
        let run = tool.run;
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(self, &resolved)
        })) {
            Ok(r) => r.map(clean_floats),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                Err(anyhow!("internal error in '{name}': {msg}. The project was rolled back; please report this."))
            }
        };
        if result.is_ok() && tool.mutates {
            self.project.ensure_fx_ids();
        }
        let mut result = result;
        if let (Some(s), Ok(Value::Object(m))) = (fresh_seed, &mut result) {
            m.entry("seed").or_insert(Value::from(s));
            m.insert("seed_source".into(), Value::from("fresh"));
        }
        (result, resolved)
    }

    /// A tool call nested inside another tool (`batch`): the same validation
    /// and safeguards as `call_from`, logged with the outer source, and
    /// rolled back on its own failure; the outer call keeps the single undo
    /// step and the revision bump.
    pub(crate) fn call_nested(&mut self, name: &str, args: &Value, source: &str) -> Result<Value> {
        let tool = tools::find(name).ok_or_else(|| unknown_tool(name))?;
        let snapshot = if tool.mutates {
            Some(self.project.clone())
        } else {
            None
        };
        // a call in a user's batch is a user's call: same fresh-seed rule
        let top = tools::CallDepth::current() <= 1;
        let (result, args) = self.dispatch_validated(tool, name, args, top);
        if result.is_err() {
            if let Some(s) = snapshot {
                self.project = s;
            }
        }
        self.push_log(name, &args, result.is_ok(), source);
        result
    }

    fn push_log(&mut self, name: &str, args: &Value, ok: bool, source: &str) {
        self.log.push(LogEntry {
            tool: name.to_string(),
            summary: summarize_args(args),
            ok,
            source: source.to_string(),
        });
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo.pop() {
            let cur = std::mem::replace(&mut self.project, prev);
            self.redo.push(cur);
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop() {
            let cur = std::mem::replace(&mut self.project, next);
            self.undo.push(cur);
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub fn undo_depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    /// Replace the project wholesale (load), keeping undo.
    pub fn replace_project(&mut self, mut p: Project) {
        // migrate legacy projects: positional effects get stable ids
        p.ensure_fx_ids();
        let old = std::mem::replace(&mut self.project, p);
        self.undo.push(old);
        self.redo.clear();
        self.revision += 1;
        let errs = self.bank.sync(&self.project.samples);
        for e in errs {
            self.log.push(LogEntry {
                tool: "load_sample".into(),
                summary: e,
                ok: false,
                source: "engine".into(),
            });
        }
    }

    /// Render the song (cached per revision and by project hash) with
    /// per-track stats.
    pub fn mix(&mut self) -> Result<Arc<Mix>> {
        if let Some((rev, m)) = &self.cached_mix {
            if *rev == self.revision {
                return Ok(m.clone());
            }
        }
        let p = self.project.clone();
        let m = self.render_version(&p)?;
        self.cached_mix = Some((self.revision, m.clone()));
        Ok(m)
    }

    /// Remember a full render of `p` made earlier (same options as `mix`).
    pub fn prime_render(&mut self, p: &Project, m: Arc<Mix>) {
        let key = crate::listen::project_hash(p);
        self.render_cache.retain(|(k, _)| *k != key);
        if self.render_cache.len() >= RENDER_CACHE {
            self.render_cache.remove(0);
        }
        self.render_cache.push((key, m));
    }

    /// Render any project version with per-track stats. Full-quality render;
    /// the last couple of renders are kept by project hash and reused.
    pub fn render_version(&mut self, p: &Project) -> Result<Arc<Mix>> {
        let key = crate::listen::project_hash(p);
        if let Some(i) = self.render_cache.iter().position(|(k, _)| *k == key) {
            let hit = self.render_cache.remove(i);
            let m = hit.1.clone();
            self.render_cache.push(hit);
            return Ok(m);
        }
        self.bank.sync(&p.samples);
        let m = Arc::new(render::render_cached(
            p,
            &self.bank,
            &RenderOptions {
                // measure tracks without holding every stem in RAM (31-track songs)
                track_stats: true,
                ..Default::default()
            },
            self.track_cache.as_mut(),
        )?);
        if self.render_cache.len() >= RENDER_CACHE {
            self.render_cache.remove(0);
        }
        self.render_cache.push((key, m.clone()));
        Ok(m)
    }

    /// Look up a version by name: a snapshot or "current".
    pub fn version(&self, name: &str) -> Result<Project> {
        if name.eq_ignore_ascii_case("current") {
            return Ok(self.project.clone());
        }
        self.snapshots
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, s)| s.project.clone())
            .ok_or_else(|| {
                anyhow!(
                    "no snapshot '{name}'. Snapshots: [{}] (or 'current')",
                    self.snapshots
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    pub fn analyze(&mut self) -> Result<analysis::Report> {
        let m = self.mix()?;
        let mut rep = analysis::analyze(&m);
        // arrangement-aware: don't suggest sections the song already has
        let roles: Vec<&str> = self
            .project
            .song_sections()
            .iter()
            .map(|s| crate::ears::role(&s.pattern))
            .collect();
        let has_break = roles.contains(&"break");
        let has_hook = roles.contains(&"hook");
        for s in rep.suggestions.iter_mut() {
            if s.contains("a breakdown pattern without drums") && (has_break || has_hook) {
                *s = if has_break && has_hook {
                    "Balanced mix. The arrangement already has a breakdown and a hook: check their contrast with analyze_sections (hook should be +1..3 LU over the verse).".into()
                } else if has_break {
                    "Balanced mix. The arrangement has a breakdown; make sure the section after it hits harder (analyze_sections deltas).".into()
                } else {
                    "Balanced mix. Try a breakdown before the last hook for contrast (vary_section / add_pattern copy_from without drums).".into()
                };
            }
        }
        // make suggestions aware of what's already in place
        for s in rep.suggestions.iter_mut() {
            if let Some(rest) = s.strip_prefix('\'') {
                let a = rest.split('\'').next().unwrap_or("").to_string();
                if let Some(o) = s
                    .split("sidechain effect on '")
                    .nth(1)
                    .and_then(|x| x.split('\'').next())
                {
                    let o = o.to_string();
                    let has = self.project.tracks.iter().find(|t| t.name == o).map(|t| {
                        t.effects
                            .iter()
                            .position(|e| matches!(e, crate::fx::Effect::Sidechain(_)))
                    });
                    if let Some(Some(idx)) = has {
                        *s = format!("'{a}' and '{o}' still overlap in the low end. Raise the sidechain amount on '{o}' (tweak_effect index {idx}, amount 0.8+) or lower '{o}' 2 dB.");
                    }
                }
            }
        }
        Ok(rep)
    }
}

fn unknown_tool(name: &str) -> anyhow::Error {
    anyhow!("unknown tool '{name}'. Call list_tools or get_guide to see what's available.")
}

/// Unknown top-level arguments are an error, never silently ignored.
fn check_unknown_args(tool: &tools::Tool, name: &str, args: &Value) -> Result<()> {
    let schema = (tool.schema)();
    if schema["additionalProperties"] != Value::Bool(false) {
        return Ok(());
    }
    if let (Some(props), Some(given)) = (schema["properties"].as_object(), args.as_object()) {
        let bad: Vec<&String> = given.keys().filter(|k| !props.contains_key(*k)).collect();
        if !bad.is_empty() {
            let mut valid: Vec<&String> = props.keys().collect();
            valid.sort();
            return Err(anyhow!(
                "unknown argument(s) {} for '{name}'. Valid: {}",
                bad.iter()
                    .map(|k| format!("'{k}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
                valid
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// Rewrite stable effect ids in tool arguments to chain positions:
/// `index`/`from`/`to`/`effect` given as an id, and `fx.<id>.<param>` in
/// `param`, `target_param`, and the keys of `changes`.
pub fn resolve_effect_refs(p: &Project, args: &Value) -> Result<Value> {
    let mut a = args.clone();
    let owner = ["track", "owner", "target", "bus"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .map(String::from);
    let Some(owner) = owner else { return Ok(a) };
    let Some(chain) = p.chain_of(&owner) else {
        return Ok(a);
    };
    let fix_param = |s: &str| -> Option<String> {
        let mut parts = s.splitn(3, '.');
        let (head, key, rest) = (parts.next()?, parts.next()?, parts.next()?);
        if !(head.eq_ignore_ascii_case("fx") || head.eq_ignore_ascii_case("effect"))
            || key.parse::<usize>().is_ok()
        {
            return None;
        }
        crate::fx::find(chain, key).map(|i| format!("fx.{i}.{rest}"))
    };
    if let Value::Object(m) = &mut a {
        for k in ["index", "from", "to"] {
            if let Some(Value::String(s)) = m.get(k) {
                match crate::fx::find(chain, s).or_else(|| s.trim().parse::<usize>().ok()) {
                    Some(i) => {
                        m.insert(k.into(), Value::from(i));
                    }
                    None => {
                        return Err(anyhow!(
                            "no effect '{s}' on '{owner}'. Effects: [{}]",
                            chain
                                .iter()
                                .enumerate()
                                .map(|(i, e)| format!("{i}:{}", e.id()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                }
            }
        }
        for k in ["param", "target_param"] {
            if let Some(Value::String(s)) = m.get(k) {
                if let Some(n) = fix_param(s) {
                    m.insert(k.into(), Value::String(n));
                }
            }
        }
        if let Some(Value::Object(ch)) = m.get("changes").cloned() {
            let mut out = serde_json::Map::new();
            for (k, v) in ch {
                out.insert(fix_param(&k).unwrap_or(k), v);
            }
            m.insert("changes".into(), Value::Object(out));
        }
    }
    Ok(a)
}

fn summarize_args(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 140 {
        format!(
            "{}…",
            &s[..s
                .char_indices()
                .take_while(|(i, _)| *i < 140)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0)]
        )
    } else {
        s
    }
}

/// Round floats so f32 noise (0.6000000238) doesn't reach users or models.
pub fn clean_floats(v: Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(0.0);
            let r = (f * 10_000.0).round() / 10_000.0;
            serde_json::Number::from_f64(r)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(clean_floats).collect()),
        Value::Object(o) => {
            Value::Object(o.into_iter().map(|(k, v)| (k, clean_floats(v))).collect())
        }
        other => other,
    }
}
