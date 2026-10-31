// Adapted from SoundCraft `crates/ui-egui/src/midi_editor.rs` (commit eac0edd): note drawing
// with velocity shading, keyboard strip, bar/grid lines, click-to-add, drag to move or resize
// with grid snapping, rubber-band and shift-click selection, arrow-key nudge/transpose, Delete,
// a header toolbar of whole-pattern ops and a velocity lane.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! The piano roll panel. Notes are in Beatbox steps (16ths); the snap grid comes from the
//! PPQ timebase (`timebase::NoteValue`). Every edit is ONE engine tool call
//! (`add_notes` with `replace`, or `quantize` / `legato` / `edit_notes` for toolbar ops), so
//! a gesture is one undo step and an MCP client watching the project sees the same change.

use super::theme::Tokens;
use super::widgets::*;
use super::Studio;
use crate::note_edit::{apply_shift, snap_to, NoteShift};
use crate::project::{Note, Project};
use crate::theory;
use crate::timebase::{tick_to_step, NoteValue};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke,
};
use serde_json::{json, Value};

fn matching_pitch_selection(notes: &[Note], selected: &[usize]) -> Vec<usize> {
    let pitches: std::collections::HashSet<u8> = selected
        .iter()
        .filter_map(|i| notes.get(*i))
        .map(|n| n.pitch)
        .collect();
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| pitches.contains(&n.pitch).then_some(i))
        .collect()
}

fn matching_onset_selection(notes: &[Note], selected: &[usize]) -> Vec<usize> {
    let mut starts: Vec<f32> = selected
        .iter()
        .filter_map(|i| notes.get(*i))
        .map(|n| n.start)
        .filter(|t| t.is_finite())
        .collect();
    starts.sort_by(f32::total_cmp);
    starts.dedup();
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            if !n.start.is_finite() {
                return None;
            }
            let at = starts.partition_point(|t| *t < n.start);
            let hit = starts
                .get(at)
                .is_some_and(|t| (n.start - t).abs() <= 0.0001)
                || at
                    .checked_sub(1)
                    .and_then(|j| starts.get(j))
                    .is_some_and(|t| (n.start - t).abs() <= 0.0001);
            hit.then_some(i)
        })
        .collect()
}

fn matching_length_selection(notes: &[Note], selected: &[usize]) -> Vec<usize> {
    let mut lengths: Vec<f32> = selected
        .iter()
        .filter_map(|i| notes.get(*i))
        .map(|n| n.len)
        .filter(|len| len.is_finite() && *len > 0.)
        .collect();
    lengths.sort_by(f32::total_cmp);
    lengths.dedup();
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            if !n.len.is_finite() || n.len <= 0. {
                return None;
            }
            let at = lengths.partition_point(|len| *len < n.len);
            let matches = lengths
                .get(at)
                .is_some_and(|len| (n.len - len).abs() <= 0.0001)
                || at
                    .checked_sub(1)
                    .and_then(|j| lengths.get(j))
                    .is_some_and(|len| (n.len - len).abs() <= 0.0001);
            matches.then_some(i)
        })
        .collect()
}

fn inverted_note_selection(count: usize, selected: &[usize]) -> Vec<usize> {
    let selected: std::collections::HashSet<usize> = selected.iter().copied().collect();
    (0..count).filter(|i| !selected.contains(i)).collect()
}

fn overlapping_pitch_selection(notes: &[Note]) -> Vec<usize> {
    let mut by_pitch = std::collections::BTreeMap::<u8, Vec<usize>>::new();
    for (i, n) in notes.iter().enumerate() {
        if n.start.is_finite() && n.len.is_finite() && n.len > 0.0 && (n.start + n.len).is_finite()
        {
            by_pitch.entry(n.pitch).or_default().push(i);
        }
    }
    let mut found = std::collections::BTreeSet::new();
    for indices in by_pitch.values_mut() {
        indices.sort_by(|a, b| notes[*a].start.total_cmp(&notes[*b].start));
        let mut furthest: Option<usize> = None;
        for &i in indices.iter() {
            if let Some(j) = furthest {
                if notes[i].start < notes[j].start + notes[j].len {
                    found.insert(i);
                    found.insert(j);
                }
                if notes[i].start + notes[i].len > notes[j].start + notes[j].len {
                    furthest = Some(i);
                }
            } else {
                furthest = Some(i);
            }
        }
    }
    found.into_iter().collect()
}

fn duplicate_copy_selection(notes: &[Note]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let bits = |v: f32| if v == 0.0 { 0 } else { v.to_bits() };
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            if [n.start, n.len, n.vel, n.prob, n.offset]
                .iter()
                .any(|v| v.is_nan())
            {
                return None;
            }
            (!seen.insert((
                bits(n.start),
                bits(n.len),
                n.pitch,
                bits(n.vel),
                bits(n.prob),
                bits(n.offset),
                n.slide_to,
            )))
            .then_some(i)
        })
        .collect()
}

fn beyond_pattern_selection(notes: &[Note], steps: f32) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            (n.start.is_finite()
                && n.len.is_finite()
                && n.len > 0.0
                && (n.start >= steps || n.start + n.len > steps))
                .then_some(i)
        })
        .collect()
}

fn silent_note_selection(notes: &[Note]) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (n.vel == 0.0 || n.prob == 0.0).then_some(i))
        .collect()
}

fn slide_note_selection(notes: &[Note]) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.slide_to.is_some().then_some(i))
        .collect()
}

fn expression_note_selection(notes: &[Note], timing: bool) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let matches = if timing {
                n.offset.is_finite() && n.offset != 0.0
            } else {
                n.prob.is_finite() && (0.0..1.0).contains(&n.prob)
            };
            matches.then_some(i)
        })
        .collect()
}

fn outside_scale_selection(notes: &[Note], root: &str, scale: &str) -> Option<Vec<usize>> {
    let root = theory::pitch_class(root).ok()?;
    let intervals = theory::scale_intervals(scale).ok()?;
    Some(
        notes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                (!intervals.contains(&((n.pitch % 12 + 12 - root) % 12))).then_some(i)
            })
            .collect(),
    )
}

/// Resolve shared-onset stems to a selected chord voice before storage-order ties.
fn nearest_velocity_note(notes: &[Note], selected: &[usize], at: f32, cell: f32) -> Option<usize> {
    let distance = |n: &Note| ((n.start - at) * cell).abs();
    let (nearest, best) = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (i, distance(n)))
        .filter(|(_, d)| d.is_finite() && *d < 8.0)
        .min_by(|(a, ad), (b, bd)| ad.total_cmp(bd).then_with(|| a.cmp(b)))?;
    // Selection order and duplicates must not change storage-order tie breaking.
    selected
        .iter()
        .copied()
        .filter(|i| {
            notes
                .get(*i)
                .is_some_and(|n| distance(n).total_cmp(&best).is_eq())
        })
        .min()
        .or(Some(nearest))
}

fn velocity_readout_label(pitch: u8, velocity: f32) -> String {
    format!(
        "{}: {}%",
        theory::note_name(pitch),
        (velocity * 100.0).round()
    )
}

fn velocity_label_origin(lane: Rect, desired: Pos2, size: egui::Vec2) -> Pos2 {
    let left = lane.left() + 2.0;
    let top = lane.top() + 2.0;
    pos2(
        desired
            .x
            .clamp(left, (lane.right() - 2.0 - size.x).max(left)),
        desired
            .y
            .clamp(top, (lane.bottom() - 2.0 - size.y).max(top)),
    )
}

fn velocity_lane_value(bottom: f32, height: f32, pointer_y: f32) -> Option<f32> {
    if !bottom.is_finite() || !height.is_finite() || height <= 4.0 || !pointer_y.is_finite() {
        return None;
    }
    let value = (bottom - 2.0 - pointer_y) / (height - 4.0);
    value.is_finite().then(|| value.clamp(0.0, 1.0))
}

fn unique_note_index(notes: &[Note], id: &str) -> Option<usize> {
    let mut matches = notes.iter().enumerate().filter(|(_, n)| n.id == id);
    let first = matches.next()?.0;
    (matches.next().is_none() && !id.is_empty()).then_some(first)
}

fn note_identity_matches(notes: &[Note], ids: &[String]) -> bool {
    notes.len() == ids.len() && notes.iter().zip(ids).all(|(n, id)| n.id == *id)
}

fn remap_note_selection(notes: &[Note], previous_ids: &[String], selected: &[usize]) -> Vec<usize> {
    let ids: std::collections::HashSet<&str> = selected
        .iter()
        .filter_map(|i| previous_ids.get(*i))
        .map(String::as_str)
        .collect();
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| ids.contains(n.id.as_str()).then_some(i))
        .collect()
}

fn selected_index_mask(count: usize, selected: &[usize]) -> Vec<bool> {
    let mut mask = vec![false; count];
    for &i in selected {
        if let Some(value) = mask.get_mut(i) {
            *value = true;
        }
    }
    mask
}

fn selection_from_ids(notes: &[Note], ids: &[String]) -> Vec<usize> {
    let ids: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    notes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| ids.contains(n.id.as_str()).then_some(i))
        .collect()
}

fn note_expression_hint(note: &Note) -> String {
    let mut lines = Vec::new();
    if note.prob.is_finite() {
        lines.push(format!("Trigger chance: {:.1}%", note.prob * 100.0));
    }
    if note.offset.is_finite() {
        lines.push(format!("Timing offset: {:+.3} steps", note.offset));
    }
    if let Some(pitch) = note.slide_to {
        lines.push(format!(
            "Slide to: {} (MIDI {})",
            theory::note_name(pitch),
            pitch
        ));
    }
    lines.join("\n")
}

fn selection_summary(notes: &[Note], selected: &[usize]) -> Option<String> {
    let mut chances: Option<(f32, f32)> = None;
    let mut offsets: Option<(f32, f32)> = None;
    let mut span: Option<(f32, f32)> = None;
    let mut pitches: Option<(u8, u8)> = None;
    let mut lengths: Option<(f32, f32)> = None;
    let mut velocities: Option<(f32, f32)> = None;
    for n in selected.iter().filter_map(|i| notes.get(*i)) {
        if n.prob.is_finite() {
            chances = Some(chances.map_or((n.prob, n.prob), |(lo, hi)| {
                (lo.min(n.prob), hi.max(n.prob))
            }));
        }
        if n.offset.is_finite() {
            offsets = Some(offsets.map_or((n.offset, n.offset), |(lo, hi)| {
                (lo.min(n.offset), hi.max(n.offset))
            }));
        }
        let end = n.start + n.len;
        if n.start.is_finite() && n.len.is_finite() && n.len >= 0. && end.is_finite() {
            span = Some(span.map_or((n.start, end), |(lo, hi)| (lo.min(n.start), hi.max(end))));
        }
        pitches = Some(pitches.map_or((n.pitch, n.pitch), |(lo, hi)| {
            (lo.min(n.pitch), hi.max(n.pitch))
        }));
        if n.len.is_finite() {
            lengths =
                Some(lengths.map_or((n.len, n.len), |(lo, hi)| (lo.min(n.len), hi.max(n.len))));
        }
        if n.vel.is_finite() {
            velocities =
                Some(velocities.map_or((n.vel, n.vel), |(lo, hi)| (lo.min(n.vel), hi.max(n.vel))));
        }
    }
    let (lo, hi) = pitches?;
    let mut lines = vec![format!(
        "Selected pitch: {} to {}",
        theory::note_name(lo),
        theory::note_name(hi)
    )];
    if let Some((lo, hi)) = span {
        lines.push(format!("Nominal span: {lo:.3} to {hi:.3} steps"));
    }
    if let Some((lo, hi)) = lengths {
        lines.push(format!("Selected lengths: {lo:.3} to {hi:.3} steps"));
    }
    if let Some((lo, hi)) = velocities {
        lines.push(format!(
            "Selected velocity: {:.1}% to {:.1}%",
            lo * 100.,
            hi * 100.
        ));
    }
    if let Some((lo, hi)) = chances {
        lines.push(format!(
            "Trigger chance: {:.1}% to {:.1}%",
            lo * 100.,
            hi * 100.
        ));
    }
    if let Some((lo, hi)) = offsets {
        lines.push(format!("Timing offsets: {lo:+.3} to {hi:+.3} steps"));
    }
    Some(lines.join("\n"))
}

fn selection_shortcut(events: &[egui::Event]) -> Option<bool> {
    events.iter().rev().find_map(|event| match event {
        egui::Event::Key {
            key: egui::Key::A,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command && !modifiers.alt => Some(modifiers.shift),
        _ => None,
    })
}

fn note_arrow_shortcut(events: &[egui::Event], grid: f32, bar: f32) -> (i32, f32) {
    let mut semi = 0;
    let mut steps = 0.;
    for event in events {
        if let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        {
            if modifiers.command || modifiers.ctrl || modifiers.alt || modifiers.mac_cmd {
                continue;
            }
            match key {
                egui::Key::ArrowUp => semi += if modifiers.shift { 12 } else { 1 },
                egui::Key::ArrowDown => semi -= if modifiers.shift { 12 } else { 1 },
                egui::Key::ArrowLeft => {
                    steps += horizontal_nudge(true, false, modifiers.shift, grid, bar)
                }
                egui::Key::ArrowRight => {
                    steps += horizontal_nudge(false, true, modifiers.shift, grid, bar)
                }
                _ => {}
            }
        }
    }
    (semi, steps)
}

const KEY_W: f32 = 52.0;
const HEADER_H: f32 = 22.0;
const PIANO_RULER_H: f32 = 16.0;

fn ruler_label_stride(bar_width: f32) -> u32 {
    (32.0 / bar_width.max(0.01)).ceil().max(1.0) as u32
}
fn horizontal_nudge(left: bool, right: bool, shift: bool, grid: f32, bar_steps: f32) -> f32 {
    (f32::from(u8::from(right)) - f32::from(u8::from(left))) * if shift { bar_steps } else { grid }
}
const VEL_H: f32 = 48.0;
fn velocity_lane_height(panel_height: f32) -> f32 {
    (panel_height - HEADER_H * 2.0 - PIANO_RULER_H - 48.0).clamp(16.0, VEL_H)
}

/// Snap choices: label and grid in steps (None = off).
pub const SNAPS: [(&str, Option<NoteValue>); 5] = [
    ("1/4", Some(NoteValue::Quarter)),
    ("1/8", Some(NoteValue::Eighth)),
    ("1/16", Some(NoteValue::Sixteenth)),
    ("1/32", Some(NoteValue::ThirtySecond)),
    ("off", None),
];

/// Grid size in steps for a snap choice (off = 1/64 so drags stay sane).
pub fn snap_steps(i: usize) -> f32 {
    let v = SNAPS
        .get(i)
        .and_then(|s| s.1)
        .unwrap_or(NoteValue::SixtyFourth);
    tick_to_step(v.ticks())
}

#[derive(Debug, Clone)]
pub struct EuclideanControls {
    pub pulses: u32,
    pub slots: u32,
    pub rotation: i32,
    pub replace: bool,
}
impl Default for EuclideanControls {
    fn default() -> Self {
        Self {
            pulses: 5,
            slots: 16,
            rotation: 0,
            replace: true,
        }
    }
}
impl EuclideanControls {
    fn args(&self) -> Value {
        let slots = self.slots.clamp(1, 128);
        json!({"pulses":self.pulses.min(slots),"slots":slots,"rotation":self.rotation,"replace":self.replace})
    }
}

#[derive(Debug, Clone)]
pub struct EchoControls {
    pub delay: f32,
    pub repeats: u32,
    pub decay: f32,
    pub transpose: i32,
}
impl Default for EchoControls {
    fn default() -> Self {
        Self {
            delay: 2.0,
            repeats: 3,
            decay: 0.6,
            transpose: 0,
        }
    }
}
impl EchoControls {
    fn args(&self) -> Value {
        json!({"delay":self.delay.clamp(0.0625,64.0),"repeats":self.repeats.clamp(1,64),"decay":self.decay.clamp(0.0,1.0),"transpose":self.transpose.clamp(-127,127)})
    }
}

#[derive(Debug, Clone)]
pub struct StretchControls {
    pub factor: f32,
    pub pattern_anchor: bool,
}
impl Default for StretchControls {
    fn default() -> Self {
        Self {
            factor: 1.5,
            pattern_anchor: false,
        }
    }
}
impl StretchControls {
    fn args(&self) -> Value {
        let mut a = json!({"factor":self.factor.clamp(0.0625,16.0)});
        if self.pattern_anchor {
            a["anchor"] = json!(0.0);
        }
        a
    }
    fn preview(&self, notes: &[Note], selected: &[usize], steps: f32) -> Result<usize, String> {
        let mut copy = notes.to_vec();
        let mut sel = crate::midi_ops::Selection::default();
        if !selected.is_empty() {
            sel.ids = selected
                .iter()
                .filter_map(|i| notes.get(*i))
                .map(|n| n.id.clone())
                .collect();
        }
        crate::midi_ops::stretch_phrase(
            &mut copy,
            &sel,
            self.factor.clamp(0.0625, 16.0),
            self.pattern_anchor.then_some(0.0),
            steps,
        )
        .map_err(|e| e.to_string())
    }
}

fn repeat_capacity(notes: &[Note], selected: &[usize], period: f32, steps: f32) -> (usize, u32) {
    if !period.is_finite() || period <= 0.0 || !steps.is_finite() || steps < 0.0 {
        return (0, 0);
    }
    let membership = selected_index_mask(notes.len(), selected);
    let mut count = 0usize;
    let mut fit = None;
    for (i, n) in notes.iter().enumerate() {
        if !selected.is_empty() && !membership[i] {
            continue;
        }
        count += 1;
        let valid = n.start.is_finite()
            && n.start >= 0.0
            && n.len.is_finite()
            && n.len >= 0.05
            && (n.start + n.len).is_finite();
        let copies = if valid {
            ((steps - n.start - n.len + 1e-4) / period)
                .floor()
                .clamp(0.0, 64.0) as u32
        } else {
            0
        };
        fit = Some(fit.map_or(copies, |previous: u32| previous.min(copies)));
    }
    let fit = fit.unwrap_or(0);
    (count, fit.min((100_000 / count.max(1)).min(64) as u32))
}

// A local, transient frontend to the existing quantize tool. No preview mutations.
#[derive(Debug, Clone)]
pub(super) struct FeelDraft {
    revision: u64,
    generation: u64,
    track: usize,
    pattern: usize,
    track_id: String,
    pattern_id: String,
    track_name: String,
    pattern_name: String,
    view: usize,
    selected: Vec<String>,
    ids: Vec<String>,
    clip: Vec<Note>,
    steps: f32,
    grid: usize,
    strength: u32,
    swing: u32,
    preview_cache: Option<FeelCache>,
}

#[derive(Debug, Clone)]
struct FeelCache {
    controls: (usize, u32, u32),
    result: Result<FeelPreview, String>,
}

#[derive(Debug, Clone)]
struct FeelPreview {
    clip: Vec<Note>,
    starts: usize,
    offsets: usize,
    changed: bool,
}

fn feel_bytes(notes: &[Note]) -> Vec<u8> {
    // Validation rejects nonfinite fields before serialization. Include IDs and order:
    // Note::PartialEq intentionally excludes identity.
    serde_json::to_vec(notes).expect("notes serialize")
}

fn feel_clip_valid(notes: &[Note], steps: f32) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if !steps.is_finite() || steps <= 0.0 || notes.is_empty() {
        return Err("No valid notes in this source. Reopen after choosing notes.".into());
    }
    for n in notes {
        if n.id.trim().is_empty() || n.id.trim() != n.id || !ids.insert(&n.id) {
            return Err("Ambiguous or missing note IDs. Cannot safely quantize.".into());
        }
        if ![n.start, n.len, n.vel, n.prob, n.offset]
            .iter()
            .all(|v| v.is_finite())
            || n.start < 0.0
            || n.len <= 0.0
            || n.pitch > 127
            || !(0.0..=1.0).contains(&n.vel)
            || !(0.0..=1.0).contains(&n.prob)
        {
            return Err("Invalid note values. Cannot safely quantize.".into());
        }
        if n.start >= steps {
            return Err("A note starts at or past the pattern end, including unselected notes. Review it before quantizing; no notes were trimmed.".into());
        }
    }
    Ok(())
}

impl FeelDraft {
    fn new(
        p: &Project,
        stamp: (u64, u64),
        owner: (usize, usize, usize),
        selected: Vec<String>,
        grid: usize,
    ) -> Result<Self, String> {
        let (revision, generation) = stamp;
        let (track, pattern, view) = owner;
        let t = p.tracks.get(track).ok_or("Channel missing")?;
        let pat = p.patterns.get(pattern).ok_or("Pattern missing")?;
        let clip = pat.notes(&t.name).to_vec();
        let ids = if selected.is_empty() {
            clip.iter().map(|n| n.id.clone()).collect()
        } else {
            selected.clone()
        };
        let d = Self {
            revision,
            generation,
            track,
            pattern,
            track_id: t.id.clone(),
            pattern_id: pat.id.clone(),
            track_name: t.name.clone(),
            pattern_name: pat.name.clone(),
            view,
            selected,
            ids,
            clip,
            steps: pat.steps() as f32,
            grid,
            strength: 50,
            swing: 0,
            preview_cache: None,
        };
        d.validate_source(p)?;
        d.preview()?;
        Ok(d)
    }

    fn validate_source(&self, p: &Project) -> Result<(), String> {
        let t = p
            .tracks
            .get(self.track)
            .ok_or("Channel changed. Reopen Quantize.")?;
        let pat = p
            .patterns
            .get(self.pattern)
            .ok_or("Pattern changed. Reopen Quantize.")?;
        let key_ok = |id: &str| !id.trim().is_empty() && id.trim() == id;
        if !key_ok(&self.track_id)
            || !key_ok(&self.pattern_id)
            || self.pattern_id.to_lowercase().starts_with("pl:")
            || pat.name.to_lowercase().starts_with("pl:")
            || t.id != self.track_id
            || pat.id != self.pattern_id
            || t.name != self.track_name
            || pat.name != self.pattern_name
            || p.tracks.iter().filter(|x| x.id == self.track_id).count() != 1
            || p.patterns
                .iter()
                .filter(|x| x.id == self.pattern_id)
                .count()
                != 1
            || p.tracks
                .iter()
                .filter(|x| x.name.to_lowercase() == t.name.to_lowercase())
                .count()
                != 1
            || p.patterns
                .iter()
                .filter(|x| x.name.to_lowercase() == pat.name.to_lowercase())
                .count()
                != 1
            || p.tracks
                .iter()
                .any(|x| x.name.trim().to_lowercase() == self.track_id.to_lowercase())
            || p.patterns
                .iter()
                .any(|x| x.name.trim().to_lowercase() == self.pattern_id.to_lowercase())
            || p.track_index(&self.track_id).ok() != Some(self.track)
            || p.pattern_index(&self.pattern_id).ok() != Some(self.pattern)
        {
            return Err("Source identity is ambiguous, a playlist copy, or changed. Choose a unique source pattern and reopen Quantize.".into());
        }
        // Check the exact dispatch arguments through Engine's real normalizer,
        // then the tool's real lookups. An ID may normalize to an ambiguous name.
        self.normalized_args(p)?;
        let notes = pat.notes(&t.name);
        feel_clip_valid(notes, pat.steps() as f32)?;
        // Dispatch normalizes IDs across the project before recompiling playlist
        // copies. Prove these source identities win that exact existing pass.
        let owners = crate::ids::owner_keys(p);
        let note_owner = format!("note:{}/{}", pat.name.to_lowercase(), t.name.to_lowercase());
        if owners.get(&t.id) != Some(&format!("track:{}", t.name.to_lowercase()))
            || owners.get(&pat.id) != Some(&format!("pattern:{}", pat.name.to_lowercase()))
            || notes.iter().any(|n| owners.get(&n.id) != Some(&note_owner))
        {
            return Err("Source identities would be reassigned. Reopen a unique source.".into());
        }
        let source_ids: std::collections::HashSet<&str> =
            notes.iter().map(|n| n.id.as_str()).collect();
        for (pi, other) in p.patterns.iter().enumerate() {
            for (track, clip) in &other.clips {
                if pi == self.pattern && track == &t.name.to_lowercase() {
                    continue;
                }
                // Compiled copies normally follow all sources and intentionally
                // mirror IDs. Earlier copies or another source are ambiguous.
                if (pi <= self.pattern || !other.name.starts_with("pl:"))
                    && clip.iter().any(|n| source_ids.contains(n.id.as_str()))
                {
                    return Err("Note identities are shared with another source. Choose unique source notes.".into());
                }
            }
        }
        if pat.steps() as f32 != self.steps || feel_bytes(notes) != feel_bytes(&self.clip) {
            return Err("Source notes changed. Reopen Quantize.".into());
        }
        let expected_scope: Vec<String> = if self.selected.is_empty() {
            self.clip.iter().map(|n| n.id.clone()).collect()
        } else {
            self.selected.clone()
        };
        let current_ids: std::collections::HashSet<&str> =
            notes.iter().map(|n| n.id.as_str()).collect();
        let mut ids = std::collections::HashSet::new();
        if self.ids != expected_scope
            || self.ids.is_empty()
            || self
                .ids
                .iter()
                .any(|id| !ids.insert(id) || !current_ids.contains(id.as_str()))
        {
            return Err("Frozen note scope is missing or ambiguous. Reopen Quantize.".into());
        }
        Ok(())
    }

    fn preview(&self) -> Result<FeelPreview, String> {
        feel_clip_valid(&self.clip, self.steps)?;
        let grid = SNAPS
            .get(self.grid)
            .and_then(|(_, g)| *g)
            .map(|g| tick_to_step(g.ticks()))
            .filter(|g| g.is_finite() && *g >= 0.0625)
            .ok_or("Choose a quantize grid; Snap off cannot apply.")?;
        if self.strength > 100 || self.swing > 100 || self.ids.is_empty() {
            return Err("Invalid quantize controls or empty scope.".into());
        }
        let original_ids: std::collections::HashSet<&str> =
            self.clip.iter().map(|n| n.id.as_str()).collect();
        let mut seen = std::collections::HashSet::new();
        if self
            .ids
            .iter()
            .any(|id| !seen.insert(id.as_str()) || !original_ids.contains(id.as_str()))
        {
            return Err("Frozen note scope is missing or ambiguous.".into());
        }
        let mut clip = self.clip.clone();
        crate::midi_ops::quantize(
            &mut clip,
            &crate::midi_ops::Selection {
                ids: self.ids.clone(),
                ..Default::default()
            },
            grid,
            self.strength as f32 / 100.0,
            self.swing as f32 / 100.0,
            false,
        );
        feel_clip_valid(&clip, self.steps)?;
        let result_by_id: std::collections::HashMap<&str, &Note> =
            clip.iter().map(|n| (n.id.as_str(), n)).collect();
        if clip.len() != self.clip.len()
            || self
                .clip
                .iter()
                .any(|n| !result_by_id.contains_key(n.id.as_str()))
        {
            return Err("Preview would lose or add note identities. No edit allowed.".into());
        }
        let mut starts = 0;
        let mut offsets = 0;
        for old in &self.clip {
            let new = result_by_id
                .get(old.id.as_str())
                .copied()
                .ok_or("Preview identity changed")?;
            starts += usize::from(old.start != new.start);
            offsets += usize::from(seen.contains(old.id.as_str()) && old.offset != new.offset);
            let mut allowed = old.clone();
            if seen.contains(old.id.as_str()) {
                allowed.start = new.start;
                allowed.offset = new.offset;
            }
            if feel_bytes(&[allowed]) != feel_bytes(std::slice::from_ref(new)) {
                return Err("Preview changed a field outside the timing scope.".into());
            }
        }
        let changed = feel_bytes(&clip) != feel_bytes(&self.clip);
        // Signed-zero-only serialization changes do not create Engine history.
        // Identity/payload safety above is still byte-based, never Note equality.
        if changed && clip == self.clip {
            return Err("No musical timing change. Apply disabled.".into());
        }
        Ok(FeelPreview {
            clip,
            starts,
            offsets,
            changed,
        })
    }

