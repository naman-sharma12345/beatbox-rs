//! Local, unsaved precise rack drafts. All dispatch validation stays under the caller's lock.
use crate::engine::Engine;
use crate::project::{Note, Pattern, Project};
use eframe::egui;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

const EXPIRED: &str = "Step edit expired. Reopen Step velocity to continue.";
const REPAIR: &str = "Project identity data needs repair before precise step editing";
const COMPILED: &str =
    "This source is used by a compiled playlist. Precise rack editing is not supported here yet.";
pub(super) const DRUM_ONLY: &str =
    "Step velocity is available for drums and one-shot samples. Use Piano roll for this channel.";

type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub(super) struct OpenContext {
    project: Project,
    revision: u64,
    generation: u64,
    track_id: String,
    pattern_id: String,
    track_name: String,
    pattern_name: String,
}

#[derive(Clone, Debug)]
struct Cell {
    step: u32,
    notes: Vec<Note>,
}

#[derive(Clone)]
pub(super) struct Draft {
    owner: OpenContext,
    cells: Vec<Cell>,
    cell: usize,
    cell_serial: u64,
    text: String,
    initial_text: String,
    baseline: Option<f32>,
    serial: u64,
    error: Option<String>,
    expired: bool,
}

#[derive(Default)]
pub(super) struct EditorState {
    pub(super) draft: Option<Draft>,
    serial: u64,
    entry_ids: Vec<egui::Id>,
    group: Vec<egui::Id>,
    window_chrome: Vec<egui::Id>,
    rect: Option<egui::Rect>,
    had_focus: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Intent {
    Apply {
        serial: u64,
        cell_serial: u64,
        value: f32,
    },
    Cancel,
}

#[derive(Debug, PartialEq)]
pub(super) enum Applied {
    Changed(usize),
    NoChange,
}

pub(super) fn drum(t: &crate::project::Track) -> bool {
    t.instrument.is_drum()
        || matches!(&t.instrument, crate::instruments::Instrument::Sampler(s) if s.one_shot)
}

fn extent(pat: &Pattern) -> Result<(u32, u32)> {
    if !(1..=64).contains(&pat.bars)
        || pat
            .meter
            .is_some_and(|(n, d)| !(1..=32).contains(&n) || !matches!(d, 2 | 4 | 8 | 16))
    {
        return Err("Invalid pattern extent or meter".into());
    }
    let per_bar = pat.steps_per_bar();
    let count = per_bar
        .checked_mul(pat.bars)
        .filter(|n| *n > 0)
        .ok_or("Invalid pattern extent")?;
    Ok((per_bar, count))
}

fn identity_guard(p: &Project) -> Result<()> {
    let mut clone = p.clone();
    let keys = crate::ids::owner_keys(p);
    if clone.ensure_ids_after(&keys) || clone != *p {
        return Err(REPAIR.into());
    }
    Ok(())
}

fn unique_index<'a>(ids: impl Iterator<Item = &'a str>, wanted: &str) -> Result<usize> {
    if wanted.is_empty() {
        return Err(EXPIRED.into());
    }
    let found: Vec<_> = ids
        .enumerate()
        .filter(|(_, id)| *id == wanted)
        .map(|(i, _)| i)
        .collect();
    if found.len() != 1 {
        return Err(EXPIRED.into());
    }
    Ok(found[0])
}

fn owners(p: &Project, owner: &OpenContext) -> Result<(usize, usize)> {
    let ti = unique_index(p.tracks.iter().map(|t| t.id.as_str()), &owner.track_id)?;
    let pi = unique_index(
        p.patterns.iter().map(|pat| pat.id.as_str()),
        &owner.pattern_id,
    )?;
    let t = &p.tracks[ti];
    let pat = &p.patterns[pi];
    if t.name != owner.track_name || pat.name != owner.pattern_name {
        return Err(EXPIRED.into());
    }
    if !drum(t) {
        return Err(DRUM_ONLY.into());
    }
    if pat.name.starts_with(crate::tools_fl::PL) {
        return Err("Compiled pattern editing is not supported here yet.".into());
    }
    for names in [
        p.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        p.patterns
            .iter()
            .map(|pat| pat.name.as_str())
            .collect::<Vec<_>>(),
    ] {
        let mut seen = HashSet::new();
        for name in names {
            if name.trim().is_empty() || !seen.insert(name.to_lowercase()) {
                return Err("Channel or pattern names are ambiguous".into());
            }
        }
    }
    // Names with outer whitespace cannot be round-tripped by the real trimmed lookup.
    if t.name.trim() != t.name || pat.name.trim() != pat.name {
        return Err("Channel or pattern name cannot be resolved precisely".into());
    }
    let canonical = t.name.to_lowercase();
    if pat
        .clips
        .keys()
        .any(|k| k.trim().to_lowercase() == canonical && *k != canonical)
    {
        return Err("Channel clip name is not canonical".into());
    }
    extent(pat)?;
    Ok((ti, pi))
}

fn cells(p: &Project, ti: usize, pi: usize) -> Result<Vec<Cell>> {
    let pat = &p.patterns[pi];
    let (_, count) = extent(pat)?;
    let mut ids = HashSet::new();
    let mut groups: BTreeMap<u32, Vec<Note>> = BTreeMap::new();
    for n in pat.notes(&p.tracks[ti].name) {
        if n.id.is_empty() || !ids.insert(n.id.clone()) {
            return Err("Hits need unique nonempty identities".into());
        }
        if !n.start.is_finite()
            || n.start < 0.0
            || n.start >= count as f32
            || n.pitch > 127
            || !n.vel.is_finite()
            || !(0.0..=1.0).contains(&n.vel)
        {
            return Err("Invalid hit position, pitch or velocity".into());
        }
        // Exactly [s, s+1), without a tolerance or offset/probability filter.
        groups
            .entry(n.start.floor() as u32)
            .or_default()
            .push(n.clone());
    }
    Ok(groups
        .into_iter()
        .map(|(step, notes)| Cell { step, notes })
        .collect())
}

fn final_selection(p: &Project, ti: usize, pi: usize, notes: &[Note], value: f32) -> Result<Value> {
    if notes.is_empty() || notes.iter().any(|n| n.id.is_empty()) {
        return Err(EXPIRED.into());
    }
    let args = json!({"track":p.tracks[ti].name,"pattern":p.patterns[pi].name,
        "ids":notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),"velocity":value});
    let processed = crate::ids::resolve_id_refs(p, &args);
    let actual_ti = p
        .track_index(processed["track"].as_str().ok_or(EXPIRED)?)
        .map_err(|_| EXPIRED.to_string())?;
    let actual_pi = p
        .pattern_index(processed["pattern"].as_str().ok_or(EXPIRED)?)
        .map_err(|_| EXPIRED.to_string())?;
    if actual_ti != ti
        || actual_pi != pi
        || p.tracks[actual_ti].id != p.tracks[ti].id
        || p.patterns[actual_pi].id != p.patterns[pi].id
    {
        return Err("Channel or pattern name resolves to another owner".into());
    }
    // Use the actual parser, including ID trimming, and actual default Selection::matches.
    let selected = crate::tools_midi::selection_of(&processed).map_err(|e| e.to_string())?;
    let matches: Vec<_> = p.patterns[pi]
        .notes(&p.tracks[ti].name)
        .iter()
        .filter(|n| selected.matches(n))
        .map(|n| n.id.as_str())
        .collect();
    let frozen: Vec<_> = notes.iter().map(|n| n.id.as_str()).collect();
    if matches != frozen {
        return Err("Hit identities do not resolve precisely".into());
    }
    Ok(args)
}

fn same_note(a: &Note, b: &Note) -> bool {
    a.id == b.id
        && a.start.to_bits() == b.start.to_bits()
        && a.len.to_bits() == b.len.to_bits()
        && a.pitch == b.pitch
        && a.vel.to_bits() == b.vel.to_bits()
        && a.prob.to_bits() == b.prob.to_bits()
        && a.offset.to_bits() == b.offset.to_bits()
        && a.slide_to == b.slide_to
}

fn compiled_source(p: &Project, pi: usize) -> bool {
    !p.playlist.is_empty()
        && !p.arrangement.is_empty()
        && p.arrangement
            .iter()
            .all(|s| s.pattern.starts_with(crate::tools_fl::PL))
        && p.playlist
            .iter()
            .any(|c| p.patterns[pi].name.eq_ignore_ascii_case(&c.pattern))
}

/// Only this helper acquires the opening lock. The visible indices are never reused on live data.
pub(super) fn capture(
    engine: &Arc<Mutex<Engine>>,
    visible: &Project,
    ti: usize,
    pi: usize,
) -> Result<OpenContext> {
    let track = visible.tracks.get(ti).ok_or(EXPIRED)?;
    let pattern = visible.patterns.get(pi).ok_or(EXPIRED)?;
    let e = engine.lock().map_err(|_| EXPIRED.to_string())?;
    let owner = OpenContext {
        project: e.project.clone(),
        revision: e.revision,
        generation: e.project_load_generation,
        track_id: track.id.clone(),
        pattern_id: pattern.id.clone(),
        track_name: track.name.clone(),
        pattern_name: pattern.name.clone(),
    };
    let (ti, pi) = owners(&owner.project, &owner)?;
    identity_guard(&owner.project)?;
    let occupied = cells(&owner.project, ti, pi)?;
    // Also reject alias collisions before an empty editor or an armed draft is displayed.
    let probe = json!({"track":owner.project.tracks[ti].name,"pattern":owner.project.patterns[pi].name,
        "ids":occupied.first().map(|c|c.notes.iter().map(|n|n.id.clone()).collect::<Vec<_>>()).unwrap_or_default(),"velocity":0.0});
    let resolved = crate::ids::resolve_id_refs(&owner.project, &probe);
    if owner
        .project
        .track_index(resolved["track"].as_str().ok_or(EXPIRED)?)
        .ok()
        != Some(ti)
        || owner
            .project
            .pattern_index(resolved["pattern"].as_str().ok_or(EXPIRED)?)
            .ok()
            != Some(pi)
    {
        return Err("Channel or pattern name resolves to another owner".into());
    }
    for cell in &occupied {
        final_selection(&owner.project, ti, pi, &cell.notes, 0.0)?;
    }
    Ok(owner)
}

fn percent(text: &str) -> Result<f32> {
    let number: f64 = text
        .trim()
        .parse()
        .map_err(|_| "Enter a number from 0 to 100".to_string())?;
    if !number.is_finite() || !(0.0..=100.0).contains(&number) {
        return Err("Enter a finite number from 0 to 100".into());
    }
    let value = (number / 100.0) as f32;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("Invalid velocity".into());
    }
    Ok(value)
}

