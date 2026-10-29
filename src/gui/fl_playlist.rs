//! FL-style pattern playlist: pattern clips on numbered playlist tracks,
//! selected, painted, dragged and resized directly on a bar grid. Select is
//! the safe default; Paint places the selected pattern in an empty cell, drag a clip to move it (bars and lanes),
//! drag its right edge to resize it, right-click it for split / trim /
//! duplicate / delete. Every gesture is one call to the same playlist tool an
//! MCP client uses (place_pattern, playlist_clip), so it is one undo step.

use super::widgets::*;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use serde_json::{json, Value};

pub(crate) const HEAD_W: f32 = 96.0;
const LANE_H: f32 = 28.0;
const RULER_H: f32 = 16.0;
const EDGE_PX: f32 = 7.0;
// Keep compiled step counts, as well as clip endpoints, inside u32.
const EDIT_BAR_LIMIT: u32 = u32::MAX / crate::project::STEPS_PER_BAR;

/// Playlist copies are stored as "pl:<bar>:<name>"; people only see the name.
pub fn friendly_name(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix(crate::tools_fl::PL) {
        if let Some((_, n)) = rest.split_once(':') {
            return n;
        }
    }
    name
}

/// A pattern's clip colour, stable per name (FL tints each pattern).
pub fn pattern_color(name: &str) -> Color32 {
    let h = name
        .bytes()
        .fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
    let hue = (h % 360) as f32 / 360.0;
    let c = egui::ecolor::Hsva::new(hue, 0.42, 0.78, 1.0);
    Color32::from(c)
}

/// Where a drag lands: whole bars and lanes from pixel deltas.
pub fn snap_drag(dx: f32, dy: f32, bar_px: f32) -> (i64, i64) {
    (
        (dx / bar_px.max(f32::MIN_POSITIVE)).round() as i64,
        (dy / LANE_H).round() as i64,
    )
}

/// The calls that paint `pattern` at (bar, lane): the section arrangement is
/// converted to playlist clips first when the playlist is still empty.
pub fn paint_calls(p: &Project, pattern: &str, bar: u32, lane: u32) -> Vec<(&'static str, Value)> {
    let place = json!({"pattern": pattern, "start_bar": bar, "lane": lane});
    if p.playlist.is_empty() && !p.arrangement.is_empty() {
        // The implicit conversion belongs to this same paint gesture. A
        // failed placement must roll it back; Undo must undo both together.
        vec![(
            "batch",
            json!({"calls": [
                {"tool": "arrangement_to_playlist", "args": {}},
                {"tool": "place_pattern", "args": place}
            ]}),
        )]
    } else {
        vec![("place_pattern", place)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Move,
    /// the right edge: changes the length
    Resize,
    /// the left edge: changes the start (and the pattern offset), keeps the end
    ResizeStart,
}

impl Grab {
    fn code(self) -> u8 {
        match self {
            Grab::Move => 0,
            Grab::Resize => 1,
            Grab::ResizeStart => 2,
        }
    }
    fn from_code(c: u8) -> Grab {
        match c {
            1 => Grab::Resize,
            2 => Grab::ResizeStart,
            _ => Grab::Move,
        }
    }
}

/// At most this many lanes are shown at once; more scroll (mouse wheel).
pub const MAX_VISIBLE_LANES: usize = 8;
pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 4096.0;

/// What part of the song the grid shows: `scroll_bar` bars hidden off the
/// left, `scroll_lane` lanes hidden off the top, and `zoom` times the width
/// that fits the whole song. Wheel scrolls lanes, Shift+wheel (or a
/// sideways wheel) scrolls bars, Ctrl+wheel zooms around the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaylistView {
    pub scroll_bar: f32,
    pub scroll_lane: f32,
    pub zoom: f32,
}

impl Default for PlaylistView {
    fn default() -> Self {
        PlaylistView {
            scroll_bar: 0.0,
            scroll_lane: 0.0,
            zoom: 1.0,
        }
    }
}

impl PlaylistView {
    /// Keep the view inside the song: zoom in range, never scrolled before
    /// bar 0 / lane 0, never past the song's bars or the last lane page.
    pub fn clamped(self, total_bars: u32, lanes: usize) -> Self {
        let visible = lanes.min(MAX_VISIBLE_LANES);
        let z = if self.zoom.is_finite() {
            self.zoom.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            1.0
        };
        let sb = if self.scroll_bar.is_finite() {
            self.scroll_bar.clamp(0.0, total_bars as f32)
        } else {
            0.0
        };
        let sl = if self.scroll_lane.is_finite() {
            self.scroll_lane
                .clamp(0.0, lanes.saturating_sub(visible) as f32)
        } else {
            0.0
        };
        PlaylistView {
            scroll_bar: sb,
            scroll_lane: sl,
            zoom: z,
        }
    }
}

/// One frame's grid geometry: song bars and lanes to screen points and
/// back, through the view's scroll and zoom. Every hit test goes through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridGeo {
    pub grid: Rect,
    pub bar_px: f32,
    pub scroll_bar: f32,
    pub scroll_lane: f32,
}

impl GridGeo {
    pub fn x_of(&self, bar: f32) -> f32 {
        self.grid.min.x + (bar - self.scroll_bar) * self.bar_px
    }
    pub fn y_of(&self, lane: f32) -> f32 {
        self.grid.min.y + (lane - self.scroll_lane) * LANE_H
    }
    /// The song bar (fractional) under screen x.
    pub fn bar_at(&self, x: f32) -> f32 {
        self.scroll_bar + (x - self.grid.min.x) / self.bar_px
    }
    /// The lane (fractional) under screen y.
    pub fn lane_at(&self, y: f32) -> f32 {
        self.scroll_lane + (y - self.grid.min.y) / LANE_H
    }
}

/// Where a clip lands after a drag of `d` points: whole bars and lanes,
/// clamped so it never starts before bar 0, never sits on a lane past the
/// last track (`lanes - 1`) and is never shorter than one grid step (one
/// bar: clips are whole bars). A left-edge trim keeps the clip's end.
/// Returns (start_bar, lane, bars).
fn dragged_clip(
    start: u32,
    lane: u32,
    bars: u32,
    grab: Option<(Grab, egui::Vec2)>,
    bar_px: f32,
    lanes: usize,
) -> (u32, u32, u32) {
    let (s, l, n) = (start as i64, lane as i64, bars.max(1) as i64);
    let last_lane = (lanes.max(1) - 1) as i64;
    match grab {
        Some((Grab::Move, d)) => {
            let (db, dl) = snap_drag(d.x, d.y, bar_px);
            (
                (s.saturating_add(db)).clamp(0, i64::from(u32::MAX).saturating_sub(n)) as u32,
                (l + dl).clamp(0, last_lane) as u32,
                n as u32,
            )
        }
        Some((Grab::Resize, d)) => (
            s as u32,
            l as u32,
            (n.saturating_add((d.x / bar_px.max(f32::MIN_POSITIVE)).round() as i64))
                .clamp(1, i64::from(u32::MAX).saturating_sub(s).max(1)) as u32,
        ),
        Some((Grab::ResizeStart, d)) => {
            let end = s + n;
            let ns = (s.saturating_add((d.x / bar_px.max(f32::MIN_POSITIVE)).round() as i64))
                .clamp(0, end - 1);
            (ns as u32, l as u32, (end - ns) as u32)
        }
        None => (s as u32, l as u32, n as u32),
    }
}

/// What the grid reads and changes besides the project: the selected
/// pattern (what a click paints), the selected clip, a toast to show, the
/// view (scroll and zoom, kept by the caller between frames) and, after a
/// frame, the geometry that frame used.
#[derive(Clone, Debug, Default)]
pub struct GridSel {
    pub pattern: usize,
    pub clip_sel: Option<String>,
    pub toast: Option<String>,
    pub view: PlaylistView,
    pub geo: Option<GridGeo>,
    pub(super) paint: bool,
    pub(super) owner: GridOwner,
    pub(super) draft: Option<ClipDraft>,
    pub(super) swallow_release: bool,
    pub(super) song_navigation: bool,
    pub(super) active_drag: Option<(GridOwner, bool)>,
}

/// Identity is captured at gesture/draft origin, not replaced on release.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GridOwner {
    revision: u64,
    generation: u64,
}

#[derive(Clone, Debug)]
pub(super) struct ClipDraft {
    id: String,
    owner: GridOwner,
    start: u64,
    lane: u64,
    length: u64,
    copies: u64,
}

