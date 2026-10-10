//! Mixer and Automation views of the central panel. Like everything else in
//! the studio, every edit is an `Engine` tool call.

use super::console::{
    console_fader, counter_box, meter_scale, name_plate, pan_knob, selector_box, text_toggle,
    zoned_meter,
};
use super::theme::Tokens;
use super::widgets::*;
use super::Studio;
use crate::automation::{self, AutomationLane};
use crate::console_law::{db_text, fader_db_to_pos, pan_text};
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

// Strip layout and metering adapted from SoundCraft `crates/ui-egui/src/mix_window.rs`
// (commit eac0edd): flat console strips with labelled sections, insert/send slots,
// I/O selector, pan knob over a counter readout, S/M toggles, fader + zoned meters
// with peak hold and clip LED, a dB counter and a name plate.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors (MIT OR Apache-2.0,
// used here under MIT). See THIRD_PARTY_NOTICES.md.
const STRIP_W: f32 = 96.0;
const MASTER_W: f32 = 150.0;
/// Insert / send slot rows: (inserts, sends) for a strip of height `h`. Short windows get
/// fewer rows so the fader keeps a usable throw.
fn slot_rows(h: f32) -> (usize, usize) {
    if h < 560.0 {
        (2, 2)
    } else {
        (4, 3)
    }
}

fn fmt_db(db: f32) -> String {
    if db <= -59.5 {
        "-inf".into()
    } else {
        format!("{db:+.1}")
    }
}

/// What one mixer strip shows.
struct StripData {
    kind: StripKind,
    name: String,
    sub: String,
    color: Color32,
    volume_db: f32,
    pan: f32,
    mute: bool,
    solo: bool,
    out: String,
    sends: Vec<(String, f32)>,
    inserts: Vec<(String, bool)>,
}

/// A labelled strip section background; returns the content rect under its title.
fn section(p: &egui::Painter, r: Rect, title: &str) -> Rect {
    let t = Tokens::DARK;
    p.rect_filled(r, 2.0, t.strip_section);
    p.text(
        Pos2::new(r.center().x, r.min.y + 7.0),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(9.0),
        t.header_text,
    );
    Rect::from_min_max(Pos2::new(r.min.x + 2.0, r.min.y + 15.0), r.max)
}

fn slot_rect(area: Rect, k: usize) -> Rect {
    Rect::from_min_size(
        Pos2::new(area.min.x, area.min.y + k as f32 * 17.0),
        Vec2::new(area.width() - 2.0, 15.0),
    )
}

impl Studio {
    fn live_pos(&self) -> Option<f32> {
        self.player
            .is_playing()
            .then(|| self.player.position_secs())
    }

