//! The device rack view: every strip (tracks, buses, master) as a column
//! of devices (the instrument, then the inserts in order) with one control
//! per parameter, built from the rack state's parameter specs: a number
//! with a range is a slider (logarithmic where the spec says log), an enum
//! a drop-down, a bool a checkbox. Above the strips a patch bay draws every
//! route (outputs, aux sends, sidechain keys) as a cable between the strips'
//! columns. Every edit is one `rack_apply` command, the same surface remote
//! clients use.

use super::Studio;
use crate::project::Project;
use crate::rack::{Device, DeviceKind, Param, RackState, Route, Strip, StripKind};
use eframe::egui::{self, Color32, Rect, RichText};
use serde_json::{json, Value};
use std::collections::HashMap;

/// A strip column's widest and narrowest width.
const COL_W: f32 = 280.0;
const MIN_COL_W: f32 = 256.0;
const DIM: Color32 = super::theme::Tokens::DARK.text_dim;
const ACCENT: Color32 = super::theme::Tokens::DARK.accent;
const OFF: Color32 = Color32::from_rgb(120, 80, 80);

/// Where the controls landed this frame ("bypass:<device>", "param:<id>",
/// "remove:<device>", "left:<device>", "right:<device>"), for tests and
/// pointer automation.
pub type RackGeo = HashMap<String, Rect>;

/// The words a parameter shows: its path inside the device.
fn short(p: &Param) -> String {
    let mut parts: Vec<&str> = p.path.split('.').collect();
    if parts.first() == Some(&"fx") && parts.len() > 2 {
        parts.drain(..2);
    } else if parts.first() == Some(&"instrument") && parts.len() > 1 {
        parts.drain(..1);
    }
    parts.join(" ").replace('_', " ")
}

fn set(p: &Param, v: Value) -> Value {
    json!({"op": "set_param", "param": p.id, "value": v})
}

/// One control for one parameter; returns the command when it is committed
/// (a slider commits when the drag ends, so one gesture is one undo step).
fn param_control(ui: &mut egui::Ui, p: &Param, geo: &mut RackGeo, w: f32) -> Option<Value> {
    let label = short(p);
    let mut out = None;
    ui.vertical(|ui| {
        ui.add(egui::Label::new(RichText::new(&label).color(DIM).size(11.0)).wrap());
        match p.kind.as_str() {
            "number" => {
                let cur = p.value.as_f64().unwrap_or(0.0);
                let (lo, hi) = (p.min.unwrap_or(0.0) as f64, p.max.unwrap_or(1.0) as f64);
                let integer = p.value.is_u64() || p.value.is_i64();
                let id = egui::Id::new(("rack-param", &p.id));
                let mut v = ui.data(|d| d.get_temp::<f64>(id)).unwrap_or(cur);
                let log = p.scale.as_deref() == Some("log") && lo > 0.0;
                let mut s = egui::Slider::new(&mut v, lo..=hi).logarithmic(log);
                if integer {
                    s = s.integer();
                }
                ui.spacing_mut().slider_width = (w - 104.0).max(120.0);
                let r = ui.add(s);
                geo.insert(format!("param:{}", p.id), r.rect);
                if r.dragged() {
                    ui.data_mut(|d| d.insert_temp(id, v));
                }
                let commit = r.drag_stopped() || (r.changed() && !r.dragged());
                if commit {
                    ui.data_mut(|d| d.remove::<f64>(id));
                    if v != cur {
                        out = Some(set(
                            p,
                            if integer {
                                json!(v.round() as i64)
                            } else {
                                json!(v)
                            },
                        ));
                    }
                }
            }
            "bool" => {
                let mut b = p.value.as_bool().unwrap_or(false);
                let r = ui.checkbox(&mut b, "");
                geo.insert(format!("param:{}", p.id), r.rect);
                if r.changed() {
                    out = Some(set(p, json!(b)));
                }
            }
            "enum" => {
                let cur = p.value.as_str().unwrap_or("").to_string();
                let mut pick = cur.clone();
                let r = egui::ComboBox::from_id_salt(("rack-enum", &p.id))
                    .selected_text(&cur)
                    .width(w - 120.0)
                    .show_ui(ui, |ui| {
                        for o in p.options.clone().unwrap_or_default() {
                            ui.selectable_value(&mut pick, o.clone(), o);
                        }
                    });
                geo.insert(format!("param:{}", p.id), r.response.rect);
                if pick != cur {
                    out = Some(set(p, json!(pick)));
                }
            }
            _ => {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{label}: {}", short_value(&p.value)))
                            .color(DIM)
                            .size(11.0),
                    )
                    .truncate(),
                );
            }
        }
        if !p.lanes.is_empty() {
            ui.label(RichText::new("auto").color(ACCENT).size(10.0))
                .on_hover_text("an automation lane drives this");
        }
    });
    out
}