impl ClipDraft {
    fn new(c: &crate::project::PlaylistClip, owner: GridOwner) -> Self {
        Self {
            id: c.id.clone(),
            owner,
            start: u64::from(c.start_bar) + 1,
            lane: u64::from(c.lane) + 1,
            length: u64::from(c.bars.max(1)),
            copies: 1,
        }
    }
    fn payload(&self, c: &crate::project::PlaylistClip, action: &str) -> Option<Value> {
        let start = self.start.checked_sub(1)?;
        let lane = self.lane.checked_sub(1)?;
        let limit = u64::from(EDIT_BAR_LIMIT);
        let valid =
            start.checked_add(u64::from(c.bars.max(1)))? <= limit && lane < u64::from(u32::MAX);
        match action {
            "move" if valid && (start != u64::from(c.start_bar) || lane != u64::from(c.lane)) => {
                Some(json!({"action": action, "id": self.id, "to_bar": start, "lane": lane}))
            }
            "trim"
                if self.length > 0
                    && self.length != u64::from(c.bars.max(1))
                    && u64::from(c.start_bar).checked_add(self.length)? <= limit =>
            {
                Some(
                    json!({"action": action, "id": self.id, "end_bar": u64::from(c.start_bar) + self.length}),
                )
            }
            "duplicate"
                if (1..=64).contains(&self.copies)
                    && u64::from(c.start_bar)
                        .checked_add(u64::from(c.bars.max(1)).checked_mul(self.copies + 1)?)?
                        <= limit =>
            {
                Some(json!({"action": action, "id": self.id, "count": self.copies}))
            }
            "make_unique" => Some(json!({"action": action, "id": self.id})),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct GridDrag {
    kind: u8,
    x: f32,
    y: f32,
    owner: GridOwner,
    paint: bool,
}

fn grid_scale(width: f32, bars: u32, zoom: f32) -> f32 {
    let width = if width.is_finite() {
        width.max(1.0)
    } else {
        1.0
    };
    let zoom = if zoom.is_finite() {
        zoom.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.0
    };
    (f64::from(width) / f64::from(bars.max(1)) * f64::from(zoom)).min(f64::from(f32::MAX)) as f32
}

fn guarded_grid_call(
    e: &mut crate::engine::Engine,
    owner: GridOwner,
    tool: &str,
    args: &Value,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        e.revision == owner.revision && e.project_load_generation == owner.generation,
        "Clip edit expired after a project change. Refresh and try again."
    );
    e.call_from(tool, args, "you")
}

impl Studio {
    pub(super) fn fl_playlist(&mut self, ui: &mut egui::Ui, p: &Project, revision: u64) {
        let bar_now = super::playlist::marker_beat(p, self.player.position_secs()) / 4.0;
        let vid = egui::Id::new("flpl_state");
        let owner = GridOwner {
            revision,
            generation: self.seen_project_load,
        };
        let mut sel = ui.data(|d| d.get_temp::<GridSel>(vid)).unwrap_or_default();
        if sel.owner.generation != owner.generation {
            let had_pattern_selection = sel.draft.is_some();
            sel = GridSel {
                swallow_release: true,
                ..Default::default()
            };
            if had_pattern_selection
                || p.playlist
                    .iter()
                    .any(|c| Some(&c.id) == self.clip_sel.as_ref())
            {
                self.clip_sel = None;
            }
            sel.toast = Some("Project loaded. Clip controls refreshed in Select mode.".into());
        }
        sel.owner = owner;
        sel.song_navigation =
            self.perf.is_none() && self.perf_next.is_none() && !self.player.launch_pending();
        sel.pattern = self.pattern;
        sel.clip_sel = self.clip_sel.clone();
        let actions = playlist_grid(ui, p, &mut sel, bar_now);
        self.pattern = sel.pattern;
        self.clip_sel = sel.clip_sel.clone();
        for (tool, args) in actions {
            // Validation and dispatch share this lock; never check, unlock, call.
            let result = {
                let mut e = self.engine.lock().unwrap();
                guarded_grid_call(&mut e, owner, tool, &args).map(|value| {
                    if let Some(id) = value["new_ids"]
                        .as_array()
                        .and_then(|ids| ids.first())
                        .and_then(Value::as_str)
                    {
                        if e.project.playlist.iter().any(|c| c.id == id) {
                            self.clip_sel = Some(id.to_owned());
                        }
                    }
                    if let Some(name) = value["new_pattern"].as_str() {
                        if let Ok(pi) = e.project.pattern_index(name) {
                            self.pattern = pi;
                        }
                    }
                })
            };
            if let Err(err) = result {
                sel.toast = Some(format!("{err:#}"));
                break;
            }
        }
        // Automation keeps the exact grid axis and its existing command handlers.
        if let Some(geo) = sel.geo {
            for (tool, args) in super::fl_automation::automation_lanes(ui, p, &geo) {
                self.call(tool, args);
            }
        }
        if let Some(t) = sel.toast.take() {
            self.toast = Some((t, 0.0, false));
        }
        ui.data_mut(|d| d.insert_temp(vid, sel));
    }
}

/// Sample the visible source repetitions, never a prefix of the song.
/// Returned positions are clip-local bars, including the source offset phase.
fn preview_repeats(
    left: f64,
    right: f64,
    len: f64,
    source: f64,
    offset: u32,
    budget: usize,
) -> impl Iterator<Item = f64> + Clone {
    let span = source.max(1.0 / f64::from(crate::project::STEPS_PER_BAR));
    let off = f64::from(offset);
    let left = left.max(0.0);
    let right = right.min(len);
    let first = ((left + off) / span).floor() as u64;
    let end = ((right + off) / span).ceil() as u64;
    let count = if right > left {
        end.saturating_sub(first)
    } else {
        0
    };
    let samples = count.min(budget.clamp(1, 64) as u64);
    (0..samples).map(move |i| (first + i * count / samples) as f64 * span - off)
}

fn clip_controls(
    ui: &mut egui::Ui,
    p: &Project,
    sel: &mut GridSel,
    bar_now: f32,
    total_bars: u32,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let selected = p
        .playlist
        .iter()
        .find(|c| Some(&c.id) == sel.clip_sel.as_ref());
    ui.horizontal_wrapped(|ui| {
        let _response = ui.selectable_value(&mut sel.paint, false, "Select");
        #[cfg(test)]
        tests::record_target(ui, "Select", &_response);
        let _response = ui.selectable_value(&mut sel.paint, true, "Paint");
        #[cfg(test)]
        tests::record_target(ui, "Paint", &_response);
        let response = ui.button("Fit song");
        #[cfg(test)]
        tests::record_target(ui, "Fit song", &response);
        if response.clicked() {
            sel.view.scroll_bar = 0.0;
            sel.view.zoom = 1.0;
        }
        let response = ui.add_enabled(selected.is_some(), egui::Button::new("Reveal clip"));
        #[cfg(test)]
        tests::record_target(ui, "Reveal clip", &response);
        if response.clicked() {
            let c = selected.unwrap();
            sel.view.scroll_bar = c.start_bar as f32;
            sel.view.scroll_lane = c.lane as f32;
            sel.view.zoom =
                (total_bars as f32 / (c.bars.max(1) as f32 + 2.0)).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        let response = ui
            .add_enabled(sel.song_navigation, egui::Button::new("Go to playhead"))
            .on_hover_text("Scroll only. Unavailable during performance or a queued launch.");
        #[cfg(test)]
        tests::record_target(ui, "Go to playhead", &response);
        if response.clicked() {
            sel.view.scroll_bar = bar_now.clamp(0.0, total_bars as f32);
        }
        let response = ui.button("Zoom out");
        #[cfg(test)]
        tests::record_target(ui, "Zoom out", &response);
        if response.clicked() {
            sel.view.zoom = (sel.view.zoom / 2.0).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        let response = ui.button("Zoom in");
        #[cfg(test)]
        tests::record_target(ui, "Zoom in", &response);
        if response.clicked() {
            sel.view.zoom = (sel.view.zoom * 2.0).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        ui.label(format!("{:.0}%", sel.view.zoom * 100.0));
    });
    let source = p
        .patterns
        .get(sel.pattern)
        .filter(|p| !p.name.starts_with("pl:"));
    ui.label(if sel.paint {
        source
            .map(|p| {
                format!(
                    "Paint: {}. Click an empty cell to place it.",
                    friendly_name(&p.name)
                )
            })
            .unwrap_or_else(|| "Painting unavailable. Select a source pattern first.".into())
    } else {
        "Select: empty clicks clear a pattern clip selection without adding music.".into()
    });
    ui.small("Playlist grid bars: 4 quarter-note beats");
    let Some(c) = selected else {
        sel.draft = None;
        return;
    };
    let changed = sel
        .draft
        .as_ref()
        .is_none_or(|d| d.id != c.id || d.owner != sel.owner);
    let stale = sel
        .draft
        .as_ref()
        .is_some_and(|d| d.id == c.id && d.owner != sel.owner);
    if changed {
        sel.draft = Some(ClipDraft::new(c, sel.owner));
    }
    if stale {
        sel.toast = Some("Clip changed. Draft refreshed; review values before applying.".into());
    }
    let d = sel.draft.as_mut().unwrap();
    let shared = p
        .pattern_index(&c.pattern)
        .ok()
        .map(|pi| {
            p.playlist
                .iter()
                .filter(|other| p.pattern_index(&other.pattern).ok() == Some(pi))
                .count()
        })
        .unwrap_or(0);
    ui.label(format!("Selected clip: {} | start {} | end {} | length {} | lane {} | source offset {} | {} placements",
        friendly_name(&c.pattern), u64::from(c.start_bar)+1,
        u64::from(c.start_bar)+u64::from(c.bars.max(1)), c.bars.max(1), u64::from(c.lane)+1, c.offset_bars, shared));
    ui.small("Lane changes display only. Editing this pattern changes its shared placements.");
    // A fresh owner must never inherit egui's focused numeric text buffer.
    ui.push_id((&d.id, d.owner.revision, d.owner.generation), |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, field) in [
                ("Start", &mut d.start),
                ("Lane", &mut d.lane),
                ("Length", &mut d.length),
                ("Copies", &mut d.copies),
            ] {
                ui.label(label);
                let _response = ui
                    .push_id(label, |ui| {
                        ui.add(egui::DragValue::new(field).range(
                            1..=if label == "Copies" {
                                64
                            } else if label == "Lane" {
                                u64::from(u32::MAX)
                            } else {
                                u64::from(EDIT_BAR_LIMIT)
                            },
                        ))
                    })
                    .inner;
                #[cfg(test)]
                tests::record_target(ui, label, &_response);
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        for (label, action) in [("Move", "move"), ("Set length", "trim"), ("Duplicate after", "duplicate"), ("Make independent", "make_unique")] {
            let payload = d.payload(c, action);
            let response = ui.add_enabled(payload.is_some() && !stale, egui::Button::new(label)).on_disabled_hover_text("No change, expired draft, or values outside the grid range.");
            let response = if action == "make_unique" { response.on_hover_text("Copy pattern notes without changing placement timing. Other placements keep the original source.") }
                else if action == "duplicate" { response.on_hover_text("1 to 64 copies after this clip, sharing its source.") } else { response };
            #[cfg(test)]
            tests::record_target(ui, label, &response);
            if response.clicked() { actions.push(("playlist_clip", payload.unwrap())); }
        }
    });
}

/// The playlist grid itself, driven by egui pointer input (a real mouse,
/// or synthetic events in tests): returns the engine calls a click, drag or
/// edge drag asks for. Hit tests honour the view's scroll and zoom.
pub fn playlist_grid(
    ui: &mut egui::Ui,
    p: &Project,
    sel: &mut GridSel,
    bar_now: f32,
) -> Vec<(&'static str, Value)> {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let lanes = p
        .playlist
        .iter()
        .map(|c| c.lane.saturating_add(1))
        .max()
        .unwrap_or(0)
        .max(3) as usize
        + 1;
    let visible = lanes.min(MAX_VISIBLE_LANES);
    let end_bar = p
        .playlist
        .iter()
        .map(|c| c.start_bar.saturating_add(c.bars.max(1)))
        .max()
        .unwrap_or(0);
    let total_bars = end_bar.saturating_add(8).max(16);
    let was_paint = sel.paint;
    clip_controls(ui, p, sel, bar_now, total_bars, &mut actions);
    let mode_changed = was_paint != sel.paint;
    let cancel_key = sel.active_drag.is_some()
        && !ui.ctx().wants_keyboard_input()
        && ui.input(|i| i.key_pressed(egui::Key::Escape));
    let cancel = mode_changed
        || cancel_key
        || sel
            .active_drag
            .is_some_and(|(owner, paint)| owner != sel.owner || paint != sel.paint);
    if cancel {
        sel.toast = Some("Drag cancelled. No clip edit applied.".into());
    }
    let swallow = sel.swallow_release || cancel;
    if !ui.input(|i| i.pointer.primary_down()) {
        sel.swallow_release = false;
        sel.active_drag = None;
    } else if cancel {
        sel.swallow_release = true;
    }
    let h = RULER_H + LANE_H * visible as f32 + 2.0;
    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let grid = Rect::from_min_max(
        pos2(full.min.x + HEAD_W, full.min.y + RULER_H),
        pos2(
            full.max.x.max(full.min.x + HEAD_W + 1.0),
            full.min.y + RULER_H + LANE_H * visible as f32,
        ),
    );
    // mouse wheel over the grid: Ctrl zooms around the pointer, Shift (or a
    // sideways wheel) scrolls bars, plain wheel scrolls lanes when they overflow
    let mut view = sel.view.clamped(total_bars, lanes);
    if let Some(hp) = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|hp| grid.contains(*hp))
    {
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 && zoom.is_finite() {
            let old_px = grid_scale(grid.width(), total_bars, view.zoom);
            let anchor = view.scroll_bar + (hp.x - grid.min.x) / old_px;
            view.zoom = (view.zoom * zoom).clamp(ZOOM_MIN, ZOOM_MAX);
            let new_px = grid_scale(grid.width(), total_bars, view.zoom);
            view.scroll_bar = anchor - (hp.x - grid.min.x) / new_px;
        }
        let d = ui.input(|i| i.smooth_scroll_delta);
        let px = grid_scale(grid.width(), total_bars, view.zoom);
        let mut used = egui::Vec2::ZERO;
        if d.x != 0.0 {
            view.scroll_bar -= d.x / px;
            used.x = d.x;
        }
        if d.y != 0.0 && lanes > visible {
            view.scroll_lane -= d.y / LANE_H;
            used.y = d.y;
        }
        if used != egui::Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta -= used);
        }
        view = view.clamped(total_bars, lanes);
    }
    sel.view = view;
    let geo = GridGeo {
        grid,
        bar_px: grid_scale(grid.width(), total_bars, view.zoom),
        scroll_bar: view.scroll_bar,
        scroll_lane: view.scroll_lane,
    };
    sel.geo = Some(geo);
    let bar_px = geo.bar_px;
    let pt = ui.painter_at(full);
    let gp = ui.painter_at(grid);
    let ruler = ui.painter_at(Rect::from_min_max(
        pos2(grid.min.x, full.min.y),
        pos2(full.max.x, grid.min.y),
    ));
    let heads = ui.painter_at(Rect::from_min_max(
        pos2(full.min.x, grid.min.y),
        pos2(grid.min.x, grid.max.y),
    ));
    pt.rect_filled(full, 4.0, super::theme::Tokens::DARK.playlist_bg);
    // ruler + bar grid (only the bars in view)
    pt.rect_filled(
        Rect::from_min_max(pos2(full.min.x, full.min.y), pos2(full.max.x, grid.min.y)),
        0.0,
        super::theme::Tokens::DARK.ruler_bg,
    );
    pt.text(
        pos2(full.min.x + 8.0, full.min.y + RULER_H * 0.5),
        Align2::LEFT_CENTER,
        "PLAYLIST",
        FontId::proportional(10.0),
        DIM,
    );
    let b0 = geo.bar_at(grid.min.x).floor().max(0.0) as u32;
    let b1 = geo.bar_at(grid.max.x).ceil().max(0.0) as u32;
    let label_every = if bar_px >= 12.0 {
        4
    } else if bar_px >= 6.0 {
        8
    } else {
        16
    };
    let stride = ((b1.saturating_sub(b0)) / 1024).max(1);
    for b in (b0..=b1).step_by(stride as usize) {
        let x = geo.x_of(b as f32);
        let major = b % 4 == 0;
        gp.line_segment(
            [pos2(x, grid.min.y), pos2(x, grid.max.y)],
            Stroke::new(
                1.0_f32,
                if major {
                    super::theme::Tokens::DARK.bar_line
                } else {
                    super::theme::Tokens::DARK.grid_line
                },
            ),
        );
        if b % label_every == 0 {
            ruler.text(
                pos2(x + 3.0, full.min.y + RULER_H * 0.5),
                Align2::LEFT_CENTER,
                (b + 1).to_string(),
                FontId::monospace(9.5),
                DIM,
            );
        }
    }
    // lanes in view
    let l0 = view.scroll_lane.floor().max(0.0) as usize;
    let l1 = ((view.scroll_lane + visible as f32).ceil() as usize).min(lanes);
    for l in l0..l1 {
        let y = geo.y_of(l as f32);
        let head = Rect::from_min_size(pos2(full.min.x, y), vec2(HEAD_W - 2.0, LANE_H - 1.0));
        heads.rect_filled(
            head,
            0.0,
            if l % 2 == 0 {
                super::theme::Tokens::DARK.playlist_alt
            } else {
                super::theme::Tokens::DARK.playlist_bg
            },
        );
        heads.text(
            head.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            format!("Lane {}", l + 1),
            FontId::proportional(11.0),
            TEXT,
        );
        gp.line_segment(
            [pos2(grid.min.x, y + LANE_H), pos2(grid.max.x, y + LANE_H)],
            Stroke::new(1.0_f32, super::theme::Tokens::DARK.border),
        );
        heads.line_segment(
            [pos2(full.min.x, y + LANE_H), pos2(grid.min.x, y + LANE_H)],
            Stroke::new(1.0_f32, super::theme::Tokens::DARK.border),
        );
        // paint: a click on an empty cell drops the selected pattern
        let lane_r =
            Rect::from_min_max(pos2(grid.min.x, y), pos2(grid.max.x, y + LANE_H)).intersect(grid);
        if !lane_r.is_positive() {
            continue;
        }
        let resp = ui.interact(lane_r, ui.id().with(("flpl_lane", l)), Sense::click());
        if resp.clicked() && !swallow {
            if let Some(pos) = resp.interact_pointer_pos() {
                let bar = geo.bar_at(pos.x).floor().max(0.0) as u32;
                let lane = (geo.lane_at(pos.y).floor().max(0.0) as usize).min(lanes - 1) as u32;
                if p.playlist.iter().any(|c| {
                    (c.lane == lane
                        && bar >= c.start_bar
                        && bar < c.start_bar.saturating_add(c.bars.max(1)))
                        || Rect::from_min_size(
                            pos2(
                                geo.x_of(c.start_bar as f32) + 1.0,
                                geo.y_of(c.lane as f32) + 2.0,
                            ),
                            vec2((c.bars.max(1) as f32 * bar_px - 2.0).max(1.0), LANE_H - 4.0),
                        )
                        .contains(pos)
                }) {
                    continue;
                }
                if !sel.paint {
                    if p.playlist
                        .iter()
                        .any(|c| Some(&c.id) == sel.clip_sel.as_ref())
                    {
                        sel.clip_sel = None;
                    }
                    continue;
                }
                match p
                    .patterns
                    .get(sel.pattern)
                    .map(|pt| pt.name.clone())
                    .filter(|n| !n.starts_with("pl:"))
                {
                    Some(name) => {
                        let bars = p.patterns[sel.pattern].bars.max(1);
                        if bar
                            .checked_add(bars)
                            .is_some_and(|end| end <= EDIT_BAR_LIMIT)
                        {
                            actions.extend(paint_calls(p, &name, bar, lane));
                        } else {
                            sel.toast = Some("Placement exceeds the grid range.".into());
                        }
                    }
                    None => {
                        sel.toast = Some(
                            "Select a source pattern to paint; section copies cannot be placed"
                                .into(),
                        )
                    }
                }
            }
        }
    }
    // clips
    for c in &p.playlist {
        let id = ui.id().with(("flpl_clip", c.id.as_str()));
        let origin = ui.data(|d| d.get_temp::<GridDrag>(id));
        let stale_drag = origin
            .as_ref()
            .is_some_and(|g| g.owner != sel.owner || g.paint != sel.paint);
        let blocked = swallow || stale_drag;
        if blocked && origin.is_some() {
            ui.data_mut(|d| d.remove::<GridDrag>(id));
            sel.swallow_release = ui.input(|i| i.pointer.primary_down());
            sel.toast = Some("Drag cancelled. No clip edit applied.".into());
        }
        let grab = origin
            .filter(|_| !blocked)
            .map(|g| (Grab::from_code(g.kind), vec2(g.x, g.y)));
        let (start, lane, len) = dragged_clip(c.start_bar, c.lane, c.bars, grab, bar_px, lanes);
        let (start, lane, len) = (start as f32, lane as f32, len as f32);
        let r = Rect::from_min_size(
            pos2(geo.x_of(start) + 1.0, geo.y_of(lane) + 2.0),
            vec2((len * bar_px - 2.0).max(1.0), LANE_H - 4.0),
        );
        let vr = r.intersect(grid);
        if !vr.is_positive() && grab.is_none() {
            continue; // scrolled out of view: not drawn, not hit
        }
        let col = pattern_color(&c.pattern);
        let is_sel = sel.clip_sel.as_deref() == Some(c.id.as_str()) && !c.id.is_empty();
        gp.rect_filled(r, 3.0, col.gamma_multiply(0.38));
        let title = Rect::from_min_size(r.min, vec2(r.width(), 10.0));
        gp.rect_filled(title, 3.0, col);
        if vr.is_positive() {
            gp.with_clip_rect(vr).text(
                pos2(vr.min.x.max(r.min.x) + 4.0, title.center().y),
                Align2::LEFT_CENTER,
                friendly_name(&c.pattern),
                FontId::proportional(8.5),
                Color32::from_rgb(20, 20, 24),
            );
        }
        // the pattern's notes, tiny, so a clip reads as its music
        let preview = grid
            .intersect(ui.clip_rect())
            .intersect(ui.ctx().screen_rect());
        if let Some(pi) = p
            .pattern_index(&c.pattern)
            .ok()
            .filter(|_| r.intersect(preview).is_positive())
        {
            let pat = &p.patterns[pi];
            let steps = pat.steps().max(1) as f32;
            let body = Rect::from_min_max(
                pos2(r.min.x, title.max.y + 1.0),
                pos2(r.max.x, r.max.y - 1.0),
            );
            let source_bars =
                f64::from(pat.steps().max(1)) / f64::from(crate::project::STEPS_PER_BAR);
            let per_rep = source_bars as f32 * bar_px;
            let left = f64::from(geo.bar_at(preview.min.x)) - f64::from(start);
            let reps: Vec<_> = preview_repeats(
                left,
                left + f64::from(preview.width() / bar_px),
                f64::from(len),
                source_bars,
                c.offset_bars,
                preview.width().ceil() as usize,
            )
            .collect();
            // At most 16,384 note visits per clip, regardless of song extent.
            let note_budget = (16384 / (8 * reps.len().max(1))).min(256);
            #[cfg(test)]
            let mut preview_visits = 0usize;
            for (ti, (_, notes)) in pat.clips.iter().enumerate().take(8) {
                let yy = body.min.y + 1.0 + (ti as f32 * 1.6) % (body.height() - 2.0).max(1.0);
                for &bar in &reps {
                    for n in notes.iter().take(note_budget) {
                        #[cfg(test)]
                        {
                            preview_visits += 1;
                        }
                        let x0 = geo.x_of((f64::from(start) + bar) as f32)
                            + 1.0
                            + n.start / steps * per_rep;
                        if x0 < r.min.x
                            || x0 >= r.max.x - 1.0
                            || x0 < grid.min.x - 2.0
                            || x0 > grid.max.x
                        {
                            continue;
                        }
                        gp.line_segment(
                            [pos2(x0, yy), pos2((x0 + 1.5).min(r.max.x), yy)],
                            Stroke::new(1.0_f32, col.gamma_multiply(0.95)),
                        );
                    }
                }
            }
            #[cfg(test)]
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("preview-note-visits"), preview_visits));
        }
        gp.rect_stroke(
            r,
            3.0,
            Stroke::new(
                if is_sel { 1.5_f32 } else { 1.0 },
                if is_sel {
                    Color32::WHITE
                } else {
                    col.gamma_multiply(0.7)
                },
            ),
        );
        // edge grips: right edge resizes, left edge trims the start
        let ew = EDGE_PX.min(r.width() / 3.0).max(2.0);
        gp.line_segment(
            [
                pos2(r.max.x - 3.0, r.min.y + 12.0),
                pos2(r.max.x - 3.0, r.max.y - 3.0),
            ],
            Stroke::new(1.0_f32, Color32::from_white_alpha(90)),
        );
        gp.line_segment(
            [
                pos2(r.min.x + 3.0, r.min.y + 12.0),
                pos2(r.min.x + 3.0, r.max.y - 3.0),
            ],
            Stroke::new(1.0_f32, Color32::from_white_alpha(50)),
        );
        let edge_full = Rect::from_min_max(pos2(r.max.x - ew, r.min.y), r.max);
        let left_full = Rect::from_min_max(r.min, pos2(r.min.x + ew, r.max.y));
        // hit rects stay inside the grid (a clip scrolled half out of view is
        // only hit where it shows), except while it is being dragged
        let hit = |rr: Rect| {
            if grab.is_some() {
                rr
            } else {
                rr.intersect(grid)
            }
        };
        let precise = r.width() >= EDGE_PX * 4.0;
        let body_resp = ui.interact(
            hit(r),
            id.with("body"),
            if precise {
                Sense::click_and_drag()
            } else {
                Sense::click()
            },
        );
        let edge_sense = if precise {
            Sense::drag()
        } else {
            Sense::hover()
        };
        let left_resp = ui
            .interact(hit(left_full), id.with("left"), edge_sense)
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let edge_resp = ui
            .interact(hit(edge_full), id.with("edge"), edge_sense)
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let busy = |r: &egui::Response| r.drag_started() || r.dragged() || r.drag_stopped();
        let (resp, kind) = if busy(&edge_resp) {
            (&edge_resp, Grab::Resize)
        } else if busy(&left_resp) {
            (&left_resp, Grab::ResizeStart)
        } else {
            (&body_resp, Grab::Move)
        };
        if resp.drag_started() && !blocked {
            sel.active_drag = Some((sel.owner, sel.paint));
            ui.data_mut(|d| {
                d.insert_temp(
                    id,
                    GridDrag {
                        kind: kind.code(),
                        x: 0.0,
                        y: 0.0,
                        owner: sel.owner,
                        paint: sel.paint,
                    },
                )
            });
        }
        if resp.dragged() && !blocked {
            let delta = resp.drag_delta();
            if let Some(mut cur) = ui.data(|m| m.get_temp::<GridDrag>(id)) {
                cur.x += delta.x;
                cur.y += delta.y;
                ui.data_mut(|m| m.insert_temp(id, cur));
            }
        }
        if resp.drag_stopped() && !blocked && grab.is_some() {
            ui.data_mut(|m| m.remove::<GridDrag>(id));
            let (s, l, n) = (start as u32, lane as u32, len as u32);
            match kind {
                Grab::Resize if n != c.bars.max(1) => actions.push((
                    "playlist_clip",
                    json!({"action": "trim", "id": c.id, "end_bar": s as i64 + n as i64}),
                )),
                Grab::ResizeStart if s != c.start_bar => actions.push((
                    "playlist_clip",
                    json!({"action": "trim", "id": c.id, "start_bar": s as i64}),
                )),
                Grab::Move if s != c.start_bar || l != c.lane => actions.push((
                    "playlist_clip",
                    json!({"action": "move", "id": c.id, "to_bar": s as i64, "lane": l as i64}),
                )),
                _ => {}
            }
        }
        if body_resp.clicked() && !blocked {
            sel.clip_sel = Some(c.id.clone());
            let preview = grid
                .intersect(ui.clip_rect())
                .intersect(ui.ctx().screen_rect());
            if let Some(pi) = p
                .pattern_index(&c.pattern)
                .ok()
                .filter(|_| r.intersect(preview).is_positive())
            {
                sel.pattern = pi;
            }
        }
        let menu = super::playlist::pattern_clip_menu(c, bar_now);
        body_resp
            .on_hover_text(format!(
                "{} · grid bars {} to {} · zoom in or use exact fields for narrow clips; right-click for more",
                friendly_name(&c.pattern),
                u64::from(c.start_bar) + 1,
                u64::from(c.start_bar) + u64::from(c.bars.max(1))
            ))
            .context_menu(|ui| {
                for (label, tool, args) in menu.clone() {
                    if ui.button(label).clicked() && !blocked {
                        actions.push((tool, args));
                        ui.close_menu();
                    }
                }
            });
    }
    // playhead
    let x = geo.x_of(bar_now);
    if bar_now > 0.0 && x >= grid.min.x && x <= grid.max.x {
        pt.line_segment(
            [pos2(x, full.min.y), pos2(x, full.max.y)],
            Stroke::new(1.5_f32, Color32::from_rgb(120, 220, 140)),
        );
    }
    let overlaps = p.playlist.iter().enumerate().any(|(i, a)| {
        p.playlist[i + 1..].iter().any(|b| {
            a.start_bar < b.start_bar.saturating_add(b.bars.max(1))
                && b.start_bar < a.start_bar.saturating_add(a.bars.max(1))
        })
    });
    if overlaps {
        pt.text(
            grid.left_bottom() + vec2(6.0, -4.0),
            Align2::LEFT_BOTTOM,
            "Blocks that share the same bars play together, on any lane.",
            FontId::proportional(11.0),
            DIM,
        );
    }
    if p.playlist.is_empty() {
        pt.text(
            grid.center(),
            Align2::CENTER_CENTER,
            "Choose Paint to place clips; Select never adds music",
            FontId::proportional(12.0),
            DIM,
        );
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn drag_snaps_to_bars_and_lanes() {
        assert_eq!(snap_drag(31.0, 0.0, 20.0), (2, 0));
        assert_eq!(snap_drag(-9.0, 30.0, 20.0), (0, 1));
        assert_eq!(snap_drag(-11.0, -15.0, 20.0), (-1, -1));
    }

    #[test]
    fn colors_are_stable_and_distinct() {
        assert_eq!(pattern_color("verse"), pattern_color("verse"));
        assert_ne!(pattern_color("verse"), pattern_color("hook"));
    }

    #[test]
    fn painting_on_a_section_song_converts_then_places() {
        let d = std::env::temp_dir().join(format!("flpl_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_pattern", &json!({"name": "verse", "bars": 2}))
            .unwrap();
        let first = "verse".to_string();
        e.call(
            "set_arrangement",
            &json!({"sections": [{"pattern": first, "repeats": 2}]}),
        )
        .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let calls = paint_calls(&e.project, &first, 6, 1);
        if !e.project.arrangement.is_empty() {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "batch");
        }
        for (t, a) in calls {
            e.call(t, &a).unwrap();
        }
        assert!(
            e.project
                .playlist
                .iter()
                .any(|c| c.start_bar == 6 && c.lane == 1),
            "{:?}",
            e.project
                .playlist
                .iter()
                .map(|c| (c.start_bar, c.lane))
                .collect::<Vec<_>>()
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }

    #[test]
    fn drags_and_trims_are_clamped() {
        let lanes = 4;
        let m = |dx: f32, dy: f32| Some((Grab::Move, vec2(dx, dy)));
        // no negative bars: a drag far left stops at bar 0
        assert_eq!(
            dragged_clip(2, 1, 3, m(-500.0, 0.0), 20.0, lanes),
            (0, 1, 3)
        );
        // no lane past the last track, none above the first
        assert_eq!(
            dragged_clip(2, 1, 3, m(0.0, 10.0 * LANE_H), 20.0, lanes),
            (2, 3, 3)
        );
        assert_eq!(
            dragged_clip(2, 1, 3, m(0.0, -10.0 * LANE_H), 20.0, lanes),
            (2, 0, 3)
        );
        // right edge: never shorter than one step (one bar)
        assert_eq!(
            dragged_clip(
                2,
                1,
                3,
                Some((Grab::Resize, vec2(-500.0, 0.0))),
                20.0,
                lanes
            ),
            (2, 1, 1)
        );
        assert_eq!(
            dragged_clip(2, 1, 3, Some((Grab::Resize, vec2(41.0, 0.0))), 20.0, lanes),
            (2, 1, 5)
        );
        // left edge: never before bar 0, never past the end minus one step
        assert_eq!(
            dragged_clip(
                2,
                1,
                3,
                Some((Grab::ResizeStart, vec2(-500.0, 0.0))),
                20.0,
                lanes
            ),
            (0, 1, 5)
        );
        assert_eq!(
            dragged_clip(
                2,
                1,
                3,
                Some((Grab::ResizeStart, vec2(500.0, 0.0))),
                20.0,
                lanes
            ),
            (4, 1, 1)
        );
        assert_eq!(
            dragged_clip(
                2,
                1,
                3,
                Some((Grab::ResizeStart, vec2(21.0, 0.0))),
                20.0,
                lanes
            ),
            (3, 1, 2)
        );
        // a zero-length clip counts as one bar
        assert_eq!(dragged_clip(2, 1, 0, None, 20.0, lanes), (2, 1, 1));
    }

    #[test]
    fn the_view_stays_inside_the_song_and_geometry_round_trips() {
        let v = PlaylistView {
            scroll_bar: -3.0,
            scroll_lane: 99.0,
            zoom: ZOOM_MAX * 2.0,
        }
        .clamped(32, 11);
        assert_eq!(
            v,
            PlaylistView {
                scroll_bar: 0.0,
                scroll_lane: 3.0,
                zoom: ZOOM_MAX
            }
        );
        let v = PlaylistView {
            scroll_bar: 50.0,
            scroll_lane: 2.0,
            zoom: 0.0,
        }
        .clamped(32, 4);
        assert_eq!(
            v,
            PlaylistView {
                scroll_bar: 32.0,
                scroll_lane: 0.0,
                zoom: ZOOM_MIN
            }
        );
        let g = GridGeo {
            grid: Rect::from_min_size(pos2(100.0, 20.0), vec2(800.0, 224.0)),
            bar_px: 55.5,
            scroll_bar: 3.25,
            scroll_lane: 1.5,
        };
        for (b, l) in [(3.25_f32, 1.5_f32), (7.0, 2.0), (12.6, 6.25)] {
            assert!(
                (g.bar_at(g.x_of(b)) - b).abs() < 1e-4 && (g.lane_at(g.y_of(l)) - l).abs() < 1e-4
            );
        }
        assert_eq!(g.x_of(3.25), 100.0);
        assert_eq!(g.y_of(1.5), 20.0);
    }

    /// Drive the grid with real egui pointer events (press, moves, release)
    /// and return every engine call it asked for, plus the final view. `at`
    /// maps a song (bar, lane) to the point the grid shows it at, using the
    /// geometry the grid itself reported (so through its scroll and zoom).
    fn drive_view(
        p: &Project,
        ppp: f32,
        view: PlaylistView,
        gesture: &dyn Fn(&dyn Fn(f32, f32) -> egui::Pos2) -> Vec<Vec<egui::Event>>,
    ) -> (Vec<(&'static str, Value)>, GridSel) {
        drive_mode(p, ppp, view, false, gesture)
    }

    fn drive_mode(
        p: &Project,
        ppp: f32,
        view: PlaylistView,
        paint: bool,
        gesture: &dyn Fn(&dyn Fn(f32, f32) -> egui::Pos2) -> Vec<Vec<egui::Event>>,
    ) -> (Vec<(&'static str, Value)>, GridSel) {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let mut sel = GridSel {
            view,
            paint,
            ..Default::default()
        };
        let mut out = Vec::new();
        // egui input is in points; the windowing layer has already divided
        // physical pixels by pixels_per_point (HiDPI) before it gets here
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 700.0));
        let frame = |events: Vec<egui::Event>,
                     k: usize,
                     out: &mut Vec<(&'static str, Value)>,
                     sel: &mut GridSel| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out.extend(playlist_grid(ui, p, sel, 0.0));
                });
            });
        };
        frame(vec![], 0, &mut out, &mut sel);
        frame(vec![], 1, &mut out, &mut sel);
        let geo = sel.geo.unwrap();
        let at = move |bar: f32, lane: f32| pos2(geo.x_of(bar), geo.y_of(lane) + LANE_H * 0.5);
        for (k, ev) in gesture(&at).into_iter().enumerate() {
            frame(ev, k + 2, &mut out, &mut sel);
        }
        (out, sel)
    }

    fn drive(
        p: &Project,
        ppp: f32,
        gesture: &dyn Fn(&dyn Fn(f32, f32) -> egui::Pos2) -> Vec<Vec<egui::Event>>,
    ) -> Vec<(&'static str, Value)> {
        drive_view(p, ppp, PlaylistView::default(), gesture).0
    }

    fn drag(from: egui::Pos2, to: egui::Pos2) -> Vec<Vec<egui::Event>> {
        use egui::{Event, Modifiers, PointerButton};
        let mut f = vec![
            vec![Event::PointerMoved(from)],
            vec![Event::PointerButton {
                pos: from,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        ];
        for i in 1..=8 {
            f.push(vec![Event::PointerMoved(
                from + (to - from) * (i as f32 / 8.0),
            )]);
        }
        f.push(vec![Event::PointerButton {
            pos: to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        }]);
        f.push(vec![]);
        f
    }

    fn one_clip_project() -> Project {
        let d = std::env::temp_dir().join(format!("flpl_ptr_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_pattern", &json!({"name": "verse", "bars": 2}))
            .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        e.project.clone()
    }

    #[test]
    fn a_real_pointer_drags_resizes_and_paints_clips() {
        let p = one_clip_project();
        let clip = p.playlist[0].clone();
        assert_eq!((clip.start_bar, clip.lane, clip.bars.max(1)), (0, 0, 2));
        for ppp in [1.0_f32, 2.0] {
            // grab the clip's body and drag it 3 bars right and one lane down
            let moved = drive(&p, ppp, &|at| drag(at(0.8, 0.0), at(3.8, 1.0)));
            assert!(
                moved.iter().any(|(t, a)| *t == "playlist_clip"
                    && a["action"] == "move"
                    && a["to_bar"] == 3
                    && a["lane"] == 1),
                "ppp {ppp}: {moved:?}"
            );
            // grab the right edge and pull it 2 bars longer
            let resized = drive(&p, ppp, &|at| {
                let e = at(2.0, 0.0) - egui::vec2(3.0, 0.0);
                drag(e, e + egui::vec2(at(4.0, 0.0).x - at(2.0, 0.0).x, 0.0))
            });
            assert!(
                resized.iter().any(|(t, a)| *t == "playlist_clip"
                    && a["action"] == "trim"
                    && a["end_bar"] == 4),
                "ppp {ppp}: {resized:?}"
            );
            // click an empty cell: the selected pattern is painted there
            let painted = drive_mode(&p, ppp, PlaylistView::default(), true, &|at| {
                let c = at(10.5, 1.0);
                drag(c, c)
            });
            assert!(
                painted
                    .0
                    .iter()
                    .any(|(t, a)| *t == "place_pattern" && a["start_bar"] == 10 && a["lane"] == 1),
                "ppp {ppp}: {painted:?}"
            );
        }
    }

    fn two_lane_project() -> Project {
        // a verse at bars 4..6 on lane 2 and one at bars 0..2 on lane 9:
        // 11 lanes (more than the 8 shown), so lanes scroll too
        let d = std::env::temp_dir().join(format!("flpl_view_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_pattern", &json!({"name": "verse", "bars": 2}))
            .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 4, "lane": 2}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 9}),
        )
        .unwrap();
        e.project.clone()
    }

    fn find<'a>(calls: &'a [(&'static str, Value)], action: &str) -> Vec<&'a Value> {
        calls
            .iter()
            .filter(|(t, a)| *t == "playlist_clip" && a["action"] == action)
            .map(|(_, a)| a)
            .collect()
    }

    #[test]
    fn a_real_pointer_hits_the_right_clip_when_scrolled_and_zoomed() {
        let p = two_lane_project();
        let id = p.playlist.iter().find(|c| c.lane == 2).unwrap().id.clone();
        let views = [
            PlaylistView {
                scroll_bar: 3.0,
                scroll_lane: 1.0,
                zoom: 2.0,
            },
            PlaylistView {
                scroll_bar: 3.0,
                scroll_lane: 1.0,
                zoom: 0.5,
            },
            PlaylistView {
                scroll_bar: 2.5,
                scroll_lane: 2.0,
                zoom: 3.0,
            },
        ];
        for ppp in [1.0_f32, 2.0] {
            for v in views {
                let tag = format!("ppp {ppp} view {v:?}");
                // the view survives a frame unchanged (it is inside the song)
                let (_, s) = drive_view(&p, ppp, v, &|_| vec![]);
                assert_eq!(s.view, v, "{tag}");
                // move: 3 bars right, 1 lane down, grabbed where the scrolled, zoomed grid shows it
                let (c, _) = drive_view(&p, ppp, v, &|at| drag(at(4.8, 2.0), at(7.8, 3.0)));
                let mv = find(&c, "move");
                assert!(
                    mv.len() == 1
                        && mv[0]["id"] == id.as_str()
                        && mv[0]["to_bar"] == 7
                        && mv[0]["lane"] == 3,
                    "{tag}: {c:?}"
                );
                // right edge: 2 bars longer
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let e = at(6.0, 2.0) - vec2(2.0, 0.0);
                    drag(e, e + vec2(at(8.0, 2.0).x - at(6.0, 2.0).x, 0.0))
                });
                let tr = find(&c, "trim");
                assert!(tr.len() == 1 && tr[0]["end_bar"] == 8, "{tag}: {c:?}");
                // left edge: start one bar earlier (end kept)
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let e = at(4.0, 2.0) + vec2(2.0, 0.0);
                    drag(e, e - vec2(at(5.0, 2.0).x - at(4.0, 2.0).x, 0.0))
                });
                let tr = find(&c, "trim");
                assert!(
                    tr.len() == 1 && tr[0]["start_bar"] == 3 && tr[0].get("end_bar").is_none(),
                    "{tag}: {c:?}"
                );
                // clamps under scroll/zoom: far left -> bar 0, far down -> last track (lane 10)
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let a = at(4.8, 2.0);
                    drag(a, pos2(-400.0, a.y))
                });
                let mv = find(&c, "move");
                assert!(
                    mv.len() == 1 && mv[0]["to_bar"] == 0 && mv[0]["lane"] == 2,
                    "{tag}: {c:?}"
                );
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let a = at(4.8, 2.0);
                    drag(a, a + vec2(0.0, 20.0 * LANE_H))
                });
                let mv = find(&c, "move");
                assert!(
                    mv.len() == 1 && mv[0]["to_bar"] == 4 && mv[0]["lane"] == 10,
                    "{tag}: {c:?}"
                );
                // left edge dragged far left: trims to bar 0, never negative
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let e = at(4.0, 2.0) + vec2(2.0, 0.0);
                    drag(e, pos2(-400.0, e.y))
                });
                let tr = find(&c, "trim");
                assert!(tr.len() == 1 && tr[0]["start_bar"] == 0, "{tag}: {c:?}");
                // right edge dragged far left: one bar long, not zero or negative
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let e = at(6.0, 2.0) - vec2(2.0, 0.0);
                    drag(e, pos2(-400.0, e.y))
                });
                let tr = find(&c, "trim");
                assert!(tr.len() == 1 && tr[0]["end_bar"] == 5, "{tag}: {c:?}");
                // paint: an empty cell in view maps through scroll and zoom
                let (c, _) = drive_mode(&p, ppp, v, true, &|at| {
                    // bar 7.5 is in view for every view above (the song shows 16 bars at least, so the most zoomed one shows bars 2.5 to 7.8)
                    let a = at(7.5, 4.0);
                    drag(a, a)
                });
                assert!(
                    c.iter().any(|(t, a)| *t == "place_pattern"
                        && a["start_bar"] == 7
                        && a["lane"] == 4),
                    "{tag}: {c:?}"
                );
            }
        }
    }

    #[test]
    fn hidden_clips_are_not_hit() {
        // the lane-9 clip (bars 0..2) is scrolled out of view; a click where
        // it would be without scroll paints instead of grabbing it
        let p = two_lane_project();
        for ppp in [1.0_f32, 2.0] {
            let v = PlaylistView {
                scroll_bar: 3.0,
                scroll_lane: 2.0,
                zoom: 2.0,
            };
            let (c, s) = drive_view(&p, ppp, v, &|at| {
                let a = at(3.5, 9.0);
                drag(a, a + vec2(80.0, 0.0))
            });
            assert!(find(&c, "move").is_empty(), "ppp {ppp}: {c:?}");
            let g = s.geo.unwrap();
            assert!(
                g.y_of(9.0) + LANE_H * 0.5 < g.grid.max.y,
                "lane 9 is in view at scroll_lane 2"
            );
        }
    }

    #[test]
    fn the_wheel_scrolls_and_ctrl_wheel_zooms_around_the_pointer() {
        use egui::{Event, Modifiers, MouseWheelUnit};
        let p = two_lane_project();
        for ppp in [1.0_f32, 2.0] {
            let wheel = |pos: egui::Pos2, dy: f32, m: Modifiers| {
                let mut f = vec![
                    vec![Event::PointerMoved(pos)],
                    vec![Event::MouseWheel {
                        unit: MouseWheelUnit::Line,
                        delta: vec2(0.0, dy),
                        modifiers: m,
                    }],
                ];
                f.extend((0..40).map(|_| vec![Event::PointerMoved(pos)]));
                f
            };
            // Ctrl+wheel up: zoom in, the bar under the pointer stays put
            let v0 = PlaylistView {
                scroll_bar: 2.0,
                scroll_lane: 0.0,
                zoom: 1.0,
            };
            let anchor = std::cell::Cell::new(0.0_f32);
            let (_, s) = drive_view(&p, ppp, v0, &|at| {
                let a = at(9.0, 1.0);
                anchor.set(a.x);
                wheel(a, 1.0, Modifiers::CTRL)
            });
            let g = s.geo.unwrap();
            assert!(s.view.zoom > 1.05, "ppp {ppp}: {:?}", s.view);
            assert!(
                (g.bar_at(anchor.get()) - 9.0).abs() < 0.05,
                "ppp {ppp}: bar under pointer {} ({:?})",
                g.bar_at(anchor.get()),
                s.view
            );
            // Shift+wheel down: scrolls later bars into view
            let (_, s) = drive_view(&p, ppp, v0, &|at| {
                wheel(at(9.0, 1.0), -1.0, Modifiers::SHIFT)
            });
            assert!(
                s.view.scroll_bar > 2.5 && s.view.zoom == 1.0,
                "ppp {ppp}: {:?}",
                s.view
            );
            // plain wheel down: lanes scroll (11 lanes, 8 shown), bars don't
            let (_, s) = drive_view(&p, ppp, v0, &|at| {
                wheel(at(9.0, 1.0), -1.0, Modifiers::NONE)
            });
            assert!(
                s.view.scroll_lane > 0.5 && s.view.scroll_lane <= 3.0 && s.view.scroll_bar == 2.0,
                "ppp {ppp}: {:?}",
                s.view
            );
        }
    }

    #[derive(Clone, Debug)]
    struct Target {
        name: String,
        response: egui::Response,
        visible: Rect,
    }
    const TARGETS: &str = "playlist-grid-test-responses";
    pub(super) fn record_target(ui: &egui::Ui, name: &str, response: &egui::Response) {
        let target = Target {
            name: name.into(),
            response: response.clone(),
            visible: if ui.is_visible() {
                response
                    .interact_rect
                    .intersect(ui.clip_rect())
                    .intersect(ui.ctx().screen_rect())
            } else {
                Rect::NOTHING
            },
        };
        ui.ctx().data_mut(|d| {
            let key = egui::Id::new(TARGETS);
            let mut targets = d.get_temp::<Vec<Target>>(key).unwrap_or_default();
            targets.push(target);
            d.insert_temp(key, targets);
        });
    }
    /// Tests below use real egui events. They are authored here for the existing
    /// adequate GUI test runner; local extracted probes do not execute them.
    struct EventGrid {
        ctx: egui::Context,
        p: Project,
        sel: GridSel,
        calls: Vec<(&'static str, Value)>,
        targets: Vec<Target>,
        shapes: Vec<egui::epaint::ClippedShape>,
        screen: Rect,
        tick: usize,
        bar_now: f32,
    }

    impl EventGrid {
        fn new(p: Project, ppp: f32, size: egui::Vec2) -> Self {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let mut r = Self {
                ctx,
                p,
                sel: GridSel {
                    song_navigation: true,
                    ..Default::default()
                },
                calls: Vec::new(),
                targets: Vec::new(),
                shapes: Vec::new(),
                screen: Rect::from_min_size(pos2(0.0, 0.0), size),
                tick: 0,
                bar_now: 0.0,
            };
            r.frame(vec![]);
            r.frame(vec![]);
            r
        }
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.tick += 1;
            self.ctx
                .data_mut(|d| d.insert_temp(egui::Id::new("preview-note-visits"), 0usize));
            self.ctx
                .data_mut(|d| d.insert_temp(egui::Id::new(TARGETS), Vec::<Target>::new()));
            let out = self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(self.screen),
                    events,
                    time: Some(self.tick as f64 * 0.05),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        self.calls
                            .extend(playlist_grid(ui, &self.p, &mut self.sel, self.bar_now));
                    });
                },
            );
            self.shapes = out.shapes;
            self.targets = self.ctx.data_mut(|d| {
                d.get_temp::<Vec<Target>>(egui::Id::new(TARGETS))
                    .unwrap_or_default()
            });
        }
        fn visible_target(&self, name: &str) -> Result<&Target, String> {
            let matches: Vec<_> = self.targets.iter().filter(|t| t.name == name).collect();
            if matches.len() != 1 {
                return Err(format!(
                    "Expected unique control {name}, got {}",
                    matches.len()
                ));
            }
            let t = matches[0];
            if !t.visible.is_positive() || !t.visible.contains_rect(t.response.rect) {
                return Err(format!("Control {name} is hidden or clipped"));
            }
            Ok(t)
        }
        fn target(&self, name: &str) -> Result<&Target, String> {
            let t = self.visible_target(name)?;
            if !t.response.enabled() {
                return Err(format!("Control {name} is disabled"));
            }
            Ok(t)
        }
        fn disabled_point(&self, name: &str) -> egui::Pos2 {
            let t = self.visible_target(name).unwrap();
            assert!(!t.response.enabled(), "{name} must really be disabled");
            assert!(self.target(name).is_err());
            t.visible.center()
        }
        fn point(&self, name: &str) -> egui::Pos2 {
            self.target(name)
                .unwrap_or_else(|e| panic!("{e}"))
                .visible
                .center()
        }
        fn press(&mut self, at: egui::Pos2, down: bool) {
            self.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: down,
                    modifiers: egui::Modifiers::default(),
                },
            ]);
        }
        fn click(&mut self, at: egui::Pos2) {
            self.press(at, true);
            self.press(at, false);
            self.frame(vec![]);
        }
        fn button(&mut self, label: &str) {
            let at = self.point(label);
            self.click(at);
        }
        fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                }]);
            }
        }
        fn focus_number(&mut self, name: &str) -> egui::Id {
            let id = self.target(name).unwrap().response.id;
            let at = self.point(name);
            self.click(at);
            self.click(at);
            assert!(
                self.ctx.memory(|m| m.has_focus(id)),
                "Numeric response {name} must own focus before typing"
            );
            self.frame(vec![]);
            assert!(self.ctx.wants_keyboard_input());
            id
        }
        fn pending_number(&mut self, name: &str, value: &str) -> egui::Id {
            let id = self.focus_number(name);
            self.key(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            );
            self.frame(vec![egui::Event::Text(value.to_owned())]);
            assert!(self.ctx.memory(|m| m.has_focus(id)));
            assert_eq!(
                self.ctx.data_mut(|d| d.get_temp::<String>(id)).as_deref(),
                Some(value)
            );
            id
        }
        fn edit_number(&mut self, label: &str, value: &str) {
            self.pending_number(label, value);
            self.key(egui::Key::Enter, egui::Modifiers::default());
            assert!(self.calls.is_empty(), "Typing must not dispatch a mutation");
        }
    }

    fn test_engine(tag: &str) -> Engine {
        let mut e = Engine::new(
            std::env::temp_dir().join(format!("flpl_form_{tag}_{}", std::process::id())),
        );
        e.call("add_track", &json!({"name":"kick", "preset":"kick"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"verse", "bars":2}))
            .unwrap();
        e.call(
            "toggle_step",
            &json!({"pattern":"verse", "track":"kick", "step":0}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern":"verse", "start_bar":2, "lane":0}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern":"verse", "start_bar":8, "lane":1}),
        )
        .unwrap();
        e
    }
    fn owner(e: &Engine) -> GridOwner {
        GridOwner {
            revision: e.revision,
            generation: e.project_load_generation,
        }
    }
    fn project_value(e: &Engine) -> Value {
        serde_json::to_value(&e.project).unwrap()
    }

    #[test]
    fn select_default_clear_preserves_audio_selection_and_paint_is_intentional() {
        let p = one_clip_project();
        for ppp in [1.0, 2.0] {
            for size in [vec2(1280.0, 720.0), vec2(1920.0, 1080.0)] {
                let mut r = EventGrid::new(p.clone(), ppp, size);
                assert!(!r.sel.paint);
                let g = r.sel.geo.unwrap();
                r.click(pos2(g.x_of(0.8), g.y_of(0.0) + 14.0));
                assert_eq!(r.sel.clip_sel.as_deref(), Some(p.playlist[0].id.as_str()));
                assert!(r.calls.is_empty());
                // Selection adds a form, so obtain the current geometry again.
                let g = r.sel.geo.unwrap();
                r.click(pos2(g.x_of(10.5), g.y_of(1.0) + 14.0));
                assert!(r.sel.clip_sel.is_none());
                assert!(r.calls.is_empty());
                r.sel.clip_sel = Some("audio-selection-not-a-pattern-clip".into());
                r.frame(vec![]);
                let g = r.sel.geo.unwrap();
                r.click(pos2(g.x_of(10.5), g.y_of(1.0) + 14.0));
                assert_eq!(
                    r.sel.clip_sel.as_deref(),
                    Some("audio-selection-not-a-pattern-clip")
                );
                r.button("Paint");
                assert!(r.sel.paint);
                assert!(r.calls.is_empty());
                let g = r.sel.geo.unwrap();
                r.click(pos2(g.x_of(10.5), g.y_of(1.0) + 14.0));
                assert_eq!(
                    r.calls,
                    vec![(
                        "place_pattern",
                        json!({"pattern":"verse","start_bar":10,"lane":1})
                    )]
                );
                r.calls.clear();
                r.click(pos2(g.x_of(0.8), g.y_of(0.0) + 14.0));
                assert!(
                    r.calls.is_empty(),
                    "Clip clicks cannot paint behind the clip"
                );
            }
        }
    }

    #[test]
    fn form_buttons_are_exact_single_edits_with_exact_undo_and_redo() {
        for (label, action) in [
            ("Move", "move"),
            ("Set length", "trim"),
            ("Duplicate after", "duplicate"),
            ("Make independent", "make_unique"),
        ] {
            for categorized in [false, true] {
                let mut e = test_engine(action);
                let id = e.project.playlist[0].id.clone();
                e.call(
                    "playlist_clip",
                    &json!({"id":id,"action":"trim","start_bar":3}),
                )
                .unwrap();
                let c = e.project.playlist[0].clone();
                assert_eq!(c.offset_bars, 1);
                let before = project_value(&e);
                let depth = e.undo_depth();
                let rev = e.revision;
                let mut r = EventGrid::new(e.project.clone(), 1.0, vec2(1280.0, 720.0));
                r.sel.owner = owner(&e);
                r.sel.clip_sel = Some(c.id.clone());
                r.frame(vec![]);
                let d = r.sel.draft.as_mut().unwrap();
                d.start = 6;
                d.lane = 4;
                d.length = 5;
                d.copies = 2;
                r.frame(vec![]);
                r.button(label);
                r.frame(vec![]);
                assert_eq!(r.calls.len(), 1, "{label}");
                let (tool, args) = &r.calls[0];
                assert_eq!(*tool, "playlist_clip");
                let expected = match action {
                    "move" => json!({"action":"move", "id":c.id, "to_bar":5, "lane":3}),
                    "trim" => json!({"action":"trim", "id":c.id, "end_bar":8}),
                    "duplicate" => json!({"action":"duplicate", "id":c.id, "count":2}),
                    _ => json!({"action":"make_unique", "id":c.id}),
                };
                assert_eq!(*args, expected);
                let out = if categorized {
                    let mut args = args.clone();
                    args["op"] = args["action"].take();
                    args["action"] = json!("playlist_clip");
                    guarded_grid_call(&mut e, r.sel.owner, "timeline", &args).unwrap()
                } else {
                    guarded_grid_call(&mut e, r.sel.owner, tool, args).unwrap()
                };
                assert_eq!(e.revision, rev + 1);
                assert_eq!(e.undo_depth(), (depth.0 + 1, 0));
                let now = &e.project.playlist[0];
                assert_eq!(now.id, c.id);
                assert_eq!(now.offset_bars, c.offset_bars);
                assert_eq!(
                    e.project.playlist[1], r.p.playlist[1],
                    "Unrelated placement"
                );
                match action {
                    "move" => assert_eq!((now.start_bar, now.lane, now.bars), (5, 3, 1)),
                    "trim" => assert_eq!((now.start_bar, now.bars), (3, 5)),
                    "duplicate" => {
                        let ids = out["new_ids"].as_array().unwrap();
                        assert_eq!(ids.len(), 2);
                        assert_ne!(ids[0], ids[1]);
                        for (k, id) in ids.iter().enumerate() {
                            let copy = e
                                .project
                                .playlist
                                .iter()
                                .find(|p| p.id == id.as_str().unwrap())
                                .unwrap();
                            assert_eq!(copy.pattern, c.pattern);
                            assert_eq!(copy.start_bar, 4 + k as u32);
                        }
                    }
                    _ => {
                        assert_eq!(out["new_pattern"], "verse #2");
                        assert_eq!(now.pattern, "verse #2");
                    }
                }
                let after = project_value(&e);
                e.call("undo", &json!({})).unwrap();
                assert_eq!(project_value(&e), before);
                e.call("redo", &json!({})).unwrap();
                assert_eq!(project_value(&e), after);
            }
        }
    }

    #[test]
    fn independent_source_notes_do_not_change_the_other_placement() {
        let mut e = test_engine("isolation");
        let c = e.project.playlist[0].clone();
        let d = ClipDraft::new(&c, owner(&e));
        let old_source = e.project.patterns[e.project.pattern_index("verse").unwrap()].clone();
        guarded_grid_call(
            &mut e,
            d.owner,
            "playlist_clip",
            &d.payload(&c, "make_unique").unwrap(),
        )
        .unwrap();
        assert_eq!(e.project.playlist[0].id, c.id);
        e.call(
            "toggle_step",
            &json!({"pattern":"verse #2", "track":"kick", "step":4}),
        )
        .unwrap();
        assert_eq!(
            e.project.patterns[e.project.pattern_index("verse").unwrap()],
            old_source
        );
        assert_eq!(e.project.playlist[1].pattern, "verse");
        assert_ne!(
            e.project.patterns[e.project.pattern_index("verse #2").unwrap()].clips,
            old_source.clips
        );
    }

    #[test]
    fn grid_only_drafts_persist_through_navigation_and_numeric_focus_emits_no_grid_calls() {
        let e = test_engine("focus");
        let mut r = EventGrid::new(e.project.clone(), 1.0, vec2(1280.0, 720.0));
        r.sel.owner = owner(&e);
        r.sel.clip_sel = Some(e.project.playlist[0].id.clone());
        r.frame(vec![]);
        r.edit_number("Start", "6");
        assert_eq!(r.sel.draft.as_ref().unwrap().start, 6);
        r.edit_number("Length", "5");
        assert_eq!(r.sel.draft.as_ref().unwrap().length, 5);
        let numeric_id = r.target("Start").unwrap().response.id;
        for label in [
            "Fit song",
            "Reveal clip",
            "Go to playhead",
            "Paint",
            "Select",
            "Zoom in",
            "Zoom out",
        ] {
            r.button(label);
        }
        for _ in 0..4 {
            r.frame(vec![]);
        }
        assert_eq!(r.sel.draft.as_ref().unwrap().start, 6);
        assert_eq!(r.sel.draft.as_ref().unwrap().length, 5);
        assert_eq!(r.target("Start").unwrap().response.id, numeric_id);
        assert!(r.calls.is_empty());
        assert_eq!(r.sel.pattern, 0);
        // Ordinary keyboard focus can activate buttons. Do not add shortcuts.
        let mut keyboard = EventGrid::new(one_clip_project(), 1.0, vec2(1280.0, 720.0));
        let mut reached_paint = false;
        for _ in 0..24 {
            keyboard.key(egui::Key::Tab, egui::Modifiers::default());
            keyboard.key(egui::Key::Space, egui::Modifiers::default());
            if keyboard.sel.paint {
                reached_paint = true;
                break;
            }
        }
        assert!(reached_paint, "Paint must be keyboard-focusable");
        assert!(keyboard.calls.is_empty());
    }

    #[test]
    fn focused_start_text_is_not_rebound_after_engine_edit_undo_or_reused_id_load() {
        for ppp in [1.0, 2.0] {
            for change in ["edit", "undo", "load"] {
                for commit in ["enter", "pointer"] {
                    let mut e = test_engine(&format!("focused_{change}_{commit}_{ppp}"));
                    // Undo must retain both selected clip and its stable ID.
                    e.call("set_tempo", &json!({"bpm":123})).unwrap();
                    let id = e.project.playlist[0].id.clone();
                    let mut r = EventGrid::new(e.project.clone(), ppp, vec2(1280.0, 720.0));
                    r.sel.owner = owner(&e);
                    r.sel.clip_sel = Some(id.clone());
                    r.frame(vec![]);
                    let old_id = r.pending_number("Start", "77");
                    assert_eq!(r.sel.draft.as_ref().unwrap().start, 77);
                    match change {
                        "undo" => {
                            e.call("undo", &json!({})).unwrap();
                        }
                        "load" => e.replace_project(e.project.clone()),
                        _ => {
                            e.call("set_tempo", &json!({"bpm":130})).unwrap();
                        }
                    }
                    let before = project_value(&e);
                    let rev = e.revision;
                    let depth = e.undo_depth();
                    r.p = e.project.clone();
                    r.sel.owner = owner(&e);
                    r.frame(vec![]);
                    assert_eq!(r.sel.clip_sel.as_deref(), Some(id.as_str()));
                    assert_ne!(r.target("Start").unwrap().response.id, old_id);
                    let fresh_start = u64::from(e.project.playlist[0].start_bar) + 1;
                    assert_eq!(r.sel.draft.as_ref().unwrap().start, fresh_start);
                    assert!(r.sel.toast.as_deref().unwrap().contains("refreshed"));
                    if commit == "enter" {
                        r.key(egui::Key::Enter, Default::default());
                    } else {
                        r.button("Select");
                    }
                    for _ in 0..3 {
                        r.frame(vec![]);
                    }
                    assert_eq!(r.sel.draft.as_ref().unwrap().start, fresh_start);
                    assert!(!r.ctx.memory(|m| m.has_focus(old_id)));
                    let at = r.disabled_point("Move");
                    r.click(at);
                    assert!(r.calls.is_empty());
                    assert_eq!(project_value(&e), before);
                    assert_eq!(e.revision, rev);
                    assert_eq!(e.undo_depth(), depth);
                    r.edit_number("Start", "11");
                    r.button("Move");
                    assert_eq!(
                        r.calls,
                        vec![(
                            "playlist_clip",
                            json!({"action":"move", "id":id, "to_bar":10, "lane":0})
                        )]
                    );
                    let (tool, args) = &r.calls[0];
                    guarded_grid_call(&mut e, r.sel.owner, tool, args).unwrap();
                    assert_eq!(e.project.playlist[0].start_bar, 10);
                    assert_eq!(e.revision, rev + 1);
                    assert_eq!(e.undo_depth(), (depth.0 + 1, 0));
                    e.call("undo", &json!({})).unwrap();
                    assert_eq!(project_value(&e), before);
                }
            }
        }
    }

    #[test]
    fn preview_shapes_include_repeat_5000_of_8192_and_visible_iteration_budget() {
        for ppp in [1.0, 2.0] {
            let e = test_engine("preview");
            let mut p = e.project.clone();
            p.playlist.truncate(1);
            p.playlist[0].start_bar = 0;
            p.playlist[0].bars = 8192;
            let pi = p.pattern_index("verse").unwrap();
            p.patterns[pi].bars = 1;
            let mut r = EventGrid::new(p, ppp, vec2(1280.0, 720.0));
            r.sel.view.scroll_bar = 5000.0;
            r.sel.view.zoom = ZOOM_MAX;
            r.frame(vec![]);
            let g = r.sel.geo.unwrap();
            let color = pattern_color("verse").gamma_multiply(0.95);
            fn notes(shape: &egui::epaint::Shape, color: Color32, out: &mut Vec<[egui::Pos2; 2]>) {
                match shape {
                    egui::epaint::Shape::LineSegment { points, stroke }
                        if stroke.color == egui::epaint::ColorMode::Solid(color)
                            && stroke.width == 1.0 =>
                    {
                        out.push(*points)
                    }
                    egui::epaint::Shape::Vec(v) => {
                        for s in v {
                            notes(s, color, out);
                        }
                    }
                    _ => {}
                }
            }
            let mut strokes = Vec::new();
            for shape in &r.shapes {
                let mut found = Vec::new();
                notes(&shape.shape, color, &mut found);
                strokes.extend(
                    found
                        .into_iter()
                        .filter(|s| shape.clip_rect.contains(s[0]) && r.screen.contains(s[0])),
                );
            }
            assert!(
                strokes
                    .iter()
                    .any(|s| (s[0].x - (g.x_of(5000.0) + 1.0)).abs() < 0.1),
                "Missing actual preview shape at repetition 5000"
            );
            // Compiler offset semantics: a two-bar source at offset one puts
            // its step-zero note on odd clip-local bars, not on bar 5000.
            let pi = r.p.pattern_index("verse").unwrap();
            r.p.patterns[pi].bars = 2;
            r.p.playlist[0].offset_bars = 1;
            r.frame(vec![]);
            let g = r.sel.geo.unwrap();
            let mut offset_strokes = Vec::new();
            for shape in &r.shapes {
                notes(&shape.shape, color, &mut offset_strokes);
            }
            assert!(offset_strokes
                .iter()
                .any(|s| (s[0].x - (g.x_of(5001.0) + 1.0)).abs() < 0.1));
            assert!(!offset_strokes
                .iter()
                .any(|s| (s[0].x - (g.x_of(5000.0) + 1.0)).abs() < 0.1));
            // A partial first source repetition must not draw its earlier
            // notes outside the left edge of a clip that starts onscreen.
            r.p.playlist[0].start_bar = 2;
            r.sel.view.scroll_bar = 0.0;
            r.sel.view.zoom = 1024.0;
            r.frame(vec![]);
            let g = r.sel.geo.unwrap();
            let mut initial_strokes = Vec::new();
            for shape in &r.shapes {
                notes(&shape.shape, color, &mut initial_strokes);
            }
            assert!(initial_strokes
                .iter()
                .any(|s| (s[0].x - (g.x_of(3.0) + 1.0)).abs() < 0.1));
            assert!(initial_strokes.iter().all(|s| s[0].x >= g.x_of(2.0) + 1.0));
            r.p.playlist[0].start_bar = 0;
            r.sel.view.zoom = ZOOM_MAX;
            // No Engine compilation of this synthetic extreme clip: exercise
            // actual drawing-loop visits with eight dense source rows.
            let note = r.p.patterns[pi].clips.values().next().unwrap()[0].clone();
            r.p.patterns[pi].clips.clear();
            for track in 0..8 {
                r.p.patterns[pi]
                    .clips
                    .insert(format!("dense-{track}"), vec![note.clone(); 1024]);
            }
            r.p.playlist[0].bars = u32::MAX;
            r.sel.view.scroll_bar = 3_000_000_000.0;
            r.frame(vec![]);
            let visits = r
                .ctx
                .data_mut(|d| d.get_temp::<usize>(egui::Id::new("preview-note-visits")))
                .unwrap();
            assert_eq!(
                visits, 16384,
                "Actual dense visible drawing loop must obey its budget"
            );
        }
        // The same production sampler bounds dense extreme extents around the
        // actual viewport, and retains the offset phase used by the compiler.
        for width in [1, 320, 1920, 3840] {
            let reps: Vec<_> = preview_repeats(
                3_000_000_000.0,
                3_100_000_000.0,
                f64::from(u32::MAX),
                0.5,
                17,
                width,
            )
            .collect();
            assert!(reps.len() <= width.min(64));
            assert!(reps
                .iter()
                .all(|b| *b >= 3_000_000_000.0 - 0.5 && *b < 3_100_000_000.0));
            let per_track = (16384 / (8 * reps.len().max(1))).min(256);
            assert!(8 * reps.len() * per_track <= 16384);
        }
        assert_eq!(
            preview_repeats(0.0, 4.0, 4.0, 2.0, 1, 64).collect::<Vec<_>>(),
            vec![-1.0, 1.0, 3.0]
        );
        assert!(preview_repeats(5.0, 6.0, 4.0, 1.0, 0, 64).next().is_none());
    }

    #[test]
    fn response_targets_reject_hidden_offviewport_duplicate_and_disabled_at_both_scales() {
        for ppp in [1.0, 2.0] {
            let mut r = EventGrid::new(one_clip_project(), ppp, vec2(210.0, 720.0));
            r.sel.clip_sel = Some(r.p.playlist[0].id.clone());
            r.frame(vec![]);
            // A narrow layout wraps controls. Actual responses, not a value
            // painted next to a label, identify every editable field.
            let at = r.disabled_point("Move");
            r.click(at);
            assert!(r.calls.is_empty());
            let start_id = r.target("Start").unwrap().response.id;
            r.edit_number("Start", "17");
            assert_eq!(r.sel.draft.as_ref().unwrap().start, 17);
            assert_eq!(r.target("Start").unwrap().response.id, start_id);
            assert_eq!(r.sel.draft.as_ref().unwrap().length, 2);
            assert_eq!(r.sel.draft.as_ref().unwrap().lane, 1);
            assert_eq!(r.sel.draft.as_ref().unwrap().copies, 1);
            // Record real distinct responses in both duplicate and hidden UIs.
            for hidden in [false, true] {
                r.ctx
                    .data_mut(|d| d.insert_temp(egui::Id::new(TARGETS), Vec::<Target>::new()));
                let _ = r.ctx.run(
                    egui::RawInput {
                        screen_rect: Some(r.screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            if hidden {
                                ui.set_invisible();
                            }
                            for _ in 0..if hidden { 1 } else { 2 } {
                                let response = ui.button("Start");
                                record_target(ui, "Start", &response);
                            }
                        });
                    },
                );
                r.targets = r
                    .ctx
                    .data_mut(|d| d.get_temp::<Vec<Target>>(egui::Id::new(TARGETS)).unwrap());
                assert!(
                    r.target("Start").is_err(),
                    "Duplicate or hidden responses must not be clicked"
                );
                if !hidden {
                    assert_eq!(r.targets.len(), 2);
                    assert_ne!(r.targets[0].response.id, r.targets[1].response.id);
                }
            }
            r.screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(210.0, 36.0));
            r.frame(vec![]);
            assert!(
                r.target("Start").is_err(),
                "An offscreen field must not be clicked"
            );
            assert!(r.calls.is_empty());
        }
    }

    #[test]
    fn stale_drafts_and_locked_guard_reject_revision_or_reused_id_generation() {
        for change in ["edit", "load", "undo", "generation-only"] {
            let mut e = test_engine("stale");
            let c = e.project.playlist[0].clone();
            let mut d = ClipDraft::new(&c, owner(&e));
            d.start = 5;
            match change {
                "load" => e.replace_project(e.project.clone()),
                "undo" => {
                    e.call("undo", &json!({})).unwrap();
                }
                "generation-only" => e.project_load_generation += 1,
                _ => {
                    e.call("set_tempo", &json!({"bpm":130})).unwrap();
                }
            }
            assert!(e.project.playlist.iter().any(|now| now.id == c.id));
            let before = project_value(&e);
            let revision = e.revision;
            let depth = e.undo_depth();
            assert!(guarded_grid_call(
                &mut e,
                d.owner,
                "playlist_clip",
                &d.payload(&c, "move").unwrap()
            )
            .is_err());
            assert_eq!(project_value(&e), before);
            assert_eq!(e.revision, revision);
            assert_eq!(e.undo_depth(), depth);
            d.owner = owner(&e);
            guarded_grid_call(
                &mut e,
                d.owner,
                "playlist_clip",
                &d.payload(&c, "move").unwrap(),
            )
            .unwrap();
            assert_eq!(e.project.playlist[0].start_bar, 4);
        }
    }

    #[test]
    fn invalid_noop_and_stale_form_clicks_do_not_emit_calls() {
        let e = test_engine("invalid");
        let c = e.project.playlist[0].clone();
        let mut r = EventGrid::new(e.project.clone(), 1.0, vec2(1280.0, 720.0));
        r.sel.owner = owner(&e);
        r.sel.clip_sel = Some(c.id.clone());
        r.frame(vec![]);
        for label in ["Move", "Set length"] {
            let at = r.disabled_point(label);
            r.click(at);
        }
        assert!(r.calls.is_empty(), "No-op buttons are disabled");
        let mut d = ClipDraft::new(&c, owner(&e));
        for value in [0, u64::MAX] {
            d.start = value;
            assert!(d.payload(&c, "move").is_none());
        }
        d = ClipDraft::new(&c, owner(&e));
        d.length = 0;
        assert!(d.payload(&c, "trim").is_none());
        d.length = u64::MAX;
        assert!(d.payload(&c, "trim").is_none());
        for n in [0, 65, u64::MAX] {
            d.copies = n;
            assert!(d.payload(&c, "duplicate").is_none());
        }
        d = ClipDraft::new(&c, owner(&e));
        d.copies = 64;
        assert_eq!(d.payload(&c, "duplicate").unwrap()["count"], 64);
        let mut far = c.clone();
        far.start_bar = u32::MAX - 2;
        assert!(ClipDraft::new(&far, owner(&e))
            .payload(&far, "duplicate")
            .is_none());
        r.sel.draft.as_mut().unwrap().start = 12;
        r.sel.owner.revision += 1;
        let at = r.disabled_point("Move");
        r.press(at, true);
        r.press(at, false);
        r.frame(vec![]);
        assert!(r.calls.is_empty());
        assert!(r.sel.toast.as_deref().unwrap().contains("refreshed"));
        assert_eq!(
            r.sel.draft.as_ref().unwrap().start,
            u64::from(c.start_bar) + 1
        );
    }

    #[test]
    fn stale_or_cancelled_drag_swallows_release_even_over_empty_paint_cell() {
        for change in ["revision", "generation", "mode", "escape", "deleted"] {
            let p = one_clip_project();
            let mut r = EventGrid::new(p, 1.0, vec2(1280.0, 720.0));
            r.button("Paint");
            let g = r.sel.geo.unwrap();
            let from = pos2(g.x_of(0.8), g.y_of(0.0) + 14.0);
            let to = pos2(g.x_of(5.8), g.y_of(1.0) + 14.0);
            r.press(from, true);
            r.frame(vec![egui::Event::PointerMoved(from + vec2(20.0, 0.0))]);
            assert!(r.sel.active_drag.is_some());
            match change {
                "revision" => r.sel.owner.revision += 1,
                "generation" => r.sel.owner.generation += 1,
                "mode" => r.sel.paint = false,
                "deleted" => {
                    r.p.playlist.clear();
                    r.sel.owner.revision += 1;
                }
                _ => {
                    r.key(egui::Key::Escape, egui::Modifiers::default());
                }
            }
            r.frame(vec![egui::Event::PointerMoved(to)]);
            r.press(to, false);
            r.frame(vec![]);
            assert!(r.calls.is_empty(), "{change}: {:?}", r.calls);
            assert!(r.sel.active_drag.is_none());
        }
    }

    #[test]
    fn fit_1024_reveal_lane_20_and_navigation_leave_engine_untouched() {
        let mut e = test_engine("navigation");
        let id = e.project.playlist[0].id.clone();
        e.call(
            "playlist_clip",
            &json!({"id":id,"action":"move","to_bar":1022,"lane":20}),
        )
        .unwrap();
        let before = project_value(&e);
        let rev = e.revision;
        let depth = e.undo_depth();
        for ppp in [1.0, 2.0] {
            for size in [
                vec2(640.0, 720.0),
                vec2(1280.0, 720.0),
                vec2(1920.0, 1080.0),
            ] {
                let mut r = EventGrid::new(e.project.clone(), ppp, size);
                r.sel.owner = owner(&e);
                r.button("Fit song");
                let g = r.sel.geo.unwrap();
                assert!(g.x_of(1024.0) <= g.grid.max.x);
                assert_eq!(g.scroll_bar, 0.0);
                if size.x == 640.0 {
                    assert!(g.bar_px < 1.0);
                }
                r.sel.clip_sel = Some(id.clone());
                r.frame(vec![]);
                r.button("Reveal clip");
                let g = r.sel.geo.unwrap();
                assert!(
                    g.y_of(20.0) >= g.grid.min.y && g.y_of(20.0) + LANE_H <= g.grid.max.y + 0.01
                );
                assert!(g.x_of(1022.0) >= g.grid.min.x && g.x_of(1024.0) <= g.grid.max.x);
                let row = super::super::fl_automation::AutoRow {
                    rect: g.grid,
                    lo: 0.0,
                    hi: 1.0,
                };
                assert_eq!(row.x_of_beat(&g, 4096.0), g.x_of(1024.0));
                r.bar_now = 50.5;
                r.button("Go to playhead");
                assert!((r.sel.view.scroll_bar - 50.5).abs() < 0.01);
                r.sel.song_navigation = false;
                r.bar_now = 80.0;
                r.frame(vec![]);
                let at = r.disabled_point("Go to playhead");
                r.click(at);
                assert!((r.sel.view.scroll_bar - 50.5).abs() < 0.01);
                assert!(r.calls.is_empty());
                assert_eq!(project_value(&e), before);
                assert_eq!(e.revision, rev);
                assert_eq!(e.undo_depth(), depth);
            }
        }
    }

    #[test]
    fn overview_clip_has_no_ambiguous_resize_or_move() {
        let mut p = one_clip_project();
        p.playlist[0].bars = 1024;
        let far = p.playlist[0].clone();
        let mut tiny = far.clone();
        tiny.id = "overview-tiny".into();
        tiny.start_bar = 512;
        tiny.bars = 1;
        tiny.lane = 1;
        p.playlist.push(tiny);
        let mut r = EventGrid::new(p, 1.0, vec2(640.0, 720.0));
        let g = r.sel.geo.unwrap();
        assert!(g.bar_px < 1.0);
        let at = pos2(g.x_of(512.0) + 1.25, g.y_of(1.0) + 14.0);
        for frame in drag(at, at + vec2(30.0, 0.0)) {
            r.frame(frame);
        }
        assert!(r.calls.is_empty());
        r.sel.paint = true;
        r.frame(vec![]);
        let g = r.sel.geo.unwrap();
        r.click(pos2(g.x_of(512.0) + 1.25, g.y_of(1.0) + 14.0));
        assert!(
            r.calls.is_empty(),
            "Minimum-width overview hits cannot paint behind clips"
        );
    }

    #[test]
    fn finite_geometry_and_tempo_ramps_keep_fixed_four_quarter_grid() {
        for w in [0.0, -5.0, f32::NAN, f32::INFINITY, 1.0, 640.0, f32::MAX] {
            for z in [0.0, -1.0, f32::NAN, f32::INFINITY, 0.25, 1.0, 4096.0] {
                let px = grid_scale(w, 1032, z);
                assert!(px.is_finite() && px > 0.0, "{w} {z}: {px}");
            }
        }
        let mut p = one_clip_project();
        p.bpm = 120.0;
        let source = p.pattern_index("verse").unwrap();
        p.patterns[source].meter = Some((7, 8));
        p.tempo_points = vec![
            crate::project::TempoPoint {
                beat: 4.0,
                bpm: 60.0,
                ramp: false,
            },
            crate::project::TempoPoint {
                beat: 12.0,
                bpm: 180.0,
                ramp: true,
            },
        ];
        let curve = crate::tempo_curve::TempoCurve::new(&p);
        for beat in [0.0, 4.0, 8.0, 12.0, 20.0] {
            let sec = curve.secs_at_beat(beat) as f32;
            let grid_bar = super::super::playlist::marker_beat(&p, sec) / 4.0;
            assert!((f64::from(grid_bar) - beat / 4.0).abs() < 0.0001);
        }
    }
    #[test]
    fn renamed_source_keeps_stable_clip_owner_and_engine_errors_keep_history() {
        let mut e = test_engine("rename");
        let id = e.project.playlist[0].id.clone();
        let name = "A long readable source name with spaces and a second verse";
        e.call(
            "rename_pattern",
            &json!({"pattern":"verse","new_name":name}),
        )
        .unwrap();
        let c = e
            .project
            .playlist
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .clone();
        let mut d = ClipDraft::new(&c, owner(&e));
        d.lane = 21;
        let args = d.payload(&c, "move").unwrap();
        assert_eq!(args["id"], id);
        guarded_grid_call(&mut e, d.owner, "playlist_clip", &args).unwrap();
        assert_eq!(e.project.playlist[0].pattern, name);
        assert_eq!(e.project.playlist[0].lane, 20);
        let before = project_value(&e);
        let rev = e.revision;
        let history = e.history();
        let depth = e.undo_depth();
        let fresh = owner(&e);
        assert!(guarded_grid_call(
            &mut e,
            fresh,
            "playlist_clip",
            &json!({"action":"move","id":"absent","to_bar":1})
        )
        .is_err());
        assert_eq!(project_value(&e), before);
        assert_eq!(e.revision, rev);
        assert_eq!(e.history(), history);
        assert_eq!(e.undo_depth(), depth);
    }

    #[test]
    fn unavailable_paint_source_cannot_create_a_clip() {
        let mut r = EventGrid::new(one_clip_project(), 1.0, vec2(1280.0, 720.0));
        r.sel.pattern = usize::MAX;
        r.button("Paint");
        let g = r.sel.geo.unwrap();
        r.click(pos2(g.x_of(10.5), g.y_of(1.0) + 14.0));
        assert!(r.calls.is_empty());
        assert!(r.sel.toast.as_deref().unwrap().contains("source pattern"));
        r.p.patterns.clear();
        r.p.playlist.clear();
        r.p.arrangement.clear();
        r.frame(vec![]);
        let g = r.sel.geo.unwrap();
        r.click(pos2(g.x_of(5.5), g.y_of(1.0) + 14.0));
        assert!(r.calls.is_empty());
    }
}
