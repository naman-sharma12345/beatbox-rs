//! The FL-style channel rack: one row per channel (track) with, left to
//! right, a mute LED and a solo button, pan and volume knobs, the channel's
//! name button (click = select it), the piano-roll keys button, then the
//! pattern's step buttons (drums) or a mini piano-roll lane (melodic). Every
//! gesture is one call to the same tool an MCP client uses (set_mixer,
//! toggle_step, edit_notes), so it is one undo step; the function only reads
//! the project and returns those calls, so tests drive it with real egui
//! pointer events.

use super::widgets::*;
use crate::instruments;
use crate::project::Project;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use serde_json::{json, Value};

/// Width of the controls left of the steps.
pub const RACK_HEAD_W: f32 = 286.0;
pub const RACK_RULER_H: f32 = 18.0;
pub const DRUM_ROW_H: f32 = 30.0;
pub const SYNTH_ROW_H: f32 = 46.0;

/// Where one channel row's controls sit (screen points). Every hit test
/// goes through it, and tests read it to aim the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RackRow {
    pub row: Rect,
    pub mute: Rect,
    pub solo: Rect,
    pub pan: Rect,
    pub vol: Rect,
    pub name: Rect,
    pub keys: Rect,
    /// the step area: step s is [x0 + s*cell, x0 + (s+1)*cell)
    pub steps: Rect,
    pub cell: f32,
}

impl RackRow {
    pub fn layout(row: Rect, cell: f32, steps: usize) -> RackRow {
        let cy = row.top() + (row.height() - 4.0) * 0.5;
        let x = row.left();
        let sq = |cx: f32, w: f32, h: f32| Rect::from_center_size(pos2(cx, cy), vec2(w, h));
        let name = Rect::from_min_max(
            pos2(x + 98.0, row.top() + 2.0),
            pos2(x + 246.0, row.bottom() - 6.0),
        );
        RackRow {
            row,
            mute: sq(x + 12.0, 14.0, 14.0),
            solo: sq(x + 30.0, 14.0, 14.0),
            pan: sq(x + 56.0, 22.0, 22.0),
            vol: sq(x + 82.0, 22.0, 22.0),
            name,
            keys: sq(x + 264.0, 22.0, 16.0),
            steps: Rect::from_min_max(
                pos2(x + RACK_HEAD_W, row.top()),
                pos2(x + RACK_HEAD_W + cell * steps as f32, row.bottom() - 4.0),
            ),
            cell,
        }
    }
    pub fn step_rect(&self, s: usize) -> Rect {
        Rect::from_min_size(
            pos2(
                self.steps.left() + s as f32 * self.cell + 1.5,
                self.steps.top() + 3.0,
            ),
            vec2(self.cell - 3.0, self.steps.height() - 6.0),
        )
    }
    /// The step under screen x (None outside the steps).
    #[cfg(test)]
    pub fn step_at(&self, x: f32) -> Option<usize> {
        if x < self.steps.left() || x >= self.steps.right() {
            return None;
        }
        Some(((x - self.steps.left()) / self.cell) as usize)
    }
}

/// What the rack keeps between frames and reports back: the selected
/// channel, and (after a frame) each row's layout.
#[derive(Clone, Debug, Default)]
pub struct RackSel {
    pub selected: usize,
    pub open_editor: bool,
    pub rows: Vec<RackRow>,
}

fn is_drum(t: &crate::project::Track) -> bool {
    t.instrument.is_drum()
        || matches!(&t.instrument, instruments::Instrument::Sampler(s) if s.one_shot)
}