    fn control_preview(&mut self) -> &Result<FeelPreview, String> {
        let controls = (self.grid, self.strength, self.swing);
        if self
            .preview_cache
            .as_ref()
            .is_none_or(|cache| cache.controls != controls)
        {
            self.preview_cache = Some(FeelCache {
                controls,
                result: self.preview(),
            });
        }
        &self.preview_cache.as_ref().expect("preview cached").result
    }

    fn context_matches(
        &self,
        revision: u64,
        generation: Option<u64>,
        owner: (usize, usize, usize),
        selected: &[String],
    ) -> bool {
        revision == self.revision
            && generation == Some(self.generation)
            && owner == (self.track, self.pattern, self.view)
            && selected == self.selected
    }

    fn normalized_args(&self, p: &Project) -> Result<Value, String> {
        let args = self.args()?;
        let normalized = crate::ids::resolve_id_refs(p, &args);
        let track = normalized.get("track").and_then(Value::as_str);
        let pattern = normalized.get("pattern").and_then(Value::as_str);
        if track.and_then(|key| p.track_index(key).ok()) != Some(self.track)
            || pattern.and_then(|key| p.pattern_index(key).ok()) != Some(self.pattern)
        {
            return Err(
                "Normalized source selectors are ambiguous or changed. Reopen Quantize.".into(),
            );
        }
        // Dispatch the original exact arguments; Engine performs this same pass.
        Ok(args)
    }

    fn args(&self) -> Result<Value, String> {
        let grid = SNAPS
            .get(self.grid)
            .and_then(|(_, g)| *g)
            .ok_or("Snap off")?;
        Ok(
            json!({"track":self.track_id,"pattern":self.pattern_id,"ids":self.ids,
            "grid":tick_to_step(grid.ticks()),"strength":self.strength as f32 / 100.0,
            "swing":self.swing as f32 / 100.0,"ends":false}),
        )
    }
}

/// Called while the caller holds ONE Engine lock; never call Studio::call here.
fn apply_feel(e: &mut crate::engine::Engine, d: &FeelDraft) -> Result<FeelPreview, String> {
    if e.revision != d.revision || e.project_load_generation != d.generation {
        return Err("Quantize expired after a project edit or reload. Reopen Quantize.".into());
    }
    d.validate_source(&e.project)?;
    let preview = d.preview()?;
    if d.strength == 0 || !preview.changed {
        return Err("No change. Nothing applied.".into());
    }
    feel_clip_valid(&preview.clip, d.steps)?;
    let args = d.normalized_args(&e.project)?;
    e.call_from("quantize", &args, "you")
        .map_err(|e| format!("{e:#}"))?;
    Ok(preview)
}

// Focus remains registered across frames so shortcuts::pressed (before workspace
// rendering) sees keyboard ownership even when no numeric editor is active.
pub(super) fn feel_frame_generation(
    e: &crate::engine::Engine,
    revision: u64,
    seen: u64,
) -> Option<u64> {
    (e.revision == revision && e.project_load_generation == seen).then_some(seen)
}

fn feel_gesture_idle(dragging: bool, note_drag: bool, band: bool, held: bool) -> bool {
    !dragging && !note_drag && !band && !held
}

fn feel_dismissed(ctx: &egui::Context) -> bool {
    ctx.input(|i| !i.focused || i.key_pressed(egui::Key::Escape))
}

fn feel_focus_id() -> egui::Id {
    egui::Id::new("quantize-feel-focus")
}

#[derive(Clone)]
struct FeelInputOwner {
    viewport: egui::ViewportId,
    ids: [Option<egui::Id>; 10],
}

fn feel_owner_key() -> egui::Id {
    egui::Id::new("quantize-feel-input-owner")
}

fn feel_owned_control(ctx: &egui::Context, role: usize, id: egui::Id) {
    let viewport = ctx.viewport_id();
    ctx.data_mut(|data| {
        if let Some(mut owner) = data.get_temp::<FeelInputOwner>(feel_owner_key()) {
            if owner.viewport == viewport {
                owner.ids[role] = Some(id);
                data.insert_temp(feel_owner_key(), owner);
            }
        }
    });
}

fn feel_input_owner(ctx: &egui::Context, active: bool) {
    if !active
        && ctx
            .data(|data| data.get_temp::<FeelInputOwner>(feel_owner_key()))
            .is_none()
    {
        return;
    }
    let installed = egui::Id::new("quantize-feel-input-installed");
    let viewport = ctx.viewport_id();
    let needs_install = ctx.data_mut(|data| {
        let install = !data.get_temp::<bool>(installed).unwrap_or(false);
        data.insert_temp(installed, true);
        if active && data.get_temp::<FeelInputOwner>(feel_owner_key()).is_none() {
            data.insert_temp(
                feel_owner_key(),
                FeelInputOwner {
                    viewport,
                    // Sentinel, combo, two numeric editors, two buttons,
                    // and four grid choices. No foreign/global focus IDs.
                    ids: [
                        Some(feel_focus_id()),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ],
                },
            );
        }
        install
    });
    if needs_install {
        // Shortcuts run before workspace. egui clears Escape focus first, so
        // restore only this panel's lease, only in its owning viewport.
        ctx.on_begin_pass(
            "quantize-feel-input",
            std::sync::Arc::new(|ctx| {
                let owner = ctx.data(|data| data.get_temp::<FeelInputOwner>(feel_owner_key()));
                if owner.is_some_and(|owner| owner.viewport == ctx.viewport_id())
                    && !ctx.wants_keyboard_input()
                {
                    ctx.memory_mut(|m| m.request_focus(feel_focus_id()));
                }
            }),
        );
    }
    let owner = ctx.data(|data| data.get_temp::<FeelInputOwner>(feel_owner_key()));
    if let Some(owner) = owner {
        if owner.viewport == ctx.viewport_id() {
            ctx.memory_mut(|m| {
                if active && m.focused().is_none() {
                    m.request_focus(feel_focus_id());
                } else if !active {
                    if let Some(id) = m.focused().filter(|id| owner.ids.contains(&Some(*id))) {
                        m.surrender_focus(id);
                    }
                }
            });
            if !active {
                ctx.data_mut(|data| data.remove::<FeelInputOwner>(feel_owner_key()));
            }
        }
    }
}

#[derive(Default)]
struct FeelAction {
    apply: bool,
    cancel: bool,
}

fn feel_controls(ui: &mut egui::Ui, d: &mut FeelDraft) -> FeelAction {
    feel_input_owner(ui.ctx(), true);
    let mut action = FeelAction::default();
    ui.set_max_width(350.0);
    ui.label(format!("{} / {}", d.track_name, d.pattern_name));
    ui.label(if d.selected.is_empty() {
        format!("All {} notes in this channel/pattern", d.ids.len())
    } else {
        format!("{} selected notes (frozen scope)", d.ids.len())
    });
    ui.label("Starts only. Shared source edits affect all its placements.");
    ui.label("Grid (independent of Snap)");
    let grid = egui::ComboBox::from_id_salt("feel-grid")
        .selected_text(SNAPS[d.grid.min(SNAPS.len() - 1)].0)
        .show_ui(ui, |ui| {
            for (i, (name, g)) in SNAPS.iter().enumerate() {
                if g.is_some() {
                    let choice = ui.selectable_value(&mut d.grid, i, *name);
                    feel_owned_control(ui.ctx(), 6 + i, choice.id);
                }
            }
        });
    feel_owned_control(ui.ctx(), 1, grid.response.id);
    let _strength = ui
        .horizontal(|ui| {
            ui.label("Strength %");
            ui.add(egui::DragValue::new(&mut d.strength).range(0..=100))
        })
        .inner;
    let _swing = ui
        .horizontal(|ui| {
            ui.label("Swing %");
            ui.add(egui::DragValue::new(&mut d.swing).range(0..=100))
        })
        .inner;
    feel_owned_control(ui.ctx(), 2, _strength.id);
    feel_owned_control(ui.ctx(), 3, _swing.id);
    #[cfg(test)]
    {
        feel_tests::record(ui, "Strength", &_strength);
        feel_tests::record(ui, "Swing", &_swing);
    }
    ui.label("100% strength clears selected saved timing offsets. Partial strength retains them.");
    ui.label("Quantize swing is separate from song swing, which stays unchanged and may add more playback swing.");
    let strength = d.strength;
    let preview = d.control_preview();
    match preview {
        Ok(p) => {
            ui.label(format!(
                "{} changed starts; {} saved offsets reset",
                p.starts, p.offsets
            ));
            if !p.changed || strength == 0 {
                ui.label("No change. Apply disabled.");
            }
        }
        Err(e) => {
            ui.colored_label(Color32::YELLOW, e);
        }
    }
    ui.horizontal(|ui| {
        let apply = ui.add_enabled(
            strength > 0 && preview.as_ref().is_ok_and(|p| p.changed),
            egui::Button::new("Apply"),
        );
        let cancel = ui.button("Cancel");
        feel_owned_control(ui.ctx(), 4, apply.id);
        feel_owned_control(ui.ctx(), 5, cancel.id);
        #[cfg(test)]
        {
            feel_tests::record(ui, "Apply", &apply);
            feel_tests::record(ui, "Cancel", &cancel);
        }
        action.apply = apply.clicked();
        action.cancel = cancel.clicked();
    });
    ui.interact(
        ui.min_rect(),
        feel_focus_id(),
        Sense::focusable_noninteractive(),
    );
    if !ui.ctx().wants_keyboard_input() {
        ui.memory_mut(|m| m.request_focus(feel_focus_id()));
    }
    action
}

// This is the whole local dialog lifecycle, including pre-render expiry and
// dismissal, real controls, held-gesture gating, cleanup and guarded dispatch.
#[derive(Clone)]
struct FeelContext {
    revision: u64,
    generation: Option<u64>,
    owner: (usize, usize, usize),
    selected: Vec<String>,
    selection_valid: bool,
    gesture_idle: bool,
}

enum FeelOutcome {
    Applied(FeelPreview, Vec<String>),
    Failed(String),
    Expired,
}

