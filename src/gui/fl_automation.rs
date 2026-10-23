//! Track automation lanes in the FL playlist: one row per automation lane
//! (track, bus or master; volume, pan, instrument.* or fx.*) under the clip
//! lanes, on the same bar axis, so they follow the playlist's scroll and
//! zoom. Click an empty spot in a row to add a breakpoint there (snapped to a
//! 16th), drag a breakpoint to move it (it stays between its neighbours),
//! right-click one to delete it, click the row's head to bypass / enable the
//! lane. Every gesture is one call to the same automation tool an MCP client
//! uses (set_automation_points), so it is one undo step.

use super::fl_playlist::{GridGeo, HEAD_W};
use crate::automation::{default_range, AutoPoint, AutomationLane};
use crate::project::Project;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use serde_json::{json, Value};

/// Height of one automation row.
pub const AUTO_H: f32 = 34.0;
/// The strip's caption above the rows.
pub const AUTO_CAPTION_H: f32 = 14.0;
/// Playlist bars are 4/4: a bar is 4 beats.
pub const BEATS_PER_BAR: f32 = 4.0;
/// Breakpoints snap to one 16th step (a quarter beat).
pub const SNAP_BEATS: f32 = 0.25;
/// Breakpoint handle radius (points); the hit area is a bit larger.
pub const POINT_R: f32 = 4.0;
const HIT_R: f32 = 7.0;

/// Where a lane's envelope is drawn, and the conversions every hit test
/// goes through (beats <-> x via the playlist's GridGeo, value <-> y via
/// the lane's display range).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoRow {
    pub rect: Rect,
    pub lo: f32,
    pub hi: f32,
}

impl AutoRow {
    pub fn x_of_beat(&self, geo: &GridGeo, beat: f32) -> f32 {
        geo.x_of(beat / BEATS_PER_BAR)
    }
    pub fn beat_at(&self, geo: &GridGeo, x: f32) -> f32 {
        geo.bar_at(x) * BEATS_PER_BAR
    }
    pub fn y_of(&self, v: f32) -> f32 {
        let t = ((v - self.lo) / (self.hi - self.lo).max(1e-6)).clamp(0.0, 1.0);
        self.rect.max.y - t * self.rect.height()
    }
    pub fn value_at(&self, y: f32) -> f32 {
        let t = ((self.rect.max.y - y) / self.rect.height().max(1e-6)).clamp(0.0, 1.0);
        self.lo + t * (self.hi - self.lo)
    }
}

/// The range a lane is drawn over: the parameter's usual range, widened to
/// every point the lane already has (so nothing is drawn off the row).
pub fn display_range(l: &AutomationLane) -> (f32, f32) {
    let (mut lo, mut hi) = default_range(&l.param);
    for p in &l.points {
        lo = lo.min(p.value);
        hi = hi.max(p.value);
    }
    if hi - lo < 1e-3 {
        hi = lo + 1.0;
    }
    (lo, hi)
}

/// Snap a beat to the 16th grid, never before the song start.
pub fn snap_beat(b: f32) -> f32 {
    ((b / SNAP_BEATS).round() * SNAP_BEATS).max(0.0)
}

/// Where point `i` lands after a drag to (beat, value): snapped, kept one
/// step clear of its neighbours (so it never jumps over or onto them),
/// never before beat 0 or past `max_beat`, value inside the row's range.
pub fn moved_point(
    l: &AutomationLane,
    i: usize,
    beat: f32,
    value: f32,
    row: &AutoRow,
    max_beat: f32,
) -> (f32, f32) {
    let lo_b = if i > 0 {
        l.points[i - 1].beat + SNAP_BEATS
    } else {
        0.0
    };
    let hi_b = l
        .points
        .get(i + 1)
        .map(|q| q.beat - SNAP_BEATS)
        .unwrap_or(max_beat.max(lo_b));
    let b = snap_beat(beat).clamp(lo_b, hi_b.max(lo_b));
    (b, value.clamp(row.lo, row.hi))
}

fn pt_json(p: &AutoPoint) -> Value {
    json!({"beat": p.beat, "value": p.value, "curve": p.curve})
}

