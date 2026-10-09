//! Mixer and Automation views of the central panel. Like everything else in
//! the studio, every edit is an `Engine` tool call.

use super::widgets::*;
use super::Studio;
use crate::automation::{self, AutomationLane};
use crate::dsp::gain_to_db;
use crate::dsp::SR;
use crate::project::Project;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use serde_json::json;

/// Samples per meter window.
const WIN: usize = 1024;

/// Pre-computed meter data for one strip (track, bus or master).
pub struct StripMeter {
    pub name: String,
    pub env_l: Vec<f32>,
    pub env_r: Vec<f32>,
    pub peak_db: f32,
    pub rms_db: f32,
}

pub fn strip_meter(name: &str, l: &[f32], r: &[f32]) -> StripMeter {
    let env = |x: &[f32]| -> Vec<f32> {
        x.chunks(WIN)
            .map(|c| {
                let pk = c.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                gain_to_db(pk)
            })
            .collect()
    };
    let pk = l.iter().chain(r.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
    let n = l.len().max(1) as f32;
    let ms = l.iter().chain(r.iter()).map(|v| v * v).sum::<f32>() / (2.0 * n);
    StripMeter {
        name: name.to_string(),
        env_l: env(l),
        env_r: env(r),
        peak_db: gain_to_db(pk),
        rms_db: gain_to_db(ms.sqrt()),
    }
}

impl StripMeter {
    /// Current (l, r) level at a song position, with a short decay so it reads like a real meter.
    fn level_at(&self, secs: Option<f32>) -> (f32, f32) {
        match secs {
            None => (self.rms_db, self.rms_db),
            Some(s) => {
                let i = (s * SR / WIN as f32) as usize;
                let pick = |e: &[f32]| -> f32 {
                    let lo = i.saturating_sub(6);
                    (lo..=i)
                        .filter_map(|k| e.get(k).map(|v| v - (i - k) as f32 * 1.6))
                        .fold(-120.0, f32::max)
                };
                (pick(&self.env_l), pick(&self.env_r))
            }
        }
    }
}

const STRIP_W: f32 = 86.0;

fn fmt_db(db: f32) -> String {
    if db <= -59.5 {
        "-inf".into()
    } else {
        format!("{db:+.1}")
    }
}

impl Studio {
    fn live_pos(&self) -> Option<f32> {
        self.player
            .is_playing()
            .then(|| self.player.position_secs())
    }

    // ---------------- mixer ----------------
    pub(super) fn mixer(&mut self, ui: &mut egui::Ui, p: &Project) {
        let h = ui.available_height().max(260.0);
        let pos = self.live_pos();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let scroll_w = ui.available_width() - 158.0;
            ui.allocate_ui(Vec2::new(scroll_w, h), |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("mixer")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (ti, t) in p.tracks.iter().enumerate() {
                                let color = track_color(&t.name, t.instrument.kind_name());
                                let out = t.output.clone().unwrap_or_else(|| "master".into());
                                let sends: Vec<(String, f32)> =
                                    t.sends.iter().map(|s| (s.bus.clone(), s.db)).collect();
                                self.strip(
                                    ui,
                                    p,
                                    h,
                                    StripKind::Track(ti),
                                    &t.name,
                                    t.instrument.kind_name(),
                                    color,
                                    t.volume_db,
                                    t.pan,
                                    t.mute,
                                    t.solo,
                                    &out,
                                    &sends,
                                    pos,
                                );
                            }
                            if !p.buses.is_empty() {
                                let (r, _) =
                                    ui.allocate_exact_size(Vec2::new(10.0, h), Sense::hover());
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(r.center().x, r.top() + 10.0),
                                        Pos2::new(r.center().x, r.bottom() - 10.0),
                                    ],
                                    Stroke::new(1.0_f32, LINE),
                                );
                            }
                            for b in p.buses.iter() {
                                let fxs: Vec<String> =
                                    b.effects.iter().map(|e| e.type_name()).collect();
                                let label = if fxs.is_empty() {
                                    "bus".to_string()
                                } else {
                                    fxs.join(" / ")
                                };
                                self.strip(
                                    ui,
                                    p,
                                    h,
                                    StripKind::Bus,
                                    &b.name,
                                    &label,
                                    ACCENT2,
                                    b.volume_db,
                                    b.pan,
                                    b.mute,
                                    false,
                                    "master",
                                    &[],
                                    pos,
                                );
                            }
                        });
                    });
            });
            self.master_strip(ui, p, h, pos);
        });
    }

    fn strip(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        h: f32,
        kind: StripKind,
        name: &str,
        sub: &str,
        color: Color32,
        volume_db: f32,
        pan: f32,
        mute: bool,
        solo: bool,
        out: &str,
        sends: &[(String, f32)],
        pos: Option<f32>,
    ) {
        let is_bus = matches!(kind, StripKind::Bus);
        let selected = matches!(kind, StripKind::Track(i) if i == self.selected);
        let meter = self.rendered.as_ref().and_then(|r| {
            r.strips
                .iter()
                .find(|m| m.name == name && (m.name != "master"))
                .map(|m| (m.level_at(pos), m.peak_db))
        });
        let frame = egui::Frame::none()
            .fill(if selected {
                Color32::from_rgb(30, 28, 52)
            } else {
                PANEL
            })
            .rounding(10.0)
            .stroke(Stroke::new(
                1.0_f32,
                if selected {
                    ACCENT.gamma_multiply(0.7)
                } else {
                    LINE
                },
            ))
            .inner_margin(egui::Margin::symmetric(8.0, 8.0));
        let resp = frame.show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(STRIP_W - 16.0);
                ui.set_max_width(STRIP_W - 16.0);
                ui.set_height(h - 18.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                // header
                let (hr, hresp) =
                    ui.allocate_exact_size(Vec2::new(STRIP_W - 16.0, 34.0), Sense::click());
                let pt = ui.painter();
                pt.rect_filled(
                    Rect::from_min_size(hr.min, Vec2::new(hr.width(), 3.0)),
                    2.0,
                    color,
                );
                pt.text(
                    hr.left_top() + Vec2::new(0.0, 10.0),
                    Align2::LEFT_TOP,
                    name,
                    FontId::proportional(13.0),
                    if mute { DIM } else { TEXT },
                );
                let mut subtxt = sub.to_string();
                if subtxt.len() > 14 {
                    subtxt.truncate(13);
                    subtxt.push('.');
                }
                pt.text(
                    hr.left_top() + Vec2::new(0.0, 25.0),
                    Align2::LEFT_TOP,
                    subtxt,
                    FontId::proportional(9.5),
                    if is_bus {
                        ACCENT2.gamma_multiply(0.8)
                    } else {
                        DIM
                    },
                );
                if hresp.clicked() {
                    if let StripKind::Track(i) = kind {
                        self.selected = i;
                    }
                }
                // routing chip
                let (orr, _) =
                    ui.allocate_exact_size(Vec2::new(STRIP_W - 16.0, 17.0), Sense::hover());
                let pt = ui.painter();
                pt.rect_filled(orr, 5.0, BG);
                pt.text(
                    orr.left_center() + Vec2::new(6.0, 0.0),
                    Align2::LEFT_CENTER,
                    "OUT",
                    FontId::proportional(8.5),
                    DIM,
                );
                pt.text(
                    orr.right_center() - Vec2::new(6.0, 0.0),
                    Align2::RIGHT_CENTER,
                    out,
                    FontId::proportional(10.0),
                    if out == "master" { TEXT } else { ACCENT2 },
                );
                // sends (drag horizontally to change)
                let rows = 2usize;
                for k in 0..rows {
                    let (sr, _) =
                        ui.allocate_exact_size(Vec2::new(STRIP_W - 16.0, 16.0), Sense::hover());
                    let Some((bus, db)) = sends.get(k) else {
                        ui.painter().rect_stroke(
                            sr,
                            4.0,
                            Stroke::new(1.0_f32, LINE.gamma_multiply(0.5)),
                        );
                        ui.painter().text(
                            sr.center(),
                            Align2::CENTER_CENTER,
                            if k == 0 && !is_bus { "no sends" } else { "" },
                            FontId::proportional(9.0),
                            DIM.gamma_multiply(0.6),
                        );
                        continue;
                    };
                    let id = egui::Id::new(("send", name.to_string(), bus.clone()));
                    let r = ui.interact(sr, id, Sense::drag());
                    let mut v: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(*db);
                    if r.dragged() {
                        v = (v + r.drag_delta().x * 0.25).clamp(-60.0, 6.0);
                        ui.ctx().data_mut(|d| d.insert_temp(id, v));
                        self.dragging = true;
                    }
                    if r.drag_stopped() {
                        ui.ctx().data_mut(|d| d.remove::<f32>(id));
                        self.call("set_send", json!({"track": name, "bus": bus, "db": v}));
                        self.dragging = false;
                    }
                    let pt = ui.painter();
                    pt.rect_filled(sr, 4.0, BG);
                    let frac = meter_pos(v);
                    pt.rect_filled(
                        Rect::from_min_size(sr.min, Vec2::new(sr.width() * frac, sr.height())),
                        4.0,
                        ACCENT2.gamma_multiply(0.28),
                    );
                    pt.text(
                        sr.left_center() + Vec2::new(5.0, 0.0),
                        Align2::LEFT_CENTER,
                        bus,
                        FontId::proportional(9.5),
                        TEXT,
                    );
                    pt.text(
                        sr.right_center() - Vec2::new(5.0, 0.0),
                        Align2::RIGHT_CENTER,
                        format!("{v:.0}"),
                        FontId::monospace(9.5),
                        ACCENT2,
                    );
                    r.on_hover_text(format!("send to {bus}: drag to change"));
                }
                // pan knob (centered)
                let pid = egui::Id::new(("pan", name.to_string()));
                let mut pv: f32 = ui.ctx().data(|d| d.get_temp(pid)).unwrap_or(pan);
                let before = pv;
                let done = ui
                    .horizontal(|ui| {
                        ui.add_space((STRIP_W - 16.0 - 50.0) / 2.0);
                        knob(ui, "pan", &mut pv, -1.0, 1.0, false, color).1
                    })
                    .inner;
                if (pv - before).abs() > 1e-6 {
                    ui.ctx().data_mut(|d| d.insert_temp(pid, pv));
                    self.dragging = true;
                }
                if done {
                    ui.ctx().data_mut(|d| d.remove::<f32>(pid));
                    self.call("set_mixer", json!({"track": name, "pan": pv}));
                    self.dragging = false;
                }
                // meter + fader
                let fh = (ui.available_height() - 44.0).max(80.0);
                let vid = egui::Id::new(("fader", name.to_string()));
                let mut vv: f32 = ui.ctx().data(|d| d.get_temp(vid)).unwrap_or(volume_db);
                let auto_vol = p.automation.iter().any(|l| {
                    l.enabled && l.target.eq_ignore_ascii_case(name) && l.param == "volume"
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(4.0);
                    let (mr, _) = ui.allocate_exact_size(Vec2::new(22.0, fh), Sense::hover());
                    let ((l, r), peak) = meter.unwrap_or(((-120.0, -120.0), -120.0));
                    super::widgets::meter(ui.painter(), mr, l, r, peak);
                    let (resp, done) = fader(
                        ui,
                        vid,
                        &mut vv,
                        Vec2::new(32.0, fh),
                        if auto_vol { WARN } else { color },
                    );
                    if resp.dragged() {
                        ui.ctx().data_mut(|d| d.insert_temp(vid, vv));
                        self.dragging = true;
                    }
                    if done {
                        ui.ctx().data_mut(|d| d.remove::<f32>(vid));
                        self.call("set_mixer", json!({"track": name, "volume_db": vv}));
                        self.dragging = false;
                    }
                    if auto_vol {
                        resp.on_hover_text("volume is automated: the lane overrides this fader");
                    }
                });
                // readout + mute/solo
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(
                        RichText::new(fmt_db(vv))
                            .monospace()
                            .size(11.0)
                            .color(if auto_vol { WARN } else { TEXT }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !is_bus && mini_pill(ui, "S", solo, GOOD).clicked() {
                            self.call("set_mixer", json!({"track": name, "solo": !solo}));
                        }
                        if mini_pill(ui, "M", mute, WARN).clicked() {
                            self.call("set_mixer", json!({"track": name, "mute": !mute}));
                        }
                    });
                });
                let pk = meter.map(|m| m.1).unwrap_or(-120.0);
                ui.label(
                    RichText::new(format!("peak {}", fmt_db(pk)))
                        .size(9.5)
                        .color(if pk > -0.5 { HOT } else { DIM }),
                );
            })
        });
        let _ = resp;
    }

    fn master_strip(&mut self, ui: &mut egui::Ui, p: &Project, h: f32, pos: Option<f32>) {
        let w = 150.0;
        let (lv, peak) = self
            .rendered
            .as_ref()
            .and_then(|r| {
                r.strips
                    .iter()
                    .find(|m| m.name == "master")
                    .map(|m| (m.level_at(pos), m.peak_db))
            })
            .unwrap_or(((-120.0, -120.0), -120.0));
        let loud = self.rendered.as_ref().map(|r| r.report.loudness.clone());
        egui::Frame::none()
            .fill(Color32::from_rgb(26, 22, 44))
            .rounding(10.0)
            .stroke(Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6)))
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(w - 20.0);
                    ui.set_height(h - 18.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let (hr, _) = ui.allocate_exact_size(Vec2::new(w - 20.0, 34.0), Sense::hover());
                    let pt = ui.painter();
                    pt.rect_filled(
                        Rect::from_min_size(hr.min, Vec2::new(hr.width(), 3.0)),
                        2.0,
                        ACCENT,
                    );
                    pt.text(
                        hr.left_top() + Vec2::new(0.0, 10.0),
                        Align2::LEFT_TOP,
                        "MASTER",
                        FontId::proportional(13.0),
                        TEXT,
                    );
                    pt.text(
                        hr.left_top() + Vec2::new(0.0, 25.0),
                        Align2::LEFT_TOP,
                        p.master_effects
                            .iter()
                            .map(|e| e.type_name())
                            .collect::<Vec<_>>()
                            .join(" / "),
                        FontId::proportional(9.5),
                        ACCENT.gamma_multiply(1.3),
                    );
                    if let Some(l) = &loud {
                        let rows = [
                            (
                                "LUFS",
                                format!("{:.1}", l.integrated_lufs),
                                if (l.integrated_lufs + 14.0).abs() <= 2.0 {
                                    GOOD
                                } else {
                                    WARN
                                },
                            ),
                            (
                                "TRUE PK",
                                format!("{:.1}", l.true_peak_dbtp),
                                if l.true_peak_dbtp <= -1.0 {
                                    GOOD
                                } else if l.true_peak_dbtp <= 0.0 {
                                    WARN
                                } else {
                                    HOT
                                },
                            ),
                            ("LRA", format!("{:.1} LU", l.loudness_range_lu), TEXT),
                            (
                                "MONO",
                                format!("{:+.1} dB", l.mono_fold_db),
                                if l.mono_fold_db > -3.0 { GOOD } else { WARN },
                            ),
                        ];
                        for (k, v, c) in rows {
                            let (r, _) =
                                ui.allocate_exact_size(Vec2::new(w - 20.0, 17.0), Sense::hover());
                            let pt = ui.painter();
                            pt.rect_filled(r, 5.0, BG);
                            pt.text(
                                r.left_center() + Vec2::new(6.0, 0.0),
                                Align2::LEFT_CENTER,
                                k,
                                FontId::proportional(8.5),
                                DIM,
                            );
                            pt.text(
                                r.right_center() - Vec2::new(6.0, 0.0),
                                Align2::RIGHT_CENTER,
                                v,
                                FontId::monospace(10.5),
                                c,
                            );
                        }
                    }
                    let fh = (ui.available_height() - 26.0).max(80.0);
                    let vid = egui::Id::new("fader-master");
                    let mut vv: f32 = ui
                        .ctx()
                        .data(|d| d.get_temp(vid))
                        .unwrap_or(p.master_volume_db);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add_space(20.0);
                        let (mr, _) = ui.allocate_exact_size(Vec2::new(30.0, fh), Sense::hover());
                        super::widgets::meter(ui.painter(), mr, lv.0, lv.1, peak);
                        let (resp, done) = fader(ui, vid, &mut vv, Vec2::new(36.0, fh), ACCENT);
                        if resp.dragged() {
                            ui.ctx().data_mut(|d| d.insert_temp(vid, vv));
                            self.dragging = true;
                        }
                        if done {
                            ui.ctx().data_mut(|d| d.remove::<f32>(vid));
                            self.call("set_mixer", json!({"track": "master", "volume_db": vv}));
                            self.dragging = false;
                        }
                    });
                    ui.label(
                        RichText::new(format!("{} dB", fmt_db(vv)))
                            .monospace()
                            .size(11.0),
                    );
                })
            });
    }

    // ---------------- automation ----------------
    pub(super) fn automation_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let lanes: Vec<(usize, &AutomationLane)> = p.automation.iter().enumerate().collect();
        if let Some(sel) = &self.auto_sel {
            if !p.automation.iter().any(|l| l.is_target(&sel.0, &sel.1)) {
                self.auto_sel = None;
            }
        }
        if self.auto_sel.is_none() {
            let st = p
                .tracks
                .get(self.selected)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            let pick = p
                .automation
                .iter()
                .find(|l| l.target.eq_ignore_ascii_case(&st))
                .or(p.automation.first());
            self.auto_sel = pick.map(|l| (l.target.clone(), l.param.clone()));
        }
        let avail = ui.available_size();
        ui.horizontal_top(|ui| {
            // ---- lane list + tools
            egui::Frame::none().fill(PANEL).rounding(10.0).stroke(Stroke::new(1.0_f32, LINE)).inner_margin(10.0).show(ui, |ui| ui.vertical(|ui| {
                ui.set_width(214.0);
                ui.set_height(avail.y - 22.0);
                egui::ScrollArea::vertical().id_salt("lanes").auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(RichText::new("LANES").size(10.5).color(DIM).strong().extra_letter_spacing(1.2));
                    if lanes.is_empty() {
                        ui.label(RichText::new("No automation yet. Add a lane below or ask your AI for a riser before the drop.").size(11.0).color(DIM));
                    }
                    for (_, l) in &lanes {
                        let sel = self.auto_sel.as_ref().is_some_and(|s| l.is_target(&s.0, &s.1));
                        let color = p.track_index(&l.target).map(|i| track_color(&p.tracks[i].name, p.tracks[i].instrument.kind_name())).unwrap_or(ACCENT2);
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
                        let pt = ui.painter();
                        pt.rect_filled(r, 8.0, if sel { ACCENT.gamma_multiply(0.30) } else { PANEL2 });
                        if sel {
                            pt.rect_stroke(r, 8.0, Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.8)));
                        }
                        pt.rect_filled(Rect::from_min_size(r.min + Vec2::new(0.0, 6.0), Vec2::new(3.0, r.height() - 12.0)), 2.0, color);
                        pt.text(r.left_top() + Vec2::new(10.0, 7.0), Align2::LEFT_TOP, &l.target, FontId::proportional(12.0), if l.enabled { TEXT } else { DIM });
                        pt.text(r.left_top() + Vec2::new(10.0, 23.0), Align2::LEFT_TOP, &l.param, FontId::monospace(10.0), DIM);
                        // sparkline
                        let sp = Rect::from_min_max(Pos2::new(r.right() - 70.0, r.top() + 9.0), Pos2::new(r.right() - 8.0, r.bottom() - 9.0));
                        let song = p.song_beats().max(1.0);
                        let (lo, hi, log) = display_range(l);
                        let pts: Vec<Pos2> = (0..=31)
                            .map(|k| {
                                let b = song * k as f32 / 31.0;
                                let v = l.value_at(b).unwrap_or(lo);
                                Pos2::new(sp.left() + sp.width() * k as f32 / 31.0, sp.bottom() - norm(v, lo, hi, log) * sp.height())
                            })
                            .collect();
                        pt.add(egui::Shape::line(pts, Stroke::new(1.5_f32, color)));
                        if resp.clicked() {
                            self.auto_sel = Some((l.target.clone(), l.param.clone()));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("ADD LANE").size(10.5).color(DIM).strong().extra_letter_spacing(1.2));
                    if let Some(t) = p.tracks.get(self.selected) {
                        let opts = crate::tools_studio::automatable(p, &t.name);
                        let mut pick = String::new();
                        egui::ComboBox::from_id_salt("add_lane").width(190.0).selected_text(RichText::new(format!("{} param...", t.name)).size(11.5)).show_ui(ui, |ui| {
                            for o in &opts {
                                ui.selectable_value(&mut pick, o.clone(), RichText::new(o).monospace().size(11.0));
                            }
                        });
                        if !pick.is_empty() {
                            let (lo, hi) = automation::default_range(&pick);
                            let song = p.song_beats();
                            self.call("add_automation", json!({"track": t.name, "param": pick, "points": [[0.0, hi], [song, lo]], "curve": "linear"}));
                            self.auto_sel = Some((t.name.clone(), pick));
                        }
                    }
                    if let Some((target, param)) = self.auto_sel.clone() {
                        let section = p.patterns.get(self.pattern).map(|x| x.name.clone()).unwrap_or_default();
                        ui.add_space(8.0);
                        ui.label(RichText::new(format!("SHAPE  over '{section}'")).size(10.5).color(DIM).strong().extra_letter_spacing(1.0));
                        egui::Grid::new("shapes").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
                            for (i, (label, shape, rate)) in [
                                ("riser", "riser", "1/4"),
                                ("sweep down", "sweep_down", "1/4"),
                                ("fade in", "fade_in", "1/4"),
                                ("fade out", "fade_out", "1/4"),
                                ("pump 1/4", "pump", "1/4"),
                                ("lfo 1/8", "lfo", "1/8"),
                            ]
                            .iter()
                            .enumerate()
                            {
                                let b = egui::Button::new(RichText::new(*label).size(11.5)).fill(PANEL2).min_size(Vec2::new(92.0, 24.0));
                                if ui.add(b).on_hover_text(format!("generate_automation shape={shape}")).clicked() {
                                    self.call("generate_automation", json!({"track": target, "param": param, "shape": shape, "section": section, "rate": rate}));
                                }
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let en = p.automation.iter().find(|l| l.is_target(&target, &param)).map(|l| l.enabled).unwrap_or(true);
                            if ui.add(egui::Button::new(RichText::new(if en { "bypass" } else { "enable" }).size(11.5)).fill(PANEL2).min_size(Vec2::new(92.0, 24.0))).clicked() {
                                self.call("set_automation_points", json!({"track": target, "param": param, "enabled": !en}));
                            }
                            if ui.add(egui::Button::new(RichText::new("delete lane").size(11.5).color(HOT)).fill(PANEL2).min_size(Vec2::new(92.0, 24.0))).clicked() {
                                self.call("clear_automation", json!({"track": target, "param": param}));
                                self.auto_sel = None;
                            }
                        });
                    }
                });
            }));
            // ---- curve editor
            let lane = self.auto_sel.as_ref().and_then(|s| p.automation.iter().find(|l| l.is_target(&s.0, &s.1))).cloned();
            let size = Vec2::new(ui.available_width(), avail.y - 4.0);
            self.curve_editor(ui, p, lane.as_ref(), size);
        });
    }

    fn curve_editor(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        lane: Option<&AutomationLane>,
        size: Vec2,
    ) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let pt = ui.painter_at(rect);
        pt.rect_filled(rect, 10.0, PANEL);
        pt.rect_stroke(rect, 10.0, Stroke::new(1.0_f32, LINE));
        let song = p.song_beats().max(1.0);
        let ruler_h = 30.0;
        let plot = Rect::from_min_max(
            rect.min + Vec2::new(54.0, ruler_h + 10.0),
            rect.max - Vec2::new(14.0, 40.0),
        );
        let bx = |b: f32| plot.left() + plot.width() * (b / song);
        // sections
        let mut start = 0.0;
        for (i, s) in p.song_sections().iter().enumerate() {
            let Ok(pi) = p.pattern_index(&s.pattern) else {
                continue;
            };
            let len = (p.patterns[pi].steps() * s.repeats.max(1)) as f32 / 4.0;
            let r = Rect::from_min_max(
                Pos2::new(bx(start) + 1.0, rect.top() + 8.0),
                Pos2::new(bx(start + len) - 1.0, rect.top() + ruler_h),
            );
            let col = lerp_color(ACCENT, ACCENT2, (pi as f32 * 0.37) % 1.0);
            pt.rect_filled(
                r,
                5.0,
                col.gamma_multiply(if pi == self.pattern { 0.45 } else { 0.18 }),
            );
            pt.text(
                r.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{}  {}", i, s.pattern),
                FontId::proportional(10.5),
                TEXT,
            );
            // section shading in the plot
            if i % 2 == 1 {
                pt.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(bx(start), plot.top()),
                        Pos2::new(bx(start + len), plot.bottom()),
                    ),
                    0.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 4),
                );
            }
            start += len;
        }
        // grid: bars
        let bars = (song / 4.0).ceil() as usize;
        let every = if bars > 32 {
            4
        } else if bars > 16 {
            2
        } else {
            1
        };
        for b in 0..=bars {
            let x = bx(b as f32 * 4.0);
            let strong = b % 4 == 0;
            pt.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(
                    1.0_f32,
                    if strong {
                        LINE
                    } else {
                        LINE.gamma_multiply(0.45)
                    },
                ),
            );
            if b % every == 0 && b < bars {
                pt.text(
                    Pos2::new(x + 3.0, plot.bottom() + 11.0),
                    Align2::LEFT_CENTER,
                    format!("{}", b + 1),
                    FontId::monospace(9.5),
                    DIM,
                );
            }
        }
        for k in 0..=4 {
            let y = plot.top() + plot.height() * k as f32 / 4.0;
            pt.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0_f32, LINE.gamma_multiply(0.45)),
            );
        }
        let Some(lane) = lane else {
            pt.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "Select or add an automation lane",
                FontId::proportional(15.0),
                DIM,
            );
            return;
        };
        let color = p
            .track_index(&lane.target)
            .map(|i| track_color(&p.tracks[i].name, p.tracks[i].instrument.kind_name()))
            .unwrap_or(ACCENT2);
        let (lo, hi, log) = display_range(lane);
        let vy = |v: f32| plot.bottom() - norm(v, lo, hi, log) * plot.height();
        // y labels
        for k in 0..=4 {
            let t = 1.0 - k as f32 / 4.0;
            let v = denorm(t, lo, hi, log);
            pt.text(
                Pos2::new(
                    plot.left() - 8.0,
                    plot.top() + plot.height() * k as f32 / 4.0,
                ),
                Align2::RIGHT_CENTER,
                format_value(v),
                FontId::monospace(9.5),
                DIM,
            );
        }
        pt.text(
            rect.left_bottom() + Vec2::new(14.0, -13.0),
            Align2::LEFT_CENTER,
            format!(
                "{} : {}{}",
                lane.target,
                lane.param,
                if lane.enabled { "" } else { "  (bypassed)" }
            ),
            FontId::proportional(11.0),
            color,
        );
        // curve + fill
        let n = plot.width().max(2.0) as usize;
        let mut line = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let x = plot.left() + i as f32;
            let b = (i as f32 / n as f32) * song;
            let y = vy(lane.value_at(b).unwrap_or(lo));
            if i % 2 == 0 {
                pt.line_segment(
                    [Pos2::new(x, y), Pos2::new(x, plot.bottom())],
                    Stroke::new(2.0_f32, color.gamma_multiply(0.10)),
                );
            }
            line.push(Pos2::new(x, y));
        }
        let stroke_col = if lane.enabled { color } else { DIM };
        pt.add(egui::Shape::line(line, Stroke::new(2.2_f32, stroke_col)));
        // points (thin out dense generated lanes)
        let hover = resp.hover_pos();
        let mut hit: Option<(f32, f32)> = None;
        let stride = (lane.points.len() / 160).max(1);
        for (k, q) in lane.points.iter().enumerate() {
            let c = Pos2::new(bx(q.beat), vy(q.value));
            let near = hover.is_some_and(|h| h.distance(c) < 7.0);
            if near {
                hit = Some((q.beat, q.value));
            }
            if k % stride == 0 || near {
                pt.circle_filled(
                    c,
                    if near { 5.5 } else { 3.5 },
                    if near { Color32::WHITE } else { stroke_col },
                );
                pt.circle_stroke(c, if near { 5.5 } else { 3.5 }, Stroke::new(1.0_f32, BG));
            }
        }
        // playhead
        let ppos = self.player.position_secs();
        let pb = ppos / (4.0 * p.step_secs());
        if pb <= song {
            let x = bx(pb);
            pt.line_segment(
                [Pos2::new(x, rect.top() + 6.0), Pos2::new(x, plot.bottom())],
                Stroke::new(1.5_f32, Color32::WHITE.gamma_multiply(0.85)),
            );
            if let Some(v) = lane.value_at(pb) {
                pt.circle_filled(Pos2::new(x, vy(v)), 4.0, Color32::WHITE);
                pt.text(
                    Pos2::new(x + 8.0, vy(v) - 10.0),
                    Align2::LEFT_CENTER,
                    format_value(v),
                    FontId::monospace(10.5),
                    TEXT,
                );
            }
        }
        // hover readout + edits
        if let Some(h) = hover.filter(|h| plot.expand(4.0).contains(*h)) {
            let beat = ((h.x - plot.left()) / plot.width() * song).clamp(0.0, song);
            let beat = (beat * 4.0).round() / 4.0;
            let t = ((plot.bottom() - h.y) / plot.height()).clamp(0.0, 1.0);
            let v = denorm(t, lo, hi, log);
            let label = match hit {
                Some((b, val)) => format!(
                    "beat {b:.2}  {}  (right-click to delete)",
                    format_value(val)
                ),
                None => format!("beat {beat:.2}  {}  (click to add)", format_value(v)),
            };
            pt.text(
                Pos2::new(plot.right(), rect.bottom() - 13.0),
                Align2::RIGHT_CENTER,
                label,
                FontId::monospace(10.0),
                DIM,
            );
            if resp.clicked() && hit.is_none() {
                self.call("set_automation_points", json!({"track": lane.target, "param": lane.param, "mode": "merge", "points": [[beat, v]]}));
            }
            if resp.secondary_clicked() {
                if let Some((b, _)) = hit {
                    self.call("set_automation_points", json!({"track": lane.target, "param": lane.param, "mode": "erase", "from_beat": b - 1e-3, "to_beat": b + 1e-3}));
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum StripKind {
    Track(usize),
    Bus,
}

/// Value range to draw a lane in: its points padded, or the param's default.
fn display_range(l: &AutomationLane) -> (f32, f32, bool) {
    let (dlo, dhi) = automation::default_range(&l.param);
    let (lo, hi) = l.range().unwrap_or((dlo, dhi));
    let log = automation::is_log_param(&l.param) && lo > 0.0;
    if log {
        let (a, b) = (lo.min(dlo.max(1.0)), hi.max(dhi));
        return (a, b, true);
    }
    let (a, b) = (lo.min(dlo), hi.max(dhi));
    if (b - a).abs() < 1e-6 {
        (a - 1.0, b + 1.0, false)
    } else {
        (a, b, false)
    }
}

fn norm(v: f32, lo: f32, hi: f32, log: bool) -> f32 {
    if log {
        ((v.max(1e-6).ln() - lo.ln()) / (hi.ln() - lo.ln()).max(1e-6)).clamp(0.0, 1.0)
    } else {
        ((v - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0)
    }
}

fn denorm(t: f32, lo: f32, hi: f32, log: bool) -> f32 {
    if log {
        (lo.ln() + (hi.ln() - lo.ln()) * t).exp()
    } else {
        lo + (hi - lo) * t
    }
}

/// Compact M / S toggle for mixer strips.
fn mini_pill(ui: &mut egui::Ui, text: &str, on: bool, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(18.0, 16.0), Sense::click());
    let fill = if on {
        color
    } else if resp.hovered() {
        Color32::from_rgb(44, 50, 70)
    } else {
        Color32::from_rgb(33, 38, 54)
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(9.5),
        if on { BG } else { DIM },
    );
    resp
}
