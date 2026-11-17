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

/// Keep each step clickable; long patterns scroll instead of shrinking to slivers.
pub fn rack_content_width(available: f32, steps: usize) -> f32 {
    available.max(RACK_HEAD_W + steps as f32 * 12.0 + 6.0)
}

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
    pub open_step_velocity: Option<usize>,
    pub step_velocity_entries: Vec<egui::Response>,
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
    sel.open_step_velocity = None;
    sel.step_velocity_entries.clear();
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
            let name_rect = Rect::from_min_max(
                g.name.min + vec2(8.0, 0.0),
                g.name.max - vec2(5.0, if drum { 0.0 } else { 12.0 }),
            );
            bounded_label(
                &painter,
                name_rect,
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
        #[cfg(test)]
        super::step_velocity::tests::observe(ui.ctx().clone(), &format!("rack-name-{ti}"), &nr);
        let mut start_rename = nr.double_clicked();
        nr.context_menu(|ui| {
            let response = ui.add_enabled(drum, egui::Button::new("Edit step velocity..."));
            #[cfg(test)]
            super::step_velocity::tests::observe(
                ui.ctx().clone(),
                &format!("velocity-menu-{ti}"),
                &response,
            );
            sel.step_velocity_entries.push(response.clone());
            if response.clicked() {
                sel.selected = ti;
                sel.open_step_velocity = Some(ti);
                ui.close_menu();
            }
            if !drum {
                ui.label(super::step_velocity::DRUM_ONLY);
            }
            if ui.button("Rename").clicked() {
                start_rename = true;
                ui.close_menu();
            }
        });
        if start_rename {
            ui.data_mut(|d| d.insert_temp(rid, (t.name.clone(), t.name.clone(), true)));
        }
        let keys_response = keys_button(ui, g.keys, keys_id, selected, color);
        #[cfg(test)]
        super::step_velocity::tests::observe(
            ui.ctx().clone(),
            &format!("rack-keys-{ti}"),
            &keys_response,
        );
        if keys_response.clicked() {
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
                            vec2(4.0, h),
                        ),
                        1.0,
                        Color32::from_white_alpha(210),
                    );
                    if resp.hovered() {
                        let dy = ui.input(|i| i.raw_scroll_delta.y);
                        if dy.abs() > 0.5 {
                            let add = if dy > 0.0 { 0.05 } else { -0.05 };
                            actions.push(("edit_notes", json!({"track": t.name, "pattern": pat.name, "from": s, "to": s + 1, "vel_add": add})));
                        }
                        // the wheel belongs to this step while the pointer is on it: egui
                        // spreads one notch over several frames, so the page's share has to be
                        // taken every frame, or it keeps scrolling and the next notch lands
                        // on a different row
                        ui.input_mut(|i| {
                            i.smooth_scroll_delta = egui::Vec2::ZERO;
                            i.raw_scroll_delta = egui::Vec2::ZERO;
                        });
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

/// Leave room for New, length controls and Send, not just the pattern buttons.
fn visible_pattern_slots(width: f32) -> usize {
    ((width - SEND_BTN_W - 172.0).max(0.0) / PATTERN_BTN_W) as usize
}

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
    let user: Vec<usize> = (0..p.patterns.len())
        .filter(|&i| !p.patterns[i].name.starts_with(crate::tools_fl::PL))
        .collect();
    let fit = visible_pattern_slots(ui.available_width()).max(1);
    if user.len() > fit {
        ui.horizontal(|ui| {
            ui.label("All patterns");
            egui::ComboBox::from_id_salt("all-rack-patterns")
                .selected_text(
                    p.patterns
                        .get(*current)
                        .map(|p| p.name.as_str())
                        .unwrap_or("Choose pattern"),
                )
                .width(160.0)
                .show_ui(ui, |ui| {
                    for &i in &user {
                        ui.selectable_value(current, i, &p.patterns[i].name);
                    }
                });
            ui.label(format!("{} patterns", user.len()));
        });
    }
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), PATTERN_BAR_H), Sense::hover());
    let painter = ui.painter_at(row);
    let h = PATTERN_BAR_H - 4.0;
    let send = Rect::from_min_size(
        pos2(row.right() - SEND_BTN_W, row.top() + 2.0),
        vec2(SEND_BTN_W - 2.0, h),
    );

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
        bounded_label(
            &painter,
            r.shrink2(vec2(4.0, 1.0)),
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

/// Populated-rack presentation only. Commands retain pattern_bar's arguments;
/// rename/duplicate intents are dispatched by Studio under its existing owner.
#[derive(Default)]
pub struct CompactPatternResult {
    pub actions: Vec<(&'static str, Value)>,
    pub rename: Option<String>,
    pub duplicate: bool,
}

pub fn compact_pattern_bar(
    ui: &mut egui::Ui,
    p: &Project,
    current: &mut usize,
    target: Option<(u32, u32)>,
    pattern_rename: &mut Option<(String, String)>,
) -> CompactPatternResult {
    let mut result = CompactPatternResult::default();
    let width = ui.available_width();
    #[cfg(test)]
    compact_pattern_tests::begin(ui.ctx());
    #[cfg(test)]
    compact_pattern_tests::begin_responses(ui.ctx());
    let row = ui.horizontal_wrapped(|ui| {
        // width() is only a minimum in egui 0.29.1. Retain the finite child
        // and truncated text; diagnostics observe actual widget/wrapper growth.
        let picker_width = width.min(100.0);
        // Match ComboBox's content + padding and interaction minimum before
        // the centered wrapping row allocates the selector's outer child.
        let picker_spacing = ui.spacing();
        let picker_text_width = picker_width
            - 2.0 * picker_spacing.button_padding.x
            - picker_spacing.icon_spacing
            - picker_spacing.icon_width;
        let picker_galley = egui::WidgetText::from(
            p.patterns.get(*current).map(|q| q.name.as_str()).unwrap_or("Choose pattern"),
        ).into_galley(ui, Some(egui::TextWrapMode::Truncate), picker_text_width, egui::TextStyle::Button);
        let picker_height = (picker_galley.size().y.max(picker_spacing.icon_width)
            + 2.0 * picker_spacing.button_padding.y)
            .max(picker_spacing.interact_size.y);
        #[cfg(test)]
        compact_pattern_tests::trace_budget(ui, "selector", vec2(picker_width, picker_height),
            p.patterns.get(*current).map(|q| q.name.as_str()).unwrap_or("Choose pattern"), true);
        let picker = ui.allocate_ui_with_layout(
            vec2(picker_width, picker_height), egui::Layout::top_down(egui::Align::LEFT), |ui| {
                let picker_enabled = ui.is_enabled();
                egui::ComboBox::from_id_salt("all-rack-patterns")
                    .width(picker_width)
                    .truncate()
                    .selected_text(p.patterns.get(*current).map(|q| q.name.as_str()).unwrap_or("Choose pattern"))
                    .show_ui(ui, |ui| {
                        // Popup Areas do not inherit the owning Ui enabled state.
                        if !picker_enabled { ui.disable(); }
                        // The built-in vertical ScrollArea keeps every entry;
                        // wrap full names instead of allowing an offscreen menu.
                        ui.set_width(width.clamp(100.0, 320.0));
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        for (i, pat) in p.patterns.iter().enumerate().filter(|(_, q)| !q.name.starts_with(crate::tools_fl::PL)) {
                            let choice = ui.selectable_value(current, i, &pat.name).on_hover_text(format!("{}\nPattern ID: {}", pat.name, pat.id));
                            #[cfg(test)]
                            compact_pattern_tests::record(ui.ctx(), &format!("choice:{i}"), choice.rect);
                            #[cfg(test)]
                            compact_pattern_tests::record_response(ui.ctx(), &format!("choice:{i}"), &choice);
                            let _ = choice;
                        }
                    }).response
            },
        );
        #[cfg(test)]
        compact_pattern_tests::trace_wrapper(ui, "selector-wrapper", &picker.response);
        let picker = picker.inner;
        #[cfg(test)]
        compact_pattern_tests::record(ui.ctx(), "selector", picker.rect);
        #[cfg(test)]
        compact_pattern_tests::record_response(ui.ctx(), "selector", &picker);
        if let Some(selected) = p.patterns.get(*current) {
            picker.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), format!("Pattern: {}", selected.name)));
            picker.on_hover_text(format!("{}\nPattern ID: {}\nChoose from all editable patterns", selected.name, selected.id));
        }
        if let Some(cur) = p.patterns.get(*current) {
            let bars = cur.bars;
            let new_end = (bars.saturating_sub(1) * cur.steps_per_bar()) as f32;
            let shrink_blocked = cur.clips.values().any(|v| v.iter().any(|n| n.start >= new_end));
            let minus = ui.add_enabled(bars > 1 && !shrink_blocked, egui::Button::new("\u{2212}"))
                .on_hover_text(if shrink_blocked { "The last bar has notes. Clear them first (nothing is deleted for you)." } else { "Shorter pattern" });
            #[cfg(test)]
            compact_pattern_tests::record(ui.ctx(), "minus", minus.rect);
            #[cfg(test)]
            compact_pattern_tests::record_response(ui.ctx(), "minus", &minus);
            if minus.clicked() {
                result.actions.push(("set_pattern_length", json!({"pattern":cur.name,"bars":bars-1})));
            }
            // Pre-layout against the FULL row, not its trailing sliver. Passing
            // this same galley to Label selects its normal atomic allocation:
            // the parent wraps the whole widget before Label paints any glyphs.
            let length_text = format!("{bars} bar{}", if bars == 1 { "" } else { "s" });
            let length_galley = egui::WidgetText::from(length_text.as_str()).into_galley(
                ui, Some(egui::TextWrapMode::Wrap), width, egui::TextStyle::Body,
            );
            #[cfg(test)]
            compact_pattern_tests::trace_length(ui, &length_text, &length_galley);
            let length = ui.add(egui::Label::new(length_galley));
            #[cfg(test)]
            compact_pattern_tests::record(ui.ctx(), "bars", length.rect);
            #[cfg(test)]
            compact_pattern_tests::record_response(ui.ctx(), "bars", &length);
            let _ = length;
            let plus = ui.add_enabled(bars < 64, egui::Button::new("+"))
                .on_hover_text("Longer pattern: more steps to fill, and a longer loop");
            #[cfg(test)]
            compact_pattern_tests::record(ui.ctx(), "plus", plus.rect);
            #[cfg(test)]
            compact_pattern_tests::record_response(ui.ctx(), "plus", &plus);
            if plus.clicked() {
                result.actions.push(("set_pattern_length", json!({"pattern":cur.name,"bars":bars+1})));
            }
        }
        let add = ui.add(egui::Button::new("New").wrap()).on_hover_text("New empty pattern, as long as the selected pattern");
        #[cfg(test)]
        compact_pattern_tests::record(ui.ctx(), "new", add.rect);
        #[cfg(test)]
        compact_pattern_tests::record_response(ui.ctx(), "new", &add);
        if add.clicked() {
            let bars = p.patterns.get(*current).map(|q| q.bars).unwrap_or(1).max(1);
            result.actions.push(("add_pattern", json!({"name":new_pattern_name(p),"bars":bars})));
            *current = p.patterns.len();
        }
        let selected = p.patterns.get(*current).filter(|q| !q.name.starts_with(crate::tools_fl::PL));
        let send = ui.add_enabled(selected.is_some(), egui::Button::new("Send to song").wrap());
        #[cfg(test)]
        compact_pattern_tests::record(ui.ctx(), "send", send.rect);
        #[cfg(test)]
        compact_pattern_tests::record_response(ui.ctx(), "send", &send);
        if let Some(pat) = selected {
            let (lane, bar) = drop_spot(p, pat.bars, target);
            if send.on_hover_text(format!("place {} on playlist track {} at bar {}", pat.name, lane+1, bar+1)).clicked() {
                result.actions.push(("place_pattern", json!({"pattern":pat.name,"start_bar":bar,"lane":lane})));
            }
            let identity = format!("{}:{}", pat.id, pat.name);
            if pattern_rename.as_ref().is_none_or(|v| v.0 != identity) {
                *pattern_rename = Some((identity, pat.name.clone()));
            }
        }
        #[cfg(test)]
        compact_pattern_tests::trace_menu_budget(ui, width);
        // Negotiate an atomic finite budget with the outer wrapping placer
        // before a child scope can inherit only the trailing row sliver.
        let menu_width = (egui::WidgetText::from("Pattern actions").into_galley(
            ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button,
        ).size().x + 2.0 * ui.spacing().button_padding.x).min(width);
        let menu = ui.allocate_ui_with_layout(
            vec2(menu_width, ui.spacing().interact_size.y),
            egui::Layout::top_down(egui::Align::LEFT), |ui| {
            if selected.is_none() { ui.disable(); }
            let management_enabled = ui.is_enabled();
            egui::menu::menu_custom_button(ui, egui::Button::new("Pattern actions").wrap(), |ui| {
                // Reapply the current owner state, including an inherited disable,
                // before TextEdit can process focus/keyboard input in this Area.
                if !management_enabled { ui.disable(); }
                ui.set_width(width.clamp(100.0, 320.0));
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                if let Some(pat) = selected {
                    ui.add(egui::Label::new(&pat.name).wrap());
                    ui.label("Pattern name");
                    let draft = &mut pattern_rename.as_mut().unwrap().1;
                    let edit = ui.add(egui::TextEdit::singleline(draft).interactive(management_enabled).desired_width(ui.available_width()));
                    #[cfg(test)]
                    compact_pattern_tests::record(ui.ctx(), "draft", edit.rect);
                    #[cfg(test)]
                    compact_pattern_tests::record_response(ui.ctx(), "draft", &edit);
                    let _ = edit;
                    let rename = ui.add_enabled(!draft.trim().is_empty() && draft.trim()!=pat.name, egui::Button::new("Rename").wrap())
                        .on_hover_text("Rename this pattern and its song/playlist references. Notes, stable IDs and timing stay unchanged. Undo restores.");
                    #[cfg(test)]
                    compact_pattern_tests::record(ui.ctx(), "rename", rename.rect);
                    #[cfg(test)]
                    compact_pattern_tests::record_response(ui.ctx(), "rename", &rename);
                    if rename.clicked() { result.rename = Some(draft.trim().to_owned()); }
                    let duplicate = ui.add(egui::Button::new("Duplicate pattern").wrap())
                        .on_hover_text("Make an independent copy with the same notes and meter. Selects the copy for editing, without placing it in the song. Undo removes it.");
                    #[cfg(test)]
                    compact_pattern_tests::record(ui.ctx(), "duplicate", duplicate.rect);
                    #[cfg(test)]
                    compact_pattern_tests::record_response(ui.ctx(), "duplicate", &duplicate);
                    result.duplicate = duplicate.clicked();
                }
            }).response
        });
        #[cfg(test)]
        compact_pattern_tests::trace_wrapper(ui, "manage-wrapper", &menu.response);
        let menu = menu.inner;
        #[cfg(test)]
        compact_pattern_tests::record(ui.ctx(), "manage", menu.rect);
        #[cfg(test)]
        compact_pattern_tests::record_response(ui.ctx(), "manage", &menu);
        let _ = menu;
    });
    #[cfg(test)]
    compact_pattern_tests::finish(ui.ctx(), row.response.rect);
    let _ = row;
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_labels_elide_inside_their_own_hit_rect() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(72.0, 20.0));
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                bounded_label(
                    ui.painter(),
                    rect,
                    Align2::CENTER_CENTER,
                    "Extended chorus groove with bass fills",
                    FontId::proportional(10.5),
                    Color32::WHITE,
                );
            });
        });
        let shape = out
            .shapes
            .iter()
            .find(|s| matches!(s.shape, egui::epaint::Shape::Text(_)))
            .unwrap();
        let egui::epaint::Shape::Text(text) = &shape.shape else {
            unreachable!()
        };
        assert!(text.galley.elided);
        assert!(text.galley.size().x <= rect.width());
        assert!(rect.contains_rect(shape.clip_rect));
        assert!(rect.contains_rect(Rect::from_min_size(text.pos, text.galley.size())));
    }

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
    fn pattern_slots_leave_space_for_all_actions() {
        for width in [420.0_f32, 700.0, 1000.0, 1440.0] {
            let used = visible_pattern_slots(width) as f32 * PATTERN_BTN_W + SEND_BTN_W + 172.0;
            assert!(used <= width);
            assert!(width - used < PATTERN_BTN_W);
        }
        assert_eq!(visible_pattern_slots(700.0), 5);
    }

    #[test]
    fn narrow_and_long_patterns_keep_twelve_point_steps() {
        for available in [420.0_f32, 700.0, 1440.0] {
            for steps in [16_usize, 64, 1024] {
                let width = rack_content_width(available, steps);
                assert!(width >= available);
                assert!((width - RACK_HEAD_W - 6.0) / steps as f32 >= 12.0);
            }
        }
        assert_eq!(rack_content_width(1440.0, 16), 1440.0);
    }

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

    /// The wheel over a lit step changes that step's velocity and nothing else:
    /// the page behind it must not scroll, or the next notch lands on another row.
    #[test]
    fn the_wheel_over_a_step_edits_velocity_and_does_not_scroll_the_page() {
        let mut e = engine();
        e.call(
            "toggle_step",
            &json!({"track": "kick", "step": 5, "pattern": "verse"}),
        )
        .unwrap();
        let p = e.project.clone();
        let pi = pattern_index(&p);
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 300.0));
        let mut sel = RackSel::default();
        let mut calls: Vec<(&'static str, Value)> = Vec::new();
        let mut offset = 0.0_f32;
        let mut run = |events: Vec<Event>, k: usize, sel: &mut RackSel| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out = egui::ScrollArea::vertical().show(ui, |ui| {
                        calls.extend(channel_rack(ui, &p, pi, sel, None));
                        ui.allocate_space(vec2(10.0, 2000.0));
                    });
                    offset = out.state.offset.y;
                });
            });
        };
        run(vec![], 0, &mut sel);
        run(vec![], 1, &mut sel);
        let kick = p.track_index("kick").unwrap();
        let at = sel.rows[kick].step_rect(5).center();
        run(vec![Event::PointerMoved(at)], 2, &mut sel);
        for k in 0..3 {
            run(
                vec![Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, -1.0),
                    modifiers: Modifiers::default(),
                }],
                3 + k,
                &mut sel,
            );
        }
        // egui spreads a notch over several frames: let them all pass
        for k in 10..20 {
            run(vec![], k, &mut sel);
        }
        let edits: Vec<_> = calls.iter().filter(|(t, _)| *t == "edit_notes").collect();
        assert_eq!(edits.len(), 3, "three notches, three edits: {calls:?}");
        assert!(edits
            .iter()
            .all(|(_, a)| a["vel_add"].as_f64().unwrap() < 0.0));
        assert_eq!(offset, 0.0, "the page scrolled under the wheel");
    }
}