/// Height the strip takes for a project.
pub fn strip_height(p: &Project) -> f32 {
    AUTO_CAPTION_H + AUTO_H * p.automation.len().max(1) as f32 + 2.0
}

/// The automation strip under the playlist grid. `geo` is the geometry the
/// grid used this frame (its bar axis, scroll and zoom). Returns the engine
/// calls the gestures ask for.
pub fn automation_lanes(
    ui: &mut egui::Ui,
    p: &Project,
    geo: &GridGeo,
) -> Vec<(&'static str, Value)> {
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let (full, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), strip_height(p)), Sense::hover());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, 0.0, super::theme::Tokens::DARK.playlist_bg);
    painter.text(
        pos2(full.min.x + 4.0, full.min.y + 1.0),
        Align2::LEFT_TOP,
        "AUTOMATION",
        FontId::proportional(9.0),
        Color32::from_gray(150),
    );
    let x_lo = geo.grid.min.x;
    let x_hi = geo.grid.max.x.min(full.max.x);
    let max_beat = geo.bar_at(x_hi) * BEATS_PER_BAR;
    if p.automation.is_empty() {
        let r = Rect::from_min_size(
            pos2(full.min.x, full.min.y + AUTO_CAPTION_H),
            vec2(full.width(), AUTO_H),
        );
        painter.text(r.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, "No automation lanes yet: add_automation / generate_automation (or the Automation view) put one here.", FontId::proportional(10.0), Color32::from_gray(120));
        return actions;
    }
    for (li, lane) in p.automation.iter().enumerate() {
        let top = full.min.y + AUTO_CAPTION_H + li as f32 * AUTO_H;
        let head = Rect::from_min_max(
            pos2(full.min.x, top),
            pos2(full.min.x + HEAD_W - 2.0, top + AUTO_H - 2.0),
        );
        let (lo, hi) = display_range(lane);
        let row = AutoRow {
            rect: Rect::from_min_max(pos2(x_lo, top + 4.0), pos2(x_hi, top + AUTO_H - 4.0)),
            lo,
            hi,
        };
        let key = if lane.id.is_empty() {
            format!("{}:{}", lane.target, lane.param)
        } else {
            lane.id.clone()
        };
        let args = |extra: Value| {
            let mut a = json!({"track": lane.target, "param": lane.param});
            if let (Some(m), Value::Object(x)) = (a.as_object_mut(), extra) {
                m.extend(x);
            }
            a
        };
        // the head: name, click = bypass / enable
        let hr = ui.interact(head, egui::Id::new(("auto-head", &key)), Sense::click());
        painter.rect_filled(
            head,
            3.0,
            if lane.enabled {
                super::theme::Tokens::DARK.button_hi
            } else {
                super::theme::Tokens::DARK.panel_bg2
            },
        );
        let label = format!("{} · {}", lane.target, lane.param);
        painter.text(
            head.left_center() + vec2(4.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(9.5),
            if lane.enabled {
                Color32::from_gray(220)
            } else {
                Color32::from_gray(110)
            },
        );
        if hr
            .on_hover_text("Click to bypass / enable this automation lane")
            .clicked()
        {
            actions.push((
                "set_automation_points",
                args(json!({"enabled": !lane.enabled})),
            ));
        }
        // the row: background, then the envelope
        let row_bg = Rect::from_min_max(pos2(x_lo, top), pos2(x_hi, top + AUTO_H - 2.0));
        painter.rect_filled(row_bg, 2.0, super::theme::Tokens::DARK.strip_section);
        // empty-spot click = add a point (registered first so the handles,
        // registered after it, win the hit test)
        let rr = ui.interact(row_bg, egui::Id::new(("auto-row", &key)), Sense::click());
        let col = if lane.enabled {
            Color32::from_rgb(120, 200, 255)
        } else {
            Color32::from_gray(90)
        };
        // where each point is drawn this frame (a dragged one follows the pointer)
        let mut drawn: Vec<egui::Pos2> = lane
            .points
            .iter()
            .map(|q| pos2(row.x_of_beat(geo, q.beat), row.y_of(q.value)))
            .collect();
        let mut hit_point = false;
        for (i, q) in lane.points.iter().enumerate() {
            let c = drawn[i];
            if c.x < x_lo - HIT_R || c.x > x_hi + HIT_R {
                continue;
            }
            let hit = Rect::from_center_size(c, vec2(HIT_R * 2.0, HIT_R * 2.0));
            let id = egui::Id::new(("auto-pt", &key, i));
            let r = ui.interact(hit, id, Sense::click_and_drag());
            if r.hovered() || r.dragged() || r.clicked() || r.secondary_clicked() {
                hit_point = true;
            }
            // the drag follows the pointer from where it was pressed (not the
            // summed per-frame deltas, which lose the move before egui
            // decides it is a drag); the last spot is kept for the release
            let did = id.with("d");
            if r.dragged() {
                let off = ui
                    .input(|m| {
                        m.pointer
                            .latest_pos()
                            .zip(m.pointer.press_origin())
                            .map(|(a, b)| a - b)
                    })
                    .unwrap_or_default();
                let (b, v) = moved_point(
                    lane,
                    i,
                    row.beat_at(geo, c.x + off.x),
                    row.value_at(c.y + off.y),
                    &row,
                    max_beat,
                );
                ui.ctx().data_mut(|m| m.insert_temp(did, (b, v)));
                drawn[i] = pos2(row.x_of_beat(geo, b), row.y_of(v));
            }
            if r.drag_stopped() {
                let got: Option<(f32, f32)> = ui.ctx().data(|m| m.get_temp(did));
                ui.ctx().data_mut(|m| m.remove::<(f32, f32)>(did));
                if let Some((b, v)) =
                    got.filter(|(b, v)| (b - q.beat).abs() > 1e-4 || (v - q.value).abs() > 1e-4)
                {
                    let pts: Vec<Value> = lane
                        .points
                        .iter()
                        .enumerate()
                        .map(|(j, o)| {
                            if j == i {
                                json!({"beat": b, "value": v, "curve": o.curve})
                            } else {
                                pt_json(o)
                            }
                        })
                        .collect();
                    actions.push((
                        "set_automation_points",
                        args(json!({"points": pts, "mode": "replace"})),
                    ));
                }
            } else if r.secondary_clicked() {
                actions.push(("set_automation_points", args(json!({"mode": "erase", "from_beat": q.beat - 1e-3, "to_beat": q.beat + 1e-3}))));
            }
        }
        if !hit_point && rr.clicked() {
            if let Some(pos) = rr.interact_pointer_pos() {
                let b = snap_beat(row.beat_at(geo, pos.x));
                if !lane.points.iter().any(|q| (q.beat - b).abs() < 1e-4) {
                    actions.push(("set_automation_points", args(json!({"points": [{"beat": b, "value": row.value_at(pos.y)}], "mode": "merge"}))));
                }
            }
        }
        // draw: held value before the first point / after the last, the
        // segments between (sampled so step and smooth curves show their shape)
        let clip = painter.with_clip_rect(row_bg);
        if !drawn.is_empty() {
            let mut line: Vec<egui::Pos2> = vec![pos2(x_lo, drawn[0].y)];
            for i in 0..drawn.len() {
                line.push(drawn[i]);
                if let (Some(n), Some(q)) = (drawn.get(i + 1), lane.points.get(i)) {
                    if q.curve != crate::automation::Curve::Linear {
                        let tmp = AutomationLane {
                            points: vec![
                                AutoPoint {
                                    beat: 0.0,
                                    value: drawn[i].y,
                                    curve: q.curve,
                                },
                                AutoPoint {
                                    beat: 1.0,
                                    value: n.y,
                                    curve: q.curve,
                                },
                            ],
                            ..AutomationLane::new("", "")
                        };
                        for s in 1..12 {
                            let t = s as f32 / 12.0;
                            line.push(pos2(
                                drawn[i].x + (n.x - drawn[i].x) * t,
                                tmp.value_at(t).unwrap_or(n.y),
                            ));
                        }
                    }
                }
            }
            line.push(pos2(x_hi, drawn[drawn.len() - 1].y));
            clip.add(egui::Shape::line(line, Stroke::new(1.5_f32, col)));
            for c in &drawn {
                clip.circle_filled(*c, POINT_R, col);
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::gui::fl_playlist::{playlist_grid, GridSel, PlaylistView};
    use egui::{Event, Modifiers, PointerButton};

    /// A project with one pattern clip and a volume lane on its track:
    /// points at beats 0 (-12 dB), 8 (-6 dB) and 16 (-12 dB).
    fn project() -> (Engine, Project) {
        let d = std::env::temp_dir().join(format!("flauto_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "pad", "preset": "epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "verse", "bars": 2}))
            .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "pad", "param": "volume", "points": [[0, -12], [8, -6], [16, -12]]}),
        )
        .unwrap();
        let p = e.project.clone();
        (e, p)
    }

    /// Run the playlist grid and the automation strip under it, the way the
    /// app does, feeding real egui events; `gesture` gets a function from
    /// (lane index, beat, value) to the screen point.
    fn drive(
        p: &Project,
        ppp: f32,
        view: PlaylistView,
        gesture: &dyn Fn(&dyn Fn(usize, f32, f32) -> egui::Pos2) -> Vec<Vec<Event>>,
    ) -> Vec<(&'static str, Value)> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 700.0));
        let mut sel = GridSel {
            view,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut rows: Vec<(GridGeo, AutoRow)> = Vec::new();
        let mut frame = |events: Vec<Event>,
                         k: usize,
                         out: &mut Vec<(&'static str, Value)>,
                         rows: &mut Vec<(GridGeo, AutoRow)>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out.extend(playlist_grid(ui, p, &mut sel, 0.0));
                    let geo = sel.geo.unwrap();
                    let top = ui.cursor().min.y;
                    out.extend(automation_lanes(ui, p, &geo));
                    rows.clear();
                    for (li, l) in p.automation.iter().enumerate() {
                        let t = top + AUTO_CAPTION_H + li as f32 * AUTO_H;
                        let (lo, hi) = display_range(l);
                        rows.push((
                            geo,
                            AutoRow {
                                rect: Rect::from_min_max(
                                    pos2(geo.grid.min.x, t + 4.0),
                                    pos2(geo.grid.max.x, t + AUTO_H - 4.0),
                                ),
                                lo,
                                hi,
                            },
                        ));
                    }
                });
            });
        };
        frame(vec![], 0, &mut out, &mut rows);
        frame(vec![], 1, &mut out, &mut rows);
        let rows2 = rows.clone();
        let at = move |li: usize, beat: f32, v: f32| {
            let (g, r) = rows2[li];
            pos2(r.x_of_beat(&g, beat), r.y_of(v))
        };
        for (k, ev) in gesture(&at).into_iter().enumerate() {
            frame(ev, k + 2, &mut out, &mut rows);
        }
        out
    }

    fn press(pos: egui::Pos2, button: PointerButton, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    fn drag(from: egui::Pos2, to: egui::Pos2) -> Vec<Vec<Event>> {
        let mut f = vec![
            vec![Event::PointerMoved(from)],
            vec![press(from, PointerButton::Primary, true)],
        ];
        for i in 1..=8 {
            f.push(vec![Event::PointerMoved(
                from + (to - from) * (i as f32 / 8.0),
            )]);
        }
        f.push(vec![press(to, PointerButton::Primary, false)]);
        f.push(vec![]);
        f
    }

    fn click(at: egui::Pos2, button: PointerButton) -> Vec<Vec<Event>> {
        vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, button, true)],
            vec![press(at, button, false)],
            vec![],
        ]
    }

    fn apply(e: &mut Engine, calls: &[(&'static str, Value)]) {
        for (t, a) in calls {
            e.call(t, a).unwrap();
        }
    }

    fn pts(e: &Engine) -> Vec<(f32, f32)> {
        e.project.automation[0]
            .points
            .iter()
            .map(|q| (q.beat, (q.value * 10.0).round() / 10.0))
            .collect()
    }

    #[test]
    fn rows_convert_beats_and_values_and_moves_stay_between_neighbours() {
        let (_, p) = project();
        let l = &p.automation[0];
        let (lo, hi) = display_range(l);
        assert_eq!((lo, hi), (-30.0, 0.0));
        let geo = GridGeo {
            grid: Rect::from_min_size(pos2(100.0, 0.0), vec2(800.0, 100.0)),
            bar_px: 25.0,
            scroll_bar: 2.0,
            scroll_lane: 0.0,
        };
        let row = AutoRow {
            rect: Rect::from_min_size(pos2(100.0, 200.0), vec2(800.0, 26.0)),
            lo,
            hi,
        };
        for b in [8.0f32, 13.5, 40.0] {
            assert!((row.beat_at(&geo, row.x_of_beat(&geo, b)) - b).abs() < 1e-3);
        }
        for v in [-30.0f32, -12.0, 0.0] {
            assert!((row.value_at(row.y_of(v)) - v).abs() < 1e-3);
        }
        assert_eq!(snap_beat(3.13), 3.25);
        assert_eq!(snap_beat(-2.0), 0.0);
        // the middle point can't pass beat 0's or beat 16's neighbour
        assert_eq!(moved_point(l, 1, 30.0, 5.0, &row, 100.0), (15.75, 0.0));
        assert_eq!(moved_point(l, 1, -4.0, -50.0, &row, 100.0), (0.25, -30.0));
        assert_eq!(moved_point(l, 0, -4.0, -12.0, &row, 100.0), (0.0, -12.0));
        assert_eq!(moved_point(l, 2, 500.0, -12.0, &row, 64.0), (64.0, -12.0));
    }

    #[test]
    fn a_real_pointer_adds_moves_deletes_and_bypasses_points() {
        for (ppp, view) in [
            (1.0f32, PlaylistView::default()),
            (2.0, PlaylistView::default()),
            (
                1.0,
                PlaylistView {
                    scroll_bar: 1.0,
                    scroll_lane: 0.0,
                    zoom: 2.0,
                },
            ),
            (
                2.0,
                PlaylistView {
                    scroll_bar: 1.0,
                    scroll_lane: 0.0,
                    zoom: 2.0,
                },
            ),
        ] {
            let (mut e, p) = project();
            // drag the middle point (beat 8, -6 dB) to beat 12, -3 dB
            let moved = drive(&p, ppp, view, &|at| {
                drag(at(0, 8.0, -6.0), at(0, 12.0, -3.0))
            });
            assert_eq!(moved.len(), 1, "ppp {ppp} {view:?}: {moved:?}");
            assert_eq!(moved[0].1["mode"], json!("replace"));
            apply(&mut e, &moved);
            let got = pts(&e);
            assert_eq!(got.len(), 3);
            assert_eq!(got[1].0, 12.0, "ppp {ppp} {view:?}: {got:?}");
            assert!((got[1].1 + 3.0).abs() <= 0.6, "ppp {ppp} {view:?}: {got:?}");
            // click an empty spot at beat 4, -20 dB: a new point there
            let p2 = e.project.clone();
            let added = drive(&p2, ppp, view, &|at| {
                click(at(0, 4.0, -20.0), PointerButton::Primary)
            });
            assert_eq!(added.len(), 1, "ppp {ppp} {view:?}: {added:?}");
            apply(&mut e, &added);
            let got = pts(&e);
            assert_eq!(got.len(), 4);
            assert_eq!(got[1].0, 4.0, "{got:?}");
            assert!((got[1].1 + 20.0).abs() <= 0.6, "{got:?}");
            // right-click the point at beat 12: it is gone
            let p3 = e.project.clone();
            let erased = drive(&p3, ppp, view, &|at| {
                click(at(0, 12.0, got[2].1), PointerButton::Secondary)
            });
            assert_eq!(erased.len(), 1, "{erased:?}");
            apply(&mut e, &erased);
            assert_eq!(
                pts(&e).iter().map(|q| q.0).collect::<Vec<_>>(),
                vec![0.0, 4.0, 16.0]
            );
            // click the head: bypassed (still there), click again: enabled
            let p4 = e.project.clone();
            let geo_head = drive(&p4, ppp, view, &|at| {
                let r = at(0, 0.0, -30.0);
                click(pos2(20.0, r.y - 4.0), PointerButton::Primary)
            });
            assert_eq!(
                geo_head,
                vec![(
                    "set_automation_points",
                    json!({"track": "pad", "param": "volume", "enabled": false})
                )]
            );
            apply(&mut e, &geo_head);
            assert!(!e.project.automation[0].enabled);
            assert_eq!(e.project.automation[0].points.len(), 3);
            // nothing touched: no calls (hovering is not editing)
            let idle = drive(&e.project.clone(), ppp, view, &|at| {
                vec![vec![Event::PointerMoved(at(0, 4.0, -20.0))], vec![]]
            });
            assert!(idle.is_empty(), "{idle:?}");
        }
    }

    /// End to end: a send lane drawn with the strip (a real pointer drag)
    /// changes what the engine renders.
    #[test]
    fn a_send_lane_dragged_in_the_strip_changes_the_render() {
        let d = std::env::temp_dir().join(format!("flauto_send_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "pad", "preset": "supersaw"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "verse", "bars": 4}))
            .unwrap();
        // steps: one held note per bar
        let notes: Vec<_> = (0..4)
            .map(|i| json!({"start": i * 16, "len": 16, "pitch": "C4"}))
            .collect();
        e.call(
            "add_notes",
            &json!({"track": "pad", "pattern": "verse", "notes": notes}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"}))
            .unwrap();
        e.call(
            "set_send",
            &json!({"track": "pad", "bus": "verb", "db": -60}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "pad", "param": "send.verb", "points": [[0, -60], [16, -60]]}),
        )
        .unwrap();
        assert_eq!(display_range(&e.project.automation[0]), (-60.0, 0.0));
        let before = e.mix().unwrap();
        // drag the end point (beat 16, -60 dB) up to 0 dB: a -60 -> 0 ramp
        let p = e.project.clone();
        let calls = drive(&p, 1.0, PlaylistView::default(), &|at| {
            drag(at(0, 16.0, -60.0), at(0, 16.0, 0.0))
        });
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].1["param"], json!("send.verb"));
        apply(&mut e, &calls);
        let got = pts(&e);
        assert_eq!(got[0], (0.0, -60.0));
        assert_eq!(got[1].0, 16.0);
        assert!(got[1].1 > -2.0, "{got:?}");
        let after = e.mix().unwrap();
        let sr = crate::dsp::SR;
        let beat_s = 60.0 / e.project.bpm;
        let seg = |a: f32, b: f32| ((a * beat_s * sr) as usize, (b * beat_s * sr) as usize);
        let n = before.left.len().min(after.left.len());
        let rms_diff = |(i0, i1): (usize, usize)| {
            let i1 = i1.min(n);
            let s: f32 = (i0..i1)
                .map(|i| (after.left[i] - before.left[i]).powi(2))
                .sum();
            (s / (i1 - i0).max(1) as f32).sqrt()
        };
        let head = rms_diff(seg(0.0, 4.0));
        let tail = rms_diff(seg(12.0, 16.0));
        assert!(
            tail > 1e-3 && tail > head * 4.0,
            "the drawn ramp brings the reverb in: head {head}, tail {tail}"
        );
    }

    #[test]
    fn a_project_without_lanes_shows_a_hint_and_asks_for_nothing() {
        let d = std::env::temp_dir().join(format!("flauto_empty_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = Engine::new(d).project.clone();
        assert_eq!(strip_height(&p), AUTO_CAPTION_H + AUTO_H + 2.0);
        let calls = drive(&p, 1.0, PlaylistView::default(), &|_| {
            click(pos2(500.0, 200.0), PointerButton::Primary)
        });
        assert!(
            calls.iter().all(|(t, _)| *t != "set_automation_points"),
            "{calls:?}"
        );
    }
}
