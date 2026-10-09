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
}

const MAX_UNDO: usize = 100;

impl Engine {
    pub fn new(workdir: PathBuf) -> Self {
        Engine {
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
        }
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
        let tool = tools::find(name).ok_or_else(|| {
            anyhow!("unknown tool '{name}'. Call list_tools or get_guide to see what's available.")
        })?;
        let empty = Value::Object(Default::default());
        let args = if args.is_null() { &empty } else { args };
        // unknown top-level arguments are an error, never silently ignored
        let schema = (tool.schema)();
        if schema["additionalProperties"] == Value::Bool(false) {
            if let (Some(props), Some(given)) = (schema["properties"].as_object(), args.as_object())
            {
                let bad: Vec<&String> = given.keys().filter(|k| !props.contains_key(*k)).collect();
                if !bad.is_empty() {
                    let mut valid: Vec<&String> = props.keys().collect();
                    valid.sort();
                    let err = anyhow!(
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
                    );
                    self.log.push(LogEntry {
                        tool: name.to_string(),
                        summary: summarize_args(args),
                        ok: false,
                        source: source.to_string(),
                    });
                    return Err(err);
                }
            }
        }
        let snapshot = if tool.mutates {
            Some(self.project.clone())
        } else {
            None
        };
        // a panicking tool must never take the MCP server down with it
        let run = tool.run;
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(self, args)
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
        let summary = summarize_args(args);
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
        self.log.push(LogEntry {
            tool: name.to_string(),
            summary,
            ok: result.is_ok(),
            source: source.to_string(),
        });
        if self.log.len() > 500 {
            self.log.remove(0);
        }
        result
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
    pub fn replace_project(&mut self, p: Project) {
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

    /// Render the song (cached per revision) with stems.
    pub fn mix(&mut self) -> Result<Arc<Mix>> {
        if let Some((rev, m)) = &self.cached_mix {
            if *rev == self.revision {
                return Ok(m.clone());
            }
        }
        self.bank.sync(&self.project.samples);
        let m = Arc::new(render::render(
            &self.project,
            &self.bank,
            &RenderOptions {
                // measure tracks without holding every stem in RAM (31-track songs)
                track_stats: true,
                ..Default::default()
            },
        )?);
        self.cached_mix = Some((self.revision, m.clone()));
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

    /// Render any project version (not cached) with stems.
    pub fn render_version(&mut self, p: &Project) -> Result<Mix> {
        self.bank.sync(&p.samples);
        render::render(
            p,
            &self.bank,
            &RenderOptions {
                track_stats: true,
                ..Default::default()
            },
        )
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