impl Draft {
    fn new(owner: OpenContext, serial: u64) -> Result<Self> {
        let (ti, pi) = owners(&owner.project, &owner)?;
        let occupied = cells(&owner.project, ti, pi)?;
        let mut d = Self {
            owner,
            cells: occupied,
            cell: 0,
            cell_serial: 0,
            text: String::new(),
            initial_text: String::new(),
            baseline: None,
            serial,
            error: None,
            expired: false,
        };
        d.choose(0);
        Ok(d)
    }
    fn choose(&mut self, cell: usize) {
        self.cell_serial = self.cell_serial.wrapping_add(1);
        self.cell = cell;
        self.baseline = self.cells.get(cell).and_then(|c| {
            c.notes
                .first()
                .map(|n| n.vel)
                .filter(|v| c.notes.iter().all(|n| n.vel == *v))
        });
        self.text = self
            .baseline
            .map(|v| ((v as f64) * 100.0).to_string())
            .unwrap_or_default();
        self.initial_text = self.text.clone();
        self.error = None;
    }
    fn value(&self) -> Result<f32> {
        if self.expired || self.cells.get(self.cell).is_none() {
            return Err(EXPIRED.into());
        }
        if self.text == self.initial_text {
            if let Some(v) = self.baseline {
                return Ok(v);
            }
        }
        percent(&self.text)
    }
}

/// Caller holds exactly one Engine mutex guard for this entire function, including dispatch.
pub(super) fn guarded_apply(
    e: &mut Engine,
    draft: &Draft,
    current_track: usize,
    current_pattern: usize,
    value: f32,
) -> Result<Applied> {
    if draft.expired
        || e.revision != draft.owner.revision
        || e.project_load_generation != draft.owner.generation
    {
        return Err(EXPIRED.into());
    }
    let p = &e.project;
    let (ti, pi) = owners(p, &draft.owner)?;
    if p.tracks.get(current_track).map(|t| &t.id) != Some(&draft.owner.track_id)
        || p.patterns.get(current_pattern).map(|pat| &pat.id) != Some(&draft.owner.pattern_id)
    {
        return Err(EXPIRED.into());
    }
    let live = cells(p, ti, pi)?;
    let frozen = draft.cells.get(draft.cell).ok_or(EXPIRED)?;
    let actual = live.iter().find(|c| c.step == frozen.step).ok_or(EXPIRED)?;
    if actual.notes.is_empty()
        || actual.notes.len() != frozen.notes.len()
        || actual
            .notes
            .iter()
            .zip(&frozen.notes)
            .any(|(a, b)| !same_note(a, b))
    {
        return Err(EXPIRED.into());
    }
    if !value.is_finite() || !(0.0..=1.0).contains(&value) || value != draft.value()? {
        return Err("Invalid velocity draft".into());
    }
    identity_guard(p)?;
    let changed = frozen.notes.iter().filter(|n| n.vel != value).count();
    if changed == 0 {
        return Ok(Applied::NoChange);
    }
    if compiled_source(p, pi) {
        return Err(COMPILED.into());
    }
    let args = final_selection(p, ti, pi, &frozen.notes, value)?;
    let result = e
        .call_from("set_note_velocity", &args, "you")
        .map_err(|e| e.to_string())?;
    if result["affected"].as_u64() != Some(changed as u64) {
        return Err(
            "Edit completed with an unexpected hit count. Inspect the project before continuing."
                .into(),
        );
    }
    Ok(Applied::Changed(changed))
}

pub(super) fn remember_entry(state: &mut EditorState, response: &egui::Response) {
    if !state.entry_ids.contains(&response.id) {
        state.entry_ids.push(response.id);
    }
    #[cfg(test)]
    tests::observe(response.ctx.clone(), "entry", response);
}

/// Used at the production shortcut seam, before raw global shortcut handling.
/// Keep all ordinary actions unless this editor owns the frame. On entry activation only
/// remove Space/Enter while inspecting global actions; restore events before the real button.
pub(super) fn global_actions(
    ctx: &egui::Context,
    state: &EditorState,
    active_at_start: bool,
    consumed: bool,
) -> Vec<super::shortcuts::Action> {
    if active_at_start || state.draft.is_some() || consumed {
        return Vec::new();
    }
    let entry_focused = ctx
        .memory(|m| m.focused())
        .is_some_and(|id| state.entry_ids.contains(&id));
    if !entry_focused {
        return super::shortcuts::pressed(ctx);
    }
    let events = ctx.input(|i| i.events.clone());
    ctx.input_mut(|i| {
        i.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Space | egui::Key::Enter,
                    ..
                }
            )
        })
    });
    let actions = super::shortcuts::pressed(ctx);
    ctx.input_mut(|i| i.events = events);
    actions
}

impl EditorState {
    pub(super) fn open(&mut self, owner: OpenContext) -> Result<()> {
        self.serial = self.serial.wrapping_add(1);
        self.draft = Some(Draft::new(owner, self.serial)?);
        self.group.clear();
        self.window_chrome.clear();
        self.rect = None;
        self.had_focus = false;
        Ok(())
    }
    pub(super) fn validate(&mut self, e: &Engine, ti: usize, pi: usize) -> bool {
        // O(1) selected-owner/version check. Whole-project identity and clip scans
        // happen only on explicit open/Apply, never on passive frames.
        let stale = self.draft.as_ref().is_some_and(|d| {
            e.revision != d.owner.revision
                || e.project_load_generation != d.owner.generation
                || e.project
                    .tracks
                    .get(ti)
                    .is_none_or(|t| t.id != d.owner.track_id || t.name != d.owner.track_name)
                || e.project
                    .patterns
                    .get(pi)
                    .is_none_or(|p| p.id != d.owner.pattern_id || p.name != d.owner.pattern_name)
        });
        if stale {
            self.draft = None;
        }
        stale
    }
    pub(super) fn release_focus(&mut self, ctx: &egui::Context) {
        if let Some(id) = ctx
            .memory(|m| m.focused())
            .filter(|id| self.group.contains(id))
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        self.group.clear();
        self.window_chrome.clear();
        self.rect = None;
        self.had_focus = false;
    }

    pub(super) fn finish(
        &mut self,
        e: &mut Engine,
        intent: Intent,
        ti: usize,
        pi: usize,
    ) -> Result<()> {
        match intent {
            Intent::Cancel => {
                self.draft = None;
                Ok(())
            }
            Intent::Apply {
                serial,
                cell_serial,
                value,
            } => {
                let d = self.draft.as_ref().ok_or(EXPIRED)?;
                if d.serial != serial || d.cell_serial != cell_serial {
                    return Err(EXPIRED.into());
                }
                match guarded_apply(e, d, ti, pi, value) {
                    Ok(outcome) => {
                        if let Applied::Changed(changed) = outcome {
                            debug_assert!(changed > 0);
                        }
                        self.draft = None;
                        Ok(())
                    }
                    Err(error) => {
                        if let Some(d) = self.draft.as_mut() {
                            // Fail closed even after an unexpected successful response shape.
                            d.expired = true;
                            d.error = Some(error.clone());
                        }
                        Err(error)
                    }
                }
            }
        }
    }
}

pub(super) fn footer(ui: &mut egui::Ui, p: &Project, ti: usize, state: &mut EditorState) -> bool {
    if p.tracks.is_empty() {
        return false;
    }
    let mut open = false;
    // The steps may be thousands of points wide. Wrap the footer to the actual
    // viewport instead of stretching its description along that virtual row.
    let width = ui.clip_rect().width();
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.horizontal_wrapped(|ui| {
            let track = p.tracks.get(ti);
            let enabled = track.is_some_and(drum);
            let response = ui.add_enabled(enabled, egui::Button::new("Step velocity..."));
            remember_entry(state, &response);
            if response.has_focus() {
                response.scroll_to_me(None);
            }
            open = response.clicked();
            let description = track
                .map(|t| format!("Selected channel: {}", t.name))
                .unwrap_or_else(|| "No channel selected".into());
            ui.add(egui::Label::new(description).wrap());
            if !enabled {
                ui.add(egui::Label::new(DRUM_ONLY).wrap());
            }
        });
    });
    open
}