fn feel_lifecycle(
    ctx: &egui::Context,
    p: &Project,
    frame: FeelContext,
    draft: &mut Option<FeelDraft>,
    engine: &std::sync::Mutex<crate::engine::Engine>,
) -> Option<FeelOutcome> {
    let Some(mut d) = draft.take() else {
        feel_input_owner(ctx, false);
        return None;
    };
    let expired = !d.context_matches(
        frame.revision,
        frame.generation,
        frame.owner,
        &frame.selected,
    ) || !frame.selection_valid
        || d.validate_source(p).is_err();
    if feel_dismissed(ctx) || expired {
        feel_input_owner(ctx, false);
        return expired.then_some(FeelOutcome::Expired);
    }
    let mut action = FeelAction::default();
    egui::Window::new("Quantize with feel")
        .id(egui::Id::new("quantize-feel-window"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height((ctx.screen_rect().height() - 100.0).max(120.0))
                .show(ui, |ui| {
                    action = feel_controls(ui, &mut d);
                });
        });
    if action.cancel {
        feel_input_owner(ctx, false);
    } else if action.apply && frame.gesture_idle {
        // The same Engine lock covers source revalidation and one dispatch.
        let result = apply_feel(&mut engine.lock().unwrap(), &d);
        feel_input_owner(ctx, false);
        return Some(match result {
            Ok(preview) => FeelOutcome::Applied(preview, d.selected),
            Err(e) => FeelOutcome::Failed(e),
        });
    } else {
        *draft = Some(d);
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct PianoState {
    pub(super) feel: Option<FeelDraft>,
    pub euclidean: EuclideanControls,
    pub(super) rhythm_trial: Option<super::rhythm_trial::Trial>,
    pub(super) rhythm_trial_status: String,
    pub echo: EchoControls,
    pub stretch: StretchControls,
    pub rotation_steps: f32,
    pub register_range: Option<(u8, u8)>,
    pub reduce_lowest: bool,
    pub ends_grid: f32,
    pub ends_strength: f32,
    pub chance_ramp: Option<(f32, f32)>,
    pub velocity_fit: Option<(f32, f32)>,
    pub chance_percent: Option<f32>,
    pub note_offset: f32,
    pub fixed_velocity: f32,
    pub velocity_ramp: Option<(f32, f32)>,
    pub fixed_length: f32,
    pub length_factor: f32,
    pub phrase_shift: f32,
    pub phrase_semitones: i32,
    pub normalize_peak: f32,
    pub thin_every: u32,
    pub thin_phase: u32,
    pub repeat_initialized: bool,
    pub repeat_bars: u32,
    pub repeat_copies: u32,
    /// Indices into the note list of the current track+pattern.
    pub selected: Vec<usize>,
    /// Previous frame identity map so external reorder/deletion cannot move a selection.
    pub note_ids: Vec<String>,
    pub drag: Option<NoteDrag>,
    /// Ignore the remainder of an Escape-cancelled pointer hold.
    pub gesture_cancelled: bool,
    /// Rubber-band selection start (screen position).
    pub band: Option<Pos2>,
    /// Index into [`SNAPS`].
    pub snap: usize,
    /// (pattern, track) the selection belongs to.
    pub owner: (usize, usize),
    /// Helper edits can reorder notes; recover selection by stable ID next frame.
    pub reselect_ids: Option<Vec<String>>,
    /// Pitch rows shown for this track+pattern. It only grows while editing,
    /// so drawing a note never makes the view jump.
    pub span: Option<(u8, u8)>,
    /// The user scrolled the pitch rows; keep exactly that window.
    pub manual: bool,
}

#[derive(Debug, Clone)]
pub struct NoteDrag {
    pub id: String,
    pub start: Pos2,
    pub shift: NoteShift,
}

/// Bind a manual helper to its documented scope, preserving operation arguments.
fn prepare_helper_args(id: &str, args: &mut Value, track: &str, pattern: &str, ids: &[&str]) {
    if let (Some(obj), Some(tool)) = (args.as_object_mut(), crate::tools::find(id)) {
        let schema = (tool.schema)();
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            if !ids.is_empty() && props.contains_key("ids") {
                obj.insert("ids".into(), json!(ids));
            }
            if props.contains_key("track") {
                obj.insert("track".into(), json!(track));
            }
            if props.contains_key("target_pattern") {
                obj.insert("target_pattern".into(), json!(pattern));
            }
            if props.contains_key("pattern") && !obj.contains_key("pattern") {
                obj.insert("pattern".into(), json!(pattern));
            }
        }
    }
}

impl Studio {
    pub(super) fn feel_open(&self) -> bool {
        self.piano.feel.is_some()
    }

    /// Render outside the dock so a hidden/closed Notes pane cannot strand a draft.
    pub(super) fn feel_ui(
        &mut self,
        ctx: &egui::Context,
        p: &Project,
        revision: u64,
        generation: Option<u64>,
    ) {
        if self.piano.feel.is_none() {
            feel_input_owner(ctx, false);
            return;
        }
        let selected: Vec<String> = self
            .piano
            .selected
            .iter()
            .filter_map(|i| {
                p.patterns
                    .get(self.pattern)?
                    .notes(&p.tracks.get(self.selected)?.name)
                    .get(*i)
            })
            .map(|n| n.id.clone())
            .collect();
        let frame = FeelContext {
            revision,
            generation,
            owner: (self.selected, self.pattern, self.view),
            selection_valid: selected.len() == self.piano.selected.len(),
            selected,
            gesture_idle: feel_gesture_idle(
                self.dragging,
                self.piano.drag.is_some(),
                self.piano.band.is_some(),
                ctx.input(|i| i.pointer.any_down()),
            ),
        };
        match feel_lifecycle(ctx, p, frame, &mut self.piano.feel, &self.engine) {
            Some(FeelOutcome::Applied(preview, selected)) => {
                self.piano.reselect_ids = Some(selected);
                self.toast = Some((format!("Quantized: {} changed starts; {} saved offsets reset. Undo restores timing.", preview.starts, preview.offsets), 0.0, true));
            }
            Some(FeelOutcome::Failed(e)) => self.toast = Some((e, 0.0, false)),
            Some(FeelOutcome::Expired) => self.toast = Some((
                "Quantize expired: source, selection, view or project changed. Reopen Quantize."
                    .into(),
                0.0,
                false,
            )),
            None => {}
        }
    }

    /// Commit only changed geometry by stable ID, preserving every other field.
    fn write_notes(&mut self, track: &str, pattern: &str, notes: &[Note], before: &[Note]) {
        let updates: Vec<Value> = notes
            .iter()
            .zip(before)
            .filter(|(a, b)| (a.start, a.len, a.pitch) != (b.start, b.len, b.pitch))
            .map(|(n, _)| json!({"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}))
            .collect();
        if !updates.is_empty() {
            self.call(
                "update_note_geometry",
                json!({"track":track,"pattern":pattern,"updates":updates}),
            );
        }
    }

    fn delete_piano_notes(
        &mut self,
        track: &str,
        pattern: &str,
        notes: &[Note],
        indices: &[usize],
    ) {
        let ids: Vec<&str> = indices
            .iter()
            .filter_map(|i| notes.get(*i))
            .map(|n| n.id.as_str())
            .collect();
        // Empty selection must never become the engine's default "all notes".
        if !ids.is_empty() {
            self.call(
                "delete_notes",
                json!({"track": track, "pattern": pattern, "ids": ids}),
            );
        }
    }

    pub(super) fn piano_roll(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        revision: u64,
        generation: Option<u64>,
    ) {
        if self.piano.feel.is_some() {
            ui.label(
                "Quantize with feel is open. Finish or cancel the timing preview to edit notes.",
            );
            return;
        }
        let tk = Tokens::DARK;
        let (Some(t), Some(pat)) = (p.tracks.get(self.selected), p.patterns.get(self.pattern))
        else {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(
                        "No channel yet. Add one in the Channel rack and write its notes here.",
                    )
                    .size(13.0)
                    .color(tk.text_dim),
                );
            });
            return;
        };
        if self.piano.owner != (self.pattern, self.selected) {
            self.piano.selected.clear();
            self.piano.note_ids.clear();
            self.piano.drag = None;
            self.piano.reselect_ids = None;
            self.piano.span = None;
            self.piano.manual = false;
            self.piano.owner = (self.pattern, self.selected);
        }
        let tname = t.name.clone();
        let pname = pat.name.clone();
        let color = track_color(&t.name, t.instrument.kind_name());
        let notes: Vec<Note> = pat.notes(&t.name).to_vec();
        if !note_identity_matches(&notes, &self.piano.note_ids) {
            if !self.piano.note_ids.is_empty() {
                self.piano.selected =
                    remap_note_selection(&notes, &self.piano.note_ids, &self.piano.selected);
            }
            self.piano.note_ids = notes.iter().map(|n| n.id.clone()).collect();
        }
        if let Some(drag) = &mut self.piano.drag {
            match unique_note_index(&notes, &drag.id) {
                Some(index) => drag.shift.index = index,
                None => {
                    self.piano.drag = None;
                    self.dragging = false;
                }
            }
        }
        if let Some(ids) = self.piano.reselect_ids.take() {
            self.piano.selected = selection_from_ids(&notes, &ids);
        }
        self.piano.selected.retain(|i| *i < notes.len());
        let steps = pat.steps() as f32;
        let is_drum = t.instrument.is_drum();
        let (lo, hi) = if notes.is_empty() {
            (48u8, 72u8)
        } else {
            let (l, h) = notes
                .iter()
                .fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
            let (l, h) = (l.saturating_sub(3), h.saturating_add(3).min(127));
            if h - l < 12 {
                (l.saturating_sub((12 - (h - l)) / 2), (h + 6).min(127))
            } else {
                (l, h)
            }
        };
        let (lo, hi) = match self.piano.span {
            Some((l, h)) if self.piano.manual => (l, h),
            Some((l, h)) => {
                // already framed: only grow to include notes, with no new padding
                let (nl, nh) = notes
                    .iter()
                    .fold((127u8, 0u8), |(a, b), n| (a.min(n.pitch), b.max(n.pitch)));
                if notes.is_empty() {
                    (l, h)
                } else {
                    (l.min(nl), h.max(nh))
                }
            }
            None => (lo, hi),
        };
        self.piano.span = Some((lo, hi));
        let (full, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        // mouse wheel over the roll moves the visible pitch rows (3 semitones a notch)
        let (lo, hi) = if ui.rect_contains_pointer(full) {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            if dy.abs() > 0.5 {
                let step = 3i16 * if dy > 0.0 { 1 } else { -1 };
                let width = (hi - lo) as i16;
                let nl = (lo as i16 + step).clamp(0, 127 - width);
                self.piano.span = Some((nl as u8, (nl + width) as u8));
                self.piano.manual = true;
                (nl as u8, (nl + width) as u8)
            } else {
                (lo, hi)
            }
        } else {
            (lo, hi)
        };
        // never draw past the visible panel (the velocity lane sat under its edge)
        let vis = full.intersect(ui.clip_rect());
        let full = if vis.width() > 120.0 && vis.height() > HEADER_H + VEL_H + 40.0 {
            vis
        } else {
            full
        };
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, tk.panel_bg);
        // ---- header: title, hint, snap selector, ops ----
        // title row plus three rows of helper buttons
        let header = Rect::from_min_size(full.min, vec2(full.width(), HEADER_H * 2.0));
        painter.rect_filled(header, 0.0, tk.panel_bg2);
        painter.text(
            pos2(header.min.x + 8.0, header.min.y + HEADER_H / 2.0),
            Align2::LEFT_CENTER,
            "PIANO ROLL",
            FontId::proportional(10.5),
            tk.header_text,
        );
        let bar = Rect::from_min_max(
            pos2(header.min.x + 100.0, header.min.y + 1.0),
            pos2(header.max.x - 4.0, header.min.y + HEADER_H - 1.0),
        );
        let mut tb = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        tb.set_clip_rect(bar);
        let mut op: Option<(&str, Value)> = None;
        let mut delete_selection = false;
        let g = snap_steps(self.piano.snap);
        tb.menu_button("Edit notes", |ui| {
            ui.menu_button("Select notes", |ui| {
            if ui
                .add_enabled(!notes.is_empty(), egui::Button::new("Select all notes"))
                .on_hover_text("Select all notes here: Ctrl+A (Cmd+A on Mac) while hovering the piano roll.")
                .clicked()
            {
                self.piano.selected = (0..notes.len()).collect();
                ui.close_menu();
            }
            if ui.add_enabled(!self.piano.selected.is_empty(), egui::Button::new("Select same pitches"))
                .on_hover_text("Select every note at the pitches of the selected notes, in this channel and pattern. Changes selection only.").clicked() {
                self.piano.selected = matching_pitch_selection(&notes, &self.piano.selected);
                ui.close_menu();
            }
            if ui.add_enabled(!self.piano.selected.is_empty(), egui::Button::new("Select whole chords"))
                .on_hover_text("Select all notes starting with any selected note (within0.0001 steps), in this channel and pattern. Nominal starts only, not overlapping sustains. Changes selection only.").clicked() {
                self.piano.selected = matching_onset_selection(&notes, &self.piano.selected);
                ui.close_menu();
            }
            if ui.add_enabled(!self.piano.selected.is_empty(), egui::Button::new("Select same lengths"))
                .on_hover_text("Select notes with any selected duration (within 0.0001 steps), across all pitches in this channel/pattern. Selection only; timing and lengths stay unchanged.").clicked() {
                self.piano.selected = matching_length_selection(&notes, &self.piano.selected);
                ui.close_menu();
            }
            let overlaps = overlapping_pitch_selection(&notes);
            if ui.add_enabled(!overlaps.is_empty(), egui::Button::new(format!("Select same-pitch overlaps ({})", overlaps.len())))
                .on_hover_text("Select notes overlapping another note at the same pitch in this channel/pattern. Nominal starts and lengths only; touching boundaries and different chord tones don't count. Selection only.").clicked() {
                self.piano.selected = overlaps;
                ui.close_menu();
            }
            let copies = duplicate_copy_selection(&notes);
            if ui.add_enabled(!copies.is_empty(), egui::Button::new(format!("Select duplicate copies ({})", copies.len())))
                .on_hover_text("Select extra exact copies, keeping the first copy unselected. Starts, lengths, pitch, velocity, chance, timing offset and slide destination must all match. Review only; does not delete notes.").clicked() {
                self.piano.selected = copies;
                ui.close_menu();
            }
            let beyond = beyond_pattern_selection(&notes, pat.steps() as f32);
            if ui.add_enabled(!beyond.is_empty(), egui::Button::new(format!("Select past pattern end ({})", beyond.len())))
                .on_hover_text("Select notes starting at/after the pattern end or sustaining past it. Exact end boundaries are allowed. Review only: no trim or deletion. This channel/pattern only.").clicked() {
                self.piano.selected = beyond;
                ui.close_menu();
            }
            let silent = silent_note_selection(&notes);
            if ui.add_enabled(!silent.is_empty(), egui::Button::new(format!("Select silent notes ({})", silent.len())))
                .on_hover_text("Select notes with exactly0 velocity or0 trigger chance in this channel/pattern. Review only; does not delete notes. Very quiet notes and low nonzero chances are not selected.").clicked() {
                self.piano.selected = silent;
                ui.close_menu();
            }
            for (timing, label, hint) in [
                (true, "Select timing offsets", "Select finite nonzero saved timing offsets in this channel/pattern, including tiny offsets. Nominal note starts stay unchanged. Selection only."),
                (false, "Select chance below 100%", "Select saved trigger chances from 0% up to but not including 100%, in this channel/pattern. Includes silent 0% notes. Selection only; playback is unchanged.")
            ] {
                let found = expression_note_selection(&notes, timing);
                if ui.add_enabled(!found.is_empty(), egui::Button::new(format!("{} ({})",label,found.len())))
                    .on_hover_text(hint).clicked() {
                    self.piano.selected = found;
                    ui.close_menu();
                }
            }
            let slides = slide_note_selection(&notes);
            if ui.add_enabled(!slides.is_empty(),egui::Button::new(format!("Select slide notes ({})",slides.len())))
                .on_hover_text("Select notes with a saved slide destination in this channel/pattern, including same-pitch slide destinations. Selection only; no changes to pitch, timing or slides.").clicked() {
                self.piano.selected=slides;
                ui.close_menu();
            }
            let outside = outside_scale_selection(&notes, &p.key_root, &p.scale);
            if ui.add_enabled(outside.as_ref().is_some_and(|v|!v.is_empty()), egui::Button::new(format!("Select outside song scale ({})", outside.as_ref().map_or(0,Vec::len))))
                .on_hover_text(format!("Select notes outside {} {} in this channel/pattern. Review only: no pitch snapping. Passing tones may be intentional. Not meaningful for unpitched drums.",p.key_root,p.scale)).clicked() {
                if let Some(outside) = outside {self.piano.selected = outside;}
                ui.close_menu();
            }
            let inverse = inverted_note_selection(notes.len(), &self.piano.selected);
            if ui.add_enabled(!inverse.is_empty(), egui::Button::new("Invert selection"))
                .on_hover_text("Select currently unselected notes. Disabled if every note is selected, because empty selection means all notes for editing. Use Clear selection explicitly instead.").clicked() {
                self.piano.selected = inverse;
                ui.close_menu();
            }
            });
            if ui
                .add_enabled(
                    !self.piano.selected.is_empty(),
                    egui::Button::new("Clear selection"),
                )
                .on_hover_text("Clear selection: Ctrl+Shift+A (Cmd+Shift+A on Mac) while hovering the piano roll. Selection only; Delete does nothing with no notes selected.")
                .clicked()
            {
                self.piano.selected.clear();
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    !self.piano.selected.is_empty(),
                    egui::Button::new("Delete selected notes"),
                )
                .on_hover_text("Remove only the selected notes. Undo restores them.")
                .clicked()
            {
                delete_selection = true;
                ui.close_menu();
            }
            ui.separator();
            let scope = if self.piano.selected.is_empty() {
                format!("Scope: all {} notes", notes.len())
            } else {
                format!("Scope: {} selected", self.piano.selected.len())
            };
            ui.label(RichText::new(scope).small().color(tk.text_dim));
            ui.label(RichText::new("Open a group to find an edit").small().color(tk.text_dim));
            let group_id=egui::Id::new("piano_edit_group");
            let active_group=ui.ctx().data_mut(|d|d.get_temp::<String>(group_id)).unwrap_or_default();
            let mut clicked_group=None;
            let group=egui::CollapsingHeader::new("Pitch & melody").open(Some(active_group=="Pitch & melody")).show(ui, |ui| {
            ui.menu_button("Reverse melody pitches", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Reverse pitch order through selected notes. The rhythm stays fixed.");
                ui.label(RichText::new("Only pitches change. Timing, lengths, expression and slide targets stay unchanged. Select one note per onset.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::reverse_pitches(&mut preview,&sel);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} pitches change",n)).small());},Err(_)=>{ui.label(RichText::new("Select only one note at each onset. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Reverse MIDI pitch sequence")).clicked(){op=Some(("reverse_melody_pitches",json!({})));ui.close_menu();}
            });
            ui.menu_button("Rotate melody pitches", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Cycle pitches through selected notes in time order, wrapping at the ends.");
                ui.label(RichText::new("Only pitches change. Timing, expression and slide targets stay unchanged. Select one note per onset.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                for (shift,label) in [(1,"Move pitches one note later"),(-1,"Move pitches one note earlier")] {
                    let mut preview=notes.clone();let result=crate::midi_ops::rotate_pitches(&mut preview,&sel,shift);
                    match &result {Ok(n)=>{ui.label(RichText::new(format!("{} pitches change",n)).small());},Err(_)=>{ui.label(RichText::new("Select only one note at each onset. Nothing will change.").small());}}
                    if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new(label)).clicked(){op=Some(("rotate_melody_pitches",json!({"shift":shift})));ui.close_menu();}
                }
            });
            ui.menu_button("Transpose safely", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                ui.horizontal(|ui| {ui.label("Semitones");ui.add(egui::DragValue::new(&mut self.piano.phrase_semitones).range(-127..=127));});
                ui.label("Keeps intervals intact. Rejects out-of-range pitches instead of clipping them.");
                ui.label(RichText::new("Only pitches change; slide targets stay unchanged.").small());
                let sel=crate::midi_ops::Selection {ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();
                let result=crate::midi_ops::transpose_phrase(&mut preview,&sel,self.piano.phrase_semitones);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} notes transpose; unselected notes stay intact",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit MIDI pitch range. Reduce semitones.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0) && self.piano.phrase_semitones!=0,egui::Button::new("Transpose MIDI phrase")).clicked() {
                    op=Some(("transpose_phrase",json!({"semitones":self.piano.phrase_semitones})));ui.close_menu();
                }
            });
            ui.menu_button("Fold into register", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                let (low,high)=self.piano.register_range.get_or_insert((48,72));
                ui.horizontal(|ui|{ui.label("Lowest MIDI pitch");ui.add(egui::DragValue::new(low).range(0..=127));});
                ui.horizontal(|ui|{ui.label("Highest MIDI pitch");ui.add(egui::DragValue::new(high).range(0..=127));});
                ui.label("Moves out-of-range pitches by whole octaves. Pitch classes stay unchanged.");
                ui.label(RichText::new("Only pitches change; slide targets stay unchanged.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::fold_register(&mut preview,&sel,*low,*high);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} pitches move by octaves",n)).small());},Err(_)=>{ui.label(RichText::new("Cannot fit every pitch class. Widen the register.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Fold MIDI register")).clicked(){op=Some(("fold_note_register",json!({"low":*low,"high":*high})));ui.close_menu();}
            });
            ui.menu_button("Reduce chord voices", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.checkbox(&mut self.piano.reduce_lowest,"Keep lowest pitch (off = highest)");
                ui.label("Each simultaneous onset keeps its top or bottom pitch. Same-pitch layers stay.");
                ui.label(RichText::new("Selected notes only; not a sustained-voice detector.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::reduce_chords(&mut preview,&sel,!self.piano.reduce_lowest);
                let removed=result.unwrap_or(0);ui.label(RichText::new(format!("{} notes remain; {} removed",preview.len(),removed)).small());
                if ui.add_enabled(removed>0,egui::Button::new("Reduce MIDI chord voices")).clicked(){op=Some(("reduce_chord_voices",json!({"voice":if self.piano.reduce_lowest {"lowest"}else{"highest"}})));ui.close_menu();}
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Flip pitch")).clicked() {op=Some(("flip_notes",json!({"mode":"pitch"})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Pitch & melody");}
            let group=egui::CollapsingHeader::new("Rhythm & placement").open(Some(active_group=="Rhythm & placement")).show(ui, |ui| {
            ui.menu_button("Reverse rhythm gaps", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Reverse the gaps between selected onset groups, keeping note order and endpoints fixed.");
                ui.label(RichText::new("Chords move together. Pitches, lengths and expression stay unchanged. Needs at least three onset groups.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::reverse_rhythm_gaps(&mut preview,&sel,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note starts change",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit. Shorten notes or extend the pattern.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Reverse MIDI rhythm gaps")).clicked(){op=Some(("reverse_rhythm_gaps",json!({})));ui.close_menu();}
            });
            ui.menu_button("Distribute rhythm", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Evenly spaces selected onset groups between their first and last positions.");
                ui.label(RichText::new("Chords stay together. Lengths and expression stay unchanged.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::distribute_rhythm(&mut preview,&sel,steps);
                let groups=crate::midi_ops::chord_groups(&notes,&crate::midi_ops::selected(&notes,&sel),1e-4).len();
                if groups<3 {ui.label(RichText::new("Select at least three different onset groups.").small());}
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note starts change; endpoints stay fixed",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit. Shorten notes or extend the pattern.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Distribute MIDI rhythm")).clicked(){op=Some(("distribute_note_rhythm",json!({})));ui.close_menu();}
            });
            ui.menu_button("Move phrase safely", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                ui.horizontal(|ui| {ui.label("Shift (sixteenth steps)");ui.add(egui::DragValue::new(&mut self.piano.phrase_shift).speed(0.25).range(-4096.0..=4096.0));});
                ui.label("Moves the phrase together without changing spacing or note lengths");
                let sel=crate::midi_ops::Selection {ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();
                let result=crate::midi_ops::shift_phrase(&mut preview,&sel,self.piano.phrase_shift,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} notes move; unselected notes stay intact",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit. Reduce the shift or extend the pattern.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0) && self.piano.phrase_shift.abs()>1e-5,egui::Button::new("Move MIDI phrase")).clicked() {
                    op=Some(("shift_phrase",json!({"shift":self.piano.phrase_shift})));ui.close_menu();
                }
            });
            ui.menu_button("Custom rhythm rotation", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                ui.label("Wraps starts within each note's own bar");
                ui.horizontal(|ui| {ui.label("Shift (sixteenth steps)");ui.add(egui::DragValue::new(&mut self.piano.rotation_steps).speed(0.25).range(-1024.0..=1024.0));});
                let bar=pat.steps_per_bar() as f32;
                let shift=self.piano.rotation_steps;
                ui.label(RichText::new(format!("Bar: {} steps; wrapped shift: {:.2}",bar,shift.rem_euclid(bar))).small());
                ui.label(RichText::new("Positive = later; negative = earlier. Lengths and expression stay unchanged.").small());
                if ui.add_enabled(!notes.is_empty() && shift.rem_euclid(bar).abs()>1e-4,egui::Button::new("Rotate MIDI rhythm")).clicked() {
                    op=Some(("rotate_notes",json!({"shift":shift})));ui.close_menu();
                }
            });
            ui.menu_button("Thin rhythm", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                self.piano.thin_every=self.piano.thin_every.clamp(2,64);
                ui.label("Keep one selected onset group out of N");
                ui.horizontal(|ui| {ui.label("Every N groups");ui.add(egui::DragValue::new(&mut self.piano.thin_every).range(2..=64));});
                self.piano.thin_phase=self.piano.thin_phase.min(self.piano.thin_every-1);
                ui.horizontal(|ui| {ui.label("Keep group (zero-based)");ui.add(egui::DragValue::new(&mut self.piano.thin_phase).range(0..=self.piano.thin_every-1));});
                ui.label(RichText::new("Selected chords stay together. Unselected notes stay intact.").small());
                let mut preview=notes.clone();
                let sel=crate::midi_ops::Selection {ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let removed=crate::midi_ops::thin_rhythm(&mut preview,&sel,self.piano.thin_every,self.piano.thin_phase).unwrap_or(0);
                ui.label(RichText::new(format!("{} notes remain; {} removed",preview.len(),removed)).small());
                if ui.add_enabled(removed>0,egui::Button::new("Thin MIDI rhythm")).clicked() {
                    op=Some(("thin_notes",json!({"every":self.piano.thin_every,"phase":self.piano.thin_phase})));ui.close_menu();
                }
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Reverse time")).clicked() {op=Some(("flip_notes",json!({"mode":"time"})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Rhythm & placement");}
            let group=egui::CollapsingHeader::new("Lengths & articulation").open(Some(active_group=="Lengths & articulation")).show(ui, |ui| {
            ui.menu_button("Set note lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                if self.piano.fixed_length<0.05 {self.piano.fixed_length=1.0;}
                ui.label("Set one duration for selected notes, in sixteenth steps.1 = one sixteenth note.");
                ui.label(RichText::new("Starts, pitches and expression stay unchanged. Rejects the whole edit if any new note end exceeds this pattern.").small());
                ui.add(egui::DragValue::new(&mut self.piano.fixed_length).speed(0.25).range(0.05..=steps.max(0.05)).prefix("Length: "));
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::set_lengths(&mut preview,&sel,self.piano.fixed_length,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit selected notes. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Set selected lengths")).clicked(){op=Some(("set_note_lengths",json!({"length":self.piano.fixed_length})));ui.close_menu();}
            });
            ui.menu_button("Scale note lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                if self.piano.length_factor<0.0625 {self.piano.length_factor=0.5;}
                ui.label("Multiply durations only. Starts, pitches, velocity and expression stay unchanged.");
                ui.add(egui::DragValue::new(&mut self.piano.length_factor).speed(0.05).range(0.0625..=16.0).prefix("Length multiplier: "));
                ui.label(RichText::new("Chords supported. The whole edit is rejected if a note becomes shorter than0.05 steps or cannot fit the pattern.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::scale_lengths(&mut preview,&sel,self.piano.length_factor,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit or becomes too short. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Scale selected lengths")).clicked(){op=Some(("scale_note_lengths",json!({"factor":self.piano.length_factor})));ui.close_menu();}
            });
            ui.menu_button("Average chord lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Give selected notes at each shared onset that chord's average duration.");
                ui.label(RichText::new("Only lengths change. Solo notes stay unchanged. Partial selections average selected voices only. Invalid bounds reject the whole edit.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::balance_chord_lengths(&mut preview,&sel,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("Invalid bounds or does not fit. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Average selected chord lengths")).clicked(){op=Some(("balance_chord_lengths",json!({})));ui.close_menu();}
            });
            ui.menu_button("Invert note lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Short notes become long and long notes become short within the selected duration range.");
                ui.label(RichText::new("Only lengths change. Chords supported. Every result must fit the pattern; the whole edit is rejected otherwise.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::invert_lengths(&mut preview,&sel,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("Invalid bounds or does not fit. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Invert selected lengths")).clicked(){op=Some(("invert_note_lengths",json!({})));ui.close_menu();}
            });
            ui.menu_button("Rotate note lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Move existing durations through selected notes, wrapping at the ends.");
                ui.label(RichText::new("Only lengths change. Select one note per onset. Every result must fit the pattern; no clipping.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                for (shift,label) in [(1,"Move lengths one note later"),(-1,"Move lengths one note earlier")] {
                    let mut preview=notes.clone();let result=crate::midi_ops::rotate_lengths(&mut preview,&sel,shift,steps);
                    match &result {Ok(n)=>{ui.label(RichText::new(format!("{}: {} lengths change",label,n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit or has shared onsets. Select one voice or extend the pattern.").small());}}
                    if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new(label)).clicked(){op=Some(("rotate_note_lengths",json!({"shift":shift})));ui.close_menu();}
                }
            });
            ui.menu_button("Reverse note lengths", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Reverse durations in onset order. Starts, pitches and expression stay unchanged.");
                ui.label(RichText::new("Select one note per onset. Every new length must fit the pattern; no clipping.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::reverse_lengths(&mut preview,&sel,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit or has shared onsets. Select one voice or extend the pattern.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Reverse selected lengths")).clicked(){op=Some(("reverse_note_lengths",json!({})));ui.close_menu();}
            });
            ui.menu_button("Align chord releases", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Give selected notes in each chord a shared release. Starts and expression stay unchanged.");
                ui.label(RichText::new("Only selected same-onset notes form a chord. Single notes stay unchanged.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                for (longest,mode,label) in [(true,"longest","Match longest release"),(false,"shortest","Match shortest release")] {
                    let mut preview=notes.clone();let result=crate::midi_ops::align_chord_ends(&mut preview,&sel,longest,steps);
                    match &result {Ok(n)=>{ui.label(RichText::new(format!("{}: {} note lengths change",label,n)).small());},Err(_)=>{ui.label(RichText::new("Invalid note bounds. Nothing will change.").small());}}
                    if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new(label)).clicked(){op=Some(("align_chord_ends",json!({"mode":mode})));ui.close_menu();}
                }
            });
            ui.menu_button("Quantize releases", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                if self.piano.ends_grid<0.0625 {self.piano.ends_grid=1.0;self.piano.ends_strength=1.0;}
                ui.horizontal(|ui|{ui.label("Grid (sixteenth steps)");ui.add(egui::DragValue::new(&mut self.piano.ends_grid).speed(0.25).range(0.0625..=64.0));});
                ui.horizontal(|ui|{ui.label("Strength");ui.add(egui::DragValue::new(&mut self.piano.ends_strength).speed(0.05).range(0.0..=1.0));});
                ui.label("Snaps note ends only. Starts and microtiming stay unchanged.");
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::quantize_ends(&mut preview,&sel,self.piano.ends_grid,self.piano.ends_strength,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} release positions change",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit. Reduce strength or choose a finer grid.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Quantize MIDI releases")).clicked(){op=Some(("quantize_note_ends",json!({"grid":self.piano.ends_grid,"strength":self.piano.ends_strength})));ui.close_menu();}
            });
            ui.menu_button("Custom phrase stretch", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                ui.label("Changes note timing and lengths, not tempo");
                ui.horizontal(|ui| {ui.label("Length multiplier");ui.add(egui::DragValue::new(&mut self.piano.stretch.factor).speed(0.05).range(0.0625..=16.0));});
                ui.checkbox(&mut self.piano.stretch.pattern_anchor,"Anchor at pattern start (off = first selected note)");
                let preview=self.piano.stretch.preview(&notes,&self.piano.selected,steps);
                match &preview {Ok(n)=>{ui.label(RichText::new(format!("{} notes; pitch and expression unchanged",n)).small());},Err(_)=>{ui.label(RichText::new("Does not fit or makes notes too short. Reduce the multiplier or extend the pattern.").small());}}
                if ui.add_enabled(matches!(preview,Ok(n) if n>0),egui::Button::new("Stretch MIDI phrase")).clicked() {
                    op=Some(("stretch_notes",self.piano.stretch.args()));ui.close_menu();
                }
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Twice as fast")).clicked() {op=Some(("stretch_notes",json!({"factor":0.5})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Twice as slow")).clicked() {op=Some(("stretch_notes",json!({"factor":2.0})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Legato")).clicked() {op=Some(("legato",json!({})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Staccato")).clicked() {op=Some(("articulate",json!({"style":"staccato"})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Chop")).clicked() {op=Some(("chop_notes",json!({"pattern":"x.x.x.x."})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Lengths & articulation");}
            let group=egui::CollapsingHeader::new("Repeats & echoes").open(Some(active_group=="Repeats & echoes")).show(ui, |ui| {
            ui.menu_button("Repeat phrase", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                if !self.piano.repeat_initialized {
                    self.piano.repeat_bars = 1;
                    self.piano.repeat_copies = 1;
                    self.piano.repeat_initialized = true;
                }
                ui.label("Adds exact copies; originals stay intact");
                ui.label(RichText::new("Source notes need valid bounds and length at least 0.05 steps.").small());
                ui.horizontal(|ui| { ui.label("Spacing (bars)"); ui.add(egui::DragValue::new(&mut self.piano.repeat_bars).range(1..=64)); });
                ui.horizontal(|ui| { ui.label("Extra copies"); ui.add(egui::DragValue::new(&mut self.piano.repeat_copies).range(1..=64)); });
                let period=self.piano.repeat_bars.max(1) as f32*pat.steps_per_bar() as f32;
                let (source,capacity)=repeat_capacity(&notes,&self.piano.selected,period,steps);
                let repeats=self.piano.repeat_copies.clamp(1,64);
                ui.label(RichText::new(format!("{} source notes; {} extra copies fit",source,capacity)).small());
                let fits=source>0 && repeats<=capacity;
                if !fits { ui.label(RichText::new("Reduce copies/spacing, select earlier notes, or extend the pattern.").small()); }
                if ui.add_enabled(fits,egui::Button::new(format!("Repeat MIDI phrase (+{} notes)",source*repeats as usize))).clicked() {
                    op=Some(("repeat_notes",json!({"period":self.piano.repeat_bars.max(1) as f32*pat.steps_per_bar() as f32,"repeats":self.piano.repeat_copies.clamp(1,64)})));
                    ui.close_menu();
                }
            });
            ui.menu_button("Custom MIDI echoes", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                ui.label("Adds editable copies; originals stay intact");
                ui.horizontal(|ui| {
                    ui.label("Delay (sixteenth steps)");
                    ui.add(
                        egui::DragValue::new(&mut self.piano.echo.delay)
                            .speed(0.25)
                            .range(0.0625..=64.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Repeats");
                    ui.add(egui::DragValue::new(&mut self.piano.echo.repeats).range(1..=64));
                });
                ui.horizontal(|ui| {
                    ui.label("Velocity multiplier / repeat");
                    ui.add(
                        egui::DragValue::new(&mut self.piano.echo.decay)
                            .speed(0.05)
                            .range(0.0..=1.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Semitones / repeat");
                    ui.add(egui::DragValue::new(&mut self.piano.echo.transpose).range(-127..=127));
                });
                ui.label(
                    RichText::new(
                        "Copies outside pattern/pitch range or below2% velocity are skipped",
                    )
                    .small(),
                );
                if ui
                    .add_enabled(!notes.is_empty(), egui::Button::new("Add MIDI echoes"))
                    .clicked()
                {
                    op = Some(("echo_notes", self.piano.echo.args()));
                    ui.close_menu();
                }
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Echo 3x (1/8)")).clicked() {op=Some(("echo_notes",json!({"delay":"1/8","repeats":3,"decay":0.6})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Repeats & echoes");}
            let group=egui::CollapsingHeader::new("Velocity & dynamics").open(Some(active_group=="Velocity & dynamics")).show(ui, |ui| {
            ui.menu_button("Velocity ramp", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Linear velocity ramp over selected note starts. Notes at the same onset get the same velocity.");
                ui.label(RichText::new("Velocity only;0 means silence. One onset uses Start. Lengths, note order and other expression stay unchanged.").small());
                let (start,end)=self.piano.velocity_ramp.get_or_insert((0.25,0.75));
                ui.add(egui::Slider::new(start,0.0..=1.0).step_by(0.01).text("Start"));ui.add(egui::Slider::new(end,0.0..=1.0).step_by(0.01).text("End"));
                let (start,end)=(*start,*end);
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::ramp_velocities(&mut preview,&sel,start,end);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} velocities change",n)).small());},Err(_)=>{ui.label(RichText::new("Invalid selected starts. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Apply selected velocity ramp")).clicked(){op=Some(("ramp_note_velocities",json!({"start_velocity":start,"end_velocity":end})));ui.close_menu();}
            });
            ui.menu_button("Set note velocity", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Set one MIDI velocity for the selected notes.0% means silence, not deletion.");
                ui.label(RichText::new("Only velocity changes. Lengths, starts, pitches, chance and timing offsets stay unchanged.").small());
                ui.add(egui::Slider::new(&mut self.piano.fixed_velocity,0.0..=1.0).step_by(0.01).text("Velocity"));
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::set_velocity(&mut preview,&sel,self.piano.fixed_velocity);
                if let Ok(n)=&result {ui.label(RichText::new(format!("{} note velocities change",n)).small());}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Set selected velocity")).clicked(){op=Some(("set_note_velocity",json!({"velocity":self.piano.fixed_velocity})));ui.close_menu();}
            });
            ui.menu_button("Fit velocity range", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Map selected softest/loudest notes to a new velocity range.");
                ui.label(RichText::new("Preserves relative dynamics. Chords supported. Flat selections use Low; 0 means silence. Only velocity changes.").small());
                let (low,high)=self.piano.velocity_fit.get_or_insert((0.3,0.9));
                ui.add(egui::Slider::new(low,0.0..=1.0).step_by(0.01).text("Low"));
                ui.add(egui::Slider::new(high,0.0..=1.0).step_by(0.01).text("High"));
                let (low,high)=(*low,*high);
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::fit_velocity_range(&mut preview,&sel,low,high);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} velocities change",n)).small());},Err(_)=>{ui.label(RichText::new("Low must not exceed High. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Fit selected dynamics")).clicked(){op=Some(("fit_note_velocity_range",json!({"low":low,"high":high})));ui.close_menu();}
            });
            ui.menu_button("Balance chord velocities", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Give the selected notes of each chord the same MIDI velocity, using that chord's average.");
                ui.label(RichText::new("Solo notes stay unchanged. Only velocities change; partial selections balance only selected notes.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::balance_chord_velocities(&mut preview,&sel);
                if let Ok(n)=result {ui.label(RichText::new(format!("{} velocities change",n)).small());}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Balance selected chords")).clicked(){op=Some(("balance_chord_velocities",json!({})));ui.close_menu();}
            });
            ui.menu_button("Invert dynamics", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Loud notes become soft and soft become loud within the current selected range.");
                ui.label(RichText::new("Only velocities change. Chords are supported; rhythm, pitches and other expression stay fixed.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::invert_velocities(&mut preview,&sel);
                if let Ok(n)=result {ui.label(RichText::new(format!("{} velocities change",n)).small());}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Invert MIDI dynamics")).clicked(){op=Some(("invert_note_velocities",json!({})));ui.close_menu();}
            });
            ui.menu_button("Reverse velocities", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Reverse the existing loud/soft sequence through selected notes.");
                ui.label(RichText::new("Only velocities change. Rhythm, pitches and other expression stay fixed. Select one note per onset.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::reverse_velocities(&mut preview,&sel);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} velocities change",n)).small());},Err(_)=>{ui.label(RichText::new("Select one valid note per onset. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Reverse MIDI velocity sequence")).clicked(){op=Some(("reverse_note_velocities",json!({})));ui.close_menu();}
            });
            ui.menu_button("Rotate velocities", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Move existing accents through selected notes, wrapping at the ends.");
                ui.label(RichText::new("Only velocities change. Rhythm, pitches and other expression stay fixed. Select one note per onset.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                for (shift,label) in [(1,"Move accents one note later"),(-1,"Move accents one note earlier")] {
                    let mut preview=notes.clone();let result=crate::midi_ops::rotate_velocities(&mut preview,&sel,shift);
                    match &result {Ok(n)=>{ui.label(RichText::new(format!("{} velocities change",n)).small());},Err(_)=>{ui.label(RichText::new("Select one valid note per onset. Nothing will change.").small());}}
                    if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new(label)).clicked(){op=Some(("rotate_note_velocities",json!({"shift":shift})));ui.close_menu();}
                }
            });
            ui.menu_button("Normalize velocity", |ui| {
                ui.set_width(280.0);
                ui.set_max_width(280.0);
                if self.piano.normalize_peak<0.02 { self.piano.normalize_peak=0.9; }
                ui.horizontal(|ui| {ui.label("Target peak velocity");ui.add(egui::DragValue::new(&mut self.piano.normalize_peak).speed(0.01).range(0.02..=1.0));});
                ui.label("Preserves relative dynamics, not an audio loudness control");
                let sel=crate::midi_ops::Selection {ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let max=notes.iter().filter(|n|sel.matches(n)).map(|n|n.vel).fold(0.0,f32::max);
                ui.label(RichText::new(format!("Current selected peak: {:.2}",max)).small());
                if ui.add_enabled(max>0.0 && (max-self.piano.normalize_peak).abs()>1e-5,egui::Button::new("Normalize MIDI velocity")).clicked() {
                    op=Some(("normalize_velocity",json!({"peak":self.piano.normalize_peak})));ui.close_menu();
                }
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Velocity LFO")).clicked() {op=Some(("lfo_tool",json!({"property":"velocity","shape":"sine","rate":"1 bar"})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Velocity +10 points")).clicked() {op=Some(("offset_note_velocities",json!({"delta":0.1})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Velocity -10 points")).clicked() {op=Some(("offset_note_velocities",json!({"delta":-0.1})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Velocity & dynamics");}
            let group=egui::CollapsingHeader::new("Cleanup").open(Some(active_group=="Cleanup")).show(ui, |ui| {
            ui.menu_button("Note microtiming", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Set note timing offsets in sixteenth steps. Negative = early; positive = late.");
                ui.label(RichText::new("Nominal starts, velocity, lengths and chance stay unchanged. Global swing stays; playback/export clamp before song start.").small());
                ui.add(egui::Slider::new(&mut self.piano.note_offset,-0.5..=0.5).step_by(0.01).text("Offset"));
                let offset=self.piano.note_offset;
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::set_note_offset(&mut preview,&sel,offset);
                if let Ok(n)=&result{ui.label(RichText::new(format!("{} note offsets change",n)).small());}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Set selected microtiming")).clicked(){op=Some(("set_note_offset",json!({"offset":offset})));ui.close_menu();}
            });
            ui.menu_button("Note chance ramp", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Ramp trigger chance across selected note starts. Chord notes share one chance; one onset uses Start.");
                ui.label(RichText::new("Chance only. 0% never plays; 100% always plays. Seeded probability, not a guaranteed hit count. Other expression stays unchanged.").small());
                let (start,end)=self.piano.chance_ramp.get_or_insert((0.25,1.0));
                ui.add(egui::Slider::new(start,0.0..=1.0).step_by(0.01).text("Start"));ui.add(egui::Slider::new(end,0.0..=1.0).step_by(0.01).text("End"));
                let (start,end)=(*start,*end);
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::ramp_note_chance(&mut preview,&sel,start,end);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note chances change",n)).small());},Err(_)=>{ui.label(RichText::new("Invalid selected onsets. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Apply selected chance ramp")).clicked(){op=Some(("ramp_note_chance",json!({"start_chance":start,"end_chance":end})));ui.close_menu();}
            });
            ui.menu_button("Note chance", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Set per-note trigger chance. 0% = never; 100% = always. Notes are not deleted.");
                ui.label(RichText::new("Velocity, lengths, pitch, offsets and slide targets stay unchanged. Playback uses seeded chance, not a guaranteed hit count.").small());
                let percent=self.piano.chance_percent.get_or_insert(100.0);
                ui.add(egui::Slider::new(percent,0.0..=100.0).text("Chance %"));
                let chance=(*percent/100.0).clamp(0.0,1.0);
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::set_note_chance(&mut preview,&sel,chance);
                if let Ok(n)=&result{ui.label(RichText::new(format!("{} note probabilities change",n)).small());}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Set selected note chance")).clicked(){op=Some(("set_note_chance",json!({"chance":chance})));ui.close_menu();}
            });
            ui.menu_button("Reset humanization", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Clear note timing offsets or restore 100% chance. Velocity, lengths, pitches and slide targets stay unchanged.");
                ui.label(RichText::new("Global swing and other groove settings are not reset.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                for(mode,timing,chance,label)in[("timing",true,false,"Clear timing offsets"),("chance",false,true,"Restore 100% chance"),("both",true,true,"Reset timing and chance")]{
                    let mut preview=notes.clone();let result=crate::midi_ops::reset_humanization(&mut preview,&sel,timing,chance);
                    if let Ok(n)=&result{ui.label(RichText::new(format!("{}: {} notes change",label,n)).small());}
                    if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new(label)).clicked(){op=Some(("reset_note_humanization",json!({"mode":mode})));ui.close_menu();}
                }
            });
            ui.menu_button("Trim at pattern end", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Shorten selected notes that sustain beyond this pattern's end.");
                ui.label(RichText::new("Never lengthens or removes notes. Starts, pitches and expression stay unchanged; uses nominal MIDI timing.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let result=crate::midi_ops::trim_pattern_ends(&mut preview,&sel,steps);
                match &result {Ok(n)=>{ui.label(RichText::new(format!("{} note lengths change",n)).small());},Err(_)=>{ui.label(RichText::new("A selected note has invalid bounds or less than0.05 steps before the end. Nothing will change.").small());}}
                if ui.add_enabled(matches!(result,Ok(n) if n>0),egui::Button::new("Trim selected note tails")).clicked(){op=Some(("trim_notes_to_pattern",json!({})));ui.close_menu();}
            });

            ui.menu_button("Remove zero-level notes", |ui| {
                ui.set_width(280.0);ui.set_max_width(280.0);
                ui.label("Delete selected notes with exactly zero velocity or zero trigger chance. Very quiet or low-chance notes remain.");
                ui.label(RichText::new("Deletion can change seeded chance outcomes on surviving notes. Their saved fields and order stay unchanged. Undo restores all removed notes.").small());
                let sel=crate::midi_ops::Selection{ids:self.piano.selected.iter().filter_map(|i|notes.get(*i)).map(|n|n.id.clone()).collect(),..Default::default()};
                let mut preview=notes.clone();let count=crate::midi_ops::remove_zero_notes(&mut preview,&sel);
                ui.label(RichText::new(format!("{} notes removed",count)).small());
                if ui.add_enabled(count>0,egui::Button::new("Remove selected zero-level notes")).clicked(){op=Some(("remove_zero_notes",json!({})));ui.close_menu();}
            });
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Trim same-pitch overlaps")).clicked() {op=Some(("trim_note_overlaps",json!({})));ui.close_menu();}
                if ui.add_enabled(!notes.is_empty(), egui::Button::new("Remove exact duplicates")).clicked() {op=Some(("remove_duplicate_notes",json!({})));ui.close_menu();}
            });
            if group.header_response.clicked() {clicked_group=Some("Cleanup");}
            if let Some(group)=clicked_group {ui.ctx().data_mut(|d|d.insert_temp(group_id,if active_group==group {String::new()} else {group.to_string()}));}
        });
        egui::ComboBox::from_id_salt(("snap", self.selected))
            .width(64.0)
            .selected_text(format!(
                "Snap {}",
                SNAPS[self.piano.snap.min(SNAPS.len() - 1)].0
            ))
            .show_ui(&mut tb, |ui| {
                for (i, (label, _)) in SNAPS.iter().enumerate() {
                    ui.selectable_value(&mut self.piano.snap, i, *label);
                }
            });
        let quantize_enabled = generation.is_some()
            && !self.dragging
            && self.piano.drag.is_none()
            && self.piano.band.is_none()
            && !notes.is_empty()
            && SNAPS
                .get(self.piano.snap)
                .is_some_and(|(_, grid)| grid.is_some());
        let quantize_hint = if notes.is_empty() {
            "No notes to quantize in this channel and pattern."
        } else if SNAPS.get(self.piano.snap).is_none_or(|(_, g)| g.is_none()) {
            "Choose a Snap value first. Snap off has no quantize grid."
        } else if !quantize_enabled {
            "Finish the held gesture and wait for a current project frame."
        } else if self.piano.selected.is_empty() {
            "Quantize all notes in this channel and pattern to the chosen Snap grid."
        } else {
            "Quantize only the selected notes to the chosen Snap grid."
        };
        if tb
            .add_enabled(quantize_enabled, egui::Button::new("Quantize feel").small())
            .on_hover_text(quantize_hint)
            .on_disabled_hover_text(quantize_hint)
            .clicked()
            && feel_gesture_idle(
                self.dragging,
                self.piano.drag.is_some(),
                self.piano.band.is_some(),
                ui.input(|i| i.pointer.any_down()),
            )
        {
            let selected = self
                .piano
                .selected
                .iter()
                .map(|i| notes[*i].id.clone())
                .collect();
            match FeelDraft::new(
                p,
                (
                    revision,
                    generation.expect("opening requires a frame stamp"),
                ),
                (self.selected, self.pattern, self.view),
                selected,
                self.piano.snap,
            ) {
                Ok(d) => {
                    self.piano.feel = Some(d);
                    feel_input_owner(ui.ctx(), true);
                    ui.memory_mut(|m| m.request_focus(feel_focus_id()));
                    return;
                }
                Err(e) => self.toast = Some((e, 0.0, false)),
            }
        }
        tb.menu_button("View", |ui| {
            if ui
                .button("Fit note pitches")
                .on_hover_text("Bring this pattern's notes back into view after scrolling")
                .clicked()
            {
                self.piano.span = None;
                self.piano.manual = false;
                ui.close_menu();
            }
            ui.separator();
            ui.label(RichText::new("Mouse").strong());
            ui.label("Wheel: scroll pitch rows");
            ui.label("Click empty space: add note. Right-click note: delete.");
            ui.label("Drag note: move. Drag right edge: resize.");
            ui.label("Drag a velocity stem: change that note's volume (bottom = 0%, silent).");
            ui.label("Shift-click or drag empty space: select notes.");
            ui.separator();
            ui.label(RichText::new("Keyboard while hovering the roll").strong());
            let command = if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Ctrl"
            };
            ui.label(format!(
                "{command}+A: select all. {command}+Shift+A: clear selection."
            ));
            ui.label("Arrows: move selected notes. Shift: octave / bar.");
            ui.label("Delete / Backspace: remove selected notes only.");
            ui.separator();
            ui.label("Esc: cancel a mouse preview before releasing.");
            ui.label("Switching away from the app also cancels the preview.");
            ui.label("Note shortcuts wait until the mouse hold is over.");
        });
        let selection_count = self.piano.selected.len();
        let note_context = if selection_count == 0 {
            format!("{} / {} / {} notes", t.name, pat.name, notes.len())
        } else {
            format!("{} selected / {} / {}", selection_count, t.name, pat.name)
        };
        tb.label(RichText::new(note_context).size(11.0).color(tk.text_dim))
            .on_hover_ui(|ui| {
                ui.set_width(280.0);
                ui.label(format!(
                    "Channel: {}\nPattern: {}\n{} notes; {} selected",
                    t.name,
                    pat.name,
                    notes.len(),
                    selection_count
                ));
                // Dense selections are scanned only while this tooltip is open.
                if let Some(summary) = selection_summary(&notes, &self.piano.selected) {
                    ui.label(summary);
                }
            });
        // ---- helper rows under the title row: gen helpers, then edit helpers
        // split across two rows. Fixed rows, each clipped to its own strip, so
        // every button stays inside the panel at any window size ----
        let gen_items: [(&str, &str, Value, &str); 7] = [
            (
                "Euclidean 5/16",
                "generate_euclidean",
                json!({"pulses":5,"slots":16}),
                "Replace this track's notes in this pattern with5 evenly spaced hits per bar, on16 subdivisions",
            ),
            (
                "Melody",
                "generate_melody",
                json!({}),
                "a fresh melody on this track",
            ),
            (
                "Fill pattern",
                "generate_fill",
                json!({"style": "auto"}),
                "a fill into the next section",
            ),
            (
                "Ghost",
                "generate_ghost_notes",
                json!({}),
                "quiet in-between hits",
            ),
            (
                "Variation track",
                "generate_variation",
                json!({"kind": "both"}),
                "a twist on this pattern",
            ),
            (
                "Counter",
                "counter_melody",
                json!({}),
                "an answering line for this track",
            ),
            (
                "Harmony",
                "harmonize",
                json!({"interval": "3rd"}),
                "a harmony line above this track",
            ),
        ];
        let edit_a: [(&str, &str, Value, &str); 7] = [
            ("Rotate +1 step", "rotate_notes", json!({"shift":1}), "Move selected notes (or all notes) a sixteenth later, wrapping inside each bar; lengths unchanged"),
            ("Rotate -1 step", "rotate_notes", json!({"shift":-1}), "Move selected notes (or all notes) a sixteenth earlier, wrapping inside each bar; lengths unchanged"),
            (
                "Humanize track",
                "humanize",
                json!({}),
                "loosen timing and velocity",
            ),
            (
                "Slides track",
                "add_slides",
                json!({}),
                "glides between notes",
            ),
            (
                "Roll",
                "roll_notes",
                json!({"mode": "roll", "count": 3}),
                "a roll on the selected steps",
            ),
            (
                "Split",
                "split_notes",
                json!({"every": "1/8"}),
                "cut held notes into 8ths",
            ),
            (
                "Merge",
                "merge_notes",
                json!({}),
                "glue touching notes together",
            ),
        ];
        let edit_b: [(&str, &str, Value, &str); 5] = [
            (
                "Snap track",
                "scale_snap",
                json!({}),
                "pull notes into the song scale",
            ),
            (
                "+12",
                "transpose_phrase",
                json!({"semitones": 12}),
                "up an octave; pitches only, rejects out-of-range notes",
            ),
            (
                "-12",
                "transpose_phrase",
                json!({"semitones": -12}),
                "down an octave; pitches only, rejects out-of-range notes",
            ),
            (
                "Drop2",
                "chord_voicing",
                json!({"mode": "drop2"}),
                "drop-2 chord voicing",
            ),
            (
                "VelRamp",
                "ramp_note_velocities",
                json!({"start_velocity":0.35,"end_velocity":1.0}),
                "velocity rising from35% to100%; only velocities change",
            ),
        ];
        let helpers = Rect::from_min_max(
            pos2(header.left() + 6.0, header.top() + HEADER_H),
            pos2(header.right() - 4.0, header.bottom()),
        );
        let mut try_rhythm = false;
        let mut tr = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(helpers)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        for (label, items) in [
            ("Compose", &gen_items[..]),
            ("Timing & expression", &edit_a[..]),
            ("Pitch & velocity", &edit_b[..]),
        ] {
            tr.menu_button(label, |ui| {
                if label == "Compose" {
                    ui.set_max_width(420.0);
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    ui.label("Euclidean rhythm");
                    ui.label(RichText::new("Scope: this track, whole current pattern").small());
                    ui.label(format!("Track: {} [{}]", t.name, t.id));
                    ui.label(format!("Pattern: {} [{}]", pat.name, pat.id));
                    ui.horizontal(|ui| {
                        ui.label("Subdivisions / bar");
                        ui.add(
                            egui::DragValue::new(&mut self.piano.euclidean.slots).range(1..=128),
                        );
                    });
                    self.piano.euclidean.pulses =
                        self.piano.euclidean.pulses.min(self.piano.euclidean.slots);
                    ui.horizontal(|ui| {
                        ui.label("Hits / bar");
                        ui.add(
                            egui::DragValue::new(&mut self.piano.euclidean.pulses)
                                .range(0..=self.piano.euclidean.slots),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Rotation (slots)");
                        ui.add(
                            egui::DragValue::new(&mut self.piano.euclidean.rotation)
                                .range(-128..=128),
                        );
                    });
                    ui.checkbox(
                        &mut self.piano.euclidean.replace,
                        "Replace track notes (off = layer)",
                    );
                    let action = if self.piano.euclidean.replace {
                        "Write notes (replace)"
                    } else {
                        "Write notes (layer)"
                    };
                    ui.label(format!(
                        "{} hits / {} slots, rotation {}. {} the whole track in this pattern.",
                        self.piano.euclidean.pulses, self.piano.euclidean.slots,
                        self.piano.euclidean.rotation,
                        if self.piano.euclidean.replace { "Replace" } else { "Layer onto" }
                    ));
                    ui.label("Try captures the WHOLE PROJECT before writing. Pattern edits affect every arranged occurrence, not only selected notes.");
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(action).clicked() {
                            op = Some(("generate_euclidean", self.piano.euclidean.args()));
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.piano.rhythm_trial.is_none(), egui::Button::new("Try & compare")).clicked() {
                            try_rhythm = true;
                            ui.close_menu();
                        }
                    });
                    if self.piano.rhythm_trial.is_some() {
                        ui.label("Finish the open rhythm trial before starting another.");
                    } else if !self.piano.rhythm_trial_status.is_empty() {
                        ui.label(&self.piano.rhythm_trial_status);
                    }
                    ui.separator();
                }
                for (label, tool, args, hover) in items {
                    if ui.button(*label).on_hover_text(*hover).clicked() {
                        op = Some((*tool, args.clone()));
                        ui.close_menu();
                    }
                }
            });
        }
        if try_rhythm {
            let selected_ids = self
                .piano
                .selected
                .iter()
                .filter_map(|i| notes.get(*i))
                .map(|n| n.id.clone())
                .collect();
            self.start_rhythm_trial(p);
            self.piano.reselect_ids = Some(selected_ids);
        }
        if let Some((id, mut a)) = op {
            let ids: Vec<&str> = self
                .piano
                .selected
                .iter()
                .filter_map(|i| notes.get(*i))
                .map(|n| n.id.as_str())
                .collect();
            prepare_helper_args(id, &mut a, &tname, &pname, &ids);
            let selected_ids = ids.iter().map(|id| (*id).to_owned()).collect();
            self.call(id, a);
            self.piano.reselect_ids = Some(selected_ids);
        }
        if delete_selection {
            self.delete_piano_notes(&tname, &pname, &notes, &self.piano.selected.clone());
            self.piano.selected.clear();
        }
        // ---- geometry ----
        let velocity_height = velocity_lane_height(full.height());
        let roll = Rect::from_min_max(
            pos2(full.min.x + KEY_W, header.max.y + PIANO_RULER_H + 2.0),
            pos2(full.max.x - 4.0, full.max.y - velocity_height - 4.0),
        );
        let keys = Rect::from_min_max(pos2(full.min.x, roll.min.y), pos2(roll.min.x, roll.max.y));
        let vel = Rect::from_min_max(
            pos2(roll.min.x, roll.max.y + 4.0),
            pos2(roll.max.x, full.max.y - 2.0),
        );
        let rows = f32::from(hi - lo + 1);
        let row_h = roll.height() / rows;
        let cell = roll.width() / steps.max(1.0);
        let x_of = |s: f32| roll.min.x + s * cell;
        let y_of = |pitch: i32| roll.min.y + (i32::from(hi) - pitch) as f32 * row_h;
        let ruler = Rect::from_min_max(
            pos2(roll.left(), header.bottom() + 2.0),
            pos2(roll.right(), roll.top()),
        );
        painter.rect_filled(ruler, 0.0, tk.panel_bg2);
        let rp = painter.with_clip_rect(ruler);
        let bar_steps = pat.steps_per_bar().max(1) as f32;
        let meter = pat.meter.unwrap_or((4, 4));
        let meter_rect = Rect::from_min_max(
            pos2(full.left(), ruler.top()),
            pos2(roll.left(), ruler.bottom()),
        );
        painter.text(
            meter_rect.center(),
            Align2::CENTER_CENTER,
            format!("{}/{}", meter.0, meter.1),
            FontId::monospace(10.0),
            tk.text_dim,
        );
        ui.interact(meter_rect, ui.id().with("piano_meter"), Sense::hover())
            .on_hover_text(format!(
                "Pattern meter: {}/{}\n{} sixteenth-note steps per bar\nBar lines follow this meter; Snap sets the edit grid.",
                meter.0, meter.1, pat.steps_per_bar()
            ));
        let stride = ruler_label_stride(bar_steps * cell);
        for b in 0..pat.bars {
            let x = x_of(b as f32 * bar_steps);
            rp.line_segment(
                [pos2(x, ruler.bottom() - 4.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0_f32, tk.text_dim),
            );
            if b % stride == 0 {
                rp.text(
                    pos2(x + 3.0, ruler.center().y),
                    Align2::LEFT_CENTER,
                    (b + 1).to_string(),
                    FontId::monospace(10.0),
                    tk.text_dim,
                );
            }
        }
        let pp = painter.with_clip_rect(roll.union(keys));
        for i in 0..=(hi - lo) {
            let pitch = hi - i;
            let y = roll.min.y + f32::from(i) * row_h;
            let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
            pp.rect_filled(
                Rect::from_min_size(pos2(roll.min.x, y), vec2(roll.width(), row_h)),
                0.0,
                if black {
                    super::theme::Tokens::DARK.playlist_bg
                } else {
                    super::theme::Tokens::DARK.playlist_alt
                },
            );
            let key =
                Rect::from_min_size(pos2(keys.min.x + 2.0, y), vec2(KEY_W - 4.0, row_h - 0.5));
            pp.rect_filled(
                key,
                1.0,
                if black {
                    Color32::from_rgb(20, 20, 20)
                } else {
                    Color32::from_rgb(225, 225, 225)
                },
            );
            if pitch % 12 == 0 {
                pp.line_segment(
                    [pos2(roll.min.x, y + row_h), pos2(roll.max.x, y + row_h)],
                    Stroke::new(1.0_f32, super::theme::Tokens::DARK.bar_line),
                );
                if row_h > 6.0 {
                    pp.text(
                        pos2(key.max.x - 2.0, key.center().y),
                        Align2::RIGHT_CENTER,
                        theory::note_name(pitch),
                        FontId::proportional((row_h * 0.85).clamp(6.0, 9.0)),
                        Color32::from_rgb(40, 40, 40),
                    );
                }
            }
        }
        // grid lines at the snap value; beats and bars stronger
        let gs = g.max(0.25);
        let mut s = 0.0;
        while s <= steps + 0.001 {
            let bar_line = (s % bar_steps).abs() < 1e-3;
            let beat = (s % 4.0).abs() < 1e-3;
            if !bar_line && (gs * cell > 4.0 || beat) {
                pp.line_segment(
                    [pos2(x_of(s), roll.min.y), pos2(x_of(s), roll.max.y)],
                    Stroke::new(
                        1.0_f32,
                        if beat {
                            super::theme::Tokens::DARK.bar_line
                        } else {
                            super::theme::Tokens::DARK.grid_line
                        },
                    ),
                );
            }
            s += gs;
        }
        // Bar boundaries must not depend on snap spacing: in 7/8, for example,
        // a 14-step bar is not divisible by a quarter-note (4-step) grid.
        for b in 0..=pat.bars {
            let x = x_of(b as f32 * bar_steps);
            pp.line_segment(
                [pos2(x, roll.top()), pos2(x, roll.bottom())],
                Stroke::new(1.0_f32, tk.border_light),
            );
        }
        if !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
            self.piano.gesture_cancelled = false;
        }
        if ui.input(|i| !i.focused || i.key_pressed(egui::Key::Escape)) {
            self.piano.drag = None;
            self.piano.band = None;
            self.piano.gesture_cancelled = true;
            let vid = ui.id().with(("vel", self.selected, self.pattern));
            ui.ctx().data_mut(|d| d.remove::<(String, f32)>(vid));
            self.dragging = false;
        }
        if let Some(d) = &self.piano.drag {
            if !d.shift.resize
                && d.shift.d_steps != 0.0
                && crate::note_edit::group_move_delta(
                    &notes,
                    &self.piano.selected,
                    d.shift.index,
                    d.shift.d_steps,
                    steps,
                ) != d.shift.d_steps
            {
                pp.text(
                    pos2(roll.left() + 8.0, roll.top() + 8.0),
                    Align2::LEFT_TOP,
                    "Group move limited: preserves spacing; check pattern bounds",
                    FontId::proportional(11.0),
                    Color32::YELLOW,
                );
            }
        }
        // notes (drawn with the in-progress drag applied)
        let shown: std::borrow::Cow<'_, [Note]> = match &self.piano.drag {
            Some(d) => std::borrow::Cow::Owned(apply_shift(
                &notes,
                &self.piano.selected,
                &d.shift,
                steps,
                g,
            )),
            None => std::borrow::Cow::Borrowed(&notes),
        };
        let note_rect = |n: &Note| {
            Rect::from_min_max(
                pos2(x_of(n.start), y_of(i32::from(n.pitch)) + 0.5),
                pos2(
                    x_of(n.start + n.len).max(x_of(n.start) + 3.0),
                    y_of(i32::from(n.pitch)) + row_h - 0.5,
                ),
            )
        };
        let draw_selected = selected_index_mask(shown.len(), &self.piano.selected);
        for (i, n) in shown.iter().enumerate() {
            if n.pitch < lo || n.pitch > hi {
                continue;
            }
            let r = note_rect(n);
            let sel = draw_selected[i];
            let k = 0.45 + 0.55 * n.vel.clamp(0.0, 1.0);
            let fill = if sel {
                Color32::WHITE
            } else {
                Color32::from_rgb(
                    (f32::from(color.r()) * k) as u8,
                    (f32::from(color.g()) * k) as u8,
                    (f32::from(color.b()) * k) as u8,
                )
            };
            pp.rect(r, 2.0, fill, Stroke::new(1.0_f32, Color32::BLACK));
        }
        // playhead
        if let Some(ps) = super::locate(p, self.player.position_secs())
            .filter(|(pi, _)| *pi == self.pattern)
            .map(|(_, s)| s)
        {
            pp.line_segment(
                [pos2(x_of(ps), roll.min.y), pos2(x_of(ps), roll.max.y)],
                Stroke::new(1.5_f32, tk.playhead),
            );
        }
        // ---- interaction ----
        let resp = ui.interact(
            roll,
            ui.id().with(("roll", self.selected)),
            Sense::click_and_drag(),
        );
        let hit = |pos: Pos2| -> Option<(usize, bool)> {
            notes.iter().enumerate().rev().find_map(|(i, n)| {
                let r = note_rect(n).expand2(vec2(0.0, 0.5));
                r.contains(pos).then_some((i, pos.x > r.max.x - 5.0))
            })
        };
        if let Some((i, edge)) = resp.hover_pos().and_then(hit) {
            let note = &notes[i];
            let hint = if edge {
                "Drag right edge: resize note (Esc cancels)"
            } else {
                "Drag note: move (Esc cancels)"
            };
            ui.ctx().set_cursor_icon(if edge {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Grab
            });
            if edge && self.piano.drag.is_none() {
                let r = note_rect(note);
                pp.line_segment(
                    [
                        pos2(r.right() - 1.0, r.top()),
                        pos2(r.right() - 1.0, r.bottom()),
                    ],
                    Stroke::new(2.0_f32, Color32::WHITE),
                );
            }
            let start = note.start.max(0.0);
            let bar = (start / bar_steps).floor() as u32 + 1;
            let step_in_bar = start % bar_steps + 1.0;
            resp.clone().on_hover_text(format!(
                "{} (MIDI {})\nBar {}, step {:.2}\nLength {:.2} steps\nVelocity {:.0}%\n{}\n{}\nRight-click: delete note",
                theory::note_name(note.pitch),
                note.pitch,
                bar,
                step_in_bar,
                note.len,
                note.vel * 100.0,
                note_expression_hint(note),
                hint,
            ));
        }
        if !self.piano.gesture_cancelled && resp.drag_started() {
            if let Some(pt) = resp.interact_pointer_pos() {
                // egui starts a drag only after the pointer crosses its threshold.
                // Hit-test and measure from the original press, not that later point,
                // so a narrow resize edge and short snapped move are not lost.
                let start = ui.input(|i| i.pointer.press_origin()).unwrap_or(pt);
                match hit(start) {
                    Some((i, edge)) => {
                        if !self.piano.selected.contains(&i) {
                            self.piano.selected = vec![i];
                        }
                        self.piano.drag = Some(NoteDrag {
                            id: notes[i].id.clone(),
                            start,
                            shift: NoteShift {
                                index: i,
                                resize: edge,
                                d_steps: 0.0,
                                d_semi: 0,
                            },
                        });
                    }
                    None => self.piano.band = Some(start),
                }
                self.dragging = true;
            }
        }
        if !self.piano.gesture_cancelled && resp.dragged() {
            if let (Some(d), Some(pt)) = (&mut self.piano.drag, resp.interact_pointer_pos()) {
                d.shift.d_steps = snap_to((pt.x - d.start.x) / cell, g);
                d.shift.d_semi = if d.shift.resize {
                    0
                } else {
                    -((pt.y - d.start.y) / row_h).round() as i32
                };
            }
        }
        if let (Some(a), Some(b)) = (self.piano.band, ui.ctx().pointer_latest_pos()) {
            let band = Rect::from_two_pos(a, b);
            painter.rect(
                band,
                0.0,
                Color32::from_rgba_unmultiplied(120, 170, 255, 40),
                Stroke::new(1.0_f32, Color32::from_rgb(120, 170, 255)),
            );
            if !self.piano.gesture_cancelled && resp.drag_stopped() {
                self.piano.selected = notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| note_rect(n).intersects(band))
                    .map(|(i, _)| i)
                    .collect();
                self.piano.band = None;
                self.dragging = false;
            }
        }
        if !self.piano.gesture_cancelled && resp.drag_stopped() {
            if let Some(d) = self.piano.drag.take() {
                if d.shift.d_steps.abs() > 1e-4 || d.shift.d_semi != 0 {
                    let moved = apply_shift(&notes, &self.piano.selected, &d.shift, steps, g);
                    self.write_notes(&tname, &pname, &moved, &notes);
                    // keep the moved notes selected
                    let mut sel = self.piano.selected.clone();
                    if !sel.contains(&d.shift.index) {
                        sel.push(d.shift.index);
                    }
                    self.piano.reselect_ids = Some(
                        sel.iter()
                            .filter_map(|i| moved.get(*i))
                            .map(|n| n.id.clone())
                            .collect(),
                    );
                }
            }
            self.dragging = false;
        }
        if !self.piano.gesture_cancelled && resp.clicked() {
            if let Some(pt) = resp.interact_pointer_pos() {
                match hit(pt) {
                    Some((i, _)) => {
                        if ui.input(|x| x.modifiers.shift) {
                            if let Some(k) = self.piano.selected.iter().position(|s| *s == i) {
                                self.piano.selected.remove(k);
                            } else {
                                self.piano.selected.push(i);
                            }
                        } else {
                            self.piano.selected = vec![i];
                        }
                    }
                    None => {
                        let pitch = (i32::from(hi) - ((pt.y - roll.min.y) / row_h).floor() as i32)
                            .clamp(0, 127);
                        let start = ((pt.x - roll.min.x) / cell / g).floor() * g;
                        let len = if is_drum { 1.0 } else { g.max(1.0) * 2.0 };
                        self.call(
                            "add_notes",
                            json!({"track": tname, "pattern": pname, "notes": [{"start": start.max(0.0), "len": len, "pitch": pitch, "vel": 0.8}]}),
                        );
                        self.piano.selected.clear();
                    }
                }
            }
        }
        if !self.piano.gesture_cancelled && resp.secondary_clicked() {
            if let Some((i, _)) = resp.interact_pointer_pos().and_then(hit) {
                self.delete_piano_notes(&tname, &pname, &notes, &[i]);
                self.piano.selected.clear();
            }
        }
        // keyboard while hovering: Cmd+A / Cmd+Shift+A, arrows (Shift = octave / bar), Delete
        // A held mouse gesture owns the edit until release/cancellation. Do not
        // let note shortcuts mutate its source notes beneath the live preview.
        if ui.rect_contains_pointer(full)
            && !ui.ctx().wants_keyboard_input()
            && !ui.input(|i| i.pointer.any_down() || i.pointer.any_released())
        {
            let selection_key = ui.input(|i| selection_shortcut(&i.events));
            let del = ui
                .input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
            let (semi, dx) = ui.input(|i| note_arrow_shortcut(&i.events, g, bar_steps));
            match selection_key {
                Some(false) => self.piano.selected = (0..notes.len()).collect(),
                Some(true) => self.piano.selected.clear(),
                None => {}
            }
            if (semi != 0 || dx != 0.0) && !self.piano.selected.is_empty() {
                let d = NoteShift {
                    index: self.piano.selected[0],
                    resize: false,
                    d_steps: dx,
                    d_semi: semi,
                };
                let moved = apply_shift(&notes, &self.piano.selected, &d, steps, g);
                self.write_notes(&tname, &pname, &moved, &notes);
                // Keep identity even when a moved note overlaps an identical note.
                self.piano.reselect_ids = Some(
                    self.piano
                        .selected
                        .iter()
                        .filter_map(|i| moved.get(*i))
                        .map(|n| n.id.clone())
                        .collect(),
                );
            }
            if del && !self.piano.selected.is_empty() {
                self.delete_piano_notes(&tname, &pname, &notes, &self.piano.selected.clone());
                self.piano.selected.clear();
            }
        }
        // ---- velocity lane ----
        painter.rect_filled(vel, 0.0, super::theme::Tokens::DARK.slot_bg);
        painter.text(
            pos2(full.min.x + 4.0, vel.center().y),
            Align2::LEFT_CENTER,
            "VEL",
            FontId::proportional(9.0),
            tk.text_dim,
        );
        let vid = ui.id().with(("vel", self.selected, self.pattern));
        let vresp = ui.interact(vel, vid, Sense::click_and_drag());
        // live velocity edit kept in egui memory until the drag ends (one undo step)
        let mut live: Option<(String, f32)> = ui.ctx().data(|d| d.get_temp(vid));
        if live
            .as_ref()
            .is_some_and(|(id, _)| unique_note_index(&notes, id).is_none())
        {
            // Do not retarget a held gesture after its note disappears or its
            // identity becomes ambiguous, even if another stem is now nearby.
            live = None;
            ui.ctx().data_mut(|d| d.remove::<(String, f32)>(vid));
            self.piano.gesture_cancelled = true;
            self.dragging = false;
        }
        if !self.piano.gesture_cancelled && (vresp.clicked() || vresp.dragged()) {
            if let Some(pt) = vresp.interact_pointer_pos() {
                // Lock onto the stem pressed, not the pointer position after
                // crossing egui's drag threshold (which may be another stem).
                let origin = if live.is_none() && vresp.dragged() {
                    ui.input(|i| i.pointer.press_origin()).unwrap_or(pt)
                } else {
                    pt
                };
                let near = nearest_velocity_note(
                    &notes,
                    &self.piano.selected,
                    (origin.x - roll.min.x) / cell,
                    cell,
                );
                let target = live
                    .as_ref()
                    .and_then(|(id, _)| unique_note_index(&notes, id))
                    .or_else(|| {
                        if live.is_none() {
                            near.filter(|i| unique_note_index(&notes, &notes[*i].id) == Some(*i))
                        } else {
                            None
                        }
                    });
                if let Some(i) = target {
                    if let Some(v) = velocity_lane_value(vel.max.y, vel.height(), pt.y) {
                        live = Some((notes[i].id.clone(), v));
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(vid, (notes[i].id.clone(), v)));
                        self.dragging = true;
                    }
                }
            }
        }
        // Interaction above may change selection; take a fresh drawing snapshot.
        let draw_selected = selected_index_mask(notes.len(), &self.piano.selected);
        for (i, n) in notes.iter().enumerate() {
            let v = match &live {
                Some((id, lv)) if *id == n.id => *lv,
                _ => n.vel,
            };
            let x = x_of(n.start) + 1.0;
            let h = (vel.height() - 4.0) * v.clamp(0.0, 1.0);
            let col = if draw_selected[i] {
                Color32::WHITE
            } else {
                color
            };
            painter.line_segment(
                [pos2(x, vel.max.y - 2.0), pos2(x, vel.max.y - 2.0 - h)],
                Stroke::new(3.0_f32, col),
            );
            painter.circle_filled(pos2(x, vel.max.y - 2.0 - h), 2.5, col);
        }
        // value readout so an edit shows its number, like FL's hint bar: the
        // live value while editing, the hovered hit's value otherwise
        let readout = match &live {
            Some((id, lv)) => notes
                .iter()
                .find(|n| &n.id == id)
                .map(|n| (n.start, n.pitch, *lv, tk.text)),
            None if vresp.hovered() => ui.input(|i| i.pointer.hover_pos()).and_then(|pt| {
                nearest_velocity_note(
                    &notes,
                    &self.piano.selected,
                    (pt.x - roll.min.x) / cell,
                    cell,
                )
                .and_then(|i| notes.get(i))
                .map(|n| (n.start, n.pitch, n.vel, tk.text_dim))
            }),
            None => None,
        };
        if let Some((st, pitch, v, col)) = readout {
            let label = painter.layout_no_wrap(
                velocity_readout_label(pitch, v),
                FontId::proportional(10.0),
                col,
            );
            let desired = pos2(
                x_of(st) + 6.0,
                vel.max.y - 6.0 - (vel.height() - 4.0) * v - label.size().y,
            );
            let origin = velocity_label_origin(vel, desired, label.size());
            painter.with_clip_rect(vel).galley(origin, label, col);
        }

        if !self.piano.gesture_cancelled
            && (vresp.drag_stopped() || (vresp.clicked() && !vresp.dragged()))
        {
            if let Some((id, v)) = live {
                if let Some(i) = unique_note_index(&notes, &id) {
                    let n = &notes[i];
                    self.call(
                        "set_note_velocity",
                        json!({"track":tname,"pattern":pname,"ids":[n.id],"velocity":v}),
                    );
                }
            }
            ui.ctx().data_mut(|d| d.remove::<(String, f32)>(vid));
            self.dragging = false;
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    #[test]
    fn custom_stretch_preview_and_anchor() {
        let mut n = Note::new(4.0, 2.0, 60, 0.8);
        n.id = "a".into();
        let mut late = Note::new(60.0, 4.0, 64, 0.8);
        late.id = "b".into();
        let notes = vec![n, late];
        let mut c = StretchControls::default();
        assert_eq!(c.preview(&notes, &[0], 64.0).unwrap(), 1);
        assert!(c.preview(&notes, &[], 64.0).is_err());
        assert_eq!(c.args(), json!({"factor":1.5}));
        c.pattern_anchor = true;
        assert_eq!(c.args(), json!({"factor":1.5,"anchor":0.0}));
        c.factor = 16.0;
        assert!(c.preview(&notes, &[0], 64.0).is_err());
        c.pattern_anchor = false;
        assert!(c.preview(&notes, &[0], 64.0).is_ok());
    }
    #[test]
    fn repeat_preview_tracks_selection_bounds_and_meter() {
        let notes = vec![Note::new(0.0, 1.0, 60, 0.8), Note::new(60.0, 1.0, 64, 0.8)];
        assert_eq!(repeat_capacity(&notes, &[], 16.0, 64.0), (2, 0));
        assert_eq!(repeat_capacity(&notes, &[0], 16.0, 64.0), (1, 3));
        assert_eq!(repeat_capacity(&notes, &[0], 14.0, 56.0), (1, 3));
        assert_eq!(repeat_capacity(&notes, &[0], 0.0, 64.0), (0, 0));
        assert_eq!(repeat_capacity(&[], &[], 16.0, 64.0), (0, 0));
        assert_eq!(
            repeat_capacity(&[Note::new(0.0, 16.0, 60, 1.0)], &[], 16.0, 64.0),
            (1, 3)
        );
    }
    #[test]
    fn compact_velocity_lane_preserves_note_space() {
        assert_eq!(velocity_lane_height(104.0), 16.0);
        assert_eq!(velocity_lane_height(136.0), 28.0);
        assert_eq!(velocity_lane_height(240.0), 48.0);
        for h in [120.0, 136.0, 160.0, 240.0] {
            assert!(h - HEADER_H * 2.0 - PIANO_RULER_H - velocity_lane_height(h) - 6.0 >= 38.0);
        }
    }

    #[test]
    fn echo_controls_default_and_clamp() {
        assert_eq!(
            EchoControls::default().args(),
            json!({"delay":2.0,"repeats":3,"decay":0.6_f32,"transpose":0})
        );
        let c = EchoControls {
            delay: 0.0,
            repeats: 0,
            decay: 2.0,
            transpose: 200,
        };
        assert_eq!(
            c.args(),
            json!({"delay":0.0625,"repeats":1,"decay":1.0,"transpose":127})
        );
    }

    #[test]
    fn euclidean_controls_clamp_and_keep_layer_intent() {
        assert_eq!(
            EuclideanControls::default().args(),
            json!({"pulses":5,"slots":16,"rotation":0,"replace":true})
        );
        let c = EuclideanControls {
            pulses: 5,
            slots: 0,
            rotation: -1,
            replace: false,
        };
        assert_eq!(
            c.args(),
            json!({"pulses":1,"slots":1,"rotation":-1,"replace":false})
        );
        let c = EuclideanControls {
            pulses: 500,
            slots: 500,
            rotation: 12,
            replace: true,
        };
        assert_eq!(c.args()["slots"], 128);
        assert_eq!(c.args()["pulses"], 128);
    }

    #[test]
    fn bar_nudge_follows_pattern_meter_not_four_four() {
        for bar in [12.0, 14.0, 16.0, 20.0] {
            assert_eq!(horizontal_nudge(false, true, true, 0.5, bar), bar);
            assert_eq!(horizontal_nudge(true, false, true, 0.5, bar), -bar);
            assert_eq!(horizontal_nudge(false, true, false, 0.5, bar), 0.5);
            assert_eq!(horizontal_nudge(true, true, true, 0.5, bar), 0.0);
        }
    }
    #[test]
    fn ruler_labels_keep_readable_spacing() {
        assert_eq!(ruler_label_stride(120.0), 1);
        assert_eq!(ruler_label_stride(20.0), 2);
        for width in [0.5_f32, 2.0, 10.0, 20.0, 40.0, 160.0] {
            assert!(ruler_label_stride(width) as f32 * width >= 32.0);
        }
    }
    #[test]
    fn helper_scope_matches_live_tool_schema() {
        for id in [
            "generate_euclidean",
            "echo_notes",
            "balance_chord_velocities",
            "fit_note_velocity_range",
            "invert_note_velocities",
            "reverse_note_velocities",
            "rotate_note_velocities",
            "reverse_melody_pitches",
            "reverse_rhythm_gaps",
            "rotate_melody_pitches",
            "align_chord_ends",
            "distribute_note_rhythm",
            "fold_note_register",
            "reduce_chord_voices",
            "quantize_note_ends",
            "transpose_phrase",
            "shift_phrase",
            "normalize_velocity",
            "thin_notes",
            "repeat_notes",
            "stretch_notes",
            "rotate_notes",
            "set_note_offset",
            "set_note_velocity",
            "ramp_note_velocities",
            "set_note_chance",
            "ramp_note_chance",
            "reset_note_humanization",
            "rotate_note_lengths",
            "invert_note_lengths",
            "balance_chord_lengths",
            "scale_note_lengths",
            "set_note_lengths",
            "reverse_note_lengths",
            "trim_notes_to_pattern",
            "trim_note_overlaps",
            "remove_duplicate_notes",
            "remove_zero_notes",
            "edit_notes",
            "quantize",
            "legato",
            "articulate",
            "lfo_tool",
            "flip_notes",
            "generate_melody",
            "generate_fill",
            "generate_ghost_notes",
            "generate_variation",
            "counter_melody",
            "harmonize",
            "humanize",
            "add_slides",
            "roll_notes",
            "split_notes",
            "merge_notes",
            "scale_snap",
            "chord_voicing",
            "velocity_curve",
        ] {
            let mut a = json!({});
            prepare_helper_args(id, &mut a, "hat", "verse", &["n1"]);
            let schema = (crate::tools::find(id).unwrap().schema)();
            let props = schema["properties"].as_object().unwrap();
            assert!(
                a.as_object().unwrap().keys().all(|k| props.contains_key(k)),
                "{id}: {a}"
            );
            assert_eq!(a.get("ids").is_some(), props.contains_key("ids"), "{id}");
        }
        let mut chop = json!({"pattern":"x.x.x.x."});
        prepare_helper_args("chop_notes", &mut chop, "hat", "verse", &["n1"]);
        assert_eq!(
            chop,
            json!({"pattern":"x.x.x.x.", "target_pattern":"verse", "track":"hat", "ids":["n1"]})
        );
        let mut fill = json!({"style":"auto"});
        prepare_helper_args("generate_fill", &mut fill, "hat", "verse", &["n1"]);
        assert_eq!(fill, json!({"style":"auto","pattern":"verse"}));
        let mut edit = json!({"transpose":12});
        prepare_helper_args("edit_notes", &mut edit, "hat", "verse", &["n1"]);
        assert_eq!(
            edit,
            json!({"transpose":12,"track":"hat","pattern":"verse","ids":["n1"]})
        );
    }
}