#[cfg(test)]
mod compact_pattern_tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton};

    #[derive(Clone)]
    pub(super) struct Observation {
        pub row: Rect,
        pub controls: Vec<(String, Rect)>,
    }

    fn observation_key() -> egui::Id {
        egui::Id::new("compact-pattern-header-current-pass")
    }

    pub(super) fn begin(ctx: &egui::Context) {
        ctx.data_mut(|d| {
            d.insert_temp(
                observation_key(),
                Observation {
                    row: Rect::NOTHING,
                    controls: Vec::new(),
                },
            )
        });
    }

    pub(super) fn record(ctx: &egui::Context, name: &str, rect: Rect) {
        ctx.data_mut(|d| {
            let mut o = d.get_temp::<Observation>(observation_key()).unwrap();
            o.controls.push((name.to_owned(), rect));
            d.insert_temp(observation_key(), o);
        });
    }

    pub(super) fn finish(ctx: &egui::Context, row: Rect) {
        ctx.data_mut(|d| {
            let mut o = d.get_temp::<Observation>(observation_key()).unwrap();
            o.row = row;
            d.insert_temp(observation_key(), o);
        });
    }

    fn fixture() -> Project {
        let mut patterns: Vec<_> = (0..40)
            .map(|i| {
                let mut q = crate::project::Pattern::new(
                    &format!("Variation {i}: extended chorus groove with bass fills"),
                    4,
                );
                q.id = format!("pattern-{i}");
                q
            })
            .collect();
        patterns.push(crate::project::Pattern::new("pl:generated", 4));
        Project {
            patterns,
            ..Project::default()
        }
    }

    #[derive(Default)]
    struct HeaderState {
        current: usize,
        draft: Option<(String, String)>,
        time: usize,
    }

    fn header_frame(
        ctx: &egui::Context,
        p: &Project,
        state: &mut HeaderState,
        width: f32,
        scale: f32,
        events: Vec<Event>,
    ) -> (CompactPatternResult, Observation, egui::FullOutput) {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 2000.0));
        let mut input = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(5000.0 + state.time as f64 * 0.05),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        state.time += 1;
        let mut result = CompactPatternResult::default();
        let output = ctx.run(input, |ctx| {
            assert_eq!(ctx.pixels_per_point(), scale);
            assert_eq!(ctx.screen_rect(), screen);
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    result = compact_pattern_bar(ui, p, &mut state.current, None, &mut state.draft);
                    trace_current_pass(ctx, width, scale);
                });
        });
        let observed = ctx
            .data(|d| d.get_temp::<Observation>(observation_key()))
            .unwrap();
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                eprintln!("compact paint width={width} dpi={scale} text={:?} pos={:?} size={:?} mesh_bound={:?} clip={:?} elided={}",
                    text.galley.text(), text.pos, text.galley.size(),
                    text.galley.mesh_bounds.translate(text.pos.to_vec2()), shape.clip_rect, text.galley.elided);
            }
        }
        (result, observed, output)
    }

    fn control(o: &Observation, name: &str) -> Rect {
        o.controls
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .1
    }

    fn click_control(
        ctx: &egui::Context,
        p: &Project,
        state: &mut HeaderState,
        name: &str,
    ) -> CompactPatternResult {
        let (_, o, _) = header_frame(ctx, p, state, 580.0, 1.0, vec![]);
        let at = control(&o, name).center();
        let mut result = CompactPatternResult::default();
        for events in [
            vec![Event::PointerMoved(at)],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }],
        ] {
            let (r, _, _) = header_frame(ctx, p, state, 580.0, 1.0, events);
            result.actions.extend(r.actions);
            if r.rename.is_some() {
                result.rename = r.rename;
            }
            result.duplicate |= r.duplicate;
        }
        result
    }

    #[test]
    fn compact_header_first_pass_widgets_and_paint_fit_native_dpi_and_styles() {
        let p = fixture();
        for scale in [1.0, 2.0] {
            for enlarged in [false, true] {
                for width in [100.0, 184.0, 286.0, 292.0, 320.0, 580.0, 1000.0] {
                    let ctx = egui::Context::default();
                    if enlarged {
                        let mut style = (*ctx.style()).clone();
                        style
                            .text_styles
                            .insert(egui::TextStyle::Body, FontId::proportional(20.0));
                        style
                            .text_styles
                            .insert(egui::TextStyle::Button, FontId::proportional(22.0));
                        style.spacing.button_padding = vec2(7.0, 7.0);
                        style.spacing.interact_size.y = 32.0;
                        ctx.set_style(style);
                    }
                    let mut state = HeaderState {
                        current: 31,
                        ..Default::default()
                    };
                    let (r, o, output) = header_frame(&ctx, &p, &mut state, width, scale, vec![]);
                    assert!(r.actions.is_empty() && r.rename.is_none() && !r.duplicate);
                    assert_eq!(state.current, 31);
                    assert_eq!(
                        state.draft.as_ref().unwrap().0,
                        format!("{}:{}", p.patterns[31].id, p.patterns[31].name)
                    );
                    for (i, (_, rect)) in o.controls.iter().enumerate() {
                        assert!(o.row.expand(1.0 / scale).contains_rect(*rect));
                        assert!(rect.left() >= -1.0 / scale && rect.right() <= width + 1.0 / scale);
                        for (_, other) in o.controls.iter().skip(i + 1) {
                            assert!(
                                !rect.intersect(*other).is_positive(),
                                "overlap {rect:?} {other:?}"
                            );
                        }
                    }
                    for label in ["4 bars", "New", "Send to song", "Pattern actions"] {
                        assert!(
                            output.shapes.iter().any(|s| {
                                let egui::Shape::Text(t) = &s.shape else {
                                    return false;
                                };
                                t.galley.text() == label && !t.galley.elided
                            }),
                            "missing full label {label} at {width}"
                        );
                    }
                    for s in &output.shapes {
                        if let egui::Shape::Text(t) = &s.shape {
                            let painted = Rect::from_min_size(t.pos, t.galley.size());
                            assert!(s.clip_rect.expand(1.0 / scale).contains_rect(painted));
                            assert!(o.row.expand(1.0 / scale).contains_rect(painted));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn compact_header_reclaims_a_management_row_at_narrow_inspector_width() {
        let p = fixture();
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            let mut state = HeaderState::default();
            let (_, compact, _) = header_frame(&ctx, &p, &mut state, 580.0, scale, vec![]);
            let tallest = compact
                .controls
                .iter()
                .map(|(_, r)| r.height())
                .fold(0.0_f32, f32::max);
            assert!(
                compact.row.height() <= tallest + 1.0 / scale,
                "primary row wrapped at 580"
            );
            // Source-layout reference: unchanged production pattern_bar, followed
            // by the baseline's visible management widgets. Not a native image.
            let legacy_ctx = egui::Context::default();
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(580.0, 2000.0))),
                time: Some(5000.0),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let mut legacy_height = 0.0;
            let _ = legacy_ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        let top = ui.next_widget_position().y;
                        let mut current = 0;
                        let mut geo = PatternBarGeo::default();
                        let _ = pattern_bar(ui, &p, &mut current, None, &mut geo);
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Pattern name");
                            let mut draft = p.patterns[0].name.clone();
                            ui.add(egui::TextEdit::singleline(&mut draft).desired_width(180.0));
                            let _ = ui.add_enabled(false, egui::Button::new("Rename"));
                            let _ = ui.button("Duplicate pattern");
                        });
                        legacy_height = ui.next_widget_position().y - top;
                    });
            });
            assert!(
                legacy_height - compact.row.height() >= 20.0,
                "no material height gain"
            );
        }
    }

    #[test]
    fn compact_real_widgets_request_selection_length_new_send_and_management() {
        let p = fixture();
        let ctx = egui::Context::default();
        let mut state = HeaderState::default();
        assert!(click_control(&ctx, &p, &mut state, "selector")
            .actions
            .is_empty());
        let (_, open, _) = header_frame(&ctx, &p, &mut state, 580.0, 1.0, vec![]);
        assert!(open.controls.iter().any(|(n, _)| n == "choice:39"));
        assert!(!open.controls.iter().any(|(n, _)| n == "choice:40"));
        // A visible entry's real response, not a cached prior-pass response.
        let chosen = click_control(&ctx, &p, &mut state, "choice:1");
        assert!(chosen.actions.is_empty());
        assert_eq!(state.current, 1);
        let plus = click_control(&ctx, &p, &mut state, "plus");
        assert_eq!(
            plus.actions,
            vec![(
                "set_pattern_length",
                json!({"pattern":p.patterns[1].name,"bars":5})
            )]
        );
        let minus = click_control(&ctx, &p, &mut state, "minus");
        assert_eq!(
            minus.actions,
            vec![(
                "set_pattern_length",
                json!({"pattern":p.patterns[1].name,"bars":3})
            )]
        );
        let send = click_control(&ctx, &p, &mut state, "send");
        assert_eq!(
            send.actions,
            vec![(
                "place_pattern",
                json!({"pattern":p.patterns[1].name,"start_bar":0,"lane":0})
            )]
        );
        let _ = click_control(&ctx, &p, &mut state, "manage");
        state.draft.as_mut().unwrap().1 = "Renamed variation".to_owned();
        let rename = click_control(&ctx, &p, &mut state, "rename");
        assert_eq!(rename.rename.as_deref(), Some("Renamed variation"));
        let duplicate = click_control(&ctx, &p, &mut state, "duplicate");
        assert!(duplicate.duplicate && duplicate.actions.is_empty());
        // These are UI request assertions. Stable-id dispatch, independent copy
        // and stale-snapshot return remain in Studio, not a fake engine log here.
        let fresh_ctx = egui::Context::default();
        let mut fresh = HeaderState::default();
        let new = click_control(&fresh_ctx, &p, &mut fresh, "new");
        assert_eq!(
            new.actions,
            vec![("add_pattern", json!({"name":"Pattern 1","bars":4}))]
        );
        assert_eq!(fresh.current, p.patterns.len());
    }

    // Diagnostic-only probes: no allocation, painting, response substitution,
    // or wrapper insertion into the independent-control observation list.
    pub(super) fn trace_budget(
        ui: &egui::Ui,
        name: &str,
        requested: egui::Vec2,
        text: &str,
        selector: bool,
    ) {
        let natural = egui::WidgetText::from(text).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Button,
        );
        eprintln!("compact budget pass={} name={name} requested={requested:?} parent_cursor={:?} parent_max={:?} available_before={:?} clip={:?} natural_text_size={:?} icon_width={} icon_spacing={} button_padding={:?} selector={selector}; requested is not a hard cap; internal ComboBox content min_rect is not observed here",
            ui.ctx().cumulative_pass_nr(), ui.cursor(), ui.max_rect(), ui.available_rect_before_wrap(), ui.clip_rect(),
            natural.size(), ui.spacing().icon_width, ui.spacing().icon_spacing, ui.spacing().button_padding);
    }

    pub(super) fn trace_menu_budget(ui: &egui::Ui, width: f32) {
        let measured = egui::WidgetText::from("Pattern actions").into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Button,
        );
        let requested = (measured.size().x + 2.0 * ui.spacing().button_padding.x).min(width);
        trace_budget(
            ui,
            "manage",
            vec2(requested, ui.spacing().interact_size.y),
            "Pattern actions",
            false,
        );
    }

    pub(super) fn trace_wrapper(ui: &egui::Ui, tag: &str, response: &egui::Response) {
        eprintln!("compact wrapper-only pass={} tag={tag} rect={:?} interact_rect={:?} clip={:?}; NOT an independent control",
            ui.ctx().cumulative_pass_nr(), response.rect, response.interact_rect, ui.clip_rect());
    }

    pub(super) fn trace_length(ui: &egui::Ui, text: &str, atomic: &egui::Galley) {
        // Reconstruct the old inline layout JOB only, from archive-bound Label
        // source. This is a counterfactual probe, not an old real Response.
        let mut old_job = egui::WidgetText::from(text).into_layout_job(
            ui.style(),
            egui::FontSelection::Default,
            ui.text_valign(),
        );
        old_job.wrap.max_width = ui.available_width();
        old_job.first_row_min_height = ui.cursor().height();
        old_job.halign = egui::Align::Min;
        old_job.justify = false;
        if let Some(section) = old_job.sections.first_mut() {
            section.leading_space = ui.available_width() - ui.available_size_before_wrap().x;
        }
        let old = ui.fonts(|fonts| fonts.layout_job(old_job));
        let old_pos = pos2(ui.max_rect().left(), ui.cursor().top());
        eprintln!("compact length-before pass={} cursor={:?} max={:?} available_before={:?} full_budget={} clip={:?} Body={:?}",
            ui.ctx().cumulative_pass_nr(), ui.cursor(), ui.max_rect(), ui.available_rect_before_wrap(),
            atomic.job.wrap.max_width, ui.clip_rect(), ui.style().text_styles.get(&egui::TextStyle::Body));
        for (index, row) in old.rows.iter().enumerate() {
            eprintln!("compact old-inline-JOB-PROBE row={index} rect={:?} mesh_bound={:?} origin={old_pos:?}; NO allocation or actual old response",
                row.rect.translate(old_pos.to_vec2()), row.visuals.mesh_bounds.translate(old_pos.to_vec2()));
        }
        for (index, row) in atomic.rows.iter().enumerate() {
            eprintln!("compact new-atomic-SAME-galley row={index} local_rect={:?} local_mesh_bound={:?} size={:?} elided={}; actual paint position follows in paint log",
                row.rect, row.visuals.mesh_bounds, atomic.size(), atomic.elided);
        }
    }

    fn trace_current_pass(ctx: &egui::Context, width: f32, scale: f32) {
        let pass = ctx.cumulative_pass_nr();
        let style = ctx.style();
        let enlarged = style.text_styles.get(&egui::TextStyle::Body)
            == Some(&FontId::proportional(20.0))
            && style.text_styles.get(&egui::TextStyle::Button) == Some(&FontId::proportional(22.0));
        let rows = ctx
            .data(|d| d.get_temp::<Vec<(u64, String, egui::Response)>>(responses_key()))
            .unwrap();
        let observation = ctx
            .data(|d| d.get_temp::<Observation>(observation_key()))
            .unwrap();
        eprintln!("compact case width={width} dpi={scale} enlarged={enlarged} pass={pass} row={:?} Body={:?} Button={:?} padding={:?}",
            observation.row, style.text_styles.get(&egui::TextStyle::Body), style.text_styles.get(&egui::TextStyle::Button), style.spacing.button_padding);
        for (stamp, name, response) in &rows {
            eprintln!("compact real-response case=({width},{scale},{enlarged}) pass={pass} stamp={stamp} name={name} rect={:?} interact_rect={:?} id={:?} enabled={}",
                response.rect, response.interact_rect, response.id, response.enabled());
            assert_eq!(
                *stamp, pass,
                "diagnostic primary/popup response must belong to this ctx.run pass"
            );
        }
        for (name, _) in &observation.controls {
            assert!(
                rows.iter().any(|(_, n, _)| n == name),
                "missing real primary response {name}"
            );
        }
        // Log every named/indexed pair before the untouched old assertions.
        // No intersection, shrink, clipping, or substitution is done here.
        for (i, (name, rect)) in observation.controls.iter().enumerate() {
            eprintln!("compact control width={width} dpi={scale} enlarged={enlarged} pass={pass} index={i} name={name} rect={rect:?} row={:?}", observation.row);
            for (j, (other_name, other)) in observation.controls.iter().enumerate().skip(i + 1) {
                eprintln!("compact pair width={width} dpi={scale} enlarged={enlarged} pass={pass} i={i} name={name} rect={rect:?} j={j} other_name={other_name} other={other:?} row={:?}", observation.row);
            }
        }
    }

    fn responses_key() -> egui::Id {
        egui::Id::new("compact-pattern-header-current-responses")
    }

    pub(super) fn begin_responses(ctx: &egui::Context) {
        ctx.data_mut(|d| {
            d.insert_temp(responses_key(), Vec::<(u64, String, egui::Response)>::new())
        });
    }

    pub(super) fn record_response(ctx: &egui::Context, name: &str, response: &egui::Response) {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            let mut rows = d
                .get_temp::<Vec<(u64, String, egui::Response)>>(responses_key())
                .unwrap();
            rows.push((pass, name.to_owned(), response.clone()));
            d.insert_temp(responses_key(), rows);
        });
    }

    struct OwnerFrame {
        result: CompactPatternResult,
        observed: Observation,
        responses: Vec<(String, egui::Response)>,
    }

    fn owner_frame(
        ctx: &egui::Context,
        p: &Project,
        state: &mut HeaderState,
        enabled: bool,
        events: Vec<Event>,
    ) -> OwnerFrame {
        let mut result = CompactPatternResult::default();
        let mut responses = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(580.0, 2000.0))),
            time: Some(5000.0 + state.time as f64 * 0.05),
            events,
            ..Default::default()
        };
        state.time += 1;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    if !enabled {
                        ui.disable();
                    }
                    // The owner child inherits a disabled ancestor, with the same
                    // ID path on every enabled/disabled frame.
                    ui.scope(|ui| {
                        result =
                            compact_pattern_bar(ui, p, &mut state.current, None, &mut state.draft);
                    });
                    let rows = ctx
                        .data(|d| d.get_temp::<Vec<(u64, String, egui::Response)>>(responses_key()))
                        .unwrap();
                    responses = rows
                        .into_iter()
                        .map(|(pass, name, response)| {
                            assert_eq!(
                                pass,
                                ctx.cumulative_pass_nr(),
                                "not a current-pass response"
                            );
                            (name, response)
                        })
                        .collect();
                });
        });
        let observed = ctx
            .data(|d| d.get_temp::<Observation>(observation_key()))
            .unwrap();
        OwnerFrame {
            result,
            observed,
            responses,
        }
    }

    fn actual_response<'a>(frame: &'a OwnerFrame, name: &str) -> &'a egui::Response {
        &frame
            .responses
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("missing current response {name}"))
            .1
    }

    fn no_intents(frame: &OwnerFrame) {
        assert!(frame.result.actions.is_empty());
        assert!(frame.result.rename.is_none());
        assert!(!frame.result.duplicate);
    }

    fn owner_click(
        ctx: &egui::Context,
        p: &Project,
        state: &mut HeaderState,
        enabled: bool,
        name: &str,
    ) -> OwnerFrame {
        let settled = owner_frame(ctx, p, state, enabled, vec![]);
        no_intents(&settled);
        let at = control(&settled.observed, name).center();
        let moved = owner_frame(ctx, p, state, enabled, vec![Event::PointerMoved(at)]);
        no_intents(&moved);
        let pressed = owner_frame(
            ctx,
            p,
            state,
            enabled,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        );
        no_intents(&pressed);
        owner_frame(
            ctx,
            p,
            state,
            enabled,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }],
        )
    }

    #[test]
    fn open_management_popup_inherits_current_disabled_owner_before_editing_or_requests() {
        let p = fixture(); // Plain Project, no EnginePlayer or audio fixture.
        let ctx = egui::Context::default();
        let mut state = HeaderState::default();
        no_intents(&owner_click(&ctx, &p, &mut state, true, "manage"));
        // Finish the opening Area's sizing frame while still enabled.
        let _ = owner_frame(&ctx, &p, &mut state, true, vec![]);
        let ready = owner_frame(&ctx, &p, &mut state, true, vec![]);
        assert!(actual_response(&ready, "draft").enabled());
        let stable_id = actual_response(&ready, "draft").id;
        state.draft.as_mut().unwrap().1 = "Preserved draft intent".to_owned();
        let intended = state.draft.clone();
        let focused = owner_click(&ctx, &p, &mut state, true, "draft");
        no_intents(&focused);
        assert!(
            actual_response(&focused, "draft").has_focus(),
            "keyboard precondition must be real focus"
        );
        // First disabled frame gets a real keyboard event while the TextEdit
        // had focus. No prior-frame response lookup or forced memory focus oracle.
        let blocked = owner_frame(
            &ctx,
            &p,
            &mut state,
            false,
            vec![Event::Text("BLOCKED".to_owned())],
        );
        no_intents(&blocked);
        assert_eq!(state.draft, intended);
        assert_eq!(state.current, 0);
        for name in ["draft", "rename", "duplicate"] {
            assert!(!actual_response(&blocked, name).enabled());
        }
        let edit = actual_response(&blocked, "draft");
        assert_eq!(edit.id, stable_id);
        assert!(!edit.has_focus() && !edit.changed());
        for name in ["rename", "duplicate", "draft"] {
            let blocked = owner_click(&ctx, &p, &mut state, false, name);
            no_intents(&blocked);
            assert!(!actual_response(&blocked, name).enabled());
            assert_eq!(actual_response(&blocked, "draft").id, stable_id);
            assert_eq!(state.draft, intended);
            assert_eq!(state.current, 0);
        }
        // Reenabling does not throw away the pending draft or retag the menu.
        let resumed = owner_frame(&ctx, &p, &mut state, true, vec![]);
        assert_eq!(actual_response(&resumed, "draft").id, stable_id);
        assert!(actual_response(&resumed, "draft").enabled());
        assert_eq!(state.draft, intended);
        let rename = owner_click(&ctx, &p, &mut state, true, "rename");
        assert!(actual_response(&rename, "rename").clicked());
        assert_eq!(
            rename.result.rename.as_deref(),
            Some("Preserved draft intent")
        );
        assert!(rename.result.actions.is_empty() && !rename.result.duplicate);
        let duplicate = owner_click(&ctx, &p, &mut state, true, "duplicate");
        assert!(actual_response(&duplicate, "duplicate").clicked());
        assert!(
            duplicate.result.duplicate
                && duplicate.result.rename.is_none()
                && duplicate.result.actions.is_empty()
        );
        let focused = owner_click(&ctx, &p, &mut state, true, "draft");
        assert!(actual_response(&focused, "draft").has_focus());
        let edited = owner_frame(
            &ctx,
            &p,
            &mut state,
            true,
            vec![Event::Text("RESUMED".to_owned())],
        );
        no_intents(&edited);
        assert!(actual_response(&edited, "draft").changed());
        assert!(state.draft.as_ref().unwrap().1.contains("RESUMED"));
        // Primary-row controls remain owned by the disabled normal Ui too.
        for name in ["minus", "plus", "new", "send", "manage", "selector"] {
            let blocked = owner_click(&ctx, &p, &mut state, false, name);
            no_intents(&blocked);
            assert_eq!(state.current, 0);
        }
        // Intent assertions only: real Studio stable-ID dispatch is unchanged.
    }

    #[test]
    fn open_selection_popup_cannot_change_identity_under_a_disabled_ancestor() {
        let p = fixture();
        let ctx = egui::Context::default();
        let mut state = HeaderState::default();
        no_intents(&owner_click(&ctx, &p, &mut state, true, "selector"));
        let _ = owner_frame(&ctx, &p, &mut state, true, vec![]);
        let ready = owner_frame(&ctx, &p, &mut state, true, vec![]);
        assert!(actual_response(&ready, "choice:1").enabled());
        let intended = state.draft.clone();
        let blocked = owner_frame(&ctx, &p, &mut state, false, vec![]);
        no_intents(&blocked);
        assert!(!actual_response(&blocked, "choice:1").enabled());
        let clicked = owner_click(&ctx, &p, &mut state, false, "choice:1");
        no_intents(&clicked);
        assert!(!actual_response(&clicked, "choice:1").enabled());
        assert!(!actual_response(&clicked, "choice:1").clicked());
        assert_eq!(state.current, 0);
        assert_eq!(state.draft, intended);
        // ComboBox's CloseOnClick may close even a disabled popup after the
        // release. Reopen normally rather than assuming that lifecycle away.
        let settled = owner_frame(&ctx, &p, &mut state, true, vec![]);
        if !settled
            .observed
            .controls
            .iter()
            .any(|(n, _)| n == "choice:1")
        {
            no_intents(&owner_click(&ctx, &p, &mut state, true, "selector"));
        }
        let _ = owner_frame(&ctx, &p, &mut state, true, vec![]);
        let resumed = owner_frame(&ctx, &p, &mut state, true, vec![]);
        assert!(actual_response(&resumed, "choice:1").enabled());
        let chosen = owner_click(&ctx, &p, &mut state, true, "choice:1");
        no_intents(&chosen);
        assert!(actual_response(&chosen, "choice:1").clicked());
        assert_eq!(state.current, 1);
        assert_eq!(
            state.draft.as_ref().unwrap().0,
            format!("{}:{}", p.patterns[1].id, p.patterns[1].name)
        );
        assert_eq!(state.draft.as_ref().unwrap().1, p.patterns[1].name);
    }

    #[test]
    fn held_editor_selection_drag_cannot_edit_on_first_disabled_owner_frame() {
        let p = fixture(); // Real Project and widgets, no EnginePlayer.
        let ctx = egui::Context::default();
        let mut state = HeaderState::default();
        no_intents(&owner_click(&ctx, &p, &mut state, true, "manage"));
        state.draft.as_mut().unwrap().1 = "Held drag selection draft".to_owned();
        // Finish the Area sizing and settle the real editor before input.
        no_intents(&owner_frame(&ctx, &p, &mut state, true, vec![]));
        let ready = owner_frame(&ctx, &p, &mut state, true, vec![]);
        no_intents(&ready);
        let edit = actual_response(&ready, "draft");
        assert!(edit.enabled());
        let stable_id = edit.id;
        let rect = edit.rect;
        let start = pos2(rect.left() + rect.width() * 0.1, rect.center().y);
        let end = pos2(rect.left() + rect.width() * 0.7, rect.center().y);
        assert!(rect.contains(start) && rect.contains(end));
        let intended = state.draft.clone();
        no_intents(&owner_frame(
            &ctx,
            &p,
            &mut state,
            true,
            vec![Event::PointerMoved(start)],
        ));
        let pressed = owner_frame(
            &ctx,
            &p,
            &mut state,
            true,
            vec![Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }],
        );
        no_intents(&pressed);
        assert_eq!(actual_response(&pressed, "draft").id, stable_id);
        assert!(actual_response(&pressed, "draft").has_focus());
        assert!(ctx.input(|i| i.pointer.primary_down()));
        let dragged = owner_frame(&ctx, &p, &mut state, true, vec![Event::PointerMoved(end)]);
        no_intents(&dragged);
        let edit = actual_response(&dragged, "draft");
        assert_eq!(edit.id, stable_id);
        assert!(edit.enabled() && edit.dragged() && edit.has_focus());
        assert_eq!(ctx.dragged_id(), Some(stable_id));
        assert!(ctx.is_being_dragged(stable_id));
        assert!(ctx.input(|i| i.pointer.primary_down()));
        let selection = egui::text_edit::TextEditState::load(&ctx, stable_id)
            .unwrap()
            .cursor
            .char_range()
            .unwrap();
        assert_ne!(
            selection.primary.index, selection.secondary.index,
            "real pointer drag must select text"
        );
        assert_eq!(state.draft, intended);
        // No release, Escape, forced focus or drag-state injection intervenes.
        let blocked = owner_frame(
            &ctx,
            &p,
            &mut state,
            false,
            vec![Event::Text("BLOCKED_HELD_DRAG".to_owned())],
        );
        no_intents(&blocked);
        assert!(ctx.input(|i| i.pointer.primary_down()));
        assert_eq!(ctx.dragged_id(), Some(stable_id));
        assert!(ctx.is_being_dragged(stable_id));
        assert_eq!(state.draft, intended);
        assert_eq!(state.current, 0);
        let edit = actual_response(&blocked, "draft");
        assert_eq!(edit.id, stable_id);
        assert!(!edit.enabled() && !edit.has_focus() && !edit.changed());
        assert!(!ctx.memory(|m| m.has_focus(stable_id)));
        for name in ["rename", "duplicate"] {
            assert!(!actual_response(&blocked, name).enabled());
            assert!(!actual_response(&blocked, name).clicked());
        }
        // Release normally before reenabling and pointer-focus the same editor.
        let released = owner_frame(
            &ctx,
            &p,
            &mut state,
            false,
            vec![Event::PointerButton {
                pos: end,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }],
        );
        no_intents(&released);
        assert!(!ctx.input(|i| i.pointer.primary_down()));
        assert_eq!(state.draft, intended);
        let resumed = owner_frame(&ctx, &p, &mut state, true, vec![]);
        no_intents(&resumed);
        assert_eq!(actual_response(&resumed, "draft").id, stable_id);
        assert!(actual_response(&resumed, "draft").enabled());
        let focused = owner_click(&ctx, &p, &mut state, true, "draft");
        no_intents(&focused);
        assert_eq!(actual_response(&focused, "draft").id, stable_id);
        assert!(actual_response(&focused, "draft").has_focus());
        let edited = owner_frame(
            &ctx,
            &p,
            &mut state,
            true,
            vec![Event::Text("RESUMED_HELD_DRAG".to_owned())],
        );
        no_intents(&edited);
        let edit = actual_response(&edited, "draft");
        assert_eq!(edit.id, stable_id);
        assert!(edit.enabled() && edit.has_focus() && edit.changed());
        assert!(state
            .draft
            .as_ref()
            .unwrap()
            .1
            .contains("RESUMED_HELD_DRAG"));
        assert_ne!(state.draft, intended);
        assert_eq!(state.current, 0);
    }
}