fn short_value(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("{} items", a.len()),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn device_card(
    ui: &mut egui::Ui,
    strip: &Strip,
    d: &Device,
    geo: &mut RackGeo,
    out: &mut Vec<Value>,
    w: f32,
) {
    let fill = if d.bypassed {
        super::theme::Tokens::DARK.strip_section
    } else {
        super::theme::Tokens::DARK.panel_bg2
    };
    egui::Frame::none()
        .fill(fill)
        .rounding(6.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_width(w - 16.0);
            let title = match d.slot {
                Some(i) => format!("{} {}", i + 1, d.type_name.replace('_', " ")),
                None => d.type_name.replace('_', " "),
            };
            let title =
                RichText::new(title)
                    .strong()
                    .color(if d.bypassed { DIM } else { Color32::WHITE });
            ui.add(egui::Label::new(title.clone()).wrap());
            ui.horizontal(|ui| {
                if d.kind != DeviceKind::Effect {
                    return;
                }
                // buttons from the right edge, then the name in what is left
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.small_button("x").on_hover_text("remove");
                    geo.insert(format!("remove:{}", d.id), r.rect);
                    if r.clicked() {
                        out.push(json!({"op": "remove_device", "device": d.id}));
                    }
                    let n = strip
                        .devices
                        .iter()
                        .filter(|x| x.kind == DeviceKind::Effect)
                        .count();
                    let slot = d.slot.unwrap_or(0);
                    let r = ui
                        .add_enabled(slot + 1 < n, egui::Button::new(">").small())
                        .on_hover_text("later in the chain");
                    geo.insert(format!("right:{}", d.id), r.rect);
                    if r.clicked() {
                        out.push(
                            json!({"op": "move_device", "device": d.id, "position": slot + 1}),
                        );
                    }
                    let r = ui
                        .add_enabled(slot > 0, egui::Button::new("<").small())
                        .on_hover_text("earlier in the chain");
                    geo.insert(format!("left:{}", d.id), r.rect);
                    if r.clicked() {
                        out.push(
                            json!({"op": "move_device", "device": d.id, "position": slot - 1}),
                        );
                    }
                    let (txt, col) = if d.bypassed {
                        ("off", OFF)
                    } else {
                        ("on", ACCENT)
                    };
                    let r = ui
                        .add(
                            egui::Button::new(RichText::new(txt).color(Color32::BLACK))
                                .fill(col)
                                .small(),
                        )
                        .on_hover_text("bypass");
                    geo.insert(format!("bypass:{}", d.id), r.rect);
                    if r.clicked() {
                        out.push(json!({"op": "bypass", "device": d.id, "on": !d.bypassed}));
                    }
                });
            });
            // short inserts open, long ones (and instruments) folded, so a
            // strip fits the panel; one click opens any of them
            let open = d.kind == DeviceKind::Effect && d.params.len() <= 3;
            egui::CollapsingHeader::new(
                RichText::new(format!("{} controls", d.params.len()))
                    .size(11.0)
                    .color(DIM),
            )
            .id_salt(("rack-dev", &d.id))
            .default_open(open)
            .show(ui, |ui| {
                for p in &d.params {
                    if let Some(c) = param_control(ui, p, geo, w) {
                        out.push(c);
                    }
                }
            });
        });
}

