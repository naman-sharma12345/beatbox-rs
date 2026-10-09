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

pub struct Engine {
    pub project: Project,
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
            project: Project::default(),
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
        let snapshot = if tool.mutates {
            Some(self.project.clone())
        } else {
            None
        };
        let result = (tool.run)(self, args).map(clean_floats);
        let summary = summarize_args(args);
        match &result {
            Ok(_) => {
                if let Some(s) = snapshot {
                    if s != self.project {
                        self.undo.push(s);
                        if self.undo.len() > MAX_UNDO {
                            self.undo.remove(0);
                        }
                        self.redo.clear();
                        self.revision += 1;
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
                keep_stems: true,
                ..Default::default()
            },
        )?);
        self.cached_mix = Some((self.revision, m.clone()));
        Ok(m)
    }

    pub fn analyze(&mut self) -> Result<analysis::Report> {
        let m = self.mix()?;
        let mut rep = analysis::analyze(&m);
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