#[cfg(test)]
mod pitch_selection_tests {
    use super::*;
    #[test]
    fn matching_pitches_expand_without_changing_notes() {
        let notes: Vec<Note> = [60, 62, 60, 64, 62]
            .iter()
            .enumerate()
            .map(|(i, p)| Note {
                start: i as f32,
                len: 1.0,
                pitch: *p,
                vel: 0.5,
                prob: 0.6,
                offset: 0.2,
                slide_to: Some(70),
                id: format!("n{i}"),
            })
            .collect();
        let before = serde_json::to_value(&notes).unwrap();
        assert_eq!(matching_pitch_selection(&notes, &[0]), vec![0, 2]);
        assert_eq!(
            matching_pitch_selection(&notes, &[0, 4, 0, 999]),
            vec![0, 1, 2, 4]
        );
        assert!(matching_pitch_selection(&notes, &[]).is_empty());
        assert!(matching_pitch_selection(&notes, &[999]).is_empty());
        assert_eq!(serde_json::to_value(&notes).unwrap(), before);
    }
    #[test]
    fn matching_onsets_expand_chords_not_overlapping_sustains() {
        let notes: Vec<Note> = [0.0, 0.0, 0.00005, 1.0, 2.0, 2.0]
            .iter()
            .enumerate()
            .map(|(i, t)| Note {
                start: *t,
                len: 4.0,
                pitch: 60 + i as u8,
                vel: 0.5,
                prob: 0.7,
                offset: 0.1,
                slide_to: None,
                id: format!("n{i}"),
            })
            .collect();
        let before = serde_json::to_value(&notes).unwrap();
        assert_eq!(matching_onset_selection(&notes, &[0]), vec![0, 1, 2]);
        assert_eq!(
            matching_onset_selection(&notes, &[0, 4, 4, 999]),
            vec![0, 1, 2, 4, 5]
        );
        assert_eq!(matching_onset_selection(&notes, &[3]), vec![3]);
        assert!(matching_onset_selection(&notes, &[]).is_empty());
        assert!(matching_onset_selection(&notes, &[999]).is_empty());
        assert_eq!(serde_json::to_value(&notes).unwrap(), before);
    }
    #[test]
    fn selection_complement_is_sorted_unique_and_handles_stale_indices() {
        assert_eq!(inverted_note_selection(5, &[0, 2, 2, 99]), vec![1, 3, 4]);
        assert_eq!(inverted_note_selection(3, &[]), vec![0, 1, 2]);
        assert!(inverted_note_selection(3, &[0, 1, 2]).is_empty());
        assert!(inverted_note_selection(0, &[99]).is_empty());
        let original = vec![0, 2];
        assert_eq!(
            inverted_note_selection(5, &inverted_note_selection(5, &original)),
            original
        );
    }
    #[test]
    fn overlap_selection_finds_both_sides_and_nested_tails_not_chord_tones() {
        let make = |start, len, pitch| Note {
            start,
            len,
            pitch,
            vel: 0.5,
            prob: 0.7,
            offset: 0.1,
            slide_to: None,
            id: String::new(),
        };
        let notes = vec![
            make(2.0, 1.0, 60),
            make(0.0, 4.0, 60),
            make(4.0, 1.0, 60),
            make(0.0, 4.0, 64),
            make(0.0, 0.0, 60),
            make(1.0, 1.0, 60),
            make(8.0, 1.0, 60),
        ];
        let before = serde_json::to_value(&notes).unwrap();
        assert_eq!(overlapping_pitch_selection(&notes), vec![0, 1, 5]);
        assert_eq!(
            overlapping_pitch_selection(&[make(0.0, 1.0, 60), make(0.0, 1.0, 60)]),
            vec![0, 1]
        );
        assert!(overlapping_pitch_selection(&[make(0.0, 1.0, 60), make(1.0, 1.0, 60)]).is_empty());
        assert!(overlapping_pitch_selection(&[
            make(f32::NAN, 1.0, 60),
            make(0.0, f32::INFINITY, 60)
        ])
        .is_empty());
        assert!(overlapping_pitch_selection(&[]).is_empty());
        assert_eq!(serde_json::to_value(&notes).unwrap(), before);
    }
    #[test]
    fn outside_scale_selection_respects_root_mode_and_leaves_notes_unchanged() {
        let notes: Vec<Note> = (60..72)
            .map(|pitch| Note {
                start: 0.0,
                len: 1.0,
                pitch,
                vel: 0.5,
                prob: 1.0,
                offset: 0.0,
                slide_to: None,
                id: String::new(),
            })
            .collect();
        let before = serde_json::to_value(&notes).unwrap();
        assert_eq!(
            outside_scale_selection(&notes, "C", "major"),
            Some(vec![1, 3, 6, 8, 10])
        );
        assert_eq!(
            outside_scale_selection(&notes, "C", "minor"),
            Some(vec![1, 4, 6, 9, 11])
        );
        assert_eq!(
            outside_scale_selection(&notes, "D", "major"),
            Some(vec![0, 3, 5, 8, 10])
        );
        assert_eq!(
            outside_scale_selection(&notes, "Bb", "pentatonic minor"),
            outside_scale_selection(&notes, "A#", "pentatonic_minor")
        );
        assert!(outside_scale_selection(&notes, "?", "major").is_none());
        assert!(outside_scale_selection(&notes, "C", "unknown").is_none());
        assert_eq!(outside_scale_selection(&[], "C", "major"), Some(vec![]));
        assert_eq!(serde_json::to_value(&notes).unwrap(), before);
    }
    #[test]
    fn sorted_onset_lookup_matches_reference_for_large_unsorted_selection() {
        let notes: Vec<Note> = (0..20_000)
            .map(|i| Note {
                start: ((i * 7919) % 4000) as f32 * 0.25 + if i % 3 == 0 { 0.00005 } else { 0.0 },
                len: 1.0,
                pitch: 60,
                vel: 0.5,
                prob: 1.0,
                offset: 0.0,
                slide_to: None,
                id: String::new(),
            })
            .collect();
        let selected: Vec<usize> = (0..notes.len()).step_by(127).rev().collect();
        let expected: Vec<usize> = notes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                selected
                    .iter()
                    .any(|j| (n.start - notes[*j].start).abs() <= 0.0001)
                    .then_some(i)
            })
            .collect();
        assert_eq!(matching_onset_selection(&notes, &selected), expected);
    }
    #[test]
    fn overlap_lookup_matches_pairwise_reference_across_shuffled_nested_notes() {
        for seed in 0..24u32 {
            let notes: Vec<Note> = (0..100u32)
                .map(|i| {
                    let h = i.wrapping_mul(1664525).wrapping_add(seed * 101390422);
                    Note {
                        start: (h % 128) as f32 * 0.25,
                        len: ((h / 128) % 48) as f32 * 0.25,
                        pitch: 60 + (h % 5) as u8,
                        vel: 0.5,
                        prob: 1.0,
                        offset: 0.0,
                        slide_to: None,
                        id: String::new(),
                    }
                })
                .collect();
            let expected: Vec<usize> = notes
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    notes
                        .iter()
                        .enumerate()
                        .any(|(j, b)| {
                            i != j
                                && a.pitch == b.pitch
                                && a.len > 0.0
                                && b.len > 0.0
                                && a.start < b.start + b.len
                                && b.start < a.start + a.len
                        })
                        .then_some(i)
                })
                .collect();
            assert_eq!(overlapping_pitch_selection(&notes), expected, "seed {seed}");
        }
    }
    #[test]
    fn pitch_expansion_matches_pairwise_reference_across_shuffled_selections() {
        for seed in 0..32usize {
            let notes: Vec<Note> = (0..257usize)
                .map(|i| Note {
                    start: i as f32,
                    len: 1.0,
                    pitch: ((i * 53 + seed * 19) % 128) as u8,
                    vel: 0.5,
                    prob: 1.0,
                    offset: 0.0,
                    slide_to: None,
                    id: format!("{i}"),
                })
                .collect();
            let mut selected: Vec<usize> =
                (seed..notes.len()).step_by(7 + seed % 11).rev().collect();
            selected.extend([999, seed, seed]);
            let expected: Vec<usize> = notes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    selected
                        .iter()
                        .filter_map(|j| notes.get(*j))
                        .any(|s| s.pitch == n.pitch)
                        .then_some(i)
                })
                .collect();
            assert_eq!(
                matching_pitch_selection(&notes, &selected),
                expected,
                "seed {seed}"
            );
        }
    }
    #[test]
    fn outside_scale_matches_absolute_pitch_set_for_every_root_mode_and_midi_pitch() {
        let notes: Vec<Note> = (0..128u8)
            .map(|pitch| Note {
                start: 0.0,
                len: 1.0,
                pitch,
                vel: 0.5,
                prob: 1.0,
                offset: 0.0,
                slide_to: None,
                id: String::new(),
            })
            .collect();
        for (root, name) in theory::NOTE_NAMES.iter().enumerate() {
            for (mode, intervals) in theory::SCALES {
                let allowed: std::collections::HashSet<usize> = intervals
                    .iter()
                    .map(|p| (root + *p as usize) % 12)
                    .collect();
                let expected: Vec<usize> =
                    (0..128).filter(|p| !allowed.contains(&(p % 12))).collect();
                assert_eq!(
                    outside_scale_selection(&notes, name, mode),
                    Some(expected),
                    "{name} {mode}"
                );
            }
        }
    }
    #[test]
    fn complement_matches_boolean_reference_for_duplicate_and_stale_indices() {
        for count in 0..128usize {
            for seed in 0..12usize {
                let selected: Vec<usize> = (0..count + 5)
                    .filter(|i| (i * 7 + seed) % 5 < 2)
                    .flat_map(|i| [i, i])
                    .rev()
                    .collect();
                let mut flags = vec![false; count];
                for i in &selected {
                    if let Some(flag) = flags.get_mut(*i) {
                        *flag = true;
                    }
                }
                let expected: Vec<usize> = flags
                    .iter()
                    .enumerate()
                    .filter_map(|(i, flag)| (!flag).then_some(i))
                    .collect();
                assert_eq!(inverted_note_selection(count, &selected), expected);
            }
        }
    }
}