    // ---------------- mixer ----------------
    pub(super) fn mixer(&mut self, ui: &mut egui::Ui, p: &Project) {
        let h = ui.available_height().max(300.0);
        let pos = self.live_pos();
        let bus_names: Vec<String> = p.buses.iter().map(|b| b.name.clone()).collect();
        let mut strips: Vec<StripData> = p
            .tracks
            .iter()
            .enumerate()
            .map(|(ti, t)| StripData {
                kind: StripKind::Track(ti),
                name: t.name.clone(),
                sub: t.instrument.kind_name().to_string(),
                color: track_color(&t.name, t.instrument.kind_name()),
                volume_db: t.volume_db,
                pan: t.pan,
                mute: t.mute,
                solo: t.solo,
                out: t.output.clone().unwrap_or_else(|| "master".into()),
                sends: t.sends.iter().map(|s| (s.bus.clone(), s.db)).collect(),
                inserts: t
                    .effects
                    .iter()
                    .map(|e| (e.type_name(), e.bypassed()))
                    .collect(),
            })
            .collect();
        strips.extend(p.buses.iter().map(|b| {
            StripData {
                kind: StripKind::Bus,
                name: b.name.clone(),
                sub: "bus".into(),
                color: ACCENT2,
                volume_db: b.volume_db,
                pan: b.pan,
                mute: b.mute,
                solo: false,
                out: b.output.clone().unwrap_or_else(|| "master".into()),
                sends: Vec::new(),
                inserts: b
                    .effects
                    .iter()
                    .map(|e| (e.type_name(), e.bypassed()))
                    .collect(),
            }
        }));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let scroll_w = ui.available_width() - MASTER_W - 8.0;
            ui.allocate_ui(Vec2::new(scroll_w, h), |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("mixer")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            for s in &strips {
                                self.strip(ui, p, h, s, &bus_names, pos);
                            }
                        });
                    });
            });
            self.master_strip(ui, p, h, pos);
        });
    }

    fn strip_meter_levels(&self, name: &str, pos: Option<f32>) -> ((f32, f32), f32) {
        self.rendered
            .as_ref()
            .and_then(|r| {
                r.strips
                    .iter()
                    .find(|m| m.name == name)
                    .map(|m| (m.level_at(pos), m.peak_db))
            })
            .unwrap_or(((-120.0, -120.0), -120.0))
    }

    fn strip(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        h: f32,
        s: &StripData,
        buses: &[String],
        pos: Option<f32>,
    ) {
        let t = Tokens::DARK;
        let (insert_rows, send_rows) = slot_rows(h);
        let is_bus = matches!(s.kind, StripKind::Bus);
        let selected = matches!(s.kind, StripKind::Track(i) if i == self.selected);
        let name = s.name.as_str();
        let ((lv_l, lv_r), peak) = if name == "master" {
            ((-120.0, -120.0), -120.0)
        } else {
            self.strip_meter_levels(name, pos)
        };
        let (r, _) = ui.allocate_exact_size(Vec2::new(STRIP_W, h), Sense::hover());
        let pt = ui.painter().clone();
        pt.rect_filled(
            r,
            0.0,
            if selected {
                Color32::from_rgb(54, 58, 64)
            } else {
                t.strip_bg
            },
        );
        pt.rect_filled(
            Rect::from_min_size(r.min, Vec2::new(STRIP_W, 4.0)),
            0.0,
            s.color,
        );
        let x0 = r.min.x + 4.0;
        let iw = STRIP_W - 8.0;
        let mut y = r.min.y + 8.0;
        // kind line
        pt.text(
            Pos2::new(r.center().x, y + 5.0),
            Align2::CENTER_CENTER,
            &s.sub,
            FontId::proportional(9.0),
            if is_bus { ACCENT2 } else { t.text_dim },
        );
        y += 14.0;
        // INSERTS
        let sec = Rect::from_min_size(
            Pos2::new(x0, y),
            Vec2::new(iw, 16.0 + insert_rows as f32 * 17.0),
        );
        let area = section(&pt, sec, "INSERTS");
        for k in 0..insert_rows {
            let sr = slot_rect(area, k);
            let ins = s.inserts.get(k);
            let resp = ui.interact(sr, ui.id().with(("ins", name, k)), Sense::click());
            let fill = match ins {
                Some((_, true)) => Color32::from_rgb(70, 56, 30),
                Some(_) => Color32::from_rgb(52, 62, 80),
                None => t.slot_bg,
            };
            pt.rect(
                sr,
                2.0,
                if resp.hovered() {
                    fill.gamma_multiply(1.3)
                } else {
                    fill
                },
                Stroke::new(1.0_f32, Color32::from_rgb(16, 16, 16)),
            );
            match ins {
                Some((label, bypass)) => {
                    pt.with_clip_rect(sr).text(
                        sr.center(),
                        Align2::CENTER_CENTER,
                        label,
                        FontId::proportional(10.0),
                        if *bypass { t.text_dim } else { t.text },
                    );
                    let resp = resp.on_hover_text(format!(
                        "{label}{} · click: edit in the inspector",
                        if *bypass { " (bypassed)" } else { "" }
                    ));
                    if resp.clicked() {
                        if let StripKind::Track(i) = s.kind {
                            self.selected = i;
                        }
                    }
                }
                None => {
                    pt.circle_filled(Pos2::new(sr.min.x + 6.0, sr.center().y), 1.5, t.text_dim);
                }
            }
        }
        if s.inserts.len() > insert_rows {
            pt.text(
                Pos2::new(sec.max.x - 3.0, sec.min.y + 7.0),
                Align2::RIGHT_CENTER,
                format!("+{}", s.inserts.len() - insert_rows),
                FontId::proportional(8.5),
                ACCENT,
            );
        }
        y = sec.max.y + 4.0;
        // SENDS (drag horizontally to change)
        let sec = Rect::from_min_size(
            Pos2::new(x0, y),
            Vec2::new(iw, 16.0 + send_rows as f32 * 17.0),
        );
        let area = section(&pt, sec, "SENDS");
        for k in 0..send_rows {
            let sr = slot_rect(area, k);
            let Some((bus, db)) = s.sends.get(k) else {
                pt.rect(
                    sr,
                    2.0,
                    t.slot_bg,
                    Stroke::new(1.0_f32, Color32::from_rgb(16, 16, 16)),
                );
                continue;
            };
            let id = egui::Id::new(("send", name.to_string(), bus.clone()));
            let rr = ui.interact(sr, id, Sense::drag());
            let mut v: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(*db);
            if rr.dragged() {
                v = (v + rr.drag_delta().x * 0.25).clamp(-60.0, 6.0);
                ui.ctx().data_mut(|d| d.insert_temp(id, v));
                self.dragging = true;
            }
            if rr.drag_stopped() {
                ui.ctx().data_mut(|d| d.remove::<f32>(id));
                self.call("set_send", json!({"track": name, "bus": bus, "db": v}));
                self.dragging = false;
            }
            pt.rect(
                sr,
                2.0,
                t.slot_bg,
                Stroke::new(1.0_f32, Color32::from_rgb(16, 16, 16)),
            );
            let frac = fader_db_to_pos(v);
            pt.rect_filled(
                Rect::from_min_size(sr.min, Vec2::new(sr.width() * frac, sr.height())).shrink(1.0),
                1.0,
                t.accent_dark,
            );
            pt.with_clip_rect(sr).text(
                sr.left_center() + Vec2::new(4.0, 0.0),
                Align2::LEFT_CENTER,
                bus,
                FontId::proportional(9.5),
                t.text,
            );
            pt.text(
                sr.right_center() - Vec2::new(3.0, 0.0),
                Align2::RIGHT_CENTER,
                format!("{v:.0}"),
                FontId::monospace(9.0),
                t.counter_text,
            );
            rr.on_hover_text(format!("send to {bus}: drag to change"));
        }
        y = sec.max.y + 4.0;
        // I / O
        let sec = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(iw, 16.0 + 18.0));
        let area = section(&pt, sec, "OUTPUT");
        let out_r = Rect::from_min_size(area.min, Vec2::new(iw - 4.0, 16.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(out_r));
        let resp = selector_box(
            &mut c,
            out_r.width(),
            out_r.height(),
            &s.out,
            if s.out == "master" { t.text } else { ACCENT2 },
        );
        let popup = ui.id().with(("route", name));
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        egui::popup_below_widget(
            ui,
            popup,
            &resp,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(110.0);
                let dests = std::iter::once("master".to_string())
                    .chain(buses.iter().filter(|b| b.as_str() != name).cloned());
                for d in dests {
                    if ui.selectable_label(d == s.out, &d).clicked() && d != s.out {
                        if is_bus {
                            self.call("route_bus", json!({"bus": name, "destination": d}));
                        } else {
                            self.call("route_track", json!({"track": name, "bus": d}));
                        }
                    }
                }
            },
        );
        y = sec.max.y + 6.0;
        // pan knob + counter readout
        let ks = 34.0;
        let pid = egui::Id::new(("pan", name.to_string()));
        let mut pv: f32 = ui.ctx().data(|d| d.get_temp(pid)).unwrap_or(s.pan);
        let kr = Rect::from_center_size(Pos2::new(r.center().x, y + ks * 0.5), Vec2::splat(ks));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(kr));
        let (kresp, changed) = pan_knob(&mut c, ks, &mut pv, "Pan (drag; double-click centres)");
        if changed {
            ui.ctx().data_mut(|d| d.insert_temp(pid, pv));
            self.dragging = true;
        }
        if kresp.drag_stopped() || kresp.double_clicked() {
            ui.ctx().data_mut(|d| d.remove::<f32>(pid));
            self.call("set_mixer", json!({"track": name, "pan": pv}));
            self.dragging = false;
        }
        let pr = Rect::from_center_size(
            Pos2::new(r.center().x, y + ks + 8.0),
            Vec2::new(ks + 10.0, 13.0),
        );
        counter_box(&pt, pr, &pan_text(pv), 9.5);
        y += ks + 20.0;
        // Solo / Mute
        let bw = (iw - 6.0) / 2.0;
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(Pos2::new(x0, y), Vec2::new(iw, 20.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
        );
        row.spacing_mut().item_spacing.x = 6.0;
        if is_bus {
            row.add_space(bw + 6.0);
        } else if text_toggle(&mut row, Vec2::new(bw, 18.0), "S", s.solo, t.solo, "Solo").clicked()
        {
            self.call("set_mixer", json!({"track": name, "solo": !s.solo}));
        }
        if text_toggle(&mut row, Vec2::new(bw, 18.0), "M", s.mute, t.mute, "Mute").clicked() {
            self.call("set_mixer", json!({"track": name, "mute": !s.mute}));
        }
        y += 26.0;
        // fader + stereo meter
        let bottom_h = 58.0;
        let fh = (r.max.y - bottom_h - y).max(90.0);
        let vid = egui::Id::new(("fader", name.to_string()));
        let mut vv: f32 = ui.ctx().data(|d| d.get_temp(vid)).unwrap_or(s.volume_db);
        let auto_vol = p
            .automation
            .iter()
            .any(|l| l.enabled && l.target.eq_ignore_ascii_case(name) && l.param == "volume");
        let fr = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(iw * 0.6, fh));
        let (fresp, done) = console_fader(ui, fr, vid, &mut vv);
        if fresp.dragged() {
            ui.ctx().data_mut(|d| d.insert_temp(vid, vv));
            self.dragging = true;
        }
        if done {
            ui.ctx().data_mut(|d| d.remove::<f32>(vid));
            self.call("set_mixer", json!({"track": name, "volume_db": vv}));
            self.dragging = false;
        }
        if auto_vol {
            fresp.on_hover_text("volume is automated: the lane overrides this fader");
        }
        let mr = Rect::from_min_max(
            Pos2::new(fr.max.x + 4.0, fr.min.y + 6.0),
            Pos2::new(x0 + iw - 2.0, fr.max.y - 6.0),
        );
        let mw = mr.width() / 2.0 - 1.0;
        let clip = peak > -0.1;
        zoned_meter(
            &pt,
            Rect::from_min_size(mr.min, Vec2::new(mw, mr.height())),
            lv_l,
            peak,
            clip,
        );
        zoned_meter(
            &pt,
            Rect::from_min_size(
                Pos2::new(mr.min.x + mw + 2.0, mr.min.y),
                Vec2::new(mw, mr.height()),
            ),
            lv_r,
            peak,
            clip,
        );
        // dB counter, peak, name plate
        let vr = Rect::from_min_size(
            Pos2::new(x0 + 4.0, r.max.y - bottom_h + 4.0),
            Vec2::new(iw - 8.0, 15.0),
        );
        counter_box(&pt, vr, &db_text(vv), 10.5);
        if auto_vol {
            pt.rect_stroke(vr, 2.0, Stroke::new(1.0_f32, t.auto_read));
        }
        pt.text(
            Pos2::new(vr.center().x, vr.max.y + 8.0),
            Align2::CENTER_CENTER,
            format!("pk {}", fmt_db(peak)),
            FontId::proportional(9.0),
            if clip { HOT } else { t.text_dim },
        );
        let nr = Rect::from_min_size(Pos2::new(x0, r.max.y - 24.0), Vec2::new(iw, 18.0));
        name_plate(&pt, nr, name, selected);
        let nresp = ui.interact(nr, ui.id().with(("plate", name)), Sense::click());
        if nresp.clicked() {
            if let StripKind::Track(i) = s.kind {
                self.selected = i;
            }
        }
        pt.line_segment(
            [Pos2::new(r.max.x, r.min.y), Pos2::new(r.max.x, r.max.y)],
            Stroke::new(1.0_f32, t.border),
        );
    }

    fn master_strip(&mut self, ui: &mut egui::Ui, p: &Project, h: f32, pos: Option<f32>) {
        let t = Tokens::DARK;
        let ((lv_l, lv_r), peak) = self.strip_meter_levels("master", pos);
        let loud = self.rendered.as_ref().map(|r| r.report.loudness.clone());
        let (r, _) = ui.allocate_exact_size(Vec2::new(MASTER_W, h), Sense::hover());
        let pt = ui.painter().clone();
        pt.rect_filled(r, 0.0, Color32::from_rgb(44, 44, 46));
        pt.rect_filled(
            Rect::from_min_size(r.min, Vec2::new(MASTER_W, 4.0)),
            0.0,
            ACCENT,
        );
        let x0 = r.min.x + 5.0;
        let iw = MASTER_W - 10.0;
        let mut y = r.min.y + 8.0;
        pt.with_clip_rect(r).text(
            Pos2::new(r.center().x, y + 5.0),
            Align2::CENTER_CENTER,
            p.master_effects
                .iter()
                .map(|e| e.type_name())
                .collect::<Vec<_>>()
                .join(" / "),
            FontId::proportional(9.0),
            ACCENT,
        );
        y += 14.0;
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
            let sec = Rect::from_min_size(
                Pos2::new(x0, y),
                Vec2::new(iw, 16.0 + rows.len() as f32 * 17.0),
            );
            let area = section(&pt, sec, "LOUDNESS");
            for (k, (label, v, c)) in rows.into_iter().enumerate() {
                let sr = slot_rect(area, k);
                pt.rect_filled(sr, 2.0, t.counter_bg);
                pt.text(
                    sr.left_center() + Vec2::new(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(8.5),
                    t.text_dim,
                );
                pt.text(
                    sr.right_center() - Vec2::new(5.0, 0.0),
                    Align2::RIGHT_CENTER,
                    v,
                    FontId::monospace(10.0),
                    c,
                );
            }
            y = sec.max.y + 6.0;
        }
        let bottom_h = 58.0;
        let fh = (r.max.y - bottom_h - y).max(90.0);
        let vid = egui::Id::new("fader-master");
        let mut vv: f32 = ui
            .ctx()
            .data(|d| d.get_temp(vid))
            .unwrap_or(p.master_volume_db);
        let fr = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(54.0, fh));
        let (fresp, done) = console_fader(ui, fr, vid, &mut vv);
        if fresp.dragged() {
            ui.ctx().data_mut(|d| d.insert_temp(vid, vv));
            self.dragging = true;
        }
        if done {
            ui.ctx().data_mut(|d| d.remove::<f32>(vid));
            self.call("set_mixer", json!({"track": "master", "volume_db": vv}));
            self.dragging = false;
        }
        let mr = Rect::from_min_max(
            Pos2::new(fr.max.x + 6.0, fr.min.y + 6.0),
            Pos2::new(fr.max.x + 40.0, fr.max.y - 6.0),
        );
        let mw = mr.width() / 2.0 - 1.0;
        let clip = peak > -0.1;
        zoned_meter(
            &pt,
            Rect::from_min_size(mr.min, Vec2::new(mw, mr.height())),
            lv_l,
            peak,
            clip,
        );
        zoned_meter(
            &pt,
            Rect::from_min_size(
                Pos2::new(mr.min.x + mw + 2.0, mr.min.y),
                Vec2::new(mw, mr.height()),
            ),
            lv_r,
            peak,
            clip,
        );
        meter_scale(
            &pt,
            Rect::from_min_max(
                Pos2::new(mr.max.x + 2.0, mr.min.y),
                Pos2::new(r.max.x - 2.0, mr.max.y),
            ),
        );
        let vr = Rect::from_min_size(
            Pos2::new(x0 + 10.0, r.max.y - bottom_h + 4.0),
            Vec2::new(iw - 20.0, 15.0),
        );
        counter_box(&pt, vr, &format!("{} dB", db_text(vv)), 10.5);
        pt.text(
            Pos2::new(vr.center().x, vr.max.y + 8.0),
            Align2::CENTER_CENTER,
            format!("pk {}", fmt_db(peak)),
            FontId::proportional(9.0),
            if clip { HOT } else { t.text_dim },
        );
        let nr = Rect::from_min_size(Pos2::new(x0, r.max.y - 24.0), Vec2::new(iw, 18.0));
        name_plate(&pt, nr, "MASTER", false);
    }

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