/// Draw the channel rack for pattern `pattern` and return the engine calls
/// its gestures ask for. `playing_step` is the playhead inside this pattern.
pub fn channel_rack(
    ui: &mut egui::Ui,
    p: &Project,
    pattern: usize,
    sel: &mut RackSel,
    playing_step: Option<f32>,
) -> Vec<(&'static str, Value)> {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    sel.rows.clear();
    let Some(pat) = p.patterns.get(pattern) else {
        return actions;
    };
    let steps = pat.steps() as usize;
    let (ruler, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), RACK_RULER_H), Sense::hover());
    let cell = ((ruler.width() - RACK_HEAD_W - 6.0) / steps.max(1) as f32).max(4.0);
    for b in 0..pat.bars {
        let x = ruler.left() + RACK_HEAD_W + (b * pat.steps_per_bar()) as f32 * cell;
        ui.painter().text(
            pos2(x + 3.0, ruler.center().y),
            Align2::LEFT_CENTER,
            format!("{}", b + 1),
            FontId::monospace(11.0),
            DIM,
        );
    }
    for (ti, t) in p.tracks.iter().enumerate() {
        let kind = t.instrument.kind_name();
        let color = track_color(&t.name, kind);
        let drum = is_drum(t);
        let (row, _) = ui.allocate_exact_size(
            vec2(
                ui.available_width(),
                if drum { DRUM_ROW_H } else { SYNTH_ROW_H },
            ),
            Sense::hover(),
        );
        let g = RackRow::layout(row, cell, steps);
        sel.rows.push(g);
        let painter = ui.painter_at(row);
        let selected = ti == sel.selected;
        let head = Rect::from_min_size(row.min, vec2(RACK_HEAD_W - 8.0, row.height() - 4.0));
        painter.rect_filled(
            head,
            5.0,
            if selected {
                super::theme::Tokens::DARK.button_hi
            } else {
                PANEL2
            },
        );
        // mute LED (lit green = playing, FL-style) and solo
        let mr = ui.interact(g.mute, ui.id().with(("rack-m", ti)), Sense::click());
        painter.rect_filled(
            g.mute,
            3.0,
            if t.mute {
                Color32::from_rgb(40, 44, 40)
            } else {
                Color32::from_rgb(110, 210, 90)
            },
        );
        painter.rect_stroke(g.mute, 3.0, Stroke::new(1.0_f32, LINE));
        if mr
            .on_hover_text(if t.mute {
                "muted: click to unmute"
            } else {
                "click to mute"
            })
            .clicked()
        {
            actions.push(("set_mixer", json!({"track": t.name, "mute": !t.mute})));
        }
        let sr = ui.interact(g.solo, ui.id().with(("rack-s", ti)), Sense::click());
        painter.rect_filled(
            g.solo,
            3.0,
            if t.solo {
                SOLO
            } else {
                super::theme::Tokens::DARK.button
            },
        );
        painter.text(
            g.solo.center(),
            Align2::CENTER_CENTER,
            "S",
            FontId::proportional(9.0),
            if t.solo { Color32::BLACK } else { DIM },
        );
        if sr.on_hover_text("solo").clicked() {
            actions.push(("set_mixer", json!({"track": t.name, "solo": !t.solo})));
        }
        // pan and volume knobs
        let (pan_id, vol_id, keys_id) = (
            ui.id().with(("rack-pan", ti)),
            ui.id().with(("rack-vol", ti)),
            ui.id().with(("rack-keys", ti)),
        );
        if let Some(pn) = mini_knob(ui, g.pan, pan_id, t.pan, -1.0, 1.0, 0.0, true, color, "pan") {
            actions.push(("set_mixer", json!({"track": t.name, "pan": pn})));
        }
        if let Some(v) = mini_knob(
            ui,
            g.vol,
            vol_id,
            t.volume_db,
            -40.0,
            12.0,
            0.0,
            false,
            color,
            "volume dB",
        ) {
            actions.push(("set_mixer", json!({"track": t.name, "volume_db": v})));
        }
        // the channel name button: click selects the channel
        let nr = ui.interact(g.name, ui.id().with(("rack-name", ti)), Sense::click());
        painter.rect_filled(
            g.name,
            3.0,
            if selected {
                lerp_color(PANEL2, color, 0.35)
            } else {
                super::theme::Tokens::DARK.button
            },
        );
        painter.rect_filled(
            Rect::from_min_size(g.name.min, vec2(3.0, g.name.height())),
            1.0,
            color,
        );
        let rid = ui.id().with("rack-rename");
        let editing: Option<(String, String, bool)> = ui.data(|d| d.get_temp(rid));
        let renaming = editing.as_ref().is_some_and(|e| e.0 == t.name);
        if renaming {
            let (_, mut buf, first) = editing.unwrap_or_default();
            let te = ui.put(
                g.name.shrink2(vec2(3.0, 2.0)),
                egui::TextEdit::singleline(&mut buf).font(FontId::proportional(12.0)),
            );
            if first {
                te.request_focus();
            }
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if esc {
                ui.data_mut(|d| d.remove::<(String, String, bool)>(rid));
            } else if te.lost_focus() && !first {
                if enter && buf.trim() != t.name {
                    actions.push(("rename_track", json!({"track": t.name, "name": buf})));
                }
                ui.data_mut(|d| d.remove::<(String, String, bool)>(rid));
            } else {
                ui.data_mut(|d| d.insert_temp(rid, (t.name.clone(), buf, false)));
            }
        } else {
            painter.text(
                g.name.left_center() + vec2(8.0, if drum { 0.0 } else { -6.0 }),
                Align2::LEFT_CENTER,
                &t.name,
                FontId::proportional(12.0),
                if t.mute { DIM } else { TEXT },
            );
        }
        if !drum {
            painter.text(
                g.name.left_center() + vec2(8.0, 8.0),
                Align2::LEFT_CENTER,
                kind,
                FontId::proportional(9.5),
                DIM,
            );
        }
        let nr = nr.on_hover_text(format!(
            "{} ({kind}): click to select, double-click or right-click to rename",
            t.name
        ));
        if nr.clicked() {
            sel.selected = ti;
        }
        let mut start_rename = nr.double_clicked();
        nr.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                start_rename = true;
                ui.close_menu();
            }
        });
        if start_rename {
            ui.data_mut(|d| d.insert_temp(rid, (t.name.clone(), t.name.clone(), true)));
        }
        if keys_button(ui, g.keys, keys_id, selected, color).clicked() {
            sel.selected = ti;
            sel.open_editor = true;
        }
        // steps
        painter.rect_filled(g.steps, 4.0, BG);
        let notes = pat.notes(&t.name);
        if !ui.input(|i| i.pointer.primary_down()) {
            ui.data_mut(|d| d.remove_temp::<(bool, usize, usize)>(ui.id().with("rack-paint")));
        }
        if drum {
            for s in 0..steps {
                let r = g.step_rect(s);
                let hit = notes
                    .iter()
                    .find(|n| n.start >= s as f32 && n.start < s as f32 + 1.0);
                // FL shades every other beat (group of 4 steps)
                let base = if (s / 4) % 2 == 0 {
                    super::theme::Tokens::DARK.button
                } else {
                    // a plain warm grey, so an unlit step never looks like a dim lit one
                    Color32::from_rgb(58, 52, 44)
                };
                painter.rect_filled(
                    r,
                    2.5,
                    match hit {
                        Some(n) => color.gamma_multiply(0.35 + 0.65 * n.vel),
                        None => base,
                    },
                );
                if playing_step.map(|ps| ps as usize == s).unwrap_or(false) {
                    painter.rect_stroke(r, 2.5, Stroke::new(1.5_f32, Color32::WHITE));
                }
                let resp = ui.interact(
                    r,
                    ui.id().with(("rack-step", ti, s)),
                    Sense::click_and_drag(),
                );
                // FL Studio behaviour: a press toggles a step and dragging
                // across steps paints (or erases, if the press began on a lit
                // step); right-click erases a lit step.
                let paint_id = ui.id().with("rack-paint");
                // a fast drag can skip cells between two frames, so a cell counts
                // when the pointer's path since the last frame crossed it
                let (down, pressed, over) = ui.input(|i| {
                    let over = i.pointer.hover_pos().is_some_and(|p| {
                        let q = p - i.pointer.delta();
                        let (x0, x1) = (p.x.min(q.x), p.x.max(q.x));
                        p.y >= r.top() && p.y <= r.bottom() && x1 >= r.left() && x0 <= r.right()
                    });
                    (i.pointer.primary_down(), i.pointer.primary_pressed(), over)
                });
                let _ = pressed;
                let mut toggle = false;
                if resp.clicked() {
                    toggle = true;
                } else if resp.drag_started() {
                    ui.data_mut(|d| d.insert_temp(paint_id, (hit.is_none(), ti, s)));
                    toggle = true;
                } else if over && down {
                    let st: Option<(bool, usize, usize)> = ui.data(|d| d.get_temp(paint_id));
                    if let Some((add, lt, ls)) = st {
                        if (lt, ls) != (ti, s) && hit.is_none() == add {
                            ui.data_mut(|d| d.insert_temp(paint_id, (add, ti, s)));
                            toggle = true;
                        }
                    }
                } else if resp.secondary_clicked() && hit.is_some() {
                    toggle = true;
                }
                if toggle {
                    sel.selected = ti;
                    actions.push((
                        "toggle_step",
                        json!({"track": t.name, "step": s, "pattern": pat.name}),
                    ));
                }
                if let Some(n) = hit {
                    let h = (r.height() - 4.0) * n.vel.clamp(0.0, 1.0);
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(r.left() + 2.0, r.bottom() - 2.0 - h),
                            vec2(2.0, h),
                        ),
                        1.0,
                        Color32::from_white_alpha(150),
                    );
                    if resp.hovered() {
                        let dy = ui.input(|i| i.raw_scroll_delta.y);
                        if dy.abs() > 0.5 {
                            let add = if dy > 0.0 { 0.05 } else { -0.05 };
                            actions.push(("edit_notes", json!({"track": t.name, "pattern": pat.name, "from": s, "to": s + 1, "vel_add": add})));
                        }
                        resp.on_hover_text(format!(
                            "velocity {:.0}% (scroll to change)",
                            n.vel * 100.0
                        ));
                    }
                }
            }
        } else {
            for b in 0..=pat.bars {
                let x = g.steps.left() + (b * pat.steps_per_bar()) as f32 * cell;
                painter.line_segment(
                    [pos2(x, g.steps.top()), pos2(x, g.steps.bottom())],
                    Stroke::new(1.0_f32, LINE),
                );
            }
            let (lo, hi) = notes
                .iter()
                .fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
            let span = (hi.saturating_sub(lo)).max(12) as f32;
            for n in notes {
                let y = g.steps.bottom()
                    - 5.0
                    - (n.pitch.saturating_sub(lo)) as f32 / span * (g.steps.height() - 12.0);
                let r = Rect::from_min_size(
                    pos2(g.steps.left() + n.start * cell, y - 2.5),
                    vec2((n.len * cell - 1.0).max(2.0), 5.0),
                );
                painter.rect_filled(r, 2.0, color.gamma_multiply(0.5 + 0.5 * n.vel));
            }
            if ui
                .interact(g.steps, ui.id().with(("rack-lane", ti)), Sense::click())
                .clicked()
            {
                sel.selected = ti;
            }
        }
        if let Some(ps) = playing_step {
            let x = g.steps.left() + ps * cell;
            painter.line_segment(
                [pos2(x, g.steps.top()), pos2(x, g.steps.bottom())],
                Stroke::new(1.5_f32, Color32::WHITE.gamma_multiply(0.8)),
            );
        }
    }
    actions
}