#[cfg(test)]
mod velocity_target_tests {
    use super::*;
    #[test]
    fn shared_stems_prefer_selected_voice_but_nearest_onset_wins() {
        let notes = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 1., 64, 0.8),
            Note::new(1., 1., 67, 0.5),
        ];
        assert_eq!(nearest_velocity_note(&notes, &[], 0., 10.), Some(0));
        assert_eq!(nearest_velocity_note(&notes, &[1], 0., 10.), Some(1));
        assert_eq!(nearest_velocity_note(&notes, &[1, 0], 0., 10.), Some(0));
        assert_eq!(nearest_velocity_note(&notes, &[1], 1., 10.), Some(2));
        assert_eq!(nearest_velocity_note(&notes, &[999], 0., 10.), Some(0));
        assert_eq!(nearest_velocity_note(&notes, &[1], 4., 10.), None);
        assert_eq!(nearest_velocity_note(&[], &[], 0., 10.), None);
        assert_eq!(nearest_velocity_note(&notes, &[], f32::NAN, 10.), None);
    }
}

#[cfg(test)]
mod stable_selection_tests {
    use super::*;
    #[test]
    fn drag_target_resolves_reorder_and_rejects_missing_or_duplicate_ids() {
        let mut notes = vec![Note::new(0., 1., 60, 0.5), Note::new(4., 1., 64, 0.6)];
        notes[0].id = "a".into();
        notes[1].id = "target".into();
        assert_eq!(unique_note_index(&notes, "target"), Some(1));
        notes.reverse();
        assert_eq!(unique_note_index(&notes, "target"), Some(0));
        notes[1].id = "target".into();
        assert_eq!(unique_note_index(&notes, "target"), None);
        notes.remove(1);
        notes.remove(0);
        assert_eq!(unique_note_index(&notes, "target"), None);
    }
    #[test]
    fn velocity_identity_rejects_empty_duplicate_and_missing_stems() {
        let mut notes = vec![Note::new(4., 1., 64, 0.6), Note::new(8., 1., 67, 0.8)];
        notes[0].id = "held".into();
        notes[1].id = "other".into();
        assert_eq!(unique_note_index(&notes, "held"), Some(0));
        notes.reverse();
        assert_eq!(unique_note_index(&notes, "held"), Some(1));
        notes[0].id = "held".into();
        assert_eq!(unique_note_index(&notes, "held"), None);
        notes.retain(|n| n.id != "held");
        assert_eq!(unique_note_index(&notes, "held"), None);
        notes.push(Note::new(4., 1., 64, 0.6));
        notes[0].id.clear();
        assert_eq!(unique_note_index(&notes, ""), None);
    }
    #[test]
    fn identical_geometry_keeps_selected_identity() {
        let mut notes = vec![Note::new(6., 2., 64, 0.8), Note::new(6., 2., 64, 0.6)];
        notes[0].id = "stationary".into();
        notes[1].id = "moved".into();
        let ids = notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        assert_eq!(remap_note_selection(&notes, &ids, &[1]), vec![1]);
        notes.reverse();
        assert_eq!(remap_note_selection(&notes, &ids, &[1]), vec![0]);
    }
    #[test]
    fn selection_follows_ids_across_delete_reorder_and_disappears_with_target() {
        let mut notes = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 1., 64, 0.8),
            Note::new(8., 1., 67, 0.5),
        ];
        for (i, n) in notes.iter_mut().enumerate() {
            n.id = format!("n{i}");
        }
        let ids: Vec<String> = notes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(remap_note_selection(&notes, &ids, &[1, 1, 999]), vec![1]);
        notes.remove(0);
        assert_eq!(remap_note_selection(&notes, &ids, &[1]), vec![0]);
        notes.reverse();
        assert_eq!(remap_note_selection(&notes, &ids, &[1]), vec![1]);
        notes.retain(|n| n.id != "n1");
        assert!(remap_note_selection(&notes, &ids, &[1]).is_empty());
        assert!(remap_note_selection(&notes, &ids, &[]).is_empty());
    }
}