fn strip_column(
    ui: &mut egui::Ui,
    s: &Strip,
    rack: &RackState,
    geo: &mut RackGeo,
    out: &mut Vec<Value>,
    w: f32,
) {
    let head = match s.kind {
        StripKind::Track => super::theme::Tokens::DARK.text,
        StripKind::Bus => super::theme::Tokens::DARK.solo,
        StripKind::Master => super::theme::Tokens::DARK.accent,
    };
    let col = egui::Frame::none()
        .fill(super::theme::Tokens::DARK.strip_bg)
        .rounding(8.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&s.name).strong().size(14.0).color(head));
                    let kind = match s.kind {
                        StripKind::Track => "track",
                        StripKind::Bus => "bus",
                        StripKind::Master => "master",
                    };
                    ui.label(RichText::new(kind).size(10.0).color(DIM));
                    // where it goes, on the same row (the cable above shows it too)
                    if let Some(o) = &s.output {
                        let name = rack
                            .strip(o)
                            .map(|x| x.name.clone())
                            .unwrap_or_else(|| o.clone());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("to {name}")).size(11.0).color(DIM),
                                )
                                .truncate(),
                            );
                        });
                    }
                });
                for p in &s.mixer {
                    if let Some(c) = param_control(ui, p, geo, w) {
                        out.push(c);
                    }
                }
                ui.add_space(2.0);
                for d in &s.devices {
                    device_card(ui, s, d, geo, out, w);
                    ui.add_space(2.0);
                }
                let mut pick: Option<&str> = None;
                egui::ComboBox::from_id_salt(("rack-add", &s.id))
                    .selected_text("+ insert")
                    .width(w - 20.0)
                    .show_ui(ui, |ui| {
                        for (t, _) in crate::fx::EFFECT_TYPES.iter() {
                            if ui.selectable_label(false, t.replace('_', " ")).clicked() {
                                pick = Some(t);
                            }
                        }
                    });
                if let Some(t) = pick {
                    out.push(json!({"op": "add_device", "strip": s.id, "type": t}));
                }
            });
        });
    geo.insert(format!("strip:{}", s.id), col.response.rect);
}

/// Height of the patch bay above the strips, where the cables run.
const BAY_H: f32 = 58.0;
const CABLE_OUT: Color32 = super::theme::Tokens::DARK.accent;
const CABLE_SEND: Color32 = Color32::from_rgb(80, 200, 190);
const CABLE_KEY: Color32 = super::theme::Tokens::DARK.header_text;

/// How a kind of cable is drawn: outputs solid and thick, aux sends dashed,
/// sidechain keys dotted, each in its own colour, so the three read apart
/// at a glance (and in greyscale).
#[derive(Clone, Copy)]
enum CableStyle {
    Solid,
    Dashed,
    Dotted,
}

fn cable_style(r: &Route) -> (&'static str, Color32, CableStyle) {
    match r {
        Route::Output { .. } => ("out", CABLE_OUT, CableStyle::Solid),
        Route::Send { .. } => ("send", CABLE_SEND, CableStyle::Dashed),
        Route::Sidechain { .. } => ("key", CABLE_KEY, CableStyle::Dotted),
    }
}

/// Stroke a path in a cable style.
fn stroke_path(painter: &egui::Painter, pts: &[egui::Pos2], color: Color32, style: CableStyle) {
    match style {
        CableStyle::Solid => {
            painter.add(egui::Shape::line(
                pts.to_vec(),
                egui::Stroke::new(2.5_f32, color),
            ));
        }
        CableStyle::Dashed => {
            painter.extend(egui::Shape::dashed_line(
                pts,
                egui::Stroke::new(2.0_f32, color),
                7.0,
                4.0,
            ));
        }
        CableStyle::Dotted => {
            painter.extend(egui::Shape::dotted_line(pts, color, 5.0, 1.4));
        }
    }
}