// ---------- the pattern workflow: rack patterns -> playlist clips ----------

/// Height of the pattern picker above the rack.
pub const PATTERN_BAR_H: f32 = 22.0;
const PATTERN_BTN_W: f32 = 84.0;
const SEND_BTN_W: f32 = 104.0;

/// The bar after the last clip on any lane (where the song ends).
pub fn song_end_bar(p: &Project) -> u32 {
    p.playlist
        .iter()
        .map(|c| c.start_bar + c.bars.max(1))
        .max()
        .unwrap_or(0)
}

/// Whether bars `bar..bar+bars` on `lane` hold no clip.
pub fn lane_is_free(p: &Project, lane: u32, bar: u32, bars: u32) -> bool {
    let end = bar + bars.max(1);
    !p.playlist
        .iter()
        .any(|c| c.lane == lane && c.start_bar < end && bar < c.start_bar + c.bars.max(1))
}

/// Where "→ Playlist" drops a `bars`-long clip. With a target (the
/// selected playlist track and bar): there, if that spot is free, else the
/// next lane below it that is free over the same bars. Without one: at the
/// song's end, on the first free lane. Never collides with a clip.
pub fn drop_spot(p: &Project, bars: u32, target: Option<(u32, u32)>) -> (u32, u32) {
    let (lane0, bar) = match target {
        Some((l, b)) => (l, b),
        None => (0, song_end_bar(p)),
    };
    let lane = (lane0..)
        .find(|&l| lane_is_free(p, l, bar, bars))
        .unwrap_or(lane0);
    (lane, bar)
}