#[cfg(test)]
mod silent_selection_tests {
    use super::*;
    #[test]
    fn selects_exact_silence_without_changing_notes() {
        let mut notes = vec![
            Note::new(0., 1., 60, 0.),
            Note::new(4., 1., 64, 0.5),
            Note::new(8., 1., 67, 0.00001),
            Note::new(12., 1., 72, 0.5),
        ];
        notes[1].prob = 0.;
        notes[2].prob = 0.00001;
        notes[3].prob = 0.5;
        let before = notes.clone();
        assert_eq!(silent_note_selection(&notes), vec![0, 1]);
        assert_eq!(notes, before);
        assert!(silent_note_selection(&[]).is_empty());
    }
}

#[cfg(test)]
mod boundary_selection_tests {
    use super::*;
    #[test]
    fn boundary_selection_distinguishes_exact_ends_crossings_and_starts() {
        let notes = vec![
            Note::new(63., 1., 60, 0.5),
            Note::new(63., 2., 64, 0.5),
            Note::new(64., 1., 67, 0.5),
            Note::new(80., 0.02, 72, 0.5),
            Note::new(0., 0.02, 60, 0.5),
        ];
        let before = notes.clone();
        assert_eq!(beyond_pattern_selection(&notes, 64.), vec![1, 2, 3]);
        assert_eq!(notes, before);
        assert!(beyond_pattern_selection(&[], 64.).is_empty());
        let mut invalid = notes.clone();
        invalid[0].start = f32::NAN;
        invalid[1].len = -1.;
        assert_eq!(beyond_pattern_selection(&invalid, 64.), vec![2, 3]);
    }
}

#[cfg(test)]
mod duplicate_selection_tests {
    use super::*;
    #[test]
    fn selects_only_extra_exact_copies_not_expression_variants() {
        let n = Note::new(0., 1., 60, 0.5);
        let mut notes = vec![
            n.clone(),
            n.clone(),
            n.clone(),
            n.clone(),
            n.clone(),
            n.clone(),
            n.clone(),
        ];
        for (i, n) in notes.iter_mut().enumerate() {
            n.id = format!("n{i}");
        }
        notes[2].vel = 0.6;
        notes[3].prob = 0.5;
        notes[4].offset = 0.1;
        notes[5].slide_to = Some(64);
        notes[6].start = -0.;
        let before = notes.clone();
        assert_eq!(duplicate_copy_selection(&notes), vec![1, 6]);
        assert_eq!(notes, before);
        notes[0].len = f32::NAN;
        assert_eq!(duplicate_copy_selection(&notes), vec![6]);
        assert!(duplicate_copy_selection(&[]).is_empty());
    }
}

#[cfg(test)]
mod review_selection_audit_tests {
    use super::*;
    #[test]
    fn duplicate_review_matches_engine_cleanup_across_expression_and_order() {
        for seed in 0..128usize {
            let mut notes: Vec<Note> = Vec::new();
            for i in 0..48usize {
                let mut n = if i > 0 && (i + seed) % 3 == 0 {
                    notes[(i * 7 + seed) % notes.len()].clone()
                } else {
                    Note::new(
                        ((i * 11 + seed) % 83) as f32 * 0.5,
                        0.02 + (i % 4) as f32,
                        48 + (i % 24) as u8,
                        (i % 5) as f32 * 0.25,
                    )
                };
                n.id = format!("n{i}");
                if (i + seed) % 7 == 0 {
                    n.prob = 0.5;
                }
                if (i + seed) % 11 == 0 {
                    n.offset = 0.1;
                }
                if (i + seed) % 13 == 0 {
                    n.slide_to = Some(72);
                }
                notes.push(n);
            }
            if seed % 2 == 0 {
                notes.reverse();
            }
            let copies = duplicate_copy_selection(&notes);
            let mut cleaned = notes.clone();
            let count = crate::midi_ops::remove_exact_duplicates(
                &mut cleaned,
                &crate::midi_ops::Selection::default(),
            );
            let expected: Vec<_> = notes
                .iter()
                .enumerate()
                .filter(|(i, _)| !copies.contains(i))
                .map(|(_, n)| n.clone())
                .collect();
            assert_eq!(count, copies.len(), "seed {seed}");
            assert_eq!(
                cleaned.iter().map(|n| &n.id).collect::<Vec<_>>(),
                expected.iter().map(|n| &n.id).collect::<Vec<_>>(),
                "seed {seed}"
            );
            assert_eq!(cleaned, expected);
        }
    }
    #[test]
    fn silence_and_boundary_review_match_independent_reference() {
        for steps in [14., 16., 28., 64.] {
            let mut notes: Vec<Note> = Vec::new();
            for i in 0..192usize {
                let mut n = Note::new(
                    (i % 97) as f32 * 0.5,
                    (i % 7 + 1) as f32 * 0.25,
                    60,
                    (i % 4) as f32 * 0.25,
                );
                n.prob = (i % 5) as f32 * 0.25;
                notes.push(n);
            }
            let expected_silent: Vec<_> = notes
                .iter()
                .enumerate()
                .filter(|(_, n)| !(n.vel > 0. && n.prob > 0.))
                .map(|(i, _)| i)
                .collect();
            let expected_boundary: Vec<_> = notes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.start >= steps || n.len > steps - n.start)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(silent_note_selection(&notes), expected_silent);
            assert_eq!(beyond_pattern_selection(&notes, steps), expected_boundary);
        }
    }
}

#[cfg(test)]
mod length_selection_tests {
    use super::*;
    #[test]
    fn matching_lengths_ignore_pitch_start_expression_and_invalid_lengths() {
        let mut notes = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(4., 2., 64, 0.6),
            Note::new(8., 1., 67, 0.8),
            Note::new(12., 4., 72, 0.2),
            Note::new(80., 2., 48, 0.),
        ];
        notes[4].prob = 0.;
        let before = notes.clone();
        assert_eq!(matching_length_selection(&notes, &[0]), vec![0, 2]);
        assert_eq!(
            matching_length_selection(&notes, &[0, 1, 1, 999]),
            vec![0, 1, 2, 4]
        );
        assert_eq!(notes, before);
        assert!(matching_length_selection(&notes, &[]).is_empty());
        assert!(matching_length_selection(&[], &[0]).is_empty());
        notes[0].len = f32::NAN;
        notes[1].len = -1.;
        notes[2].len = 0.;
        notes[3].len = f32::INFINITY;
        assert!(matching_length_selection(&notes, &[0, 1, 2, 3]).is_empty());
    }
    #[test]
    fn matching_lengths_use_small_tolerance_not_snap_grid() {
        let notes = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(4., 1.00005, 64, 0.6),
            Note::new(8., 1.0002, 67, 0.8),
            Note::new(12., 0.99, 72, 0.2),
        ];
        assert_eq!(matching_length_selection(&notes, &[0]), vec![0, 1]);
    }
}

#[cfg(test)]
mod selection_shortcut_tests {
    use super::*;
    fn key(key: egui::Key, command: bool, shift: bool, alt: bool, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                command,
                shift,
                alt,
                ..Default::default()
            },
        }
    }
    #[test]
    fn quick_chord_uses_press_modifiers_not_release_modifiers() {
        let events = [
            key(egui::Key::A, true, true, false, true),
            key(egui::Key::A, false, false, false, false),
        ];
        assert_eq!(selection_shortcut(&events), Some(true));
        let events = [
            key(egui::Key::A, true, false, false, true),
            key(egui::Key::A, false, false, false, false),
        ];
        assert_eq!(selection_shortcut(&events), Some(false));
    }
    #[test]
    fn unrelated_keys_and_alt_chords_do_not_change_selection() {
        assert_eq!(selection_shortcut(&[]), None);
        for event in [
            key(egui::Key::A, false, false, false, true),
            key(egui::Key::A, true, false, true, true),
            key(egui::Key::B, true, true, false, true),
            key(egui::Key::A, true, true, false, false),
        ] {
            assert_eq!(selection_shortcut(&[event]), None);
        }
    }
    #[test]
    fn last_selection_press_in_frame_wins() {
        assert_eq!(
            selection_shortcut(&[
                key(egui::Key::A, true, false, false, true),
                key(egui::Key::A, true, true, false, true)
            ]),
            Some(true)
        );
        assert_eq!(
            selection_shortcut(&[
                key(egui::Key::A, true, true, false, true),
                key(egui::Key::A, true, false, false, true)
            ]),
            Some(false)
        );
    }
}

#[cfg(test)]
mod arrow_shortcut_tests {
    use super::*;
    fn key(key: egui::Key, shift: bool, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                shift,
                ..Default::default()
            },
        }
    }
    #[test]
    fn quick_shift_release_keeps_octave_and_pattern_bar() {
        assert_eq!(
            note_arrow_shortcut(
                &[
                    key(egui::Key::ArrowUp, true, true),
                    key(egui::Key::ArrowUp, false, false)
                ],
                1.,
                14.
            ),
            (12, 0.)
        );
        assert_eq!(
            note_arrow_shortcut(
                &[
                    key(egui::Key::ArrowRight, true, true),
                    key(egui::Key::ArrowRight, false, false)
                ],
                1.,
                14.
            ),
            (0, 14.)
        );
        assert_eq!(
            note_arrow_shortcut(
                &[
                    key(egui::Key::ArrowDown, false, true),
                    key(egui::Key::ArrowLeft, false, true)
                ],
                0.5,
                16.
            ),
            (-1, -0.5)
        );
    }
    #[test]
    fn other_modifier_arrows_do_not_edit_notes() {
        for modifiers in [
            egui::Modifiers {
                command: true,
                ..Default::default()
            },
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
            egui::Modifiers {
                mac_cmd: true,
                ..Default::default()
            },
        ] {
            let event = egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            assert_eq!(note_arrow_shortcut(&[event], 1., 16.), (0, 0.));
        }
    }
    #[test]
    fn repeated_and_mixed_arrow_presses_accumulate_once_each() {
        assert_eq!(
            note_arrow_shortcut(
                &[
                    key(egui::Key::ArrowRight, true, true),
                    key(egui::Key::ArrowRight, false, true),
                    key(egui::Key::ArrowUp, false, true)
                ],
                1.,
                14.
            ),
            (1, 15.)
        );
        assert_eq!(
            note_arrow_shortcut(
                &[
                    key(egui::Key::ArrowLeft, false, false),
                    key(egui::Key::B, true, true)
                ],
                1.,
                16.
            ),
            (0, 0.)
        );
    }
}