/// Draw every route as a patch cable in the bay above the strips: from the
/// source strip's jack to the destination's, arched higher the farther it
/// goes. Outputs: orange, solid (centre jacks); aux sends: teal, dashed,
/// with a dB badge (right of centre); sidechain keys: purple, dotted (left
/// of centre). Each cable's span lands in `geo` as
/// "cable:<kind>:<from>><to>", a send's badge as "badge:<from>><to>".
fn patch_cables(painter: &egui::Painter, bay: Rect, rack: &RackState, geo: &mut RackGeo) {
    let span = bay.width().max(1.0);
    for r in &rack.routes {
        let (Some(a), Some(b)) = (
            geo.get(&format!("strip:{}", r.from())).copied(),
            geo.get(&format!("strip:{}", r.to())).copied(),
        ) else {
            continue;
        };
        let (kind, color, style) = cable_style(r);
        let dx = match style {
            CableStyle::Solid => 0.0,
            CableStyle::Dashed => 22.0,
            CableStyle::Dotted => -22.0,
        };
        let y = bay.bottom();
        let p0 = egui::pos2(a.center().x + dx, y);
        let p3 = egui::pos2(b.center().x + dx * 0.5, y);
        let h = (BAY_H - 18.0) * (0.35 + 0.65 * ((p3.x - p0.x).abs() / span).min(1.0));
        // a cubic's peak is 3/4 of its control points' rise
        let (c1, c2) = (
            egui::pos2(p0.x, y - h * 4.0 / 3.0),
            egui::pos2(p3.x, y - h * 4.0 / 3.0),
        );
        let pts = egui::epaint::CubicBezierShape::from_points_stroke(
            [p0, c1, c2, p3],
            false,
            Color32::TRANSPARENT,
            egui::Stroke::NONE,
        )
        .flatten(Some(0.5));
        stroke_path(painter, &pts, color, style);
        painter.circle_filled(p0, 3.5, color);
        painter.circle_stroke(p3, 4.0, egui::Stroke::new(2.0_f32, color));
        let top = egui::pos2((p0.x + p3.x) * 0.5, y - h);
        if let Route::Send {
            db,
            pre_fader,
            from,
            to,
            ..
        } = r
        {
            // the level as a badge on the cable's crest
            let t = format!("{db:+.1} dB{}", if *pre_fader { " pre" } else { "" });
            let galley =
                painter.layout_no_wrap(t, egui::FontId::proportional(10.0), Color32::BLACK);
            let badge = Rect::from_center_size(top, galley.size() + egui::vec2(8.0, 2.0));
            painter.rect_filled(badge, 4.0, color);
            painter.galley(badge.center() - galley.size() * 0.5, galley, Color32::BLACK);
            geo.insert(format!("badge:{from}>{to}"), badge);
        }
        geo.insert(
            format!("cable:{kind}:{}>{}", r.from(), r.to()),
            Rect::from_points(&[p0, p3, top]),
        );
    }
}

/// Draw the whole rack; returns the commands the person made this frame.
pub fn rack_panel(ui: &mut egui::Ui, rack: &RackState, geo: &mut RackGeo) -> Vec<Value> {
    let mut out = Vec::new();
    egui::ScrollArea::both()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical(|ui| {
                // columns share the panel's width (down to MIN_COL_W, then the
                // panel scrolls); the patch bay spans every column
                let n = rack.strips.len().max(1) as f32;
                let gap = 16.0 + ui.spacing().item_spacing.x;
                let w = ((ui.available_width() - n * gap) / n)
                    .floor()
                    .clamp(MIN_COL_W, COL_W);
                let bay_w = (n * (w + gap)).max(ui.available_width());
                let (bay, _) =
                    ui.allocate_exact_size(egui::vec2(bay_w, BAY_H), egui::Sense::hover());
                // legend: a short sample of each cable (colour + stroke)
                let mut x = bay.left() + 4.0;
                let ty = bay.top() + 8.0;
                let font = egui::FontId::proportional(10.0);
                x += ui
                    .painter()
                    .text(
                        egui::pos2(x, ty),
                        egui::Align2::LEFT_CENTER,
                        "ROUTING",
                        font.clone(),
                        DIM,
                    )
                    .width()
                    + 12.0;
                for (c, s, t) in [
                    (CABLE_OUT, CableStyle::Solid, "output"),
                    (CABLE_SEND, CableStyle::Dashed, "send (dB)"),
                    (CABLE_KEY, CableStyle::Dotted, "sidechain key"),
                ] {
                    stroke_path(
                        ui.painter(),
                        &[egui::pos2(x, ty), egui::pos2(x + 22.0, ty)],
                        c,
                        s,
                    );
                    x += 28.0
                        + ui.painter()
                            .text(
                                egui::pos2(x + 28.0, ty),
                                egui::Align2::LEFT_CENTER,
                                t,
                                font.clone(),
                                DIM,
                            )
                            .width()
                        + 14.0;
                }
                ui.horizontal_top(|ui| {
                    for s in &rack.strips {
                        strip_column(ui, s, rack, geo, &mut out, w);
                    }
                });
                patch_cables(ui.painter(), bay, rack, geo);
            });
        });
    out
}