#[cfg(test)]
mod velocity_menu_tests {
    use super::super::step_velocity::tests::{key, pointer, project, responses};
    use super::*;
    fn run(
        ctx: &egui::Context,
        p: &Project,
        sel: &mut RackSel,
        dpi: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(960.0, 600.0))),
            events,
            ..Default::default()
        };
        raw.viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(dpi);
        ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let actions = channel_rack(ui, p, 0, sel, None);
                assert!(actions.is_empty());
            });
        })
    }
    fn real(ctx: &egui::Context, name: &str) -> egui::Response {
        let rows = responses(ctx);
        let (pass, _, r) = rows.iter().rev().find(|(_, n, _)| n == name).unwrap();
        assert_eq!(*pass, ctx.cumulative_pass_nr() - 1);
        r.clone()
    }
    #[test]
    fn menu_is_row_specific_and_uses_exact_drum_or_one_shot_predicate() {
        for dpi in [1.0, 2.0] {
            for mode in 0..3 {
                let ctx = egui::Context::default();
                let mut p = project();
                let mut t = p.tracks[0].clone();
                t.name = "Clicked row".into();
                t.id = "t9999".into();
                if mode == 1 {
                    t.instrument = crate::instruments::Instrument::Sampler(Default::default());
                }
                if mode == 2 {
                    t.instrument = crate::instruments::preset("epiano").unwrap();
                }
                p.tracks.push(t);
                let mut sel = RackSel::default();
                run(&ctx, &p, &mut sel, dpi, vec![]);
                run(&ctx, &p, &mut sel, dpi, vec![]);
                let name = real(&ctx, "rack-name-1");
                let pos = name.rect.center();
                let secondary = |pressed| {
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Secondary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                };
                run(&ctx, &p, &mut sel, dpi, secondary(true));
                run(&ctx, &p, &mut sel, dpi, secondary(false));
                run(&ctx, &p, &mut sel, dpi, vec![]);
                let item = sel.step_velocity_entries.last().unwrap().clone();
                assert_eq!(item.enabled(), mode != 2);
                if mode != 2 {
                    run(&ctx, &p, &mut sel, dpi, pointer(item.rect.center(), true));
                    run(&ctx, &p, &mut sel, dpi, pointer(item.rect.center(), false));
                    assert_eq!(sel.open_step_velocity, Some(1));
                    assert_eq!(sel.selected, 1);
                    assert!(!sel.open_editor);
                } else {
                    let output = run(&ctx, &p, &mut sel, dpi, vec![]);
                    assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(t) if t.galley.text()==super::super::step_velocity::DRUM_ONLY)));
                    assert!(sel.open_step_velocity.is_none());
                }
            }
        }
    }
    #[test]
    fn context_item_keyboard_activation_preserves_rename_and_no_piano_request() {
        let ctx = egui::Context::default();
        let p = project();
        let mut sel = RackSel::default();
        run(&ctx, &p, &mut sel, 1.0, vec![]);
        let pos = real(&ctx, "rack-name-0").rect.center();
        for pressed in [true, false] {
            run(
                &ctx,
                &p,
                &mut sel,
                1.0,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        run(&ctx, &p, &mut sel, 1.0, vec![]);
        let item = sel.step_velocity_entries.last().unwrap().clone();
        ctx.memory_mut(|m| m.request_focus(item.id));
        let output = run(
            &ctx,
            &p,
            &mut sel,
            1.0,
            vec![key(egui::Key::Space, egui::Modifiers::NONE)],
        );
        assert_eq!(sel.open_step_velocity, Some(0));
        assert!(!sel.open_editor);
        assert!(output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape,egui::Shape::Text(t) if t.galley.text()=="Rename")));
    }
}