#[cfg(test)]
mod length_lookup_audit_tests {
    use super::*;
    #[test]
    fn duration_lookup_matches_linear_reference_with_tolerance_boundaries() {
        for seed in 0..64usize {
            let mut notes: Vec<Note> = (0..384usize)
                .map(|i| {
                    let base = ((i * 13 + seed) % 53 + 1) as f32 * 0.125;
                    Note::new(
                        i as f32,
                        base + [0., 0.00005, -0.00005, 0.0002][i % 4],
                        60,
                        0.5,
                    )
                })
                .collect();
            notes[0].len = f32::NAN;
            notes[1].len = f32::INFINITY;
            notes[2].len = 0.;
            notes[3].len = -1.;
            let mut selected: Vec<usize> = (seed % 7..notes.len())
                .step_by(seed % 13 + 1)
                .rev()
                .collect();
            selected.extend([9999, 0, 1, 2, 3]);
            let expected: Vec<_> = notes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    (n.len.is_finite()
                        && n.len > 0.
                        && selected.iter().filter_map(|j| notes.get(*j)).any(|m| {
                            m.len.is_finite() && m.len > 0. && (n.len - m.len).abs() <= 0.0001
                        }))
                    .then_some(i)
                })
                .collect();
            assert_eq!(
                matching_length_selection(&notes, &selected),
                expected,
                "seed{seed}"
            );
        }
    }
    #[test]
    fn dense_duration_selection_handles_large_reversed_selection() {
        let notes: Vec<_> = (0..16384usize)
            .map(|i| Note::new(i as f32, (i % 128 + 1) as f32 * 0.25, 60, 0.5))
            .collect();
        let selected: Vec<_> = (0..notes.len()).rev().collect();
        assert_eq!(
            matching_length_selection(&notes, &selected),
            (0..notes.len()).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod selection_summary_tests {
    use super::*;
    #[test]
    fn summary_uses_selected_notes_only_and_keeps_project_unchanged() {
        let notes = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(4., 2., 64, 0.6),
            Note::new(8., 1., 67, 1.),
        ];
        let before = notes.clone();
        assert_eq!(selection_summary(&notes, &[2,0,2,999]), Some("Selected pitch: C4 to G4\nNominal span: 0.000 to 9.000 steps\nSelected lengths: 0.020 to 1.000 steps\nSelected velocity: 0.0% to 100.0%\nTrigger chance: 100.0% to 100.0%\nTiming offsets: +0.000 to +0.000 steps".into()));
        assert_eq!(notes, before);
        assert_eq!(selection_summary(&notes, &[]), None);
        assert_eq!(selection_summary(&notes, &[999]), None);
    }
}

#[cfg(test)]
mod selection_summary_dense_tests {
    use super::*;
    #[test]
    fn dense_summary_accepts_duplicates_and_stale_indices() {
        let notes: Vec<_> = (0..16384)
            .map(|i| {
                Note::new(
                    i as f32,
                    0.25 + (i % 8) as f32,
                    48 + (i % 24) as u8,
                    (i % 101) as f32 / 100.,
                )
            })
            .collect();
        let selected: Vec<_> = (0..notes.len() + 1).chain(0..notes.len()).collect();
        assert_eq!(selection_summary(&notes,&selected),Some("Selected pitch: C3 to B4\nNominal span: 0.000 to 16390.250 steps\nSelected lengths: 0.250 to 7.250 steps\nSelected velocity: 0.0% to 100.0%\nTrigger chance: 100.0% to 100.0%\nTiming offsets: +0.000 to +0.000 steps".into()));
    }
    #[test]
    fn nonfinite_lengths_and_velocity_do_not_pollute_ranges() {
        let mut notes = vec![Note::new(0., 1., 60, 0.5), Note::new(1., 2., 67, 0.75)];
        notes[0].len = f32::NAN;
        notes[0].vel = f32::INFINITY;
        assert_eq!(selection_summary(&notes,&[0,1]),Some("Selected pitch: C4 to G4\nNominal span: 1.000 to 3.000 steps\nSelected lengths: 2.000 to 2.000 steps\nSelected velocity: 75.0% to 75.0%\nTrigger chance: 100.0% to 100.0%\nTiming offsets: +0.000 to +0.000 steps".into()));
        assert_eq!(
            selection_summary(&notes, &[0]),
            Some("Selected pitch: C4 to C4\nTrigger chance: 100.0% to 100.0%\nTiming offsets: +0.000 to +0.000 steps".into())
        );
    }
}

#[cfg(test)]
mod selection_span_tests {
    use super::*;
    #[test]
    fn span_uses_selected_nominal_bounds_including_off_pattern_tails() {
        let mut notes = vec![
            Note::new(8., 2., 67, 0.8),
            Note::new(80., 0.02, 72, 0.9),
            Note::new(0., 1., 60, 0.5),
        ];
        notes[0].offset = 0.4;
        assert!(selection_summary(&notes, &[0, 1])
            .unwrap()
            .contains("Nominal span: 8.000 to 80.020 steps"));
        assert!(selection_summary(&notes, &[0])
            .unwrap()
            .contains("Nominal span: 8.000 to 10.000 steps"));
    }
    #[test]
    fn span_skips_overflow_negative_lengths_and_invalid_starts() {
        let mut notes = vec![
            Note::new(f32::MAX, f32::MAX, 60, 0.5),
            Note::new(1., 1., 64, 0.5),
            Note::new(2., 1., 67, 0.5),
            Note::new(4., 2., 72, 0.5),
        ];
        notes[1].len = -1.;
        notes[2].start = f32::NAN;
        assert!(selection_summary(&notes, &[0, 1, 2, 3])
            .unwrap()
            .contains("Nominal span: 4.000 to 6.000 steps"));
        assert!(!selection_summary(&notes, &[0, 1, 2])
            .unwrap()
            .contains("Nominal span:"));
    }
}

#[cfg(test)]
mod selection_id_lookup_tests {
    use super::*;
    #[test]
    fn indexed_id_lookup_matches_nested_reference() {
        for seed in 0..64 {
            let mut notes: Vec<_> = (0..384).map(|i| Note::new(i as f32, 1., 60, 0.8)).collect();
            for (i, n) in notes.iter_mut().enumerate() {
                n.id = format!("n{}", (i * 17 + seed) % 241);
            }
            let mut ids: Vec<_> = notes
                .iter()
                .skip(seed % 7)
                .step_by(seed % 11 + 1)
                .map(|n| n.id.clone())
                .collect();
            ids.extend(["missing".into(), ids[0].clone()]);
            let reference: Vec<_> = notes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| ids.contains(&n.id).then_some(i))
                .collect();
            assert_eq!(selection_from_ids(&notes, &ids), reference);
        }
        assert!(selection_from_ids(&[], &[]).is_empty());
    }
    #[test]
    fn dense_reselection_keeps_source_order() {
        let mut notes: Vec<_> = (0..16384)
            .map(|i| Note::new(i as f32, 1., 60, 0.8))
            .collect();
        for (i, n) in notes.iter_mut().enumerate() {
            n.id = format!("n{i}");
        }
        let ids: Vec<_> = notes.iter().rev().map(|n| n.id.clone()).collect();
        assert_eq!(
            selection_from_ids(&notes, &ids),
            (0..notes.len()).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod selected_drawing_lookup_tests {
    use super::*;
    #[test]
    fn drawing_membership_matches_reference_and_filters_stale_indices() {
        for seed in 0..64 {
            let selected: Vec<_> = (0..512)
                .filter(|i| (i + seed) % 7 < 3)
                .chain([0, 0, 999])
                .collect();
            let set = selected_index_mask(384, &selected);
            for i in 0..384 {
                assert_eq!(set[i], selected.contains(&i));
            }
            assert!(set.get(999).is_none());
        }
        assert!(selected_index_mask(0, &[0, 1]).is_empty());
    }
    #[test]
    fn dense_drawing_membership_deduplicates_and_clears() {
        let selected: Vec<_> = (0..16384).chain(0..16384).collect();
        let set = selected_index_mask(16384, &selected);
        assert_eq!(set.iter().filter(|v| **v).count(), 16384);
        assert!(selected_index_mask(16384, &[]).iter().all(|v| !v));
    }
}

#[cfg(test)]
mod note_identity_cache_tests {
    use super::*;
    #[test]
    fn identity_map_ignores_geometry_but_detects_reorder_replace_delete_and_empty() {
        let mut notes: Vec<_> = (0..3)
            .map(|i| {
                let mut n = Note::new(i as f32, 1., 60, 0.5);
                n.id = format!("n{i}");
                n
            })
            .collect();
        let ids: Vec<_> = notes.iter().map(|n| n.id.clone()).collect();
        assert!(note_identity_matches(&notes, &ids));
        notes[0].pitch = 72;
        notes[1].vel = 0.2;
        assert!(note_identity_matches(&notes, &ids));
        notes.swap(0, 2);
        assert!(!note_identity_matches(&notes, &ids));
        assert_eq!(remap_note_selection(&notes, &ids, &[0]), vec![2]);
        notes.swap(0, 2);
        notes[0].id = "replacement".into();
        assert!(!note_identity_matches(&notes, &ids));
        assert!(remap_note_selection(&notes, &ids, &[0]).is_empty());
        notes.pop();
        assert!(!note_identity_matches(&notes, &ids));
        assert!(note_identity_matches(&[], &[]));
        assert!(!note_identity_matches(&notes, &[]));
    }
}

#[cfg(test)]
mod identity_cache_transition_audit_tests {
    use super::*;
    #[test]
    fn cached_identity_path_matches_unconditional_remapping_across_edit_sequences() {
        for seed in 0..64usize {
            let mut notes: Vec<_> = (0..128)
                .map(|i| {
                    let mut n = Note::new(i as f32, 1., 60, 0.5);
                    n.id = format!("n{i}");
                    n
                })
                .collect();
            let mut ids: Vec<_> = notes.iter().map(|n| n.id.clone()).collect();
            let mut selected: Vec<_> = (seed % 5..notes.len()).step_by(seed % 7 + 1).collect();
            for step in 0..64 {
                match (step + seed) % 6 {
                    0 => {
                        for n in &mut notes {
                            n.pitch = 48 + (step % 24) as u8;
                            n.vel = 0.25;
                        }
                    }
                    1 => notes.reverse(),
                    2 => {
                        if !notes.is_empty() {
                            let at = (seed + step) % notes.len();
                            notes.remove(at);
                        }
                    }
                    3 => {
                        let mut n = Note::new(step as f32, 0.25, 67, 0.8);
                        n.id = format!("added-{seed}-{step}");
                        notes.insert(0, n);
                    }
                    4 => {
                        if let Some(n) = notes.first_mut() {
                            n.id = format!("replacement-{seed}-{step}");
                        }
                    }
                    _ => {
                        let len = notes.len();
                        if len > 1 {
                            notes.rotate_left((seed + step) % len);
                        }
                    }
                }
                let expected = remap_note_selection(&notes, &ids, &selected);
                let mut cached = selected.clone();
                if !note_identity_matches(&notes, &ids) {
                    cached = remap_note_selection(&notes, &ids, &cached);
                }
                cached.retain(|i| *i < notes.len());
                assert_eq!(cached, expected, "seed {seed}, step {step}");
                if !note_identity_matches(&notes, &ids) {
                    ids = notes.iter().map(|n| n.id.clone()).collect();
                }
                selected = cached;
            }
        }
    }
}

#[cfg(test)]
mod repeat_capacity_lookup_audit_tests {
    use super::*;
    fn reference(notes: &[Note], selected: &[usize], period: f32, steps: f32) -> (usize, u32) {
        if !period.is_finite() || period <= 0. {
            return (0, 0);
        }
        let source: Vec<_> = notes
            .iter()
            .enumerate()
            .filter(|(i, _)| selected.is_empty() || selected.contains(i))
            .map(|(_, n)| n)
            .collect();
        let count = source.len();
        let fit = source
            .iter()
            .map(|n| {
                ((steps - n.start - n.len + 1e-4) / period)
                    .floor()
                    .clamp(0., 64.) as u32
            })
            .min()
            .unwrap_or(0);
        (count, fit.min((100_000 / count.max(1)).min(64) as u32))
    }
    #[test]
    fn indexed_repeat_capacity_matches_reference_for_selected_and_all_notes() {
        for seed in 0..64usize {
            let notes: Vec<_> = (0..384)
                .map(|i| {
                    Note::new(
                        ((i * 17 + seed) % 97) as f32,
                        (i % 11 + 1) as f32 * 0.25,
                        60,
                        0.5,
                    )
                })
                .collect();
            let selected: Vec<_> = (seed % 7..notes.len())
                .step_by(seed % 11 + 1)
                .chain([999, 0, 0])
                .collect();
            for sel in [&selected[..], &[][..]] {
                for period in [0., f32::NAN, 0.25, 14., 16., 64.] {
                    for steps in [0., 56., 64., 256.] {
                        assert_eq!(
                            repeat_capacity(&notes, sel, period, steps),
                            reference(&notes, sel, period, steps)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn dense_repeat_capacity_preserves_generated_note_limit() {
        let notes: Vec<_> = (0..16384).map(|_| Note::new(0., 1., 60, 0.5)).collect();
        let selected: Vec<_> = (0..notes.len()).rev().collect();
        assert_eq!(repeat_capacity(&notes, &selected, 1., 64.), (16384, 6));
        assert_eq!(repeat_capacity(&notes, &[99999], 1., 64.), (0, 0));
    }
}

#[cfg(test)]
mod repeat_capacity_numeric_tests {
    use super::*;
    #[test]
    fn unsupported_bounds_offer_no_copies_but_do_not_hide_source_count() {
        for (start, len) in [
            (0., 0.02),
            (-1., 1.),
            (0., f32::NAN),
            (f32::INFINITY, 1.),
            (f32::MAX, f32::MAX),
        ] {
            let notes = [Note::new(start, len, 60, 0.5)];
            assert_eq!(repeat_capacity(&notes, &[], 16., 64.), (1, 0));
        }
        for steps in [f32::NAN, f32::INFINITY, -1.] {
            assert_eq!(
                repeat_capacity(&[Note::new(0., 1., 60, 0.5)], &[], 16., steps),
                (0, 0)
            );
        }
    }
    #[test]
    fn unselected_short_note_does_not_block_valid_selected_source() {
        let notes = [Note::new(0., 0.02, 60, 0.5), Note::new(0., 1., 64, 0.5)];
        assert_eq!(repeat_capacity(&notes, &[], 16., 64.), (2, 0));
        assert_eq!(repeat_capacity(&notes, &[1], 16., 64.), (1, 3));
    }
}
#[cfg(test)]
mod repeat_engine_capacity_audit_tests {
    use super::*;
    #[test]
    fn offered_repeat_capacity_is_accepted_at_fit_boundaries() {
        for steps in [14.0f32, 16., 56., 64.] {
            for period in [0.25, 1., 3.5, 14., 16.] {
                for len in [0.05, 0.25, 1., 2.] {
                    for end_delta in [-0.0002, 0., 0.0002] {
                        let start = (steps - period - len + end_delta).max(0.);
                        let notes = vec![Note::new(start, len, 60, 0.5)];
                        let (_, capacity) = repeat_capacity(&notes, &[], period, steps);
                        for copies in 1..=capacity {
                            let mut edited = notes.clone();
                            assert!(crate::midi_ops::repeat_phrase(&mut edited,&crate::midi_ops::Selection::default(),period,copies,steps).is_ok(),"steps={steps} period={period} len={len} start={start} copies={copies}");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod selection_expression_summary_tests {
    use super::*;
    #[test]
    fn expression_ranges_are_selected_only_and_signed() {
        let mut notes = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(4., 1., 64, 0.5),
            Note::new(8., 1., 67, 0.5),
        ];
        notes[0].prob = 0.25;
        notes[0].offset = -0.125;
        notes[1].prob = 0.75;
        notes[1].offset = 0.25;
        notes[2].prob = 0.;
        notes[2].offset = -0.5;
        let text = selection_summary(&notes, &[0, 1, 1, 999]).unwrap();
        assert!(text.contains("Trigger chance: 25.0% to 75.0%"));
        assert!(text.contains("Timing offsets: -0.125 to +0.250 steps"));
        notes[0].prob = f32::NAN;
        notes[0].offset = f32::INFINITY;
        let text = selection_summary(&notes, &[0]).unwrap();
        assert!(!text.contains("Trigger chance:"));
        assert!(!text.contains("Timing offsets:"));
    }
}

#[cfg(test)]
mod expression_selection_tests {
    use super::*;
    #[test]
    fn exact_expression_filters_preserve_order_and_include_zero_chance() {
        let values = [
            (1.0, 0.0),
            (0.75, 0.25),
            (0.0, -0.125),
            (0.5, -0.0),
            (1.0, f32::EPSILON),
            (f32::NAN, f32::INFINITY),
            (f32::INFINITY, f32::NAN),
            (-0.1, 0.0),
            (1.1, 0.0),
        ];
        let notes: Vec<Note> = values
            .into_iter()
            .enumerate()
            .map(|(i, (prob, offset))| Note {
                id: format!("n{i}"),
                prob,
                offset,
                ..Default::default()
            })
            .collect();
        assert_eq!(expression_note_selection(&notes, true), vec![1, 2, 4]);
        assert_eq!(expression_note_selection(&notes, false), vec![1, 2, 3]);
        assert!(expression_note_selection(&[], true).is_empty());
        assert!(expression_note_selection(&[], false).is_empty());
    }
}

#[cfg(test)]
mod velocity_target_linear_audit_tests {
    use super::*;
    fn reference(notes: &[Note], selected: &[usize], at: f32, cell: f32) -> Option<usize> {
        notes
            .iter()
            .enumerate()
            .map(|(i, n)| (i, ((n.start - at) * cell).abs()))
            .filter(|(_, distance)| distance.is_finite() && *distance < 8.0)
            .min_by(|(a, ad), (b, bd)| {
                ad.total_cmp(bd)
                    .then_with(|| selected.contains(b).cmp(&selected.contains(a)))
                    .then_with(|| a.cmp(b))
            })
            .map(|(i, _)| i)
    }

    #[test]
    fn generated_targeting_matches_original_tie_rules() {
        for seed in 0..64usize {
            let notes: Vec<_> = (0..192)
                .map(|i| {
                    let mut n = Note::new(((i * 7 + seed) % 13) as f32 * 0.25, 1., 60, 0.5);
                    if i % 31 == 0 {
                        n.start = f32::NAN;
                    }
                    n
                })
                .collect();
            let mut selected: Vec<_> = (seed % 7..notes.len())
                .step_by(seed % 5 + 1)
                .rev()
                .collect();
            selected.extend([999, seed, seed]);
            for at in [-1., 0., 0.125, 0.25, 1., 2.75, 4., f32::NAN, f32::INFINITY] {
                for cell in [0., -10., 1., 10., 100., f32::INFINITY] {
                    assert_eq!(
                        nearest_velocity_note(&notes, &selected, at, cell),
                        reference(&notes, &selected, at, cell)
                    );
                }
            }
        }
    }
    #[test]
    fn dense_shared_onset_keeps_lowest_selected_index() {
        let notes: Vec<_> = (0..16384).map(|_| Note::new(0., 1., 60, 0.5)).collect();
        let selected: Vec<_> = (8192..notes.len()).rev().collect();
        assert_eq!(
            nearest_velocity_note(&notes, &selected, 0., 10.),
            Some(8192)
        );
        assert_eq!(nearest_velocity_note(&notes, &[], 0., 10.), Some(0));
    }
}

#[cfg(test)]
mod note_expression_hint_tests {
    use super::*;
    #[test]
    fn exact_saved_expression_and_slide_are_visible() {
        let note = Note {
            prob: 0.375,
            offset: -0.125,
            slide_to: Some(67),
            ..Default::default()
        };
        assert_eq!(
            note_expression_hint(&note),
            "Trigger chance: 37.5%\nTiming offset: -0.125 steps\nSlide to: G4 (MIDI 67)"
        );
        let note = Note {
            prob: 1.0,
            offset: 0.25,
            ..Default::default()
        };
        assert_eq!(
            note_expression_hint(&note),
            "Trigger chance: 100.0%\nTiming offset: +0.250 steps"
        );
    }
    #[test]
    fn nonfinite_expression_does_not_hide_valid_slide() {
        let note = Note {
            prob: f32::NAN,
            offset: f32::INFINITY,
            slide_to: Some(60),
            ..Default::default()
        };
        assert_eq!(note_expression_hint(&note), "Slide to: C4 (MIDI 60)");
    }
}

#[cfg(test)]
mod velocity_expression_history_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn id_targeted_chord_velocity_preserves_expression_and_exact_history_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "beatbox-velocity-expression-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut engine = crate::Engine::new(dir.clone());
        engine
            .call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let notes: Vec<Note> = (0..3)
            .map(|i| Note {
                id: format!("voice{i}"),
                start: 4.,
                len: 2.,
                pitch: 60 + i * 4,
                vel: 0.4 + i as f32 * 0.2,
                prob: 0.25 + i as f32 * 0.25,
                offset: -0.125 + i as f32 * 0.125,
                slide_to: Some(72),
            })
            .collect();
        engine.project.patterns[0]
            .clips
            .insert("keys".into(), notes);
        let before = serde_json::to_value(&engine.project).unwrap();
        let history = engine.history().0.len();
        engine
            .call(
                "set_note_velocity",
                &json!({"track":"keys","ids":["voice1"],"velocity":0.25}),
            )
            .unwrap();
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"][1]["vel"] = json!(0.25);
        assert_eq!(serde_json::to_value(&engine.project).unwrap(), expected);
        assert_eq!(engine.history().0.len(), history + 1);
        engine.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&engine.project).unwrap(), before);
        engine.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&engine.project).unwrap(), expected);
        engine
            .call("save_project", &json!({"path":"velocity-roundtrip.json"}))
            .unwrap();
        engine
            .call("load_project", &json!({"path":"velocity-roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&engine.project).unwrap(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod velocity_lane_value_tests {
    use super::*;
    #[test]
    fn full_lane_reaches_true_silence_and_unity() {
        assert_eq!(velocity_lane_value(100., 24., 98.), Some(0.));
        assert_eq!(velocity_lane_value(100., 24., 78.), Some(1.));
        assert_eq!(velocity_lane_value(100., 24., 88.), Some(0.5));
        assert_eq!(velocity_lane_value(100., 24., 120.), Some(0.));
        assert_eq!(velocity_lane_value(100., 24., 0.), Some(1.));
    }
    #[test]
    fn invalid_geometry_cannot_write_a_velocity() {
        for (bottom, height, y) in [
            (100., 4., 88.),
            (100., 0., 88.),
            (100., -1., 88.),
            (f32::NAN, 24., 88.),
            (100., f32::INFINITY, 88.),
            (100., 24., f32::NAN),
            (f32::MAX, 24., -f32::MAX),
        ] {
            assert_eq!(velocity_lane_value(bottom, height, y), None);
        }
    }
}

#[cfg(test)]
mod velocity_boundary_history_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn zero_and_full_velocity_repeat_without_extra_history_and_undo_exactly() {
        let dir = std::env::temp_dir().join(format!(
            "beatbox-velocity-boundaries-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut engine = crate::Engine::new(dir.clone());
        engine
            .call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let note = Note {
            id: "boundary".into(),
            start: 4.,
            len: 2.,
            pitch: 64,
            vel: 0.6,
            prob: 0.5,
            offset: 0.1,
            slide_to: Some(67),
        };
        engine.project.patterns[0]
            .clips
            .insert("keys".into(), vec![note]);
        let before = serde_json::to_value(&engine.project).unwrap();
        for velocity in [0., 1.] {
            let h = engine.history().0.len();
            let args = json!({"track":"keys","ids":["boundary"],"velocity":velocity});
            engine.call("set_note_velocity", &args).unwrap();
            let after = serde_json::to_value(&engine.project).unwrap();
            let mut expected = before.clone();
            expected["patterns"][0]["clips"]["keys"][0]["vel"] = json!(velocity);
            assert_eq!(after, expected);
            assert_eq!(engine.history().0.len(), h + 1);
            engine.call("set_note_velocity", &args).unwrap();
            assert_eq!(engine.history().0.len(), h + 1);
            assert_eq!(serde_json::to_value(&engine.project).unwrap(), after);
            engine.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&engine.project).unwrap(), before);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod velocity_readout_label_tests {
    use super::*;
    #[test]
    fn readout_identifies_pitch_including_silence() {
        assert_eq!(velocity_readout_label(64, 0.6), "E4: 60%");
        assert_eq!(velocity_readout_label(60, 0.0), "C4: 0%");
        assert_eq!(velocity_readout_label(67, 0.8095238), "G4: 81%");
        assert_eq!(velocity_readout_label(127, 1.0), "G9: 100%");
    }
}

#[cfg(test)]
mod velocity_label_placement_tests {
    use super::*;
    #[test]
    fn edge_and_full_velocity_labels_stay_inside_lane() {
        let lane = Rect::from_min_max(egui::pos2(80., 720.), egui::pos2(860., 770.));
        let size = egui::vec2(58., 12.);
        for desired in [
            egui::pos2(850., 710.),
            egui::pos2(20., 780.),
            egui::pos2(132., 745.),
        ] {
            let at = velocity_label_origin(lane, desired, size);
            assert!(lane.contains_rect(Rect::from_min_size(at, size)));
        }
        assert_eq!(
            velocity_label_origin(lane, egui::pos2(850., 710.), size),
            egui::pos2(800., 722.)
        );
        let narrow = Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(20., 10.));
        assert_eq!(
            velocity_label_origin(narrow, egui::pos2(30., 20.), size),
            egui::pos2(2., 2.)
        );
    }
}

#[cfg(test)]
mod slide_note_selection_tests {
    use super::*;
    #[test]
    fn saved_slide_presence_selects_in_storage_order() {
        let notes = vec![
            Note::new(0., 1., 60, 0.5),
            Note {
                pitch: 60,
                slide_to: Some(60),
                ..Default::default()
            },
            Note {
                slide_to: Some(127),
                ..Default::default()
            },
            Note::new(4., 1., 64, 0.8),
        ];
        assert_eq!(slide_note_selection(&notes), vec![1, 2]);
        assert!(slide_note_selection(&[]).is_empty());
        assert!(slide_note_selection(&notes[..1]).is_empty());
    }
}

#[cfg(test)]
mod geometry_reindex_history_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn earlier_deletion_keeps_held_move_and_resize_on_original_id() {
        for resize in [false, true] {
            let dir = std::env::temp_dir()
                .join(format!("beatbox-reindex-{resize}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut engine = crate::Engine::new(dir.clone());
            engine
                .call("add_track", &json!({"name":"keys","preset":"epiano"}))
                .unwrap();
            let notes: Vec<Note> = (0..3)
                .map(|i| Note {
                    id: format!("voice{i}"),
                    start: i as f32 * 4.,
                    len: 2.,
                    pitch: 60 + i * 4,
                    vel: 0.6,
                    prob: 0.5,
                    offset: 0.1,
                    slide_to: Some(72),
                })
                .collect();
            engine.project.patterns[0]
                .clips
                .insert("keys".into(), notes.clone());
            let before = serde_json::to_value(&engine.project).unwrap();
            let prior_ids: Vec<_> = notes.iter().map(|n| n.id.clone()).collect();
            engine
                .call("delete_notes", &json!({"track":"keys","ids":["voice0"]}))
                .unwrap();
            let removed = serde_json::to_value(&engine.project).unwrap();
            let current = engine.project.patterns[0].notes("keys").to_vec();
            let selected = remap_note_selection(&current, &prior_ids, &[2]);
            assert_eq!(selected, vec![1]);
            let index = unique_note_index(&current, "voice2").unwrap();
            assert_eq!(index, 1);
            let edited = apply_shift(
                &current,
                &selected,
                &NoteShift {
                    index,
                    resize,
                    d_steps: 2.,
                    d_semi: if resize { 0 } else { 1 },
                },
                64.,
                1.,
            );
            let updates: Vec<_> = edited
                .iter()
                .zip(&current)
                .filter(|(a, b)| (a.start, a.len, a.pitch) != (b.start, b.len, b.pitch))
                .map(|(n, _)| json!({"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}))
                .collect();
            engine
                .call(
                    "update_note_geometry",
                    &json!({"track":"keys","updates":updates}),
                )
                .unwrap();
            let mut expected = removed.clone();
            expected["patterns"][0]["clips"]["keys"] = serde_json::to_value(&edited).unwrap();
            assert_eq!(serde_json::to_value(&engine.project).unwrap(), expected);
            assert_eq!(edited[0], current[0]);
            let mut changed = current[1].clone();
            if resize {
                changed.len += 2.;
            } else {
                changed.start += 2.;
                changed.pitch += 1;
            }
            assert_eq!(edited[1], changed);
            engine.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&engine.project).unwrap(), removed);
            engine.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&engine.project).unwrap(), before);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(test)]
mod feel_tests {
    use super::*;
    use crate::engine::Engine;

    fn engine(tag: &str) -> Engine {
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("feel_{tag}_{}", std::process::id())));
        e.call("add_track", &json!({"name":"keys","preset":"kick"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"phrase","bars":2}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","pattern":"phrase","notes":[
                {"start":0.3,"len":1.7,"pitch":60,"vel":0.7,"prob":0.6,"offset":0.12,"slide_to":67},
                {"start":1.2,"len":2.1,"pitch":64,"vel":0.9,"prob":0.8,"offset":-0.1},
                {"start":2.7,"len":1.1,"pitch":67,"vel":0.5,"prob":0.4,"offset":0.2}
            ]}),
        )
        .unwrap();
        e
    }
    fn draft(e: &Engine, selected: bool) -> FeelDraft {
        let ti = e.project.track_index("keys").unwrap();
        let pi = e.project.pattern_index("phrase").unwrap();
        let ids = if selected {
            vec![e.project.patterns[pi].notes("keys")[0].id.clone()]
        } else {
            vec![]
        };
        FeelDraft::new(
            &e.project,
            (e.revision, e.project_load_generation),
            (ti, pi, 0),
            ids,
            2,
        )
        .unwrap()
    }
    fn state(e: &Engine) -> Value {
        json!({"project":e.project,"revision":e.revision,"generation":e.project_load_generation,
            "history":e.undo_depth(),"log":e.log.len(),
            "log_entries":format!("{:?}", e.log),
            "transport":format!("{:?}",e.transport)})
    }
    fn rejects_unchanged(e: &mut Engine, d: &FeelDraft) {
        let before = state(e);
        assert!(apply_feel(e, d).is_err());
        assert_eq!(
            state(e),
            before,
            "A rejection must not dispatch, mutate history, or touch transport"
        );
    }

    #[test]
    fn selected_preview_matches_real_dispatch_and_one_step_undo_redo() {
        let mut e = engine("selected");
        let d = draft(&e, true);
        let before = serde_json::to_value(&e.project).unwrap();
        let history = e.undo_depth();
        let rev = e.revision;
        let logs = e.log.len();
        let transport = format!("{:?}", e.transport);
        let predicted = d.preview().unwrap();
        assert_eq!((predicted.starts, predicted.offsets), (1, 0));
        assert!((predicted.clip[0].start - 0.15).abs() < 1e-6);
        assert_eq!(feel_bytes(&predicted.clip[1..]), feel_bytes(&d.clip[1..]));
        apply_feel(&mut e, &d).unwrap();
        assert_eq!(
            feel_bytes(e.project.patterns[d.pattern].notes("keys")),
            feel_bytes(&predicted.clip)
        );
        assert_eq!(e.revision, rev + 1);
        assert_eq!(e.undo_depth(), (history.0 + 1, 0));
        assert_eq!(e.log.len(), logs + 1);
        assert_eq!(format!("{:?}", e.transport), transport);
        let after = serde_json::to_value(&e.project).unwrap();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }

    #[test]
    fn all_explicit_ids_strength_swing_and_supported_grids_match_engine() {
        for selected in [false, true] {
            for strength in [0, 50, 100] {
                for swing in [0, 50, 100] {
                    for grid in 0..4 {
                        let mut e = engine("matrix");
                        let mut d = draft(&e, selected);
                        d.strength = strength;
                        d.swing = swing;
                        d.grid = grid;
                        assert!(!d.ids.is_empty());
                        if !selected {
                            assert_eq!(d.ids.len(), 3);
                        }
                        let p = d.preview().unwrap();
                        if strength == 0 || !p.changed {
                            rejects_unchanged(&mut e, &d);
                            continue;
                        }
                        let song_swing = e.project.swing;
                        let before = state(&e);
                        apply_feel(&mut e, &d).unwrap();
                        assert_eq!(
                            feel_bytes(e.project.patterns[d.pattern].notes("keys")),
                            feel_bytes(&p.clip)
                        );
                        assert_eq!(e.project.swing, song_swing);
                        assert_eq!(e.revision, before["revision"].as_u64().unwrap() + 1);
                        if strength == 100 {
                            assert_eq!(p.offsets, if selected { 1 } else { 3 });
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn swing_delays_odd_grid_only_and_full_strength_resets_offsets() {
        let e = engine("swing");
        let mut d = draft(&e, false);
        d.strength = 100;
        d.swing = 100;
        let p = d.preview().unwrap();
        for old in &d.clip {
            let n = &p.clip[unique_note_index(&p.clip, &old.id).unwrap()];
            let idx = old.start.round();
            let expected = idx + if idx as i64 % 2 == 1 { 1.0 / 3.0 } else { 0.0 };
            assert!((n.start - expected).abs() < 1e-6);
            assert_eq!(n.offset, 0.0);
            assert_eq!(
                (n.len, n.pitch, n.vel, n.prob, n.slide_to),
                (old.len, old.pitch, old.vel, old.prob, old.slide_to)
            );
        }
    }

    #[test]
    fn legacy_unselected_and_new_boundary_notes_are_blocked_without_dispatch() {
        let mut e = engine("boundary");
        let mut d = draft(&e, true);
        let pi = d.pattern;
        e.project.patterns[pi].notes_mut("keys")[2].start = d.steps;
        d.clip = e.project.patterns[pi].notes("keys").to_vec();
        assert!(d.preview().unwrap_err().contains("pattern end"));
        rejects_unchanged(&mut e, &d);
        let mut e = engine("result_boundary");
        let mut d = draft(&e, true);
        e.project.patterns[d.pattern].notes_mut("keys")[0].start = d.steps - 0.1;
        d.clip = e.project.patterns[d.pattern].notes("keys").to_vec();
        d.strength = 100;
        assert!(d.preview().unwrap_err().contains("pattern end"));
        rejects_unchanged(&mut e, &d);
    }

    #[test]
    fn sustained_end_may_cross_boundary_but_nominal_start_must_not() {
        let mut e = engine("sustain");
        let mut d = draft(&e, true);
        e.project.patterns[d.pattern].notes_mut("keys")[0].len = 100.0;
        d.clip = e.project.patterns[d.pattern].notes("keys").to_vec();
        let p = apply_feel(&mut e, &d).unwrap();
        assert_eq!(p.clip[0].len, 100.0);
    }

    #[test]
    fn no_op_empty_scope_snap_off_invalid_and_duplicate_ids_fail_closed() {
        for change in [
            "zero",
            "off",
            "duplicate",
            "empty",
            "nan",
            "infinity",
            "invalid-strength",
            "missing-id",
            "scope-missing",
            "scope-duplicate",
            "scope-replaced",
            "already",
        ] {
            let mut e = engine(change);
            let mut d = draft(&e, true);
            match change {
                "zero" => d.strength = 0,
                "off" => d.grid = 4,
                "duplicate" => d.clip[1].id = d.clip[0].id.clone(),
                "empty" => d.ids.clear(),
                "nan" => d.clip[1].start = f32::NAN,
                "infinity" => d.clip[1].offset = f32::INFINITY,
                "invalid-strength" => d.strength = 101,
                "missing-id" => d.clip[0].id.clear(),
                "scope-missing" => d.ids[0] = "gone".into(),
                "scope-duplicate" => d.ids.push(d.ids[0].clone()),
                "scope-replaced" => d.ids = vec![d.clip[1].id.clone()],
                "already" => {
                    let notes = e.project.patterns[d.pattern].notes_mut("keys");
                    notes[0].start = 0.0;
                    d.clip = notes.clone();
                    assert!(!d.preview().unwrap().changed);
                }
                _ => unreachable!(),
            }
            rejects_unchanged(&mut e, &d);
        }
    }

    #[test]
    fn every_revision_undo_reload_and_generation_only_change_expire() {
        for change in [
            "edit",
            "undo",
            "redo",
            "identical-reload",
            "generation-only",
        ] {
            let mut e = engine(change);
            let d = draft(&e, true);
            match change {
                "edit" => {
                    e.call("set_tempo", &json!({"bpm":133})).unwrap();
                }
                "undo" => {
                    e.call("undo", &json!({})).unwrap();
                }
                "redo" => {
                    e.call("undo", &json!({})).unwrap();
                    e.call("redo", &json!({})).unwrap();
                }
                "identical-reload" => e.replace_project(e.project.clone()),
                "generation-only" => e.project_load_generation += 1,
                _ => unreachable!(),
            }
            rejects_unchanged(&mut e, &d);
            assert_eq!(feel_frame_generation(&e, d.revision, d.generation), None);
        }
    }

    #[test]
    fn owner_selector_and_whole_clip_drift_reject_even_without_revision_bump() {
        for change in [
            "track-replaced",
            "pattern-replaced",
            "track-rename",
            "pattern-rename",
            "track-shadow",
            "pattern-shadow",
            "duplicate-track",
            "duplicate-pattern",
            "generated-name",
            "generated-id",
            "reorder",
            "delete",
            "note-id",
            "unselected-value",
        ] {
            let mut e = engine(change);
            let d = draft(&e, true);
            match change {
                "track-replaced" => e.project.tracks[d.track].id = "replacement".into(),
                "pattern-replaced" => e.project.patterns[d.pattern].id = "replacement".into(),
                "track-rename" => e.project.tracks[d.track].name = "renamed".into(),
                "pattern-rename" => e.project.patterns[d.pattern].name = "renamed".into(),
                "track-shadow" => {
                    let mut t = e.project.tracks[d.track].clone();
                    t.id = "other".into();
                    t.name = d.track_id.clone();
                    e.project.tracks.push(t);
                }
                "pattern-shadow" => {
                    let mut p = e.project.patterns[d.pattern].clone();
                    p.id = "other".into();
                    p.name = d.pattern_id.clone();
                    e.project.patterns.push(p);
                }
                "duplicate-track" => e.project.tracks.push(e.project.tracks[d.track].clone()),
                "duplicate-pattern" => e
                    .project
                    .patterns
                    .push(e.project.patterns[d.pattern].clone()),
                "generated-name" => e.project.patterns[d.pattern].name = "pl:0:phrase".into(),
                "generated-id" => e.project.patterns[d.pattern].id = "pl:source".into(),
                "reorder" => e.project.patterns[d.pattern].notes_mut("keys").swap(0, 1),
                "delete" => {
                    e.project.patterns[d.pattern].notes_mut("keys").pop();
                }
                "note-id" => {
                    e.project.patterns[d.pattern].notes_mut("keys")[1].id = "replacement".into()
                }
                "unselected-value" => e.project.patterns[d.pattern].notes_mut("keys")[1].prob = 0.2,
                _ => unreachable!(),
            }
            rejects_unchanged(&mut e, &d);
        }
    }

    #[test]
    fn foreign_source_duplicate_ids_and_earlier_compiled_owners_reject() {
        for compiled in [false, true] {
            let mut e = engine("foreign-ids");
            let d = draft(&e, true);
            let mut other = e.project.patterns[d.pattern].clone();
            other.id = "foreign-pattern".into();
            other.name = if compiled {
                "pl:foreign"
            } else {
                "foreign-source"
            }
            .into();
            if compiled {
                // Even a generated copy is unsafe if it wins ID normalization.
                e.project.patterns.insert(0, other);
                let mut moved = d.clone();
                moved.pattern += 1;
                rejects_unchanged(&mut e, &moved);
            } else {
                e.project.patterns.push(other);
                rejects_unchanged(&mut e, &d);
            }
        }
    }

    #[test]
    fn generated_sources_cannot_open_and_id_name_shadow_is_ambiguous() {
        let mut e = engine("generated");
        let d = draft(&e, false);
        e.project.patterns[d.pattern].name = "pl:0:phrase".into();
        assert!(FeelDraft::new(
            &e.project,
            (e.revision, e.project_load_generation),
            (d.track, d.pattern, 0),
            vec![],
            2
        )
        .is_err());
        e.project.patterns[d.pattern].name = d.pattern_name.clone();
        e.project.tracks[d.track].name = d.track_id.clone();
        assert!(FeelDraft::new(
            &e.project,
            (e.revision, e.project_load_generation),
            (d.track, d.pattern, 0),
            vec![],
            2
        )
        .is_err());
    }

    #[test]
    fn shared_source_dispatch_updates_compiled_copies_in_one_history_step() {
        let mut e = engine("shared");
        for start in [0, 4] {
            e.call(
                "place_pattern",
                &json!({"pattern":"phrase","start_bar":start,"lane":0}),
            )
            .unwrap();
        }
        let d = draft(&e, false);
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.undo_depth();
        let rev = e.revision;
        let preview = apply_feel(&mut e, &d).unwrap();
        assert_eq!(e.undo_depth(), (h.0 + 1, 0));
        assert_eq!(e.revision, rev + 1);
        assert_eq!(e.project.playlist.len(), 2);
        let compiled: Vec<_> = e
            .project
            .patterns
            .iter()
            .filter(|p| p.name.starts_with("pl:") && !p.notes("keys").is_empty())
            .collect();
        assert_eq!(compiled.len(), 2);
        for p in compiled {
            assert_eq!(feel_bytes(p.notes("keys")), feel_bytes(&preview.clip));
        }
        let after = serde_json::to_value(&e.project).unwrap();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }

    #[test]
    fn alternate_meter_and_sorted_whole_clip_preserve_frozen_ids() {
        let mut e = engine("meter");
        let d = draft(&e, true);
        e.project.patterns[d.pattern].meter = Some((7, 8));
        e.project.patterns[d.pattern].notes_mut("keys").reverse();
        let mut d = draft(&e, false);
        d.swing = 100;
        let p = d.preview().unwrap();
        assert_eq!(d.steps, 28.0);
        let mut old: Vec<_> = d.clip.iter().map(|n| n.id.clone()).collect();
        old.sort();
        let mut new: Vec<_> = p.clip.iter().map(|n| n.id.clone()).collect();
        new.sort();
        assert_eq!(new, old);
        apply_feel(&mut e, &d).unwrap();
        assert_eq!(
            feel_bytes(e.project.patterns[d.pattern].notes("keys")),
            feel_bytes(&p.clip)
        );
    }

    #[test]
    fn context_owner_selection_view_frame_and_gesture_guards() {
        let e = engine("context");
        let d = draft(&e, true);
        assert!(d.context_matches(
            d.revision,
            Some(d.generation),
            (d.track, d.pattern, d.view),
            &d.selected
        ));
        assert!(!d.context_matches(
            d.revision,
            Some(d.generation),
            (d.track, d.pattern, d.view),
            &[]
        ));
        for owner in [
            (d.track + 1, d.pattern, d.view),
            (d.track, d.pattern + 1, d.view),
            (d.track, d.pattern, d.view + 1),
        ] {
            assert!(!d.context_matches(d.revision, Some(d.generation), owner, &d.selected));
        }
        assert!(!d.context_matches(
            d.revision + 1,
            Some(d.generation),
            (d.track, d.pattern, d.view),
            &d.selected
        ));
        assert!(!d.context_matches(d.revision, None, (d.track, d.pattern, d.view), &d.selected));
        assert_eq!(
            feel_frame_generation(&e, d.revision, d.generation),
            Some(d.generation)
        );
        assert!(feel_gesture_idle(false, false, false, false));
        for i in 0..4 {
            let mut flags = [false; 4];
            flags[i] = true;
            assert!(!feel_gesture_idle(flags[0], flags[1], flags[2], flags[3]));
        }
    }

    #[test]
    fn actual_rename_unicode_collision_blocks_open_and_apply_without_wrong_clip_trim() {
        let mut e = engine("unicode-rename");
        e.call(
            "rename_pattern",
            &json!({"pattern":"phrase","new_name":"Ä"}),
        )
        .unwrap();
        e.call("add_pattern", &json!({"name":"later","bars":2}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","pattern":"later","notes":[
            {"start":0.3,"len":1.0,"pitch":72,"vel":0.8}]}),
        )
        .unwrap();
        let ti = e.project.track_index("keys").unwrap();
        let pi = e.project.pattern_index("later").unwrap();
        let earlier = e.project.pattern_index("Ä").unwrap();
        assert!(earlier < pi);
        let mut d = FeelDraft::new(
            &e.project,
            (e.revision, e.project_load_generation),
            (ti, pi, 0),
            vec![],
            2,
        )
        .unwrap();
        // The real legacy wrapper would trim this unpreviewed earlier clip.
        let end = e.project.patterns[earlier].steps() as f32;
        e.project.patterns[earlier].notes_mut("keys")[0].start = end;
        let wrong_clip = feel_bytes(e.project.patterns[earlier].notes("keys"));
        e.call("rename_pattern", &json!({"pattern":"later","new_name":"ä"}))
            .unwrap();
        assert_ne!(e.project.patterns[earlier].id, e.project.patterns[pi].id);
        assert!(e.project.patterns[earlier]
            .notes("keys")
            .iter()
            .all(|a| e.project.patterns[pi]
                .notes("keys")
                .iter()
                .all(|b| a.id != b.id)));
        let args = crate::ids::resolve_id_refs(&e.project, &d.args().unwrap());
        assert_eq!(e.project.pattern_index(&d.pattern_id).unwrap(), pi);
        assert_eq!(args["pattern"], "ä");
        assert_eq!(
            e.project
                .pattern_index(args["pattern"].as_str().unwrap())
                .unwrap(),
            earlier
        );
        let before = state(&e);
        assert!(FeelDraft::new(
            &e.project,
            (e.revision, e.project_load_generation),
            (ti, pi, 0),
            vec![],
            2
        )
        .is_err());
        assert_eq!(state(&e), before);
        rejects_unchanged(&mut e, &d);
        // Isolate the identity guard too, rather than relying only on stale stamps.
        d.revision = e.revision;
        d.generation = e.project_load_generation;
        d.pattern_name = "ä".into();
        assert!(d.normalized_args(&e.project).is_err());
        rejects_unchanged(&mut e, &d);
        assert_eq!(
            feel_bytes(e.project.patterns[earlier].notes("keys")),
            wrong_clip
        );
    }

    #[test]
    fn actual_normalizer_names_whitespace_and_aliases_match_frozen_indices() {
        for case in [
            "ordinary",
            "case-name",
            "track-space",
            "pattern-space",
            "bus-alias",
        ] {
            let mut e = engine(case);
            let mut d = draft(&e, true);
            match case {
                "case-name" => {
                    e.project.tracks[d.track].name = "KEYS".into();
                    e.project.patterns[d.pattern].name = "PHRASE".into();
                    d.track_name = "KEYS".into();
                    d.pattern_name = "PHRASE".into();
                }
                "track-space" => {
                    e.project.tracks[d.track].name = " keys ".into();
                    let clip = e.project.patterns[d.pattern].clips.remove("keys").unwrap();
                    e.project.patterns[d.pattern]
                        .clips
                        .insert(" keys ".into(), clip);
                    d.track_name = " keys ".into();
                }
                "pattern-space" => {
                    e.project.patterns[d.pattern].name = " phrase ".into();
                    d.pattern_name = " phrase ".into();
                }
                "bus-alias" => {
                    // Existing/loaded alias state: add_bus itself refuses an
                    // ID alias. Normalization still needs to handle that state.
                    e.project.buses.push(crate::project::Bus::new(&d.track_id));
                }
                _ => {}
            }
            let args = d.args().unwrap();
            let normalized = crate::ids::resolve_id_refs(&e.project, &args);
            let ti = e
                .project
                .track_index(normalized["track"].as_str().unwrap())
                .ok();
            let pi = e
                .project
                .pattern_index(normalized["pattern"].as_str().unwrap())
                .ok();
            let safe = ti == Some(d.track) && pi == Some(d.pattern);
            assert_eq!(safe, !case.ends_with("space"));
            assert_eq!(d.normalized_args(&e.project).is_ok(), safe);
            let before = state(&e);
            let opened = FeelDraft::new(
                &e.project,
                (e.revision, e.project_load_generation),
                (d.track, d.pattern, 0),
                d.selected.clone(),
                2,
            );
            assert_eq!(opened.is_ok(), safe);
            assert_eq!(state(&e), before);
            if safe {
                let expected = d.preview().unwrap();
                apply_feel(&mut e, &d).unwrap();
                assert_eq!(
                    feel_bytes(e.project.patterns[d.pattern].notes(&d.track_name)),
                    feel_bytes(&expected.clip)
                );
            } else {
                rejects_unchanged(&mut e, &d);
            }
        }
    }

    // Exact local production lifecycle, real Window/controls and the Engine it
    // can mutate. No Studio constructor/native window/general driver required.
    struct Lifecycle {
        ctx: egui::Context,
        engine: std::sync::Mutex<Engine>,
        draft: Option<FeelDraft>,
        frame: FeelContext,
        gestures: [bool; 3],
        targets: Vec<(String, Rect, bool, egui::Id)>,
        shortcuts: usize,
        outcome: Option<FeelOutcome>,
        foreign_focus: Option<egui::Id>,
    }
    impl Lifecycle {
        fn new() -> Self {
            let e = engine("lifecycle");
            let d = draft(&e, true);
            let frame = FeelContext {
                revision: d.revision,
                generation: Some(d.generation),
                owner: (d.track, d.pattern, d.view),
                selected: d.selected.clone(),
                selection_valid: true,
                gesture_idle: true,
            };
            Self {
                ctx: egui::Context::default(),
                engine: std::sync::Mutex::new(e),
                draft: Some(d),
                frame,
                gestures: [false; 3],
                targets: vec![],
                shortcuts: 0,
                outcome: None,
                foreign_focus: None,
            }
        }
        fn state(&self) -> Value {
            state(&self.engine.lock().unwrap())
        }
        fn frame(&mut self, events: Vec<egui::Event>, focused: bool) {
            let p = self.engine.lock().unwrap().project.clone();
            self.ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(TARGETS),
                    Vec::<(String, Rect, bool, egui::Id)>::new(),
                )
            });
            let _ = self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 700.0))),
                    events,
                    focused,
                    ..Default::default()
                },
                |ctx| {
                    // Same order as Studio: shortcuts, then workspace-level dialog.
                    self.shortcuts = super::super::shortcuts::pressed(ctx).len();
                    if let Some(id) = self.foreign_focus {
                        egui::Area::new(egui::Id::new("foreign-dialog"))
                            .fixed_pos(pos2(0.0, 0.0))
                            .show(ctx, |ui| {
                                ui.interact(
                                    Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)),
                                    id,
                                    Sense::focusable_noninteractive(),
                                );
                            });
                        ctx.memory_mut(|m| m.request_focus(id));
                    }
                    let mut frame = self.frame.clone();
                    frame.gesture_idle = feel_gesture_idle(
                        self.gestures[0],
                        self.gestures[1],
                        self.gestures[2],
                        ctx.input(|i| i.pointer.any_down()),
                    );
                    self.outcome = feel_lifecycle(ctx, &p, frame, &mut self.draft, &self.engine);
                },
            );
            self.targets = self
                .ctx
                .data(|data| data.get_temp(egui::Id::new(TARGETS)).unwrap());
        }
        fn click(&mut self, label: &str) {
            let matches: Vec<_> = self
                .targets
                .iter()
                .filter(|(s, _, enabled, _)| s == label && *enabled)
                .collect();
            assert_eq!(matches.len(), 1, "Real control must be reachable: {label}");
            let pos = matches[0].1.center();
            self.frame(
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                true,
            );
            self.frame(
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
            );
        }
        fn ready(&mut self) {
            self.frame(vec![], true);
            self.frame(vec![], true);
        }
        fn assert_closed_inert(&mut self) {
            assert!(self.draft.is_none());
            assert!(self
                .ctx
                .data(|data| data.get_temp::<FeelInputOwner>(feel_owner_key()))
                .is_none());
            for _ in 0..3 {
                self.frame(vec![], true);
                assert!(
                    self.targets.is_empty(),
                    "Closed lifecycle must not render controls"
                );
                assert!(
                    !self.ctx.memory(|m| m.has_focus(feel_focus_id())),
                    "Persistent callback must not reacquire focus after close"
                );
            }
        }
    }

    #[test]
    fn lifecycle_hidden_notes_apply_dispatches_once_and_exact_undo_redo() {
        let mut f = Lifecycle::new();
        let before = f.state();
        let predicted = f.draft.as_ref().unwrap().preview().unwrap();
        let pi = f.draft.as_ref().unwrap().pattern;
        // No Notes pane rendered: the same workspace-level lifecycle still works.
        f.ready();
        assert_eq!(
            f.state(),
            before,
            "Preview is inert on its reachable Engine"
        );
        f.click("Apply");
        assert!(matches!(f.outcome, Some(FeelOutcome::Applied(_, _))));
        let after = f.state();
        assert_eq!(
            after["revision"].as_u64().unwrap(),
            before["revision"].as_u64().unwrap() + 1
        );
        assert_eq!(
            after["log"].as_u64().unwrap(),
            before["log"].as_u64().unwrap() + 1
        );
        assert_eq!(
            after["history"][0].as_u64().unwrap(),
            before["history"][0].as_u64().unwrap() + 1
        );
        assert_eq!(after["history"][1], 0);
        assert_eq!(after["transport"], before["transport"]);
        assert_eq!(after["generation"], before["generation"]);
        assert_eq!(
            feel_bytes(f.engine.lock().unwrap().project.patterns[pi].notes("keys")),
            feel_bytes(&predicted.clip)
        );
        f.assert_closed_inert();
        assert_eq!(f.state(), after);
        let mut e = f.engine.lock().unwrap();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before["project"]);
        assert_eq!(
            format!("{:?}", e.transport),
            before["transport"].as_str().unwrap()
        );
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after["project"]);
        assert_eq!(
            format!("{:?}", e.transport),
            before["transport"].as_str().unwrap()
        );
    }

    #[test]
    fn lifecycle_cancel_escape_blur_expiry_and_postclose_passes_are_inert() {
        for reason in [
            "cancel",
            "escape-combined",
            "blur",
            "owner",
            "pattern-owner",
            "view",
            "selection",
            "invalid-selection",
            "revision",
            "generation",
            "source",
        ] {
            let mut f = Lifecycle::new();
            f.ready();
            let before = f.state();
            match reason {
                "cancel" => f.click("Cancel"),
                "escape-combined" => {
                    f.frame(
                        [egui::Key::Escape, egui::Key::Space, egui::Key::Enter]
                            .into_iter()
                            .map(|key| egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            })
                            .collect(),
                        true,
                    );
                    assert_eq!(
                        f.shortcuts, 0,
                        "Dismissal frame shortcuts must stay suppressed"
                    );
                }
                "blur" => f.frame(vec![], false),
                _ => {
                    match reason {
                        "owner" => f.frame.owner.0 += 1,
                        "pattern-owner" => f.frame.owner.1 += 1,
                        "view" => f.frame.owner.2 += 1,
                        "selection" => f.frame.selected.clear(),
                        "invalid-selection" => f.frame.selection_valid = false,
                        "revision" => f.frame.revision += 1,
                        "generation" => f.frame.generation = None,
                        "source" => f.draft.as_mut().unwrap().clip[0].prob = 0.1,
                        _ => unreachable!(),
                    }
                    f.frame(vec![], true);
                    assert!(matches!(f.outcome, Some(FeelOutcome::Expired)));
                    assert!(f.targets.is_empty(), "Expiry must happen before rendering");
                }
            }
            assert_eq!(
                f.state(),
                before,
                "{reason}: reachable Engine must remain unchanged"
            );
            f.assert_closed_inert();
            assert_eq!(f.state(), before);
        }
    }

    #[test]
    fn lifecycle_held_gestures_preserve_draft_then_release_allows_real_apply() {
        for held in 0..3 {
            let mut f = Lifecycle::new();
            f.ready();
            let before = f.state();
            f.gestures[held] = true;
            f.click("Apply");
            assert!(f.draft.is_some());
            assert_eq!(f.state(), before);
            f.gestures[held] = false;
            f.click("Apply");
            assert!(matches!(f.outcome, Some(FeelOutcome::Applied(_, _))));
            assert_eq!(
                f.state()["log"].as_u64().unwrap(),
                before["log"].as_u64().unwrap() + 1
            );
            assert_eq!(f.state()["transport"], before["transport"]);
            f.assert_closed_inert();
        }
        let mut f = Lifecycle::new();
        f.ready();
        let before = f.state();
        let pos = f
            .targets
            .iter()
            .find(|(s, _, _, _)| s == "Apply")
            .unwrap()
            .1
            .center();
        f.frame(
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            true,
        );
        assert!(f.draft.is_some());
        assert_eq!(f.state(), before, "Pointer-down preview cannot commit");
        f.frame(vec![], false);
        f.assert_closed_inert();
        assert_eq!(f.state(), before);
    }

    #[test]
    fn lifecycle_numeric_input_and_preview_keyboard_events_do_not_dispatch() {
        let mut f = Lifecycle::new();
        f.ready();
        let before = f.state();
        f.click("Strength");
        f.click("Strength");
        let id = f
            .targets
            .iter()
            .find(|(s, _, _, _)| s == "Strength")
            .unwrap()
            .3;
        assert!(f.ctx.memory(|m| m.has_focus(id)));
        let mods = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        for pressed in [true, false] {
            f.frame(
                vec![egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: mods,
                }],
                true,
            );
        }
        f.frame(vec![egui::Event::Text("75".into())], true);
        for key in [
            egui::Key::Enter,
            egui::Key::Space,
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
        ] {
            for pressed in [true, false] {
                f.frame(
                    vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    true,
                );
                assert_eq!(f.shortcuts, 0);
                assert!(f.draft.is_some());
                assert_eq!(f.state(), before);
            }
        }
        assert_eq!(f.draft.as_ref().unwrap().strength, 75);
        f.click("Cancel");
        f.assert_closed_inert();
        assert_eq!(f.state(), before);
    }

    #[test]
    fn lifecycle_apply_lock_rechecks_actual_engine_revision_and_generation() {
        for reload in [false, true] {
            let mut f = Lifecycle::new();
            f.ready();
            {
                let mut e = f.engine.lock().unwrap();
                if reload {
                    let p = e.project.clone();
                    e.replace_project(p);
                } else {
                    e.call("set_tempo", &json!({"bpm":133})).unwrap();
                }
            }
            // Keep the old GUI frame stamps, as in a same-frame remote edit.
            let before = f.state();
            f.click("Apply");
            assert!(matches!(f.outcome, Some(FeelOutcome::Failed(_))));
            assert_eq!(f.state(), before);
            f.assert_closed_inert();
            assert_eq!(f.state(), before);
        }
    }

    #[test]
    fn lifecycle_owned_numeric_cleanup_foreign_focus_preservation_and_reopen() {
        for reason in ["cancel", "escape", "blur", "expiry"] {
            for foreign in [false, true] {
                let mut f = Lifecycle::new();
                for _ in 0..2 {
                    f.ready();
                    f.click("Strength");
                    f.click("Strength");
                    let numeric = f
                        .targets
                        .iter()
                        .find(|(s, _, _, _)| s == "Strength")
                        .unwrap()
                        .3;
                    assert!(f.ctx.memory(|m| m.has_focus(numeric)));
                    let id = if foreign {
                        egui::Id::new("unrelated-dialog-editor")
                    } else {
                        numeric
                    };
                    f.ctx.memory_mut(|m| m.request_focus(id));
                    let before = f.state();
                    // Expiry/blur/Escape return before rendering controls, just
                    // as production does, while a foreign dialog owns focus.
                    f.foreign_focus = foreign.then_some(id);
                    match reason {
                        "cancel" => f.click("Cancel"),
                        "escape" => f.frame(
                            vec![egui::Event::Key {
                                key: egui::Key::Escape,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            }],
                            true,
                        ),
                        "blur" => f.frame(vec![], false),
                        "expiry" => {
                            f.frame.owner.2 += 1;
                            f.frame(vec![], true);
                        }
                        _ => unreachable!(),
                    }
                    if foreign {
                        assert!(
                            f.ctx.memory(|m| m.has_focus(id)),
                            "{reason}: actual lifecycle cleanup must preserve the foreign widget"
                        );
                    } else {
                        assert!(!f.ctx.memory(|m| m.has_focus(numeric)));
                    }
                    assert_eq!(f.state(), before);
                    f.assert_closed_inert();
                    assert_eq!(f.state(), before);
                    if foreign {
                        assert!(f.ctx.memory(|m| m.has_focus(id)));
                        f.ctx.memory_mut(|m| m.surrender_focus(id));
                    }
                    f.foreign_focus = None;
                    let d = draft(&f.engine.lock().unwrap(), true);
                    f.frame.owner = (d.track, d.pattern, d.view);
                    f.draft = Some(d);
                }
            }
        }
    }

    // Component-only controls and predicates below do not exercise the dialog lifecycle.
    // Lifecycle regressions below use feel_lifecycle with its actual reachable Engine.
    const TARGETS: &str = "feel-test-targets";
    pub(super) fn record(ui: &egui::Ui, label: &str, r: &egui::Response) {
        let rect = r.rect.intersect(ui.clip_rect());
        ui.ctx().data_mut(|data| {
            let mut v = data
                .get_temp::<Vec<(String, Rect, bool, egui::Id)>>(egui::Id::new(TARGETS))
                .unwrap_or_default();
            v.push((label.into(), rect, r.enabled() && r.rect == rect, r.id));
            data.insert_temp(egui::Id::new(TARGETS), v);
        });
    }
    struct Form {
        ctx: egui::Context,
        d: FeelDraft,
        targets: Vec<(String, Rect, bool, egui::Id)>,
        shortcuts: usize,
        action: FeelAction,
        dismiss: bool,
    }
    impl Form {
        fn new() -> Self {
            let e = engine("form");
            Self {
                ctx: egui::Context::default(),
                d: draft(&e, true),
                targets: vec![],
                shortcuts: 0,
                action: FeelAction::default(),
                dismiss: false,
            }
        }
        fn frame(&mut self, events: Vec<egui::Event>, focused: bool) {
            self.ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(TARGETS),
                    Vec::<(String, Rect, bool, egui::Id)>::new(),
                )
            });
            let _ = self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(480.0, 700.0))),
                    events,
                    focused,
                    ..Default::default()
                },
                |ctx| {
                    self.shortcuts = super::super::shortcuts::pressed(ctx).len();
                    self.dismiss = feel_dismissed(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        self.action = feel_controls(ui, &mut self.d);
                    });
                },
            );
            self.targets = self
                .ctx
                .data_mut(|d| d.get_temp(egui::Id::new(TARGETS)).unwrap());
        }
        fn click(&mut self, label: &str) {
            let targets: Vec<_> = self
                .targets
                .iter()
                .filter(|(s, _, enabled, _)| s == label && *enabled)
                .collect();
            assert_eq!(targets.len(), 1);
            let p = targets[0].1.center();
            self.frame(
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                true,
            );
            self.frame(
                vec![egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
            );
        }
        fn key(&mut self, key: egui::Key) {
            self.frame(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
            );
            assert_eq!(
                self.shortcuts, 0,
                "Panel focus must suppress global transport and piano shortcuts"
            );
            self.frame(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
            );
        }
    }
    #[test]
    fn component_controls_apply_cancel_and_keyboard_focus() {
        let mut f = Form::new();
        f.frame(vec![], true);
        f.frame(vec![], true);
        assert!(f.ctx.wants_keyboard_input());
        for k in [
            egui::Key::Space,
            egui::Key::Enter,
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
        ] {
            f.key(k);
        }
        f.click("Apply");
        assert!(f.action.apply);
        f.click("Cancel");
        assert!(f.action.cancel);
        f.click("Strength");
        f.key(egui::Key::ArrowLeft);
        assert!(f.ctx.wants_keyboard_input());
        f.d.strength = 0;
        f.frame(vec![], true);
        assert!(
            !f.targets
                .iter()
                .find(|(s, _, _, _)| s == "Apply")
                .unwrap()
                .2
        );
    }
    #[test]
    fn component_numeric_entry_and_shortcut_ownership() {
        let mut f = Form::new();
        f.frame(vec![], true);
        f.frame(vec![], true);
        f.click("Strength");
        f.click("Strength");
        let id = f
            .targets
            .iter()
            .find(|(s, _, _, _)| s == "Strength")
            .unwrap()
            .3;
        assert!(f.ctx.memory(|m| m.has_focus(id)));
        let mods = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        for pressed in [true, false] {
            f.frame(
                vec![egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: mods,
                }],
                true,
            );
        }
        f.frame(vec![egui::Event::Text("75".into())], true);
        f.key(egui::Key::Enter);
        assert_eq!(f.d.strength, 75);
        assert!(!f.action.apply);
        let events = [egui::Key::Escape, egui::Key::Space, egui::Key::Enter]
            .into_iter()
            .map(|key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect();
        f.frame(events, true);
        assert!(f.dismiss);
        assert_eq!(f.shortcuts, 0);
        // Closing releases ownership; the callback remains installed but inert.
        feel_input_owner(&f.ctx, false);
        assert!(!f.ctx.wants_keyboard_input());
    }

    #[test]
    fn component_dismissal_predicates_with_unconnected_engine() {
        let e = engine("dismiss");
        let before = state(&e);
        let mut f = Form::new();
        f.frame(vec![], true);
        f.frame(
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );
        assert!(f.dismiss);
        f.frame(vec![], false);
        assert!(f.dismiss);
        assert_eq!(state(&e), before);
    }
}

