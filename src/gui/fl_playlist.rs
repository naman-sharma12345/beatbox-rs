//! FL-style pattern playlist: pattern clips on numbered playlist tracks,
//! painted, dragged and resized directly on a bar grid. Click an empty cell to
//! paint the selected pattern there, drag a clip to move it (bars and lanes),
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
        (dx / bar_px.max(1.0)).round() as i64,
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
pub const ZOOM_MAX: f32 = 8.0;

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
                (s + db).max(0) as u32,
                (l + dl).clamp(0, last_lane) as u32,
                n as u32,
            )
        }
        Some((Grab::Resize, d)) => (
            s as u32,
            l as u32,
            (n + (d.x / bar_px.max(1.0)).round() as i64).max(1) as u32,
        ),
        Some((Grab::ResizeStart, d)) => {
            let end = s + n;
            let ns = (s + (d.x / bar_px.max(1.0)).round() as i64).clamp(0, end - 1);
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
}

impl Studio {
    pub(super) fn fl_playlist(&mut self, ui: &mut egui::Ui, p: &Project) {
        let bar_now = self.player.position_secs() * p.bpm / 240.0;
        let vid = egui::Id::new("flpl_view");
        let view = ui
            .data(|d| d.get_temp::<PlaylistView>(vid))
            .unwrap_or_default();
        let mut sel = GridSel {
            pattern: self.pattern,
            clip_sel: self.clip_sel.clone(),
            toast: None,
            view,
            geo: None,
        };
        let mut actions = playlist_grid(ui, p, &mut sel, bar_now);
        // the track automation lanes, on the grid's bar axis (scroll + zoom)
        if let Some(geo) = sel.geo {
            actions.extend(super::fl_automation::automation_lanes(ui, p, &geo));
        }
        ui.data_mut(|d| d.insert_temp(vid, sel.view));
        self.pattern = sel.pattern;
        self.clip_sel = sel.clip_sel;
        if let Some(t) = sel.toast {
            self.toast = Some((t, 0.0, false));
        }
        for (tool, args) in actions {
            // "Make unique" appends a new pattern; edit that copy next, so
            // the piano roll and step grid change what the block now plays.
            let unique =
                (args.get("action").and_then(|a| a.as_str()) == Some("make_unique")).then(|| {
                    args.get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string()
                });
            self.call(tool, args);
            if let Some(id) = unique {
                let e = self.engine.lock().unwrap();
                let now = &e.project;
                if let Some(pi) = now
                    .playlist
                    .iter()
                    .find(|c| c.id == id)
                    .and_then(|c| now.pattern_index(&c.pattern).ok())
                {
                    self.pattern = pi;
                }
            }
        }
    }
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
        .map(|c| c.lane + 1)
        .max()
        .unwrap_or(0)
        .max(3) as usize
        + 1;
    let visible = lanes.min(MAX_VISIBLE_LANES);
    let end_bar = p
        .playlist
        .iter()
        .map(|c| c.start_bar + c.bars.max(1))
        .max()
        .unwrap_or(0);
    let total_bars = (end_bar + 8).max(32);
    let h = RULER_H + LANE_H * visible as f32 + 2.0;
    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let grid = Rect::from_min_max(
        pos2(full.min.x + HEAD_W, full.min.y + RULER_H),
        pos2(full.max.x, full.min.y + RULER_H + LANE_H * visible as f32),
    );
    let fit_px = (grid.width() / total_bars as f32).max(10.0);
    // mouse wheel over the grid: Ctrl zooms around the pointer, Shift (or a
    // sideways wheel) scrolls bars, plain wheel scrolls lanes when they overflow
    let mut view = sel.view.clamped(total_bars, lanes);
    if let Some(hp) = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|hp| grid.contains(*hp))
    {
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 && zoom.is_finite() {
            let old_px = (fit_px * view.zoom).max(3.0);
            let anchor = view.scroll_bar + (hp.x - grid.min.x) / old_px;
            view.zoom = (view.zoom * zoom).clamp(ZOOM_MIN, ZOOM_MAX);
            let new_px = (fit_px * view.zoom).max(3.0);
            view.scroll_bar = anchor - (hp.x - grid.min.x) / new_px;
        }
        let d = ui.input(|i| i.smooth_scroll_delta);
        let px = (fit_px * view.zoom).max(3.0);
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
        bar_px: (fit_px * view.zoom).max(3.0),
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
    for b in b0..=b1 {
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
            format!("Track {}", l + 1),
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
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let bar = geo.bar_at(pos.x).floor().max(0.0) as u32;
                let lane = (geo.lane_at(pos.y).floor().max(0.0) as usize).min(lanes - 1) as u32;
                match p
                    .patterns
                    .get(sel.pattern)
                    .map(|pt| pt.name.clone())
                    .filter(|n| !n.starts_with("pl:"))
                {
                    Some(name) => actions.extend(paint_calls(p, &name, bar, lane)),
                    None => sel.toast = Some(
                        "Select a pattern to paint (the generated pl: sections can't be placed)"
                            .into(),
                    ),
                }
            }
        }
    }
    // clips
    for c in &p.playlist {
        let id = ui.id().with(("flpl_clip", c.id.as_str()));
        let grab: Option<(Grab, egui::Vec2)> = ui
            .data(|d| d.get_temp::<(u8, f32, f32)>(id))
            .map(|(g, x, y)| (Grab::from_code(g), vec2(x, y)));
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
                &c.pattern,
                FontId::proportional(8.5),
                Color32::from_rgb(20, 20, 24),
            );
        }
        // the pattern's notes, tiny, so a clip reads as its music
        if let Ok(pi) = p.pattern_index(&c.pattern) {
            let pat = &p.patterns[pi];
            let steps = pat.steps().max(1) as f32;
            let body = Rect::from_min_max(
                pos2(r.min.x, title.max.y + 1.0),
                pos2(r.max.x, r.max.y - 1.0),
            );
            let per_rep = pat.bars.max(1) as f32 * bar_px;
            let reps = (len / pat.bars.max(1) as f32).ceil() as usize;
            for (ti, (_, notes)) in pat.clips.iter().enumerate().take(8) {
                let yy = body.min.y + 1.0 + (ti as f32 * 1.6) % (body.height() - 2.0).max(1.0);
                for k in 0..reps {
                    for n in notes.iter().take(256) {
                        let x0 = r.min.x + k as f32 * per_rep + n.start / steps * per_rep;
                        if x0 >= r.max.x - 1.0 || x0 < grid.min.x - 2.0 || x0 > grid.max.x {
                            continue;
                        }
                        gp.line_segment(
                            [pos2(x0, yy), pos2((x0 + 1.5).min(r.max.x), yy)],
                            Stroke::new(1.0_f32, col.gamma_multiply(0.95)),
                        );
                    }
                }
            }
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
        let body_resp = ui.interact(hit(r), id.with("body"), Sense::click_and_drag());
        let left_resp = ui
            .interact(hit(left_full), id.with("left"), Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let edge_resp = ui
            .interact(hit(edge_full), id.with("edge"), Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let busy = |r: &egui::Response| r.drag_started() || r.dragged() || r.drag_stopped();
        let (resp, kind) = if busy(&edge_resp) {
            (&edge_resp, Grab::Resize)
        } else if busy(&left_resp) {
            (&left_resp, Grab::ResizeStart)
        } else {
            (&body_resp, Grab::Move)
        };
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(id, (kind.code(), 0.0f32, 0.0f32)));
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let cur =
                ui.data(|m| m.get_temp::<(u8, f32, f32)>(id))
                    .unwrap_or((kind.code(), 0.0, 0.0));
            ui.data_mut(|m| m.insert_temp(id, (cur.0, cur.1 + d.x, cur.2 + d.y)));
        }
        if resp.drag_stopped() {
            ui.data_mut(|m| m.remove::<(u8, f32, f32)>(id));
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
        if body_resp.clicked() {
            sel.clip_sel = Some(c.id.clone());
            if let Ok(pi) = p.pattern_index(&c.pattern) {
                sel.pattern = pi;
            }
        }
        let menu = super::playlist::pattern_clip_menu(c, bar_now);
        body_resp
            .on_hover_text(format!(
                "{} · bar {}–{} · drag to move, drag an edge to trim, right-click for more",
                c.pattern,
                c.start_bar + 1,
                c.start_bar + c.bars.max(1)
            ))
            .context_menu(|ui| {
                for (label, tool, args) in menu.clone() {
                    if ui.button(label).clicked() {
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
            a.start_bar < b.start_bar + b.bars.max(1) && b.start_bar < a.start_bar + a.bars.max(1)
        })
    });
    if overlaps {
        pt.text(
            grid.left_bottom() + vec2(6.0, -4.0),
            Align2::LEFT_BOTTOM,
            "Blocks that share the same bars play together, on any track.",
            FontId::proportional(11.0),
            DIM,
        );
    }
    if p.playlist.is_empty() {
        pt.text(
            grid.center(),
            Align2::CENTER_CENTER,
            "Click a cell to paint the selected pattern here",
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
            zoom: 100.0,
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
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let mut sel = GridSel {
            view,
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
            let painted = drive(&p, ppp, &|at| {
                let c = at(10.5, 1.0);
                drag(c, c)
            });
            assert!(
                painted
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
                let (c, _) = drive_view(&p, ppp, v, &|at| {
                    let a = at(8.5, 4.0);
                    drag(a, a)
                });
                assert!(
                    c.iter().any(|(t, a)| *t == "place_pattern"
                        && a["start_bar"] == 8
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
}