/// A pattern name not taken yet: "Pattern 1", "Pattern 2", ...
pub fn new_pattern_name(p: &Project) -> String {
    (1..)
        .map(|n| format!("Pattern {n}"))
        .find(|n| !p.patterns.iter().any(|q| q.name.eq_ignore_ascii_case(n)))
        .unwrap_or_default()
}

/// Where the picker's controls sat last frame (tests aim with it):
/// (pattern index, button) for each pattern shown.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternBarGeo {
    pub buttons: Vec<(usize, Rect)>,
    pub add: Rect,
    pub send: Rect,
}

impl Default for PatternBarGeo {
    fn default() -> Self {
        PatternBarGeo {
            buttons: Vec::new(),
            add: Rect::NOTHING,
            send: Rect::NOTHING,
        }
    }
}

impl PatternBarGeo {
    #[cfg(test)]
    pub fn button(&self, pattern: usize) -> Option<Rect> {
        self.buttons
            .iter()
            .find(|(i, _)| *i == pattern)
            .map(|(_, r)| *r)
    }
}

/// The pattern picker, FL-style, above the channel rack: one button per
/// pattern (the playlist's generated "pl:" sections are not patterns you
/// edit, so they are left out; click = edit it in the rack, and it is what a
/// click paints in the playlist), "+" makes a new empty pattern (as long as
/// the current one) and selects it, "→ Playlist" drops the current pattern
/// as a clip at `drop_spot` (the selected playlist track and bar when there
/// is one). Returns the engine calls.
pub fn pattern_bar(
    ui: &mut egui::Ui,
    p: &Project,
    current: &mut usize,
    target: Option<(u32, u32)>,
    geo: &mut PatternBarGeo,
) -> Vec<(&'static str, Value)> {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), PATTERN_BAR_H), Sense::hover());
    let painter = ui.painter_at(row);
    let h = PATTERN_BAR_H - 4.0;
    let send = Rect::from_min_size(
        pos2(row.right() - SEND_BTN_W, row.top() + 2.0),
        vec2(SEND_BTN_W - 2.0, h),
    );
    let room = (send.left() - row.left() - 30.0).max(0.0);
    let fit = ((room / PATTERN_BTN_W) as usize).max(1);
    let user: Vec<usize> = (0..p.patterns.len())
        .filter(|&i| !p.patterns[i].name.starts_with(crate::tools_fl::PL))
        .collect();
    // keep the current pattern in view when there are more than fit
    let pos = user.iter().position(|&i| i == *current).unwrap_or(0);
    let first = pos
        .saturating_sub(fit - 1)
        .min(user.len().saturating_sub(fit));
    geo.buttons.clear();
    let mut x = row.left() + 2.0;
    for &i in user.iter().skip(first).take(fit) {
        let pat = &p.patterns[i];
        let r = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(PATTERN_BTN_W - 4.0, h));
        x += PATTERN_BTN_W;
        geo.buttons.push((i, r));
        let on = i == *current;
        let color = super::fl_playlist::pattern_color(&pat.name);
        painter.rect_filled(
            r,
            3.0,
            if on {
                color.gamma_multiply(0.8)
            } else {
                super::theme::Tokens::DARK.button
            },
        );
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            &pat.name,
            FontId::proportional(10.5),
            if on { Color32::BLACK } else { TEXT },
        );
        if ui
            .interact(r, ui.id().with(("pat-btn", i)), Sense::click())
            .on_hover_text(format!(
                "{} ({} bars): edit in the rack, paint in the playlist",
                pat.name, pat.bars
            ))
            .clicked()
        {
            *current = i;
        }
    }
    let add = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(22.0, h));
    geo.add = add;
    painter.rect_filled(add, 3.0, super::theme::Tokens::DARK.button);
    painter.text(
        add.center(),
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(13.0),
        TEXT,
    );
    if ui
        .interact(add, ui.id().with("pat-add"), Sense::click())
        .on_hover_text("new empty pattern")
        .clicked()
    {
        let bars = p.patterns.get(*current).map(|q| q.bars).unwrap_or(1).max(1);
        actions.push((
            "add_pattern",
            json!({"name": new_pattern_name(p), "bars": bars}),
        ));
        *current = p.patterns.len();
    }
    // pattern length: how many bars this pattern loops (what Play and Render use)
    if let Some(cur) = p.patterns.get(*current) {
        let bars = cur.bars;
        let name = cur.name.clone();
        let tk = super::theme::Tokens::DARK;
        let mut lx = add.right() + 16.0;
        let minus = Rect::from_min_size(pos2(lx, row.top() + 2.0), vec2(20.0, h));
        lx += 24.0;
        let label = Rect::from_min_size(pos2(lx, row.top() + 2.0), vec2(56.0, h));
        lx += 60.0;
        let plus = Rect::from_min_size(pos2(lx, row.top() + 2.0), vec2(20.0, h));
        let new_end = (bars.saturating_sub(1) * cur.steps_per_bar()) as f32;
        let shrink_blocked = cur
            .clips
            .values()
            .any(|v| v.iter().any(|n| n.start >= new_end));
        for (r, t) in [(minus, "\u{2212}"), (plus, "+")] {
            let dim = r == minus && (bars <= 1 || shrink_blocked);
            painter.rect_filled(
                r,
                3.0,
                if dim {
                    tk.button.gamma_multiply(0.45)
                } else {
                    tk.button
                },
            );
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                t,
                FontId::proportional(13.0),
                TEXT,
            );
        }
        painter.text(
            label.center(),
            Align2::CENTER_CENTER,
            format!("{bars} bar{}", if bars == 1 { "" } else { "s" }),
            FontId::proportional(10.5),
            TEXT,
        );
        if ui
            .interact(minus, ui.id().with("pat-len-minus"), Sense::click())
            .on_hover_text(if shrink_blocked {
                "The last bar has notes. Clear them first (nothing is deleted for you)."
            } else {
                "Shorter pattern"
            })
            .clicked()
            && bars > 1
            && !shrink_blocked
        {
            actions.push((
                "set_pattern_length",
                json!({"pattern": name, "bars": bars - 1}),
            ));
        }
        if ui
            .interact(plus, ui.id().with("pat-len-plus"), Sense::click())
            .on_hover_text("Longer pattern: more steps to fill, and a longer loop")
            .clicked()
            && bars < 64
        {
            actions.push((
                "set_pattern_length",
                json!({"pattern": name, "bars": bars + 1}),
            ));
        }
    }
    geo.send = send;
    if let Some(pat) = p
        .patterns
        .get(*current)
        .filter(|q| !q.name.starts_with(crate::tools_fl::PL))
    {
        let (lane, bar) = drop_spot(p, pat.bars, target);
        painter.rect_filled(send, 3.0, Color32::from_rgb(60, 72, 52));
        painter.text(
            send.center(),
            Align2::CENTER_CENTER,
            "→ Playlist",
            FontId::proportional(10.5),
            TEXT,
        );
        if ui
            .interact(send, ui.id().with("pat-send"), Sense::click())
            .on_hover_text(format!(
                "place {} on playlist track {} at bar {}",
                pat.name,
                lane + 1,
                bar + 1
            ))
            .clicked()
        {
            actions.push((
                "place_pattern",
                json!({"pattern": pat.name, "start_bar": bar, "lane": lane}),
            ));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use egui::{Event, Modifiers, PointerButton};

    fn engine() -> Engine {
        let d = std::env::temp_dir().join(format!("rack_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "verse", "bars": 1}))
            .unwrap();
        e
    }

    fn pattern_index(p: &Project) -> usize {
        p.patterns.iter().position(|q| q.name == "verse").unwrap()
    }

    /// Run the rack on real egui input; `gesture` gets the rows' layout.
    fn drive(
        p: &Project,
        ppp: f32,
        gesture: &dyn Fn(&[RackRow]) -> Vec<Vec<Event>>,
    ) -> (Vec<(&'static str, Value)>, RackSel) {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let mut sel = RackSel::default();
        let mut out = Vec::new();
        let pi = pattern_index(p);
        let frame = |events: Vec<Event>,
                     k: usize,
                     out: &mut Vec<(&'static str, Value)>,
                     sel: &mut RackSel| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out.extend(channel_rack(ui, p, pi, sel, None));
                });
            });
        };
        frame(vec![], 0, &mut out, &mut sel);
        frame(vec![], 1, &mut out, &mut sel);
        let rows = sel.rows.clone();
        for (k, ev) in gesture(&rows).into_iter().enumerate() {
            frame(ev, k + 2, &mut out, &mut sel);
        }
        (out, sel)
    }

    fn press(pos: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    fn click(at: egui::Pos2) -> Vec<Vec<Event>> {
        vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![press(at, false)],
            vec![],
        ]
    }

    fn drag(from: egui::Pos2, to: egui::Pos2) -> Vec<Vec<Event>> {
        let mut f = vec![vec![Event::PointerMoved(from)], vec![press(from, true)]];
        for i in 1..=8 {
            f.push(vec![Event::PointerMoved(
                from + (to - from) * (i as f32 / 8.0),
            )]);
        }
        f.push(vec![press(to, false)]);
        f.push(vec![]);
        f
    }

    /// End to end: steps clicked in the rack are what the engine renders.
    /// The pattern sits on the playlist; a kick step toggled on beat 2
    /// puts a hit there, and the rack's mute silences it again.
    #[test]
    fn steps_toggled_in_the_rack_are_heard_in_the_render() {
        let mut e = engine();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        let kick = e.project.track_index("kick").unwrap();
        let rms_at = |m: &crate::render::Mix, beat0: f32, beat1: f32, bpm: f32| {
            let sr = crate::dsp::SR;
            let (a, b) = (
                (beat0 * 60.0 / bpm * sr) as usize,
                (beat1 * 60.0 / bpm * sr) as usize,
            );
            let b = b.min(m.left.len());
            let s: f32 = m.left[a..b].iter().map(|v| v * v).sum();
            (s / (b - a).max(1) as f32).sqrt()
        };
        let bpm = e.project.bpm;
        let before = e.mix().unwrap();
        let quiet = rms_at(&before, 1.0, 1.5, bpm);
        // click kick step 4 (beat 2 of the bar) in the rack
        let p = e.project.clone();
        let (calls, _) = drive(&p, 1.0, &|rows| click(rows[kick].step_rect(4).center()));
        assert_eq!(
            calls,
            vec![(
                "toggle_step",
                json!({"track": "kick", "step": 4, "pattern": "verse"})
            )]
        );
        for (t, a) in &calls {
            e.call(t, a).unwrap();
        }
        let after = e.mix().unwrap();
        let hit = rms_at(&after, 1.0, 1.5, bpm);
        assert!(
            hit > 1e-3 && hit > quiet * 10.0 + 1e-4,
            "the toggled step is audible: before {quiet}, after {hit}"
        );
        // nothing else moved: the bar's first beat is as quiet as before
        assert!(rms_at(&after, 0.0, 0.9, bpm) < hit * 0.1);
        // the rack's mute LED silences the channel in the render
        let p = e.project.clone();
        let (calls, _) = drive(&p, 1.0, &|rows| click(rows[kick].mute.center()));
        assert_eq!(
            calls,
            vec![("set_mixer", json!({"track": "kick", "mute": true}))]
        );
        for (t, a) in &calls {
            e.call(t, a).unwrap();
        }
        let muted = e.mix().unwrap();
        assert!(
            rms_at(&muted, 1.0, 1.5, bpm) < hit * 0.01,
            "muted in the rack, silent in the render"
        );
    }

    #[test]
    fn the_layout_reads_left_to_right_like_fl() {
        let r = RackRow::layout(
            Rect::from_min_size(pos2(10.0, 50.0), vec2(900.0, DRUM_ROW_H)),
            20.0,
            16,
        );
        let order = [r.mute, r.solo, r.pan, r.vol, r.name, r.keys, r.steps];
        for w in order.windows(2) {
            assert!(
                w[0].right() <= w[1].left(),
                "{:?} overlaps {:?}",
                w[0],
                w[1]
            );
        }
        assert!(order.iter().all(|x| r.row.contains(x.center())));
        assert_eq!(r.step_at(r.step_rect(5).center().x), Some(5));
        assert_eq!(r.step_at(r.steps.left() - 1.0), None);
        assert_eq!(r.step_at(r.steps.right()), None);
    }

    #[test]
    fn a_real_pointer_drives_steps_mute_solo_knobs_and_select() {
        for ppp in [1.0f32, 2.0] {
            let mut e = engine();
            let p = e.project.clone();
            let (kick, keys) = (
                p.track_index("kick").unwrap(),
                p.track_index("keys").unwrap(),
            );
            // a kick step: toggle_step on that step of this pattern, and the kick is selected
            let (calls, sel) = drive(&p, ppp, &|rows| click(rows[kick].step_rect(5).center()));
            assert_eq!(
                calls,
                vec![(
                    "toggle_step",
                    json!({"track": "kick", "step": 5, "pattern": "verse"})
                )],
                "ppp {ppp}"
            );
            assert_eq!(sel.selected, kick);
            for (t, a) in &calls {
                e.call(t, a).unwrap();
            }
            let pi = pattern_index(&e.project);
            assert!(
                e.project.patterns[pi]
                    .notes("kick")
                    .iter()
                    .any(|n| n.start == 5.0),
                "ppp {ppp}"
            );
            // the lit step is clicked again: off
            let p2 = e.project.clone();
            let (calls, _) = drive(&p2, ppp, &|rows| click(rows[kick].step_rect(5).center()));
            for (t, a) in &calls {
                e.call(t, a).unwrap();
            }
            assert!(e.project.patterns[pi].notes("kick").is_empty(), "ppp {ppp}");
            // mute and solo on the keys row
            let (calls, _) = drive(&p, ppp, &|rows| click(rows[keys].mute.center()));
            assert_eq!(
                calls,
                vec![("set_mixer", json!({"track": "keys", "mute": true}))],
                "ppp {ppp}"
            );
            let (calls, _) = drive(&p, ppp, &|rows| click(rows[keys].solo.center()));
            assert_eq!(
                calls,
                vec![("set_mixer", json!({"track": "keys", "solo": true}))],
                "ppp {ppp}"
            );
            // drag the keys volume knob up: louder; the pan knob down: left
            let before = p.tracks[keys].volume_db;
            let (calls, _) = drive(&p, ppp, &|rows| {
                let c = rows[keys].vol.center();
                drag(c, c - vec2(0.0, 40.0))
            });
            assert_eq!(calls.len(), 1, "ppp {ppp}: {calls:?}");
            assert_eq!(calls[0].1["track"], json!("keys"));
            assert!(
                calls[0].1["volume_db"].as_f64().unwrap() as f32 > before + 3.0,
                "ppp {ppp}: {calls:?} (was {before})"
            );
            let (calls, _) = drive(&p, ppp, &|rows| {
                let c = rows[keys].pan.center();
                drag(c, c + vec2(0.0, 30.0))
            });
            assert_eq!(calls.len(), 1, "ppp {ppp}: {calls:?}");
            assert!(
                calls[0].1["pan"].as_f64().unwrap() < -0.1,
                "ppp {ppp}: {calls:?}"
            );
            for (t, a) in &calls {
                e.call(t, a).unwrap();
            }
            assert!(e.project.tracks[keys].pan < -0.1);
            // the name button selects, and asks the engine for nothing
            let (calls, sel) = drive(&p, ppp, &|rows| click(rows[keys].name.center()));
            assert!(calls.is_empty(), "ppp {ppp}: {calls:?}");
            assert_eq!(sel.selected, keys);
            // the melodic row has no step buttons: a click in its lane only selects
            let (calls, sel) = drive(&p, ppp, &|rows| click(rows[keys].step_rect(3).center()));
            assert!(calls.is_empty(), "ppp {ppp}: {calls:?}");
            assert_eq!(sel.selected, keys);
        }
    }

    /// Run the pattern bar and the rack under it on real egui input and
    /// click where `click_at` says.
    fn run_bar(
        p: &Project,
        ppp: f32,
        cur: usize,
        target: Option<(u32, u32)>,
        click_at: &dyn Fn(&PatternBarGeo) -> egui::Pos2,
    ) -> (Vec<(&'static str, Value)>, usize) {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let (mut cur, mut geo, mut out) = (cur, PatternBarGeo::default(), Vec::new());
        let frame = |events: Vec<Event>,
                     k: usize,
                     cur: &mut usize,
                     geo: &mut PatternBarGeo,
                     out: &mut Vec<(&'static str, Value)>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out.extend(pattern_bar(ui, p, cur, target, geo));
                    let mut sel = RackSel::default();
                    out.extend(channel_rack(ui, p, *cur, &mut sel, None));
                });
            });
        };
        frame(vec![], 0, &mut cur, &mut geo, &mut out);
        frame(vec![], 1, &mut cur, &mut geo, &mut out);
        let at = click_at(&geo);
        for (k, ev) in click(at).into_iter().enumerate() {
            frame(ev, k + 2, &mut cur, &mut geo, &mut out);
        }
        (out, cur)
    }

    fn apply(e: &mut Engine, calls: &[(&'static str, Value)]) {
        for (t, a) in calls {
            e.call(t, a).unwrap();
        }
    }

    #[test]
    fn the_pattern_bar_selects_adds_and_drops_patterns_without_collisions() {
        for ppp in [1.0f32, 2.0] {
            let mut e = engine();
            e.call("add_pattern", &json!({"name": "hook", "bars": 2}))
                .unwrap();
            let p = e.project.clone();
            let verse = p.patterns.iter().position(|q| q.name == "verse").unwrap();
            let hook = p.patterns.iter().position(|q| q.name == "hook").unwrap();
            // click the hook's button: it is the current pattern, no engine call
            let (calls, cur) = run_bar(&p, ppp, verse, None, &|g| g.button(hook).unwrap().center());
            assert!(calls.is_empty(), "ppp {ppp}: {calls:?}");
            assert_eq!(cur, hook);
            // nothing selected in the playlist: the song's end (bar 0), first free lane
            let (calls, _) = run_bar(&p, ppp, hook, None, &|g| g.send.center());
            assert_eq!(
                calls,
                vec![(
                    "place_pattern",
                    json!({"pattern": "hook", "start_bar": 0, "lane": 0})
                )],
                "ppp {ppp}"
            );
            apply(&mut e, &calls);
            // the generated pl: sections are not shown as patterns
            let p2 = e.project.clone();
            let (calls, _) = run_bar(&p2, ppp, verse, None, &|g| {
                assert!(
                    g.buttons
                        .iter()
                        .all(|(i, _)| !p2.patterns[*i].name.starts_with("pl:")),
                    "{:?}",
                    g.buttons
                );
                g.send.center()
            });
            // nothing selected again: appended at the song's end (bar 2)
            assert_eq!(
                calls,
                vec![(
                    "place_pattern",
                    json!({"pattern": "verse", "start_bar": 2, "lane": 0})
                )],
                "ppp {ppp}"
            );
            apply(&mut e, &calls);
            // a selected track 3 at bar 5, free: dropped exactly there
            let p3 = e.project.clone();
            let (calls, _) = run_bar(&p3, ppp, hook, Some((2, 5)), &|g| g.send.center());
            assert_eq!(
                calls,
                vec![(
                    "place_pattern",
                    json!({"pattern": "hook", "start_bar": 5, "lane": 2})
                )],
                "ppp {ppp}"
            );
            // the selected spot collides (track 1, bar 1 is inside the first hook): the next free lane, same bar
            let (calls, _) = run_bar(&p3, ppp, hook, Some((0, 1)), &|g| g.send.center());
            assert_eq!(
                calls,
                vec![(
                    "place_pattern",
                    json!({"pattern": "hook", "start_bar": 1, "lane": 1})
                )],
                "ppp {ppp}"
            );
            apply(&mut e, &calls);
            let pl = &e.project.playlist;
            for (a, c) in pl.iter().enumerate() {
                for d in pl.iter().skip(a + 1) {
                    let overlap = c.lane == d.lane
                        && c.start_bar < d.start_bar + d.bars.max(1)
                        && d.start_bar < c.start_bar + c.bars.max(1);
                    assert!(!overlap, "clips collide: {} {}", c.id, d.id);
                }
            }
            // "+": a new pattern as long as the current one, and it becomes current
            let p4 = e.project.clone();
            let (calls, cur) = run_bar(&p4, ppp, hook, None, &|g| g.add.center());
            assert_eq!(
                calls,
                vec![("add_pattern", json!({"name": "Pattern 1", "bars": 2}))],
                "ppp {ppp}"
            );
            assert_eq!(cur, p4.patterns.len());
            apply(&mut e, &calls);
            assert_eq!(e.project.patterns[cur].name, "Pattern 1");
            assert_eq!(new_pattern_name(&e.project), "Pattern 2");
        }
    }

    #[test]
    fn drop_spots_never_collide() {
        let mut e = engine();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 1}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 4, "lane": 0}),
        )
        .unwrap();
        let p = e.project.clone();
        assert_eq!(song_end_bar(&p), 5);
        assert!(
            !lane_is_free(&p, 0, 0, 1) && lane_is_free(&p, 0, 1, 3) && !lane_is_free(&p, 0, 1, 4)
        );
        assert_eq!(drop_spot(&p, 2, Some((0, 1))), (0, 1));
        assert_eq!(drop_spot(&p, 4, Some((0, 1))), (1, 1));
        assert_eq!(drop_spot(&p, 1, Some((0, 0))), (2, 0));
        assert_eq!(drop_spot(&p, 3, None), (0, 5));
    }
}