#[cfg(test)]
mod feel_pure_tests {
    use super::*;
    fn note(id: &str, start: f32) -> Note {
        let mut n = Note::new(start, 1.7, 60, 0.7);
        n.id = id.into();
        n.prob = 0.6;
        n.offset = 0.12;
        n.slide_to = Some(67);
        n
    }
    #[test]
    fn exact_partial_quantize_keeps_unselected_payload_and_identity() {
        let before = vec![note("a", 0.3), note("b", 1.2)];
        let mut notes = before.clone();
        let sel = crate::midi_ops::Selection {
            ids: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(
            crate::midi_ops::quantize(&mut notes, &sel, 1.0, 0.5, 0.0, false),
            1
        );
        assert!((notes[0].start - 0.15).abs() < 1e-6);
        let a = &notes[0];
        let b = &before[0];
        assert_eq!(
            (&a.id, a.len, a.pitch, a.vel, a.prob, a.offset, a.slide_to),
            (&b.id, b.len, b.pitch, b.vel, b.prob, b.offset, b.slide_to)
        );
        assert_eq!(format!("{:?}", notes[1]), format!("{:?}", before[1]));
        assert!(feel_clip_valid(&notes, 32.0).is_ok());
    }
    #[test]
    fn exact_quantizer_odd_swing_offsets_and_matched_count_are_not_change_count() {
        let mut notes = vec![note("even", 2.0), note("odd", 1.0)];
        let sel = crate::midi_ops::Selection {
            ids: vec!["even".into(), "odd".into()],
            ..Default::default()
        };
        assert_eq!(
            crate::midi_ops::quantize(&mut notes, &sel, 1.0, 1.0, 1.0, false),
            2
        );
        assert!((notes[0].start - 4.0 / 3.0).abs() < 1e-6);
        assert_eq!(notes[1].start, 2.0);
        assert!(notes.iter().all(|n| n.offset == 0.0));
        let mut exact = vec![note("exact", 0.0)];
        let original = exact[0].start;
        assert_eq!(
            crate::midi_ops::quantize(&mut exact, &Default::default(), 1.0, 0.5, 0.0, false),
            1
        );
        assert_eq!(exact[0].start, original);
    }
    #[test]
    fn full_clip_boundary_guard_includes_unselected_and_result_notes() {
        let original = vec![note("selected", 0.3), note("unselected", 32.0)];
        assert!(feel_clip_valid(&original, 32.0)
            .unwrap_err()
            .contains("pattern end"));
        let mut result = vec![note("a", 31.9)];
        assert!(feel_clip_valid(&result, 32.0).is_ok());
        crate::midi_ops::quantize(&mut result, &Default::default(), 1.0, 1.0, 0.0, false);
        assert!(feel_clip_valid(&result, 32.0)
            .unwrap_err()
            .contains("pattern end"));
        let mut sustain = vec![note("sustain", 31.0)];
        sustain[0].len = 20.0;
        assert!(feel_clip_valid(&sustain, 32.0).is_ok());
    }
    #[test]
    fn malformed_and_nonfinite_clip_inputs_fail_closed() {
        for case in 0..10 {
            let mut notes = vec![note("a", 0.3), note("b", 1.0)];
            match case {
                0 => notes[1].id = "a".into(),
                1 => notes[1].id.clear(),
                2 => notes[1].start = f32::NAN,
                3 => notes[1].len = f32::INFINITY,
                4 => notes[1].vel = f32::NAN,
                5 => notes[1].prob = f32::NAN,
                6 => notes[1].offset = f32::NAN,
                7 => notes[1].start = -1.0,
                8 => notes[1].len = 0.0,
                9 => notes[1].id = " b ".into(),
                _ => unreachable!(),
            }
            assert!(feel_clip_valid(&notes, 32.0).is_err());
        }
        assert!(feel_clip_valid(&[], 32.0).is_err());
        assert!(feel_clip_valid(&[note("a", 0.3)], f32::NAN).is_err());
    }
    #[test]
    fn stable_note_lookup_and_each_held_gesture_guard_are_exact() {
        let notes = vec![note("a", 0.3), note("b", 1.0)];
        assert_eq!(unique_note_index(&notes, "a"), Some(0));
        assert_eq!(unique_note_index(&notes, "missing"), None);
        assert_eq!(
            unique_note_index(&[note("a", 0.3), note("a", 1.0)], "a"),
            None
        );
        assert!(feel_gesture_idle(false, false, false, false));
        for flags in [
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, true),
        ] {
            assert!(!feel_gesture_idle(flags.0, flags.1, flags.2, flags.3));
        }
    }
    #[test]
    fn unchanged_quantizer_fractional_grid_extremes_keep_all_fields_except_timing() {
        // These exercise the unchanged backend function, NOT extra GUI grid choices.
        for grid in [0.0625, 2.0 / 3.0, 0.5, 1.0, 2.0, 4.0] {
            for strength in [0.0, 0.5, 1.0] {
                for swing in [0.0, 1.0] {
                    let old = vec![note("a", 0.3), note("b", 1.2), note("c", 2.7)];
                    let mut now = old.clone();
                    let sel = crate::midi_ops::Selection {
                        ids: old.iter().map(|n| n.id.clone()).collect(),
                        ..Default::default()
                    };
                    crate::midi_ops::quantize(&mut now, &sel, grid, strength, swing, false);
                    assert!(feel_clip_valid(&now, 32.0).is_ok());
                    for before in &old {
                        let after = &now[unique_note_index(&now, &before.id).unwrap()];
                        assert_eq!(
                            (
                                before.len,
                                before.pitch,
                                before.vel,
                                before.prob,
                                before.slide_to
                            ),
                            (
                                after.len,
                                after.pitch,
                                after.vel,
                                after.prob,
                                after.slide_to
                            )
                        );
                        assert_eq!(
                            after.offset,
                            if strength == 1.0 { 0.0 } else { before.offset }
                        );
                    }
                }
            }
        }
    }
}