impl Studio {
    /// `rev` is the revision `p` was read at: an edit made on this frame's
    /// picture is refused (StaleRevision) if something else changed the
    /// project in between, instead of landing on a rack it was not made on.
    pub(super) fn rack_view(&mut self, ui: &mut egui::Ui, p: &Project, rev: u64) {
        let rack = RackState::from_project(p);
        let mut geo = RackGeo::new();
        let cmds = rack_panel(ui, &rack, &mut geo);
        if !cmds.is_empty() {
            self.call(
                "rack_apply",
                json!({"commands": cmds, "expected_revision": rev}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use egui::{pos2, vec2, Event, Modifiers, PointerButton};

    fn engine() -> Engine {
        let d = std::env::temp_dir().join("beatbox_rack_view_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("new_project", &json!({"name": "rv", "bpm": 120}))
            .unwrap();
        e.call("add_track", &json!({"name": "bass", "preset": "808"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "bass", "type": "filter", "params": {"cutoff": 800}}),
        )
        .unwrap();
        e.call("add_effect", &json!({"track": "bass", "type": "reverb"}))
            .unwrap();
        e
    }

    fn press(pos: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    /// Run the panel on real egui input: two layout frames, then `gesture`.
    fn drive(p: &Project, gesture: &dyn Fn(&RackGeo) -> Vec<Vec<Event>>) -> Vec<Value> {
        drive_at(p, 1400.0, gesture)
    }

    fn drive_at(
        p: &Project,
        width: f32,
        gesture: &dyn Fn(&RackGeo) -> Vec<Vec<Event>>,
    ) -> Vec<Value> {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 1600.0));
        let rack = RackState::from_project(p);
        let mut out = Vec::new();
        let mut geo = RackGeo::new();
        let frame = |events: Vec<Event>, k: usize, out: &mut Vec<Value>, geo: &mut RackGeo| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    geo.clear();
                    out.extend(rack_panel(ui, &rack, geo));
                });
            });
        };
        frame(vec![], 0, &mut out, &mut geo);
        frame(vec![], 1, &mut out, &mut geo);
        let g = geo.clone();
        for (k, ev) in gesture(&g).into_iter().enumerate() {
            frame(ev, k + 2, &mut out, &mut geo);
        }
        out
    }

    fn click(at: egui::Pos2) -> Vec<Vec<Event>> {
        vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![press(at, false)],
            vec![],
        ]
    }

    #[test]
    fn the_rack_view_drives_the_rack_api() {
        let mut e = engine();
        let rack = RackState::from_project(&e.project);
        let bass = rack.strip("bass").unwrap();
        let filt = bass.devices[1].id.clone();
        let verb = bass.devices[2].id.clone();
        // every device and every number shows a control
        let cmds = drive(&e.project, &|g| {
            assert!(
                g.contains_key(&format!("bypass:{filt}")),
                "{:?}",
                g.keys().collect::<Vec<_>>()
            );
            assert!(g.contains_key(&format!("param:{}:fx.0.cutoff", bass.id)));
            assert!(g.contains_key(&format!("param:{}:volume", bass.id)));
            assert!(g.contains_key("param:master:volume"));
            vec![]
        });
        assert!(cmds.is_empty(), "layout alone edits nothing: {cmds:?}");
        // bypass the reverb
        let cmds = drive(&e.project, &|g| {
            click(g[&format!("bypass:{verb}")].center())
        });
        assert_eq!(
            cmds,
            vec![json!({"op": "bypass", "device": verb, "on": true})]
        );
        e.call("rack_apply", &json!({"commands": cmds})).unwrap();
        assert!(e.project.tracks[0].effects[1].bypassed());
        // move the reverb before the filter
        let cmds = drive(&e.project, &|g| click(g[&format!("left:{verb}")].center()));
        assert_eq!(
            cmds,
            vec![json!({"op": "move_device", "device": verb, "position": 0})]
        );
        e.call("rack_apply", &json!({"commands": cmds})).unwrap();
        assert_eq!(e.project.tracks[0].effects[0].type_name(), "reverb");
        // drag the filter's cutoff (log slider) to the right: one command when the drag ends
        let pid = format!("{}:fx.1.cutoff", bass.id);
        let cmds = drive(&e.project, &|g| {
            let r = g[&format!("param:{pid}")];
            let from = pos2(r.left() + 40.0, r.center().y);
            let to = pos2(r.left() + 90.0, r.center().y);
            let mut f = vec![vec![Event::PointerMoved(from)], vec![press(from, true)]];
            for i in 1..=6 {
                f.push(vec![Event::PointerMoved(
                    from + (to - from) * (i as f32 / 6.0),
                )]);
            }
            f.push(vec![press(to, false)]);
            f.push(vec![]);
            f
        });
        assert_eq!(cmds.len(), 1, "{cmds:?}");
        assert_eq!(cmds[0]["param"], json!(pid));
        e.call("rack_apply", &json!({"commands": cmds.clone()}))
            .unwrap();
        let v = e.project.tracks[0].effects[1].clone();
        let cut = serde_json::to_value(&v).unwrap()["cutoff"]
            .as_f64()
            .unwrap();
        assert_eq!(cut, cmds[0]["value"].as_f64().unwrap());
        // the cutoff slider is logarithmic: its middle is the geometric mean
        // of 20 Hz..20 kHz (632 Hz, where a linear slider gives 10 kHz), and
        // points either side of the middle are mirror images in log space
        // (their product is 632^2; a linear slider mirrors their sum)
        let at = |frac: f32| {
            let cmds = drive(&e.project, &|g| {
                let r = g[&format!("param:{pid}")];
                click(pos2(r.left() + (COL_W - 120.0) * frac, r.center().y))
            });
            assert_eq!(cmds.len(), 1, "{cmds:?}");
            cmds[0]["value"].as_f64().unwrap()
        };
        let (lo, mid, hi) = (at(0.25), at(0.5), at(0.75));
        assert!(
            (mid / 632.5).ln().abs() < 0.1,
            "mid rail: {mid} Hz, a log slider gives ~632"
        );
        assert!(
            (lo.ln() + hi.ln() - 2.0 * 632.5f64.ln()).abs() < 0.2,
            "{lo} / {hi} Hz are not log-symmetric"
        );
        assert!(lo < 200.0 && hi > 2000.0, "{lo} / {hi}");
        // a dB fader is linear in dB (dB is already logarithmic): its middle is the middle of the range
        let vid = format!("{}:volume", bass.id);
        let cmds = drive(&e.project, &|g| {
            let r = g[&format!("param:{vid}")];
            click(pos2(r.left() + (COL_W - 120.0) * 0.5, r.center().y))
        });
        let v = cmds[0]["value"].as_f64().unwrap();
        assert!(
            (v - (-24.0)).abs() < 2.0,
            "volume at mid rail: {v} dB (range -60..12)"
        );
        // alignment: every column is the same width, and the controls inside
        // one device card start on one left edge with one width
        let _ = drive(&e.project, &|g| {
            let cols: Vec<Rect> = g
                .iter()
                .filter(|(k, _)| k.starts_with("strip:"))
                .map(|(_, r)| *r)
                .collect();
            assert_eq!(cols.len(), 2);
            assert!(
                cols.iter()
                    .all(|r| (r.width() - cols[0].width()).abs() < 0.5),
                "{cols:?}"
            );
            assert!(
                cols.iter().all(|r| (r.top() - cols[0].top()).abs() < 0.5),
                "{cols:?}"
            );
            let r = RackState::from_project(&e.project);
            for s in &r.strips {
                for d in &s.devices {
                    let lefts: Vec<f32> = d
                        .params
                        .iter()
                        .filter(|p| p.kind == "number")
                        .filter_map(|p| g.get(&format!("param:{}", p.id)))
                        .map(|r| r.left())
                        .collect();
                    assert!(
                        lefts.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5),
                        "{}: {lefts:?}",
                        d.id
                    );
                }
                let lefts: Vec<f32> = s
                    .mixer
                    .iter()
                    .filter_map(|p| g.get(&format!("param:{}", p.id)))
                    .map(|r| r.left())
                    .collect();
                assert!(
                    lefts.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5),
                    "{}: {lefts:?}",
                    s.id
                );
            }
            vec![]
        });
        // remove the reverb
        let rev = RackState::from_project(&e.project);
        let verb = rev.strip("bass").unwrap().devices[1].id.clone();
        let cmds = drive(&e.project, &|g| {
            click(g[&format!("remove:{verb}")].center())
        });
        e.call("rack_apply", &json!({"commands": cmds})).unwrap();
        assert_eq!(
            e.project.tracks[0]
                .effects
                .iter()
                .map(|f| f.type_name())
                .collect::<Vec<_>>(),
            vec!["filter"]
        );
    }

    #[test]
    fn the_patch_bay_draws_one_cable_per_route_between_the_right_columns() {
        let mut e = engine();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call(
            "rack_apply",
            &json!({"commands": [
                {"op": "add_bus", "name": "drums"},
                {"op": "add_bus", "name": "verb", "preset": "reverb"},
                {"op": "route", "strip": "kick", "to": "drums"},
                {"op": "send", "strip": "bass", "to": "verb", "db": -12.0}
            ]}),
        )
        .unwrap();
        let rack = RackState::from_project(&e.project);
        let id = |n: &str| rack.strip(n).unwrap().id.clone();
        let _ = drive(&e.project, &|g| {
            let cables: Vec<(&String, &Rect)> =
                g.iter().filter(|(k, _)| k.starts_with("cable:")).collect();
            assert_eq!(
                cables.len(),
                rack.routes.len(),
                "one cable per route: {:?} vs {:?}",
                cables,
                rack.routes
            );
            for want in [
                format!("cable:out:{}>{}", id("kick"), id("drums")),
                format!("cable:send:{}>{}", id("bass"), id("verb")),
                format!("cable:out:{}>master", id("drums")),
            ] {
                assert!(
                    g.contains_key(&want),
                    "{want} missing: {:?}",
                    g.keys()
                        .filter(|k| k.starts_with("cable:"))
                        .collect::<Vec<_>>()
                );
            }
            // the columns kept their full width here (the frame adds 8 px a side)
            assert!(
                g.iter()
                    .filter(|(k, _)| k.starts_with("strip:"))
                    .all(|(_, r)| (r.width() - (COL_W + 16.0)).abs() < 1.0),
                "{:?}",
                g.iter()
                    .filter(|(k, _)| k.starts_with("strip:"))
                    .collect::<Vec<_>>()
            );
            // the send carries its level as a badge on the cable
            let badge = g[&format!("badge:{}>{}", id("bass"), id("verb"))];
            assert!(
                g[&format!("cable:send:{}>{}", id("bass"), id("verb"))]
                    .expand(12.0)
                    .contains(badge.center()),
                "{badge:?}"
            );
            // each cable runs from its source column to its destination column, above both
            for r in &rack.routes {
                let kind = match r {
                    Route::Output { .. } => "out",
                    Route::Send { .. } => "send",
                    Route::Sidechain { .. } => "key",
                };
                let c = g[&format!("cable:{kind}:{}>{}", r.from(), r.to())];
                let (a, b) = (
                    g[&format!("strip:{}", r.from())],
                    g[&format!("strip:{}", r.to())],
                );
                let (x0, x1) = if a.center().x <= b.center().x {
                    (c.left(), c.right())
                } else {
                    (c.right(), c.left())
                };
                assert!(
                    a.x_range().contains(x0) && b.x_range().contains(x1),
                    "{kind} {r:?}: cable {c:?} vs columns {a:?} {b:?}"
                );
                assert!(
                    (c.bottom() - a.top()).abs() < 12.0 && c.top() < a.top() - 10.0,
                    "{kind} cable sits in the bay above the strips: {c:?} vs {a:?}"
                );
            }
            vec![]
        });
    }

    #[test]
    fn five_strips_fit_the_panel_the_studio_gives_the_rack() {
        let mut e = engine();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("rack_apply", &json!({"commands": [{"op": "add_bus", "name": "drums"}, {"op": "add_bus", "name": "verb", "preset": "reverb"}]})).unwrap();
        // the studio's central panel is about 1110 px wide at its default 1480 px window
        let _ = drive_at(&e.project, 1110.0, &|g| {
            let cols: Vec<Rect> = g
                .iter()
                .filter(|(k, _)| k.starts_with("strip:"))
                .map(|(_, r)| *r)
                .collect();
            assert_eq!(cols.len(), 5);
            assert!(
                cols.iter().all(|r| r.right() <= 1110.0),
                "every strip, master included, is on screen: {cols:?}"
            );
            assert!(
                cols.iter()
                    .all(|r| (r.width() - cols[0].width()).abs() < 0.5 && r.width() >= MIN_COL_W),
                "{cols:?}"
            );
            vec![]
        });
    }
                    }