pub(super) fn show(ctx: &egui::Context, state: &mut EditorState) -> Option<Intent> {
    let d = state.draft.as_mut()?;
    let prior_group_focus = ctx
        .memory(|m| m.focused())
        .is_some_and(|id| state.group.contains(&id));
    state.had_focus |= prior_group_focus;
    if ctx.input(|i| !i.focused || i.viewport().focused == Some(false))
        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
    {
        return Some(Intent::Cancel);
    }
    let mut intent = None;
    let mut open = true;
    let mut group = Vec::new();
    let mut chosen = None;
    let mut list_keyboard = false;
    let window_id = egui::Id::new((
        "rack-step-velocity",
        &d.owner.track_id,
        &d.owner.pattern_id,
        d.serial,
    ));
    let result = egui::Window::new("Step velocity")
        .id(window_id)
        .open(&mut open)
        .collapsible(false)
        .default_width(380.0)
        .max_width(ctx.screen_rect().width().max(120.0) - 24.0)
        .max_height(ctx.screen_rect().height().max(120.0) - 24.0)
        .vscroll(true)
        .show(ctx, |ui| {
            ui.label(format!("Channel: {}", d.owner.track_name));
            ui.label(format!("Pattern: {}", d.owner.pattern_name));
            let per_bar = d
                .owner
                .project
                .patterns
                .iter()
                .find(|p| p.id == d.owner.pattern_id)
                .map(|p| p.steps_per_bar())
                .unwrap_or(1);
            if d.cells.is_empty() {
                ui.label("No hits in this pattern");
            }
            let list_height = (ctx.screen_rect().height() * 0.3).clamp(30.0, 180.0);
            egui::ScrollArea::vertical()
                .id_salt((window_id, "cells"))
                .max_height(list_height)
                .show(ui, |ui| {
                    for (i, cell) in d.cells.iter().enumerate() {
                        let label = format!(
                            "Bar {}, sixteenth {} of {}: {} hits",
                            cell.step / per_bar + 1,
                            cell.step % per_bar + 1,
                            per_bar,
                            cell.notes.len()
                        );
                        let response = ui.selectable_label(i == d.cell, label);
                        group.push(response.id);
                        if response.has_focus() {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            chosen = Some(i);
                        }
                        if response.has_focus() {
                            let next = if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                                Some((i + 1).min(d.cells.len() - 1))
                            } else if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                                Some(i.saturating_sub(1))
                            } else {
                                None
                            };
                            if let Some(next) = next {
                                chosen = Some(next);
                                list_keyboard = true;
                            }
                        }
                        #[cfg(test)]
                        tests::observe(ctx.clone(), &format!("cell-{i}"), &response);
                    }
                });
            if let Some(cell) = chosen {
                d.choose(cell);
                if list_keyboard {
                    ui.memory_mut(|m| m.request_focus(group[cell]));
                }
            }
            if let Some(cell) = d.cells.get(d.cell) {
                if d.baseline.is_none() {
                    ui.label("Mixed");
                }
                if cell.notes.len() == 1 {
                    ui.label("Velocity (%)");
                } else {
                    ui.label(format!(
                        "Set all {} hits in this step to (%)",
                        cell.notes.len()
                    ));
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut d.text)
                        .id(egui::Id::new((
                            window_id,
                            cell.step,
                            d.cell_serial,
                            "percent",
                        )))
                        .desired_width(160.0),
                );
                group.push(response.id);
                if !state.had_focus || (chosen.is_some() && !list_keyboard) {
                    response.request_focus();
                }
                // Single-line Enter finishes text, never queues Apply or escapes to transport.
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    response.request_focus();
                }
                #[cfg(test)]
                tests::observe(ctx.clone(), "text", &response);
            }
            ui.label("0% silences hits; it does not delete them.");
            if let Some(pi) = d
                .owner
                .project
                .patterns
                .iter()
                .position(|p| p.id == d.owner.pattern_id)
            {
                if compiled_source(&d.owner.project, pi) {
                    ui.label(COMPILED);
                }
            }
            if let Some(error) = &d.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            let value = d.value();
            if !d.text.is_empty() && value.is_err() && !d.expired {
                ui.label("Enter a finite number from 0 to 100");
            }
            ui.horizontal_wrapped(|ui| {
                // egui 0.29 registers focus interest even on disabled buttons.
                // Keep invalid Apply disabled, but out of keyboard traversal so
                // it cannot consume Tab and surrender it before Cancel renders.
                // This also lets Shift+Tab from Cancel target the real field.
                let sense = if value.is_ok() {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let apply = ui.add_enabled(value.is_ok(), egui::Button::new("Apply").sense(sense));
                group.push(apply.id);
                if apply.has_focus() {
                    apply.scroll_to_me(None);
                }
                if apply.clicked() {
                    intent = Some(Intent::Apply {
                        serial: d.serial,
                        cell_serial: d.cell_serial,
                        value: value.unwrap(),
                    });
                }
                let cancel = ui.button("Cancel");
                if d.cells.is_empty() && !state.had_focus {
                    cancel.request_focus();
                }
                group.push(cancel.id);
                if cancel.has_focus() {
                    cancel.scroll_to_me(None);
                }
                if cancel.clicked() {
                    intent = Some(Intent::Cancel);
                }
                #[cfg(test)]
                {
                    tests::observe(ctx.clone(), "apply", &apply);
                    tests::observe(ctx.clone(), "cancel", &cancel);
                }
            });
        });
    let window_layer = result.as_ref().map(|r| r.response.layer_id);
    let rect = result.map(|r| r.response.rect).or(state.rect);
    // Window title-bar controls belong to the editor too. Window::show above
    // draws them in this pass. Exclude disappeared explicit controls, since
    // Context::read_response can otherwise fall back to a previous-pass widget.
    if let Some(id) = ctx.memory(|m| m.focused()) {
        let decoration = !group.contains(&id)
            && (!state.group.contains(&id) || state.window_chrome.contains(&id));
        if decoration {
            if let Some(response) = ctx.read_response(id) {
                if Some(response.layer_id) == window_layer
                    && rect.is_some_and(|r| r.contains_rect(response.rect))
                {
                    group.push(response.id);
                    if !state.window_chrome.contains(&id) {
                        state.window_chrome.push(id);
                    }
                    #[cfg(test)]
                    tests::observe(ctx.clone(), "window-focus", &response);
                }
            }
        }
    }
    #[cfg(test)]
    tests::window_open(ctx, open);
    let outside_pointer = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| rect.is_some_and(|r| !r.contains(pos)))
    });
    let focus = ctx.memory(|m| m.focused());
    let inside_pointer = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| rect.is_some_and(|r| r.contains(pos)))
    });
    let outside_focus = focus.is_some_and(|id| !group.contains(&id))
        || (focus.is_none() && state.had_focus && !inside_pointer);
    if !open || outside_pointer || (state.had_focus && outside_focus) {
        intent = Some(Intent::Cancel);
    }
    state.had_focus = focus.is_some_and(|id| group.contains(&id));
    state.group = group;
    state.rect = rect;
    intent
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::project::{PlaylistClip, Section, Track};
    use egui::{pos2, vec2, Event, Key, Modifiers, PointerButton, Rect};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) fn observe(ctx: egui::Context, name: &str, response: &egui::Response) {
        let pass = ctx.cumulative_pass_nr();
        let key = egui::Id::new("velocity-current-responses");
        ctx.data_mut(|data| {
            let mut rows = data
                .get_temp::<Vec<(u64, String, egui::Response)>>(key)
                .unwrap_or_default();
            if rows.first().is_some_and(|r| r.0 != pass) {
                rows.clear();
            }
            rows.push((pass, name.into(), response.clone()));
            data.insert_temp(key, rows);
        });
    }
    pub(crate) fn window_open(ctx: &egui::Context, open: bool) {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("velocity-window-open"), (pass, open));
        });
    }

    #[test]
    fn window_open_records_actual_pass_and_replaces_true_with_false_without_reentry() {
        let ctx = egui::Context::default();
        let key = egui::Id::new("velocity-window-open");
        let mut records = Vec::new();
        for open in [true, false] {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                let pass = ctx.cumulative_pass_nr();
                window_open(ctx, open);
                let stored = ctx.data(|d| d.get_temp::<(u64, bool)>(key));
                assert_eq!(stored, Some((pass, open)));
            });
            drop(output);
            let completed_pass = ctx.cumulative_pass_nr() - 1;
            let stored = ctx.data(|d| d.get_temp::<(u64, bool)>(key));
            assert_eq!(stored, Some((completed_pass, open)));
            records.push(stored.unwrap());
        }
        assert_eq!(records.len(), 2);
        assert!(records[0].1);
        assert!(!records[1].1);
        assert_eq!(records[1].0, records[0].0 + 1);
        let stored = ctx.data(|d| d.get_temp::<(u64, bool)>(key));
        assert_eq!(stored, Some(records[1]));
        assert_ne!(stored, Some(records[0]));
    }

    pub(crate) fn responses(ctx: &egui::Context) -> Vec<(u64, String, egui::Response)> {
        ctx.data(|d| d.get_temp(egui::Id::new("velocity-current-responses")))
            .unwrap_or_default()
    }
    fn response(ctx: &egui::Context, name: &str) -> egui::Response {
        let rows = responses(ctx);
        let (pass, _, r) = rows.iter().rev().find(|(_, n, _)| n == name).expect(name);
        assert_eq!(*pass, ctx.cumulative_pass_nr() - 1);
        r.clone()
    }
    pub(crate) fn project() -> Project {
        let mut p = Project::new("Velocity fixture", 120.0);
        p.tracks.push(Track::new(
            "Kick channel with a fully readable name",
            crate::instruments::preset("kick").unwrap(),
        ));
        let name = p.tracks[0].name.clone();
        p.patterns[0].notes_mut(&name).extend([
            Note {
                start: 0.0,
                len: 0.03125,
                pitch: 36,
                vel: 0.25,
                prob: 0.0,
                offset: -0.25,
                slide_to: Some(48),
                ..Default::default()
            },
            Note {
                start: 0.5,
                len: 1.75,
                pitch: 72,
                vel: 0.7,
                prob: 0.375,
                offset: 0.125,
                slide_to: None,
                ..Default::default()
            },
            Note::new(1.0, 0.0001, 60, 0.8),
            Note::new(63.999, 0.5, 61, 0.4),
        ]);
        p.ensure_fx_ids();
        p
    }
    fn owner(p: &Project) -> OpenContext {
        OpenContext {
            project: p.clone(),
            revision: 0,
            generation: 0,
            track_id: p.tracks[0].id.clone(),
            pattern_id: p.patterns[0].id.clone(),
            track_name: p.tracks[0].name.clone(),
            pattern_name: p.patterns[0].name.clone(),
        }
    }
    fn draft(p: &Project) -> Draft {
        Draft::new(owner(p), 1).unwrap()
    }
    fn serialized(p: &Project) -> Value {
        serde_json::to_value(p).unwrap()
    }
    fn set_text(d: &mut Draft, text: &str) {
        d.text = text.into();
    }

    #[test]
    fn exact_cell_edges_include_last_float_and_ignore_expression() {
        let mut p = project();
        let key = p.tracks[0].name.to_lowercase();
        let below = f32::from_bits(1.0_f32.to_bits() - 1);
        p.patterns[0].clips.insert(
            key,
            [0.0, 0.5, 0.999, below, 1.0]
                .iter()
                .enumerate()
                .map(|(i, start)| Note {
                    id: format!("edge{i}"),
                    start: *start,
                    pitch: 30 + i as u8,
                    prob: 0.0,
                    offset: 0.5,
                    slide_to: Some(90),
                    ..Default::default()
                })
                .collect(),
        );
        let c = cells(&p, 0, 0).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].notes.len(), 4);
        assert_eq!(c[1].notes.len(), 1);
        let a = final_selection(&p, 0, 0, &c[0].notes, 0.25).unwrap();
        assert_eq!(a.as_object().unwrap().len(), 4);
        let sel = crate::tools_midi::selection_of(&a).unwrap();
        assert_eq!(
            p.patterns[0]
                .notes(&p.tracks[0].name)
                .iter()
                .filter(|n| sel.matches(n))
                .count(),
            4
        );
    }
    #[test]
    fn meters_many_bars_final_cells_and_empty_never_truncate() {
        for (meter, per) in [((3, 4), 12), ((6, 8), 12), ((7, 8), 14)] {
            for bars in [1, 4, 64] {
                let mut p = project();
                p.patterns[0].meter = Some(meter);
                p.patterns[0].bars = bars;
                let name = p.tracks[0].name.clone();
                *p.patterns[0].notes_mut(&name) = (0..per * bars)
                    .map(|s| Note {
                        id: format!("cell{s}"),
                        start: s as f32 + 0.5,
                        ..Default::default()
                    })
                    .collect();
                let c = cells(&p, 0, 0).unwrap();
                assert_eq!(c.len(), (per * bars) as usize);
                assert_eq!(c.last().unwrap().step, per * bars - 1);
                p.patterns[0].notes_mut(&name).clear();
                let d = draft(&p);
                assert!(d.cells.is_empty());
                assert!(d.value().is_err());
            }
        }
    }
    #[test]
    fn extent_validation_is_checked_not_saturating() {
        for bars in [0, 65, u32::MAX] {
            let mut p = project();
            p.patterns[0].bars = bars;
            assert!(extent(&p.patterns[0]).is_err());
        }
        for meter in [(0, 4), (33, 4), (4, 0), (4, 3), (4, 32)] {
            let mut p = project();
            p.patterns[0].meter = Some(meter);
            assert!(extent(&p.patterns[0]).is_err());
        }
    }
    #[test]
    fn percent_boundaries_fraction_and_nonfinite_rejection() {
        for (text, value) in [("0", 0.0), ("25", 0.25), ("100", 1.0), ("12.5", 0.125)] {
            assert_eq!(percent(text).unwrap(), value);
        }
        for text in [
            "", " ", "words", "-1", "100.0001", "NaN", "inf", "-inf", "1e999",
        ] {
            assert!(percent(text).is_err(), "{text}");
        }
    }
    #[test]
    fn common_lossless_fraction_and_mixed_blank_all_hits() {
        let mut p = project();
        let name = p.tracks[0].name.clone();
        let v = f32::from_bits(0x3eaaaaab);
        for n in &mut p.patterns[0].notes_mut(&name)[..2] {
            n.vel = v;
        }
        let d = draft(&p);
        assert_eq!(d.value().unwrap().to_bits(), v.to_bits());
        assert_eq!(percent(&d.text).unwrap().to_bits(), v.to_bits());
        p.patterns[0].notes_mut(&name)[1].vel = 0.75;
        let mut d = draft(&p);
        assert!(d.baseline.is_none());
        assert!(d.text.is_empty());
        assert!(d.value().is_err());
        set_text(&mut d, "25");
        assert_eq!(d.value().unwrap(), 0.25);
        d.choose(1);
        assert_ne!(d.text, "25");
    }
    #[test]
    fn clip_id_validation_includes_other_cells_and_malformed_values() {
        let p = project();
        let key = p.tracks[0].name.to_lowercase();
        for index in [0, 2, 3] {
            for bad in ["", p.patterns[0].clips[&key][1].id.as_str()] {
                let mut q = p.clone();
                q.patterns[0].clips.get_mut(&key).unwrap()[index].id = bad.into();
                assert!(cells(&q, 0, 0).is_err());
            }
        }
        for start in [f32::NAN, f32::INFINITY, -0.001, 64.0] {
            let mut q = p.clone();
            q.patterns[0].clips.get_mut(&key).unwrap()[0].start = start;
            assert!(cells(&q, 0, 0).is_err());
        }
        for vel in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut q = p.clone();
            q.patterns[0].clips.get_mut(&key).unwrap()[0].vel = vel;
            assert!(cells(&q, 0, 0).is_err());
        }
        let mut q = p.clone();
        q.patterns[0].clips.get_mut(&key).unwrap()[0].pitch = 128;
        assert!(cells(&q, 0, 0).is_err());
        let mut q = p.clone();
        q.patterns[0].clips.get_mut(&key).unwrap()[0].id = " padded1 ".into();
        assert!(final_selection(&q, 0, 0, &cells(&q, 0, 0).unwrap()[0].notes, 0.25).is_err());
        assert!(final_selection(&p, 0, 0, &[], 0.25).is_err());
    }
    #[test]
    fn all_hits_same_cell_remain_ordered_and_uncapped() {
        let mut p = project();
        let name = p.tracks[0].name.clone();
        *p.patterns[0].notes_mut(&name) = (0..512)
            .map(|i| Note {
                id: format!("many{i}"),
                start: 0.5,
                pitch: (i % 128) as u8,
                ..Default::default()
            })
            .collect();
        let c = cells(&p, 0, 0).unwrap();
        assert_eq!(c[0].notes.len(), 512);
        let a = final_selection(&p, 0, 0, &c[0].notes, 0.0).unwrap();
        assert_eq!(a["ids"].as_array().unwrap().len(), 512);
        assert_eq!(a["ids"][511], "many511");
    }
    #[test]
    fn clone_identity_guard_does_not_repair_live_project() {
        for kind in 0..5 {
            let mut p = project();
            let name = p.tracks[0].name.clone();
            match kind {
                0 => p.schema_version = 0,
                1 => p.tracks[0].id.clear(),
                2 => p.patterns[0].notes_mut(&name)[3].id.clear(),
                3 => p.patterns[0].notes_mut(&name)[3].id = p.tracks[0].id.clone(),
                _ => p.master_effects.push(p.master_effects[0].clone()),
            }
            let before = serialized(&p);
            assert_eq!(identity_guard(&p).unwrap_err(), REPAIR);
            assert_eq!(serialized(&p), before);
        }
    }
    #[test]
    fn names_canonical_keys_and_real_resolver_alias_collisions() {
        let mut p = project();
        let mut other = p.patterns[0].clone();
        // Pattern lookup is vector-first name OR id, not name-first.
        other.name = "Earlier".into();
        other.id = p.patterns[0].name.clone();
        p.patterns.insert(0, other);
        let c = cells(&p, 0, 1).unwrap();
        assert!(final_selection(&p, 0, 1, &c[0].notes, 0.25).is_err());
        let mut p = project();
        let key = p.tracks[0].name.to_lowercase();
        let notes = p.patterns[0].clips.remove(&key).unwrap();
        p.patterns[0].clips.insert(key.to_uppercase(), notes);
        assert!(owners(&p, &owner(&p)).is_err());
        for name in ["", " channel "] {
            let mut p = project();
            p.tracks[0].name = name.into();
            assert!(owners(&p, &owner(&p)).is_err());
        }
        let mut p = project();
        let mut t = p.tracks[0].clone();
        t.id = "t99999".into();
        t.name = t.name.to_uppercase();
        p.tracks.push(t);
        assert!(owners(&p, &owner(&p)).is_err());
        let mut p = project();
        p.tracks[0].instrument = crate::instruments::preset("epiano").unwrap();
        assert!(owners(&p, &owner(&p)).is_err());
        p.tracks[0].instrument = crate::instruments::Instrument::Sampler(Default::default());
        assert!(drum(&p.tracks[0]));
        if let crate::instruments::Instrument::Sampler(s) = &mut p.tracks[0].instrument {
            s.one_shot = false;
        }
        assert!(!drum(&p.tracks[0]));
    }
    fn placement(p: &Project, suffix: u32) -> PlaylistClip {
        PlaylistClip {
            id: format!("c{suffix}"),
            pattern: p.patterns[0].name.clone(),
            start_bar: suffix,
            bars: 4,
            tracks: Vec::new(),
            lane: 0,
            offset_bars: 0,
        }
    }
    #[test]
    fn compiled_exclusion_matches_actual_source_predicate_not_prefix_guess() {
        let mut p = project();
        let clip = placement(&p, 1);
        p.playlist.push(clip);
        assert!(!compiled_source(&p, 0));
        p.arrangement.push(Section {
            pattern: "pl:1".into(),
            repeats: 1,
        });
        assert!(compiled_source(&p, 0));
        p.playlist[0].pattern = "a".into();
        assert!(compiled_source(&p, 0));
        p.playlist[0].pattern = " A ".into();
        assert!(!compiled_source(&p, 0));
        p.playlist[0].pattern = p.patterns[0].id.clone();
        assert!(!compiled_source(&p, 0));
        p.playlist[0].pattern = "A".into();
        p.arrangement.push(Section {
            pattern: "ordinary".into(),
            repeats: 1,
        });
        assert!(!compiled_source(&p, 0));
        p.arrangement.clear();
        p.playlist.clear();
        assert!(!compiled_source(&p, 0));
    }

    struct Fixture {
        engine: Arc<Mutex<Engine>>,
        dir: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "beatbox-step-velocity-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let mut e = Engine::new(dir.clone());
            e.call_from(
                "new_project",
                &json!({"name":"Velocity fixture","bpm":120}),
                "you",
            )
            .unwrap();
            e.project = project();
            let other = Track::new("Other keys", crate::instruments::preset("epiano").unwrap());
            e.project.tracks.push(other);
            e.project.patterns[0]
                .notes_mut("Other keys")
                .push(Note::new(3.5, 0.0625, 80, 0.33));
            let mut elsewhere = Pattern::new("Elsewhere", 2);
            elsewhere
                .notes_mut("Other keys")
                .push(Note::new(8.0, 0.25, 72, 0.6));
            let source_name = e.project.tracks[0].name.clone();
            elsewhere
                .notes_mut(&source_name)
                .push(Note::new(9.0, 0.125, 48, 0.5));
            e.project.patterns.push(elsewhere);
            e.project.ensure_fx_ids();
            Self {
                engine: Arc::new(Mutex::new(e)),
                dir,
            }
        }
        fn open(&self) -> Draft {
            let p = self.engine.lock().unwrap().project.clone();
            Draft::new(capture(&self.engine, &p, 0, 0).unwrap(), 1).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn passive(e: &Engine) -> String {
        format!("{:?}", e.transport)
    }
    fn reject_unchanged(f: &Fixture, d: &Draft, value: f32) {
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let history = e.history();
        let depths = e.undo_depth();
        let rev = e.revision;
        let logs = e.log.len();
        let transport = passive(&e);
        assert!(guarded_apply(&mut e, d, 0, 0, value).is_err());
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.history(), history);
        assert_eq!(e.undo_depth(), depths);
        assert_eq!(e.revision, rev);
        assert_eq!(e.log.len(), logs);
        assert_eq!(passive(&e), transport);
    }
    #[test]
    fn real_adapter_full_project_only_velocity_one_undo_redo_and_save_load() {
        let f = Fixture::new();
        {
            let mut e = f.engine.lock().unwrap();
            e.project.playlist = vec![placement(&e.project, 1), placement(&e.project, 7)];
            e.project.ensure_fx_ids();
            e.call_from("set_tempo", &json!({"bpm":121}), "you")
                .unwrap();
            e.call_from("undo", &json!({}), "you").unwrap();
        }
        let mut d = f.open();
        set_text(&mut d, "25");
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let depth = e.undo_depth();
        let rev = e.revision;
        let log = e.log.len();
        let transport = passive(&e);
        let mut expected = before.clone();
        let name = e.project.tracks[0].name.to_lowercase();
        expected["patterns"][0]["clips"][&name][1]["vel"] = json!(0.25);
        assert_eq!(
            guarded_apply(&mut e, &d, 0, 0, 0.25).unwrap(),
            Applied::Changed(1)
        );
        assert_eq!(serialized(&e.project), expected);
        assert_eq!(e.undo_depth(), (depth.0 + 1, 0));
        assert_eq!(e.revision, rev + 1);
        assert_eq!(e.log.len(), log + 1);
        assert_eq!(passive(&e), transport);
        e.call_from("undo", &json!({}), "you").unwrap();
        assert_eq!(serialized(&e.project), before);
        e.call_from("redo", &json!({}), "you").unwrap();
        assert_eq!(serialized(&e.project), expected);
        let generation = e.project_load_generation;
        e.call_from("save_project", &json!({"path":"velocity.json"}), "you")
            .unwrap();
        e.call_from("load_project", &json!({"path":"velocity.json"}), "you")
            .unwrap();
        assert_eq!(serialized(&e.project), expected);
        assert!(e.project_load_generation > generation);
    }
    #[test]
    fn real_zero_keeps_ids_and_noop_preserves_redo_log_and_transport() {
        let f = Fixture::new();
        let mut d = f.open();
        set_text(&mut d, "0");
        {
            let mut e = f.engine.lock().unwrap();
            let before = serialized(&e.project);
            let key = e.project.tracks[0].name.to_lowercase();
            let mut expected = before;
            for i in 0..2 {
                expected["patterns"][0]["clips"][&key][i]["vel"] = json!(0.0);
            }
            assert_eq!(
                guarded_apply(&mut e, &d, 0, 0, 0.0).unwrap(),
                Applied::Changed(2)
            );
            assert_eq!(serialized(&e.project), expected);
            e.call_from("set_tempo", &json!({"bpm":122}), "you")
                .unwrap();
            e.call_from("undo", &json!({}), "you").unwrap();
        }
        let d = f.open();
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let h = e.history();
        let depth = e.undo_depth();
        let rev = e.revision;
        let log = e.log.len();
        let t = passive(&e);
        assert_eq!(
            guarded_apply(&mut e, &d, 0, 0, 0.0).unwrap(),
            Applied::NoChange
        );
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.history(), h);
        assert_eq!(e.undo_depth(), depth);
        assert_eq!(e.revision, rev);
        assert_eq!(e.log.len(), log);
        assert_eq!(passive(&e), t);
    }
    #[test]
    fn real_adapter_stale_revision_generation_undo_redo_and_selection_reject() {
        for kind in 0..5 {
            let f = Fixture::new();
            let mut d = f.open();
            set_text(&mut d, "25");
            {
                let mut e = f.engine.lock().unwrap();
                match kind {
                    0 => e.revision += 1,
                    1 => e.project_load_generation += 1,
                    2 => {
                        e.call_from("set_tempo", &json!({"bpm":99}), "you").unwrap();
                    }
                    3 => {
                        e.call_from("set_tempo", &json!({"bpm":99}), "you").unwrap();
                        e.call_from("undo", &json!({}), "you").unwrap();
                    }
                    _ => {
                        e.call_from("set_tempo", &json!({"bpm":99}), "you").unwrap();
                        e.call_from("undo", &json!({}), "you").unwrap();
                        e.call_from("redo", &json!({}), "you").unwrap();
                    }
                }
            }
            reject_unchanged(&f, &d, 0.25);
        }
        let f = Fixture::new();
        let mut d = f.open();
        set_text(&mut d, "25");
        let mut e = f.engine.lock().unwrap();
        assert!(guarded_apply(&mut e, &d, usize::MAX, 0, 0.25).is_err());
        assert!(guarded_apply(&mut e, &d, 0, usize::MAX, 0.25).is_err());
    }
    #[test]
    fn real_adapter_rejects_unversioned_identity_membership_name_and_key_drift() {
        for kind in 0..15 {
            let f = Fixture::new();
            let mut d = f.open();
            set_text(&mut d, "25");
            {
                let mut e = f.engine.lock().unwrap();
                let key = e.project.tracks[0].name.to_lowercase();
                match kind {
                    0 => e.project.tracks[0].name = "Renamed".into(),
                    1 => e.project.patterns[0].name = "Renamed".into(),
                    2 => {
                        e.project.tracks.remove(0);
                    }
                    3 => {
                        e.project.patterns.remove(0);
                    }
                    4 => e.project.patterns[0].clips.get_mut(&key).unwrap()[0].start = 2.0,
                    5 => {
                        e.project.patterns[0].clips.get_mut(&key).unwrap().remove(0);
                    }
                    6 => {
                        let mut n = e.project.patterns[0].clips[&key][0].clone();
                        n.id = "insert999".into();
                        e.project.patterns[0].clips.get_mut(&key).unwrap().push(n);
                    }
                    7 => {
                        let id = e.project.patterns[0].clips[&key][0].id.clone();
                        e.project.patterns[0].clips.get_mut(&key).unwrap()[3].id = id;
                    }
                    8 => {
                        let t = e.project.tracks[0].clone();
                        e.project.tracks.push(t);
                    }
                    9 => {
                        let q = e.project.patterns[0].clone();
                        e.project.patterns.push(q);
                    }
                    10 => e.project.patterns[0].clips.get_mut(&key).unwrap()[0].vel = 0.9,
                    11 => {
                        let n = e.project.patterns[0].clips.remove(&key).unwrap();
                        e.project.patterns[0].clips.insert(key.to_uppercase(), n);
                    }
                    12 => e.project.patterns[0].clips.get_mut(&key).unwrap()[0].pitch = 128,
                    13 => e.project.patterns[0].clips.get_mut(&key).unwrap()[0].start = 64.0,
                    _ => {
                        let mut t = e.project.tracks[0].clone();
                        t.id = "t8888".into();
                        t.name = "Other".into();
                        e.project.tracks.insert(0, t);
                    }
                }
            }
            reject_unchanged(&f, &d, 0.25);
        }
    }
    #[test]
    fn real_adapter_unrelated_repairs_and_aliases_reject_without_live_repair() {
        for kind in 0..8 {
            let f = Fixture::new();
            let mut d = f.open();
            set_text(&mut d, "25");
            {
                let mut e = f.engine.lock().unwrap();
                match kind {
                    0 => e.project.schema_version = 0,
                    1 => {
                        let mut q = Pattern::new("Other", 1);
                        q.id = String::new();
                        e.project.patterns.push(q);
                    }
                    2 => {
                        let fx = e.project.master_effects[0].clone();
                        e.project.master_effects.push(fx);
                    }
                    3 => {
                        let mut q = Pattern::new("Other", 1);
                        q.id = "p8888".into();
                        q.notes_mut("elsewhere").push(Note::default());
                        e.project.patterns.push(q);
                    }
                    4 => {
                        let mut q = Pattern::new("Earlier", 1);
                        q.id = e.project.patterns[0].name.clone();
                        e.project.patterns.insert(0, q);
                    }
                    5 => {
                        let mut t = e.project.tracks[0].clone();
                        t.id = "t9999".into();
                        t.name = t.name.to_uppercase();
                        e.project.tracks.push(t);
                    }
                    6 => e.project.patterns[0].name = " A ".into(),
                    _ => {
                        let mut q = e.project.patterns[0].clone();
                        q.id = "p9999".into();
                        q.name = "a".into();
                        e.project.patterns.push(q);
                    }
                }
            }
            reject_unchanged(&f, &d, 0.25);
        }
        // Cross-kind numeric ID/name preprocessing is checked on exact final data.
        let mut p = project();
        let id = p.patterns[0].id.clone();
        p.tracks[0].name = id.clone();
        let old = p.patterns[0].clips.keys().next().unwrap().clone();
        let n = p.patterns[0].clips.remove(&old).unwrap();
        p.patterns[0].clips.insert(id.to_lowercase(), n);
        let c = cells(&p, 0, 0).unwrap();
        let a = final_selection(&p, 0, 0, &c[0].notes, 0.25).unwrap();
        let resolved = crate::ids::resolve_id_refs(&p, &a);
        assert_eq!(
            p.track_index(resolved["track"].as_str().unwrap()).unwrap(),
            0
        );
    }
    #[test]
    fn real_compiled_changed_rejected_noop_allowed_and_pattern_open_rejected() {
        let f = Fixture::new();
        {
            let mut e = f.engine.lock().unwrap();
            let mut q = Pattern::new("pl:1", 4);
            q.notes_mut("unrelated").push(Note::new(0.0, 1.0, 60, 0.5));
            e.project.patterns.push(q);
            let clip = placement(&e.project, 1);
            e.project.playlist.push(clip);
            e.project.arrangement.push(Section {
                pattern: "pl:1".into(),
                repeats: 1,
            });
            e.project.ensure_fx_ids();
        }
        let mut d = f.open();
        set_text(&mut d, "25");
        reject_unchanged(&f, &d, 0.25);
        {
            let mut e = f.engine.lock().unwrap();
            let name = e.project.tracks[0].name.clone();
            for n in &mut e.project.patterns[0].notes_mut(&name)[..2] {
                n.vel = 0.25;
            }
        }
        let d = f.open();
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let log = e.log.len();
        assert_eq!(
            guarded_apply(&mut e, &d, 0, 0, 0.25).unwrap(),
            Applied::NoChange
        );
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.log.len(), log);
        let p = e.project.clone();
        drop(e);
        let compiled = p.patterns.iter().position(|p| p.name == "pl:1").unwrap();
        assert!(capture(&f.engine, &p, 0, compiled).is_err());
    }
    #[test]
    fn real_capture_rejects_stale_visible_owner_and_finish_rejects_old_serial() {
        let f = Fixture::new();
        let p = f.engine.lock().unwrap().project.clone();
        {
            let mut e = f.engine.lock().unwrap();
            e.project.tracks[0].name = "Replacement".into();
        }
        assert!(capture(&f.engine, &p, 0, 0).is_err());
        let f = Fixture::new();
        let d = f.open();
        let mut state = EditorState::default();
        state.open(d.owner.clone()).unwrap();
        let serial = state.draft.as_ref().unwrap().serial;
        state.open(d.owner).unwrap();
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let log = e.log.len();
        assert!(state
            .finish(
                &mut e,
                Intent::Apply {
                    serial,
                    cell_serial: 1,
                    value: 0.25
                },
                0,
                0
            )
            .is_err());
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.log.len(), log);
        let old_cell = state.draft.as_ref().unwrap().cell_serial;
        let serial = state.draft.as_ref().unwrap().serial;
        state.draft.as_mut().unwrap().choose(1);
        assert!(state
            .finish(
                &mut e,
                Intent::Apply {
                    serial,
                    cell_serial: old_cell,
                    value: 0.8
                },
                0,
                0
            )
            .is_err());
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.log.len(), log);
        state.finish(&mut e, Intent::Cancel, 0, 0).unwrap();
        assert!(state.draft.is_none());
        assert_eq!(e.log.len(), log);
    }
    #[test]
    fn actual_invalid_direct_command_is_diagnostic_failed_log_not_undo() {
        let f = Fixture::new();
        let mut e = f.engine.lock().unwrap();
        let before = serialized(&e.project);
        let depth = e.undo_depth();
        let rev = e.revision;
        let log = e.log.len();
        let t = passive(&e);
        let name = e.project.tracks[0].name.clone();
        assert!(e
            .call_from(
                "set_note_velocity",
                &json!({"track":name,"pattern":"A","ids":["missing999"],"velocity":2.0}),
                "you"
            )
            .is_err());
        assert_eq!(serialized(&e.project), before);
        assert_eq!(e.undo_depth(), depth);
        assert_eq!(e.revision, rev);
        assert_eq!(e.log.len(), log + 1);
        assert!(!e.log.last().unwrap().ok);
        assert_eq!(passive(&e), t);
    }

    pub(crate) fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    // Local lifecycle helpers; key() remains a key-down event for all old callers.
    pub(crate) fn key_up(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }
    pub(crate) fn reveal_duration(ctx: &egui::Context) -> f64 {
        let animation = ctx.style().scroll_animation;
        assert_eq!(animation, egui::style::ScrollAnimation::default());
        assert!(animation.duration.max.is_finite() && animation.duration.max > 0.0);
        f64::from(animation.duration.max)
    }
    pub(crate) fn expected_keys(ctx: &egui::Context, events: &[Event]) -> Vec<(Key, bool, bool)> {
        let mut down = Vec::new();
        let mut expected = Vec::new();
        for event in events {
            if let Event::Key { key, pressed, .. } = event {
                let held = down
                    .iter()
                    .find(|(k, _)| k == key)
                    .map_or_else(|| ctx.input(|i| i.key_down(*key)), |(_, held)| *held);
                expected.push((*key, *pressed, *pressed && held));
                down.retain(|(k, _)| k != key);
                down.push((*key, *pressed));
            }
        }
        expected
    }
    pub(crate) fn normalized_keys(ctx: &egui::Context) -> Vec<(Key, bool, bool)> {
        ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| {
                    if let Event::Key {
                        key,
                        pressed,
                        repeat,
                        ..
                    } = event
                    {
                        Some((*key, *pressed, *repeat))
                    } else {
                        None
                    }
                })
                .collect()
        })
    }
    pub(crate) fn input_trace(ctx: &egui::Context, case: &str, phase: &str, pass: u64) {
        let focus = ctx.memory(|m| m.focused());
        ctx.input(|i| eprintln!(
            "case={case} phase={phase} pass={} time={:?} stable_dt={:?} predicted_dt={:?} focus={focus:?} wants_keyboard={} events={:?} Home_down={} Tab_down={} ArrowDown_down={}",
            pass, i.time, i.stable_dt, i.predicted_dt,
            focus.is_some(), i.events, i.key_down(Key::Home), i.key_down(Key::Tab), i.key_down(Key::ArrowDown)
        ));
    }
    pub(crate) fn reveal_trace(ctx: &egui::Context, case: &str, phase: &str, name: &str) {
        input_trace(ctx, case, phase, ctx.cumulative_pass_nr() - 1);
        let rows = responses(ctx);
        let (pass, _, r) = rows
            .iter()
            .rev()
            .find(|(_, n, _)| n == name)
            .unwrap_or_else(|| panic!("case={case} phase={phase} missing current response {name}"));
        eprintln!("case={case} phase={phase} response={name} pass={pass} id={:?} rect={:?} interact_rect={:?} size={:?} focus={:?} animation={:?}; exact UI clip and editor scroll outputs unavailable at existing test seam",
            r.id, r.rect, r.interact_rect, r.rect.size(), ctx.memory(|m| m.focused()), ctx.style().scroll_animation);
        assert_eq!(
            *pass,
            ctx.cumulative_pass_nr() - 1,
            "case={case} phase={phase} stale {name}"
        );
    }
    pub(crate) fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<Event> {
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            },
        ]
    }
    fn ui_frame(
        ctx: &egui::Context,
        state: &mut EditorState,
        size: egui::Vec2,
        dpi: f32,
        events: Vec<Event>,
    ) -> (Option<Intent>, egui::FullOutput) {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(dpi);
        let mut intent = None;
        let output = ctx.run(input, |ctx| {
            assert_eq!(ctx.pixels_per_point(), dpi);
            intent = show(ctx, state);
        });
        (intent, output)
    }
    fn ui_frame_at(
        ctx: &egui::Context,
        state: &mut EditorState,
        size: egui::Vec2,
        dpi: f32,
        events: Vec<Event>,
        now: f64,
        case: &str,
    ) -> (Option<Intent>, egui::FullOutput) {
        assert!(
            now.is_finite() && now > ctx.input(|i| i.time),
            "case={case} nonmonotonic time={now}"
        );
        let expected = expected_keys(ctx, &events);
        let supplied = events.clone();
        let first_pass = ctx.cumulative_pass_nr();
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            time: Some(now),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(dpi);
        let mut intents = Vec::new();
        let output = ctx.run(input, |ctx| {
            input_trace(ctx, case, "before-show", ctx.cumulative_pass_nr());
            assert_eq!(ctx.input(|i| i.time), now, "case={case}");
            if ctx.cumulative_pass_nr() == first_pass {
                assert_eq!(
                    normalized_keys(ctx),
                    expected,
                    "case={case} supplied={supplied:?}"
                );
            }
            assert_eq!(ctx.pixels_per_point(), dpi);
            let pass = ctx.cumulative_pass_nr();
            let pass_intent = show(ctx, state);
            if let Some(intent) = pass_intent {
                intents.push((pass, intent));
            }
            input_trace(ctx, case, "after-show", ctx.cumulative_pass_nr());
            eprintln!(
                "case={case} pass={} time={now} intent={pass_intent:?} draft={:?}",
                ctx.cumulative_pass_nr(),
                state
                    .draft
                    .as_ref()
                    .map(|d| (d.serial, d.cell_serial, d.cell))
            );
        });
        eprintln!(
            "case={case} time={now} completed_passes={} cumulative_pass={}",
            output.platform_output.num_completed_passes,
            ctx.cumulative_pass_nr()
        );
        assert!(
            intents.len() <= 1,
            "case={case} multiple non-None Intents in one call: pass/intent={intents:?}"
        );
        let intent = intents.pop().map(|(_, intent)| intent);
        (intent, output)
    }
    fn painted(output: &egui::FullOutput, label: &str, dpi: f32) -> bool {
        output.shapes.iter().any(|shape| {
            let egui::Shape::Text(text) = &shape.shape else {
                return false;
            };
            if text.galley.text() != label {
                return false;
            }
            assert!(!text.galley.elided, "elided {label}");
            let rect = Rect::from_min_size(text.pos, text.galley.size());
            assert!(
                shape.clip_rect.expand(1.0 / dpi).contains_rect(rect),
                "clipped {label}: {rect:?}"
            );
            true
        })
    }
    #[test]
    fn real_window_current_pass_responses_painted_labels_dpi_and_enlarged_style() {
        for dpi in [1.0, 2.0] {
            for enlarged in [false, true] {
                let ctx = egui::Context::default();
                if enlarged {
                    let mut style = (*ctx.style()).clone();
                    for font in style.text_styles.values_mut() {
                        font.size *= 1.3;
                    }
                    ctx.set_style(style);
                }
                let p = project();
                let mut state = EditorState::default();
                state.open(owner(&p)).unwrap();
                let size = vec2(620.0, 680.0);
                ui_frame(&ctx, &mut state, size, dpi, vec![]);
                let (_, output) = ui_frame(&ctx, &mut state, size, dpi, vec![]);
                for name in ["cell-0", "cell-1", "cell-2", "text", "apply", "cancel"] {
                    let r = response(&ctx, name);
                    assert!(r.rect.is_positive());
                    assert!(r.interact_rect.contains_rect(r.rect));
                }
                assert!(!response(&ctx, "apply").enabled());
                for label in [
                    "Mixed",
                    "Set all 2 hits in this step to (%)",
                    "0% silences hits; it does not delete them.",
                    "Apply",
                    "Cancel",
                ] {
                    assert!(painted(&output, label, dpi), "{label}");
                }
                assert!(painted(
                    &output,
                    &format!("Channel: {}", p.tracks[0].name),
                    dpi
                ));
                assert!(painted(&output, "Pattern: A", dpi));
                let (intent, _) = ui_frame(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![Event::Text("25".into()), key(Key::Enter, Modifiers::NONE)],
                );
                assert_eq!(intent, None);
                assert_eq!(state.draft.as_ref().unwrap().value().unwrap(), 0.25);
                let r = response(&ctx, "apply");
                let pos = r.rect.center();
                ui_frame(&ctx, &mut state, size, dpi, pointer(pos, true));
                let (intent, _) = ui_frame(&ctx, &mut state, size, dpi, pointer(pos, false));
                assert!(matches!(intent, Some(Intent::Apply { value: 0.25, .. })));
            }
        }
    }
    #[test]
    fn real_group_tab_transfer_escape_outside_and_viewport_blur() {
        for dpi in [1.0, 2.0] {
            let ctx = egui::Context::default();
            let p = project();
            let mut state = EditorState::default();
            state.open(owner(&p)).unwrap();
            let size = vec2(620.0, 680.0);
            ui_frame(&ctx, &mut state, size, dpi, vec![]);
            ui_frame(&ctx, &mut state, size, dpi, vec![]);
            let id = response(&ctx, "text").id;
            ctx.memory_mut(|m| m.request_focus(id));
            let (intent, _) = ui_frame(
                &ctx,
                &mut state,
                size,
                dpi,
                vec![key(Key::Tab, Modifiers::NONE)],
            );
            assert_eq!(intent, None);
            assert!(state.draft.is_some());
            assert_eq!(state.draft.as_ref().unwrap().text, "");
            assert!(!response(&ctx, "apply").enabled());
            assert!(!response(&ctx, "apply").sense.focusable);
            assert!(response(&ctx, "cancel").enabled());
            assert!(response(&ctx, "cancel").has_focus());
            // Reverse traversal in pinned egui settles on the following pass.
            // Assert both that pass and a further idle pass stay in the group.
            for text in ["", "not a percent", "101"] {
                let (intent, _) = ui_frame(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key(Key::Tab, Modifiers::SHIFT)],
                );
                assert_eq!(intent, None);
                for _ in 0..2 {
                    let (intent, _) = ui_frame(&ctx, &mut state, size, dpi, vec![]);
                    assert_eq!(intent, None);
                    assert!(response(&ctx, "text").has_focus());
                }
                let mut events = vec![key(Key::A, Modifiers::COMMAND)];
                events.push(Event::Text(text.into()));
                let (intent, _) = ui_frame(&ctx, &mut state, size, dpi, events);
                assert_eq!(intent, None);
                assert_eq!(state.draft.as_ref().unwrap().text, text);
                assert!(!response(&ctx, "apply").enabled());
                let (intent, _) = ui_frame(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key(Key::Enter, Modifiers::NONE)],
                );
                assert_eq!(intent, None);
                assert!(response(&ctx, "text").has_focus());
                let (intent, _) = ui_frame(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key(Key::Tab, Modifiers::NONE)],
                );
                assert_eq!(intent, None);
                assert!(response(&ctx, "cancel").enabled());
                assert!(response(&ctx, "cancel").has_focus());
                let (intent, _) = ui_frame(&ctx, &mut state, size, dpi, vec![]);
                assert_eq!(intent, None);
                assert!(response(&ctx, "cancel").has_focus());
            }
            // Actual outside focus is still cancellation, not a Tab exception.
            let mut intent = None;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.button("Outside editor").request_focus();
                });
                intent = show(ctx, &mut state);
            });
            assert_eq!(intent, Some(Intent::Cancel));
            let (intent, _) = ui_frame(
                &ctx,
                &mut state,
                size,
                dpi,
                vec![key(Key::Escape, Modifiers::NONE)],
            );
            assert_eq!(intent, Some(Intent::Cancel));
            let (intent, _) = ui_frame(
                &ctx,
                &mut state,
                size,
                dpi,
                pointer(pos2(610.0, 670.0), true),
            );
            assert_eq!(intent, Some(Intent::Cancel));
            let mut raw = egui::RawInput {
                focused: false,
                ..Default::default()
            };
            raw.viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .focused = Some(false);
            let mut intent = None;
            let _ = ctx.run(raw, |ctx| intent = show(ctx, &mut state));
            assert_eq!(intent, Some(Intent::Cancel));
        }
    }
    #[test]
    fn real_cell_choice_resets_field_ids_text_and_last_cell_is_reachable() {
        let ctx = egui::Context::default();
        let p = project();
        let mut state = EditorState::default();
        state.open(owner(&p)).unwrap();
        let size = vec2(620.0, 680.0);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        let old = response(&ctx, "text").id;
        ui_frame(&ctx, &mut state, size, 1.0, vec![Event::Text("25".into())]);
        let cell = response(&ctx, "cell-2");
        ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            pointer(cell.rect.center(), true),
        );
        ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            pointer(cell.rect.center(), false),
        );
        assert_eq!(
            state.draft.as_ref().unwrap().cells[state.draft.as_ref().unwrap().cell].step,
            63
        );
        assert_ne!(response(&ctx, "text").id, old);
        assert_ne!(state.draft.as_ref().unwrap().text, "25");
        let visited = response(&ctx, "text").id;
        state.draft.as_mut().unwrap().choose(0);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        assert_ne!(response(&ctx, "text").id, old);
        assert_ne!(response(&ctx, "text").id, visited);
        assert!(state.draft.as_ref().unwrap().text.is_empty());
        let old = response(&ctx, "text").id;
        state.open(owner(&p)).unwrap();
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        assert_ne!(response(&ctx, "text").id, old);
    }
    #[test]
    fn real_shortcut_gate_open_edit_apply_focus_and_closing_frames() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        let keys = [
            Key::Space,
            Key::Enter,
            Key::Home,
            Key::Delete,
            Key::Z,
            Key::M,
            Key::S,
            Key::OpenBracket,
            Key::CloseBracket,
            Key::R,
        ];
        for active in [false, true] {
            for consumed in [false, true] {
                for key_code in keys {
                    if active {
                        state.open(owner(&project())).unwrap();
                    } else {
                        state.draft = None;
                    }
                    let mut actions = Vec::new();
                    let _ = ctx.run(
                        egui::RawInput {
                            events: vec![key(key_code, Modifiers::NONE)],
                            ..Default::default()
                        },
                        |ctx| actions = global_actions(ctx, &state, active, consumed),
                    );
                    if active || consumed {
                        assert!(actions.is_empty());
                    }
                }
            }
        }
        for modifiers in [
            Modifiers::CTRL,
            Modifiers::COMMAND,
            Modifiers {
                shift: true,
                command: true,
                ..Default::default()
            },
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![key(Key::Z, modifiers)],
                    ..Default::default()
                },
                |ctx| assert!(global_actions(ctx, &state, true, false).is_empty()),
            );
        }
    }
    #[test]
    fn labels_remain_manual_and_production_precedes_tests() {
        let src = include_str!("step_velocity.rs");
        let production = src.split("\n#[cfg(test)]").next().unwrap();
        assert!(production.contains("pub(super) fn show("));
        assert!(production.contains("pub(super) fn guarded_apply("));
        for label in [
            "Step velocity",
            "Mixed",
            "No hits in this pattern",
            "Velocity (%)",
            "Apply",
            "Cancel",
            "0% silences hits; it does not delete them.",
        ] {
            assert!(production.contains(label));
        }
        // New module is outside the immutable old inventory; run the equivalent literal wording rules here.
        let mut string = false;
        let mut escaped = false;
        let mut text = String::new();
        for line in production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
        {
            for c in line.chars() {
                if string {
                    if escaped {
                        text.push(c);
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        let ident = text
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                        if !ident {
                            let low = text.to_lowercase();
                            assert!(
                                !low.split(|c: char| !c.is_ascii_alphanumeric())
                                    .any(|w| crate::gui_words::BANNED.contains(&w)),
                                "{text}"
                            );
                            for phrase in ["make with", "tool call", "auto-produce", "auto produce"]
                            {
                                assert!(!low.contains(phrase));
                            }
                        }
                        text.clear();
                        string = false;
                    } else {
                        text.push(c);
                    }
                } else if c == '"' {
                    string = true;
                }
            }
        }
    }

    #[test]
    fn actual_engine_compiled_refresh_predicate_agrees_with_guard_on_name_cases() {
        for kind in 0..7 {
            let f = Fixture::new();
            {
                let mut e = f.engine.lock().unwrap();
                let mut clip = placement(&e.project, 1);
                match kind {
                    0 => {}
                    1 => clip.pattern = "a".into(),
                    2 => clip.pattern = " A ".into(),
                    3 => clip.pattern = e.project.patterns[0].id.clone(),
                    _ => {}
                }
                e.project.playlist.push(clip);
                e.project.patterns.push(Pattern::new("pl:retained", 4));
                e.project.arrangement.push(Section {
                    pattern: "pl:retained".into(),
                    repeats: 1,
                });
                if kind == 4 {
                    e.project.arrangement.push(Section {
                        pattern: "A".into(),
                        repeats: 1,
                    });
                }
                if kind == 5 {
                    e.project.playlist.clear();
                }
                if kind == 6 {
                    e.project.arrangement.clear();
                }
                e.project.ensure_fx_ids();
            }
            let mut d = f.open();
            set_text(&mut d, "25");
            let mut e = f.engine.lock().unwrap();
            let excluded = compiled_source(&e.project, 0);
            let before = serialized(&e.project);
            let mut only_vel = before.clone();
            let name = e.project.tracks[0].name.to_lowercase();
            only_vel["patterns"][0]["clips"][&name][1]["vel"] = json!(0.25);
            let a = final_selection(&e.project, 0, 0, &d.cells[0].notes, 0.25).unwrap();
            e.call_from("set_note_velocity", &a, "you").unwrap();
            // This exercises engine.rs's real private refresh predicate through dispatch,
            // not merely a second copy of the GUI predicate.
            if excluded {
                assert_ne!(serialized(&e.project), only_vel);
                assert!(!e.project.patterns.iter().any(|p| p.name == "pl:retained"));
            } else {
                assert_eq!(serialized(&e.project), only_vel);
            }
        }
    }
    #[test]
    fn real_missing_unrelated_fx_identity_and_nonfinite_clone_equality_fail_closed() {
        let f = Fixture::new();
        let mut d = f.open();
        set_text(&mut d, "25");
        {
            f.engine.lock().unwrap().project.master_effects[0].set_id("");
        }
        reject_unchanged(&f, &d, 0.25);
        let mut p = project();
        p.bpm = f32::NAN;
        let bits = p.bpm.to_bits();
        assert!(identity_guard(&p).is_err());
        assert_eq!(p.bpm.to_bits(), bits);
    }
    #[test]
    fn real_window_close_uses_actual_focused_decoration_response_not_guessed_geometry() {
        let ctx = egui::Context::default();
        let p = project();
        let mut state = EditorState::default();
        state.open(owner(&p)).unwrap();
        let size = vec2(620.0, 680.0);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        let known: HashSet<_> = responses(&ctx)
            .iter()
            .filter(|(_, n, _)| n != "window-focus")
            .map(|(_, _, r)| r.id)
            .collect();
        let mut closed = false;
        for _ in 0..24 {
            let (intent, _) = ui_frame(
                &ctx,
                &mut state,
                size,
                1.0,
                vec![key(Key::Tab, Modifiers::NONE)],
            );
            assert_eq!(intent, None, "Tab inside this sole window is not a blur");
            if let Some(id) = ctx.memory(|m| m.focused()).filter(|id| !known.contains(id)) {
                let r = response(&ctx, "window-focus");
                assert_eq!(r.id, id);
                assert!(r.rect.is_positive());
                let (intent, _) = ui_frame(
                    &ctx,
                    &mut state,
                    size,
                    1.0,
                    vec![key(Key::Space, Modifiers::NONE)],
                );
                if intent == Some(Intent::Cancel) {
                    let (pass, open) = ctx
                        .data(|d| d.get_temp::<(u64, bool)>(egui::Id::new("velocity-window-open")))
                        .unwrap();
                    assert_eq!(pass, ctx.cumulative_pass_nr() - 1);
                    assert!(
                        !open,
                        "Must be the actual Window close outcome, not a group-blur cancellation"
                    );
                    closed = true;
                    break;
                }
            }
        }
        assert!(
            closed,
            "A real focused title-bar close control must close the editor"
        );
    }
    #[test]
    fn real_short_viewport_list_navigation_and_outer_actions_remain_reachable() {
        for dpi in [1.0, 2.0] {
            for enlarged in [false, true] {
                let ctx = egui::Context::default();
                if enlarged {
                    let mut style = (*ctx.style()).clone();
                    for f in style.text_styles.values_mut() {
                        f.size *= 1.3;
                    }
                    ctx.set_style(style);
                }
                let mut p = project();
                p.patterns[0].bars = 64;
                let name = p.tracks[0].name.clone();
                *p.patterns[0].notes_mut(&name) = (0..1024)
                    .map(|i| Note {
                        id: format!("long{i}"),
                        start: i as f32,
                        ..Default::default()
                    })
                    .collect();
                p.ensure_fx_ids();
                let mut state = EditorState::default();
                state.open(owner(&p)).unwrap();
                let size = vec2(460.0, 380.0);
                let case = format!("last-cell dpi={dpi} enlarged={enlarged}");
                let d = f64::from(egui::RawInput::default().predicted_dt);
                let duration = reveal_duration(&ctx);
                let mut now = d;
                ui_frame_at(&ctx, &mut state, size, dpi, vec![], now, &case);
                now += d;
                ui_frame_at(&ctx, &mut state, size, dpi, vec![], now, &case);
                now += d;
                let first = response(&ctx, "cell-0");
                ctx.memory_mut(|m| m.request_focus(first.id));
                let (intent, _) = ui_frame_at(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key(Key::ArrowDown, Modifiers::NONE)],
                    now,
                    &case,
                );
                assert_eq!(intent, None);
                assert_eq!(state.draft.as_ref().unwrap().cell, 1);
                let last = response(&ctx, "cell-1023");
                ctx.memory_mut(|m| m.request_focus(last.id));
                let t0 = now + d;
                ui_frame_at(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key_up(Key::ArrowDown)],
                    t0,
                    &case,
                );
                reveal_trace(&ctx, &case, "last-cell initiating", "cell-1023");
                let deadline = t0 + duration;
                ui_frame_at(&ctx, &mut state, size, dpi, vec![], deadline, &case);
                reveal_trace(&ctx, &case, "last-cell configured deadline", "cell-1023");
                let fresh_layout = deadline + d;
                ui_frame_at(&ctx, &mut state, size, dpi, vec![], fresh_layout, &case);
                reveal_trace(&ctx, &case, "last-cell fresh layout", "cell-1023");
                now = fresh_layout + d;
                assert_eq!(ctx.memory(|m| m.focused()), Some(last.id), "case={case}");
                let last = response(&ctx, "cell-1023");
                assert!(last.interact_rect.contains_rect(last.rect));
                let (intent, _) = ui_frame_at(
                    &ctx,
                    &mut state,
                    size,
                    dpi,
                    vec![key(Key::Space, Modifiers::NONE)],
                    now,
                    &case,
                );
                assert_eq!(intent, None);
                assert_eq!(state.draft.as_ref().unwrap().cell, 1023);
                for name in ["apply", "cancel"] {
                    let action = response(&ctx, name);
                    ctx.memory_mut(|m| m.request_focus(action.id));
                    let action_case = format!("{case} action={name}");
                    let t0 = now + d;
                    ui_frame_at(
                        &ctx,
                        &mut state,
                        size,
                        dpi,
                        vec![key_up(Key::Space)],
                        t0,
                        &action_case,
                    );
                    reveal_trace(&ctx, &action_case, "initiating", name);
                    let deadline = t0 + duration;
                    ui_frame_at(&ctx, &mut state, size, dpi, vec![], deadline, &action_case);
                    reveal_trace(&ctx, &action_case, "configured deadline", name);
                    let fresh_layout = deadline + d;
                    ui_frame_at(
                        &ctx,
                        &mut state,
                        size,
                        dpi,
                        vec![],
                        fresh_layout,
                        &action_case,
                    );
                    reveal_trace(&ctx, &action_case, "fresh layout", name);
                    now = fresh_layout + d;
                    let action = response(&ctx, name);
                    assert_eq!(
                        ctx.memory(|m| m.focused()),
                        Some(action.id),
                        "case={action_case}"
                    );
                    assert!(action.enabled());
                    assert!(action.interact_rect.contains_rect(action.rect));
                    // Exercise the real focused button, not a synthetic Intent.
                    let (intent, _) = ui_frame_at(
                        &ctx,
                        &mut state,
                        size,
                        dpi,
                        vec![key(Key::Space, Modifiers::NONE)],
                        now,
                        &action_case,
                    );
                    if name == "apply" {
                        let draft = state.draft.as_ref().unwrap();
                        assert_eq!(
                            intent,
                            Some(Intent::Apply {
                                serial: draft.serial,
                                cell_serial: draft.cell_serial,
                                value: draft.value().unwrap(),
                            })
                        );
                    } else {
                        assert_eq!(intent, Some(Intent::Cancel));
                    }
                }
            }
        }
    }
    #[test]
    fn real_field_text_undo_stays_local_and_cancel_transfer_does_not_commit() {
        let f = Fixture::new();
        let d = f.open();
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        state.open(d.owner).unwrap();
        let size = vec2(620.0, 680.0);
        let before = {
            let e = f.engine.lock().unwrap();
            (
                serialized(&e.project),
                e.history(),
                e.revision,
                e.log.len(),
                passive(&e),
            )
        };
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        ui_frame(&ctx, &mut state, size, 1.0, vec![Event::Text("25".into())]);
        ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Z, Modifiers::COMMAND)],
        );
        assert_eq!(state.draft.as_ref().unwrap().text, "");
        assert!(!response(&ctx, "apply").enabled());
        let (intent, _) = ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(intent, None);
        assert!(response(&ctx, "cancel").enabled());
        assert!(response(&ctx, "cancel").has_focus());
        let (intent, _) = ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Tab, Modifiers::SHIFT)],
        );
        assert_eq!(intent, None);
        for _ in 0..2 {
            let (intent, _) = ui_frame(&ctx, &mut state, size, 1.0, vec![]);
            assert_eq!(intent, None);
            assert!(response(&ctx, "text").has_focus());
        }
        // Enabled-Apply traversal is a separate deliberate valid edit.
        let (intent, _) = ui_frame(&ctx, &mut state, size, 1.0, vec![Event::Text("25".into())]);
        assert_eq!(intent, None);
        assert_eq!(state.draft.as_ref().unwrap().text, "25");
        assert!(response(&ctx, "apply").enabled());
        let (intent, _) = ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(intent, None);
        assert!(response(&ctx, "apply").has_focus());
        let (intent, _) = ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(intent, None);
        assert!(response(&ctx, "cancel").has_focus());
        let (intent, _) = ui_frame(
            &ctx,
            &mut state,
            size,
            1.0,
            vec![key(Key::Space, Modifiers::NONE)],
        );
        assert_eq!(intent, Some(Intent::Cancel));
        let mut e = f.engine.lock().unwrap();
        state.finish(&mut e, intent.unwrap(), 0, 0).unwrap();
        assert_eq!(
            (
                serialized(&e.project),
                e.history(),
                e.revision,
                e.log.len(),
                passive(&e)
            ),
            before
        );
    }

    #[test]
    fn actual_reload_reuses_stable_ids_but_expires_and_new_project_expires() {
        for new_project in [false, true] {
            let f = Fixture::new();
            let mut d = f.open();
            set_text(&mut d, "25");
            {
                let mut e = f.engine.lock().unwrap();
                let generation = e.project_load_generation;
                if new_project {
                    e.call_from("new_project", &json!({"name":"Replacement"}), "you")
                        .unwrap();
                } else {
                    let ids = (
                        e.project.tracks[0].id.clone(),
                        e.project.patterns[0].id.clone(),
                    );
                    e.call_from("save_project", &json!({"path":"reload.json"}), "you")
                        .unwrap();
                    e.call_from("load_project", &json!({"path":"reload.json"}), "you")
                        .unwrap();
                    assert_eq!(
                        (
                            e.project.tracks[0].id.clone(),
                            e.project.patterns[0].id.clone()
                        ),
                        ids
                    );
                    assert!(e.project_load_generation > generation);
                }
            }
            reject_unchanged(&f, &d, 0.25);
        }
    }

    #[test]
    fn real_empty_editor_has_no_hits_no_armed_apply_and_usable_cancel() {
        let ctx = egui::Context::default();
        let mut p = project();
        let name = p.tracks[0].name.clone();
        p.patterns[0].notes_mut(&name).clear();
        let mut state = EditorState::default();
        state.open(owner(&p)).unwrap();
        let size = vec2(620.0, 680.0);
        ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        let (intent, output) = ui_frame(&ctx, &mut state, size, 1.0, vec![]);
        assert_eq!(intent, None);
        assert!(painted(&output, "No hits in this pattern", 1.0));
        assert!(!response(&ctx, "apply").enabled());
        let cancel = response(&ctx, "cancel");
        assert!(cancel.enabled());
        let pos = cancel.rect.center();
        ui_frame(&ctx, &mut state, size, 1.0, pointer(pos, true));
        let (intent, _) = ui_frame(&ctx, &mut state, size, 1.0, pointer(pos, false));
        assert_eq!(intent, Some(Intent::Cancel));
    }
    #[test]
    fn real_occupied_labels_use_actual_meter_and_final_bar_not_compound_beats() {
        for dpi in [1.0, 2.0] {
            for (meter, steps) in [((3, 4), 12), ((6, 8), 12), ((7, 8), 14)] {
                let ctx = egui::Context::default();
                let mut p = project();
                p.patterns[0].meter = Some(meter);
                let name = p.tracks[0].name.clone();
                *p.patterns[0].notes_mut(&name) = vec![Note {
                    id: "last999".into(),
                    start: (4 * steps - 1) as f32 + 0.5,
                    ..Default::default()
                }];
                p.ensure_fx_ids();
                let mut state = EditorState::default();
                state.open(owner(&p)).unwrap();
                let size = vec2(620.0, 680.0);
                ui_frame(&ctx, &mut state, size, dpi, vec![]);
                let (_, output) = ui_frame(&ctx, &mut state, size, dpi, vec![]);
                assert!(painted(
                    &output,
                    &format!("Bar 4, sixteenth {steps} of {steps}: 1 hits"),
                    dpi
                ));
                assert!(response(&ctx, "apply").enabled());
            }
        }
    }
}
