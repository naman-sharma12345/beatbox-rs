//! Mixer and Automation views of the central panel. Like everything else in
//! the studio, every edit is an `Engine` tool call.

use super::console::{
    console_fader, counter_box, meter_scale, name_plate, pan_knob, selector_box, text_toggle,
    zoned_meter,
};
use super::theme::Tokens;
use super::widgets::*;
use super::Studio;
/// Render a mix-helper answer as plain indented lines, no markup.
pub(super) fn plain_report(v: &serde_json::Value, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                let label = k.replace('_', " ");
                match val {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push_str(&format!("{pad}{label}\n"));
                        plain_report(val, depth + 1, out);
                    }
                    _ => {
                        out.push_str(&format!("{pad}{label}: "));
                        push_scalar(val, out);
                        out.push('\n');
                    }
                }
            }
        }
        serde_json::Value::Array(items) => {
            for it in items {
                plain_report(it, depth, out);
            }
        }
        _ => {
            out.push_str(&pad);
            push_scalar(v, out);
            out.push('\n');
        }
    }
}

fn push_scalar(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Number(n) => out.push_str(&n.to_string()),
        serde_json::Value::Bool(b) => out.push_str(if *b { "yes" } else { "no" }),
        serde_json::Value::Null => out.push('-'),
        _ => out.push_str("..."),
    }
}

/// A mixer helper answer as readable text: known reports become small grouped
/// tables, anything else falls back to the generic field dump.
fn format_readout(label: &str, tool: &str, v: &serde_json::Value) -> String {
    let num = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0);
    let mut out = format!("{label}\n");
    match tool {
        "stereo_image" => {
            let o = &v["overall"];
            out.push_str(&format!(
                "overall    width {:>4.2}  corr {:>5.2}  balance {:>+5.1} dB  mono {:>+5.1} dB\n",
                num(&o["width"]),
                num(&o["correlation"]),
                num(&o["balance_db"]),
                num(&o["mono_loss_db"])
            ));
            out.push_str("band Hz        width   corr   level   mono  balance\n");
            for b in v["bands"].as_array().cloned().unwrap_or_default() {
                let (lo, hi) = (num(&b["band_hz"][0]), num(&b["band_hz"][1]));
                out.push_str(&format!(
                    "{:>5.0}-{:<5.0}   {:>5.2}  {:>5.2}  {:>6.1}  {:>+5.1}  {:>+5.1}\n",
                    lo,
                    hi,
                    num(&b["width"]),
                    num(&b["correlation"]),
                    num(&b["level_db"]),
                    num(&b["mono_loss_db"]),
                    num(&b["balance_db"])
                ));
            }
            for sec in v["per_section"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!(
                    "section {}: width {:>4.2}  corr {:>5.2}\n",
                    sec["section"].as_str().unwrap_or("?"),
                    num(&sec["width"]),
                    num(&sec["correlation"])
                ));
            }
            for f in v["findings"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!("! {}\n", f["message"].as_str().unwrap_or("?")));
            }
        }
        "eq_curve" => {
            let used: Vec<String> = v["effects_measured"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|e| e.as_str().unwrap_or("?").to_string())
                .collect();
            out.push_str(&format!(
                "chain: {}\n",
                if used.is_empty() {
                    "none (flat)".to_string()
                } else {
                    used.join(" > ")
                }
            ));
            let pts: Vec<(f64, f64)> = v["points_hz_db"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|pt| (num(&pt[0]), num(&pt[1])))
                .collect();
            for row in pts.chunks(4) {
                let cells: Vec<String> = row
                    .iter()
                    .map(|(hz, db)| format!("{:>6.0} Hz {:>+5.1}", hz, db))
                    .collect();
                out.push_str(&cells.join("   "));
                out.push('\n');
            }
        }
        _ => {
            plain_report(v, 0, &mut out);
        }
    }
    if out.len() > 4000 {
        out.truncate(4000);
        out.push_str(" ...");
    }
    out
}

use crate::automation::{self, AutomationLane};
use crate::console_law::{db_text, fader_db_to_pos};
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
    if h < 360.0 {
        (1, 1)
    } else if h < 560.0 {
        (2, 2)
    } else {
        (4, 3)
    }
}

/// Compact strips page their slots instead of hiding effects or aux sends.
fn slot_page(ui: &mut egui::Ui, rect: Rect, id: egui::Id, count: usize, rows: usize) -> usize {
    let pages = count.max(1).div_ceil(rows);
    let mut page = ui.ctx().data(|d| d.get_temp::<usize>(id)).unwrap_or(0) % pages;
    if pages > 1 {
        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 15.0));
        let response = ui.interact(header, id, Sense::click());
        ui.painter().text(
            header.right_center() - Vec2::new(3.0, 0.0),
            Align2::RIGHT_CENTER,
            format!("{}/{} >", page + 1, pages),
            FontId::monospace(10.5),
            ACCENT,
        );
        if response
            .on_hover_text("click to show the next slots")
            .clicked()
        {
            page = (page + 1) % pages;
            ui.ctx().data_mut(|d| d.insert_temp(id, page));
        }
    }
    page * rows
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
        FontId::monospace(10.5),
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
    /// Compact console: real track/bus/master controls, with overflow scrolling.
    pub(super) fn console_dock(&mut self, ui: &mut egui::Ui, p: &Project) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("CONSOLE").monospace().size(11.0).color(DIM));
            if ui.small_button("Open mixer").clicked() {
                self.view = 1;
                self.workspace.request_view(1);
            }
            ui.label(
                RichText::new("Drag a fader or pan. Open Mixer for inserts, sends and routing.")
                    .size(11.0)
                    .color(DIM),
            );
        });
        let mut channels: Vec<(String, f32, f32, bool, bool, Color32, Option<usize>)> = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    t.name.clone(),
                    t.volume_db,
                    t.pan,
                    t.mute,
                    t.solo,
                    track_color(&t.name, t.instrument.kind_name()),
                    Some(i),
                )
            })
            .collect();
        channels.extend(p.buses.iter().map(|b| {
            (
                b.name.clone(),
                b.volume_db,
                b.pan,
                b.mute,
                false,
                ACCENT2,
                None,
            )
        }));
        channels.push((
            "master".into(),
            p.master_volume_db,
            0.0,
            false,
            false,
            ACCENT,
            None,
        ));
        let n = channels.len().max(1) as f32;
        let width = ((ui.available_width() - (n - 1.0) * 6.0) / n).clamp(88.0, 156.0);
        let pos = self.live_pos();
        let h = ui.available_height().max(145.0);
        let master = channels.pop().unwrap();
        let scroll_w = (ui.available_width() - width - 6.0).max(88.0);
        let overflowing = channels.len() as f32 * (width + 6.0) - 6.0 > scroll_w;
        let strip_h = if overflowing { h - 18.0 } else { h };
        let h = strip_h.max(120.0);
        let mut draw =
            |ui: &mut egui::Ui,
             channel: &(String, f32, f32, bool, bool, Color32, Option<usize>)| {
                let (name, volume, pan, mute, solo, color, track) = channel;
                let master = name == "master";
                let (r, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
                let pt = ui.painter().clone();
                pt.rect(
                    r,
                    6.0,
                    Tokens::DARK.strip_bg,
                    Stroke::new(1.0_f32, if master { ACCENT } else { LINE }),
                );
                let nr =
                    Rect::from_min_size(r.min + Vec2::new(6.0, 4.0), Vec2::new(width - 12.0, 18.0));
                pt.with_clip_rect(nr).text(
                    nr.center(),
                    Align2::CENTER_CENTER,
                    name.to_uppercase(),
                    FontId::monospace(11.0),
                    *color,
                );
                pt.line_segment(
                    [nr.left_bottom(), nr.right_bottom()],
                    Stroke::new(1.0_f32, *color),
                );
                if ui
                    .interact(nr, ui.id().with(("dock_select", name)), Sense::click())
                    .clicked()
                {
                    if let Some(i) = track {
                        self.selected = *i;
                    }
                }
                let pid = ui.id().with(("dock_pan", name));
                let mut pv = ui.ctx().data(|d| d.get_temp::<f32>(pid)).unwrap_or(*pan);
                if !master {
                    let kr = Rect::from_center_size(
                        Pos2::new(r.center().x, r.top() + 37.0),
                        Vec2::splat(22.0),
                    );
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(kr));
                    let (resp, changed) =
                        pan_knob(&mut child, 22.0, &mut pv, "Pan; double-click to centre");
                    if changed {
                        ui.ctx().data_mut(|d| d.insert_temp(pid, pv));
                    }
                    if resp.drag_stopped() || resp.double_clicked() {
                        ui.ctx().data_mut(|d| d.remove::<f32>(pid));
                        self.call("set_mixer", json!({"track":name,"pan":pv}));
                    }
                } else if let Some(rendered) = self
                    .rendered
                    .as_ref()
                    .filter(|r| r.rev == self.engine.lock().unwrap().revision)
                {
                    pt.text(
                        Pos2::new(r.center().x, r.top() + 37.0),
                        Align2::CENTER_CENTER,
                        if p.tracks.is_empty() || rendered.report.master.peak_dbfs < -69.0 {
                            "SILENCE".into()
                        } else {
                            format!("{:.1} LUFS", rendered.report.loudness.integrated_lufs)
                        },
                        FontId::monospace(10.0),
                        TEXT,
                    );
                }
                let fr = Rect::from_min_max(
                    Pos2::new(r.left() + 9.0, r.top() + 53.0),
                    Pos2::new(r.center().x + 8.0, r.bottom() - 30.0),
                );
                let id = ui.id().with(("dock_fader", name));
                let mut v = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(*volume);
                let (resp, done) = console_fader(ui, fr, id, &mut v);
                if resp.dragged() {
                    ui.ctx().data_mut(|d| d.insert_temp(id, v));
                }
                if done {
                    ui.ctx().data_mut(|d| d.remove::<f32>(id));
                    self.call("set_mixer", json!({"track":name,"volume_db":v}));
                }
                let ((left, right), peak) = self.strip_meter_levels(name, pos);
                let mr = Rect::from_min_max(
                    Pos2::new(fr.right() + 4.0, fr.top() + 3.0),
                    Pos2::new(r.right() - 8.0, fr.bottom() - 3.0),
                );
                let mw = ((mr.width() - 2.0) / 2.0).max(2.0);
                for (k, db) in [left, right].iter().enumerate() {
                    zoned_meter(
                        &pt,
                        Rect::from_min_size(
                            mr.min + Vec2::new(k as f32 * (mw + 2.0), 0.0),
                            Vec2::new(mw, mr.height()),
                        ),
                        *db,
                        peak,
                        peak > -0.1,
                    );
                }
                let mut row = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_size(
                            Pos2::new(r.left() + 6.0, r.bottom() - 25.0),
                            Vec2::new(width - 12.0, 20.0),
                        ))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if track.is_some()
                    && text_toggle(&mut row, Vec2::new(18.0, 17.0), "S", *solo, SOLO, "Solo")
                        .clicked()
                {
                    self.call("set_mixer", json!({"track":name,"solo":!solo}));
                }
                if !master
                    && text_toggle(&mut row, Vec2::new(18.0, 17.0), "M", *mute, WARN, "Mute")
                        .clicked()
                {
                    self.call("set_mixer", json!({"track":name,"mute":!mute}));
                }
                row.label(RichText::new(fmt_db(v)).monospace().size(10.0).color(TEXT));
            };
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.allocate_ui(
                Vec2::new(scroll_w, h + if overflowing { 18.0 } else { 0.0 }),
                |ui| {
                    ui.style_mut().spacing.scroll.floating = false;
                    egui::ScrollArea::horizontal()
                        .id_salt("console_dock_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                for channel in &channels {
                                    draw(ui, channel);
                                }
                            });
                        });
                },
            );
            draw(ui, &master);
        });
    }

    fn live_pos(&self) -> Option<f32> {
        self.player
            .is_playing()
            .then(|| self.player.position_secs())
    }

    // ---------------- mixer ----------------
    pub(super) fn mixer(&mut self, ui: &mut egui::Ui, p: &Project) {
        let solo_count = p.tracks.iter().filter(|t| t.solo).count();
        egui::CollapsingHeader::new(format!("Mixer controls | {solo_count} track solos | solo, faders, inserts, checkpoints"))
            .id_salt("mixer_tools")
            .default_open(false)
            .show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Track solos: {solo_count}"));
            if ui.add_enabled(solo_count>0,egui::Button::new("Clear track solos")).on_hover_text("Clear all track solo flags in one Undo step. Track/bus mutes, faders, pan, routing and effects stay unchanged; muted sources stay silent.").clicked() {
                self.call("clear_track_solos",json!({}));
            }
            if let Some(track) = p.tracks.get(self.selected) {
                let already = track.solo && solo_count == 1;
                if ui.add_enabled(!already,egui::Button::new("Solo selected only")).on_hover_text(format!("Solo {} and clear other track solos in one Undo step. Mutes stay unchanged, including this track's mute. This does not solo buses.",track.name)).clicked() {
                    self.call("solo_track_exclusive",json!({"track":track.id}));
                }
                ui.label(format!("Selected: {}{}",track.name,if track.mute {" (muted)"} else {""}));
            }
            ui.add(egui::Label::new("Mutes and mix settings stay unchanged.").wrap());
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("All track faders:");
            for (label,delta) in [("-1 dB",-1.0),("+1 dB",1.0)] {
                if ui.add_enabled(!p.tracks.is_empty(),egui::Button::new(label)).on_hover_text("Shift every track fader together, including muted tracks. Preserves relative fader balance. Rejects all if any active volume lane or fader limit prevents it. Bus/master and everything else stay unchanged. One Undo restores.").clicked() {
                    self.call("offset_track_faders",json!({"delta_db":delta}));
                }
            }
            ui.add(egui::Label::new("Same dB offset; bus/master unchanged. Not an output loudness guarantee.").wrap());
        });
        if let Some(track) = p.tracks.get(self.selected) {
            let bypassed = track.effects.iter().filter(|fx| fx.bypassed()).count();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} inserts: {} active, {} bypassed",track.name,track.effects.len()-bypassed,bypassed));
                if ui.add_enabled(bypassed<track.effects.len(),egui::Button::new("Bypass all inserts")).on_hover_text("Bypass every insert on this selected track. Settings and other channels stay unchanged. Can change loudness. One Undo restores the exact previous bypass state.").clicked() {
                    self.call("set_track_insert_bypass",json!({"track":track.id,"bypass":true}));
                }
                if ui.add_enabled(bypassed>0,egui::Button::new("Enable all inserts")).on_hover_text("Enables EVERY insert, including previously bypassed ones. This is NOT a restore of a mixed state. Undo restores the exact prior state. Can change loudness.").clicked() {
                    self.call("set_track_insert_bypass",json!({"track":track.id,"bypass":false}));
                }
                ui.add(egui::Label::new("Undo restores mixed states; level may change.").wrap());
            });
        }
        if let Some(job) = self.mix_comparison_job.take() {
            match job.receiver.try_recv() {
                Ok(result) => {
                    let current = super::comparison_is_current(job.revision, job.generation, &self.engine.lock().unwrap());
                    if !current {
                        self.toast = Some(("Project changed. Prepare the comparison again.".into(),0.0,false));
                    } else {
                        match result {
                            Ok((value,audio)) => self.mix_comparison = Some(super::MixComparison {
                                revision:job.revision,generation:job.generation,checkpoint:job.checkpoint,value,audio,
                            }),
                            Err(error) => self.toast=Some((format!("Comparison failed: {error}"),0.0,false)),
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.mix_comparison_job=Some(job),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.toast=Some(("Comparison stopped. Prepare again.".into(),0.0,false)),
            }
        }
        if self.mix_comparison.as_ref().is_some_and(|result| !super::comparison_is_current(result.revision,result.generation,&self.engine.lock().unwrap())) {
            self.mix_comparison=None;
        }
        let checkpoint_id = egui::Id::new("mixer_checkpoint_selection");
        let mut checkpoint = ui
            .ctx()
            .data_mut(|d| d.get_temp::<String>(checkpoint_id))
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Capture checkpoint").on_hover_text("Keep a new whole-project snapshot without changing the song or overwriting earlier captures. Save writes checkpoints to a companion file; keep it with the project.").clicked() {
                let result = super::capture_mix_checkpoint(&mut self.engine.lock().unwrap());
                match result {
                    Ok(name) => { checkpoint=name; self.toast=Some(("Checkpoint captured. Save writes its snapshots companion.".into(),0.0,true)); }
                    Err(err) => { self.toast=Some((format!("Checkpoint failed: {err:#}"),0.0,false)); }
                }
            }
            let names: Vec<String> = self.engine.lock().unwrap().snapshots.iter().map(|(name,_)| name.clone()).collect();
            if !names.contains(&checkpoint) { checkpoint.clear(); }
            egui::ComboBox::from_id_salt("mixer_checkpoints").selected_text(if checkpoint.is_empty() {"Choose checkpoint"} else {&checkpoint}).show_ui(ui,|ui| {
                for name in &names {ui.selectable_value(&mut checkpoint,name.clone(),name);}
            });
            if ui.add_enabled(names.contains(&checkpoint),egui::Button::new("Restore whole project")).on_hover_text("Replace the ENTIRE current project with this checkpoint, including notes and arrangement, not only the mix. One Undo restores the current project. The checkpoint itself stays available.").clicked() {
                self.call("restore_snapshot",json!({"name":checkpoint}));
            }
        });
        ui.horizontal_wrapped(|ui| {
            let busy=self.mix_comparison_job.is_some();
            if ui.add_enabled(!busy && !checkpoint.is_empty(),egui::Button::new("Prepare matched A/B")).on_hover_text("Compare the first 8 seconds: selected whole-project checkpoint, one-second gap, current project. Attenuation-only loudness matching and shared peak safety. Rendering runs in the background. Does not restore the checkpoint or change your song.").clicked() {
                let (own,revision,generation)={
                    let engine=self.engine.lock().unwrap();
                    (super::checkpoint_comparison_engine(&engine,&checkpoint),engine.revision,engine.project_load_generation)
                };
                let (tx,rx)=std::sync::mpsc::channel();
                self.mix_comparison=None;
                self.mix_comparison_job=Some(super::MixComparisonJob{revision,generation,checkpoint:checkpoint.clone(),receiver:rx});
                let name=checkpoint.clone();let ctx=ui.ctx().clone();
                std::thread::spawn(move || {
                    let result=own.and_then(|own|super::render_checkpoint_comparison(own,&name)).map_err(|error|format!("{error:#}"));
                    let _=tx.send(result);ctx.request_repaint();
                });
            }
            if busy {ui.spinner();ui.label("Preparing comparison. You can keep editing.");}
            if let Some(result)=&self.mix_comparison {
                if result.checkpoint==checkpoint {
                    if ui.button("Play A then B").on_hover_text("Stop song playback and play checkpoint A, one-second gap, then captured current version B. The song is unchanged.").clicked() {
                        let mut engine=self.engine.lock().unwrap();
                        super::play_checkpoint_comparison(result,&checkpoint,&mut engine);
                    }
                }
            }
        });
        if self.mix_comparison.is_some() || self.engine.lock().unwrap().transport.auditioning {
            if ui.button("Stop preview").on_hover_text("Stop the current audio preview, including a comparison. Does not resume song playback.").clicked() {
                super::stop_comparison_preview(&mut self.engine.lock().unwrap());
            }
        }
        if let Some(result)=&self.mix_comparison {
            if result.checkpoint==checkpoint {
                ui.add(egui::Label::new(format!("A: {} at preparation | B: current at preparation | first 8 seconds per side (or shorter song). {}",checkpoint,if result.value["loudness_matched"].as_bool()==Some(true){"Level matched without boosting. Choose by listening, not loudness."}else{"A side is silent or below the loudness gate; not level matched."})).wrap());
            }
        }
        ui.add(egui::Label::new("Save writes a checkpoint companion file; keep it with the project. Restore replaces the whole project. Undo restores your edits.").wrap());
        ui.ctx()
            .data_mut(|d| d.insert_temp(checkpoint_id, checkpoint));
            });
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
        // one-click mix helpers, each the engine call it names; answers show
        // under the row
        let tk = Tokens::DARK;
        // Never leave an earlier answer looking current after an edit.
        if self
            .mix_readout_revision
            .is_some_and(|r| r != self.engine.lock().unwrap().revision)
        {
            self.mix_readout =
                Some("Project changed. Run the report again for current values.".into());
            self.mix_readout_revision = None;
        }
        // Analysis runs on a snapshot, never on the UI-held engine lock.
        if let Some((revision, rx)) = self.mix_job.take() {
            match rx.try_recv() {
                Ok(result) => {
                    if revision != self.engine.lock().unwrap().revision {
                        self.mix_readout = Some("Project changed while analyzing. Run the report again for current values.".into());
                    } else {
                        self.mix_readout_revision = Some(revision);
                        self.mix_readout =
                            Some(result.unwrap_or_else(|e| format!("Analysis failed: {e}")));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.mix_job = Some((revision, rx));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mix_readout = Some("Analysis stopped. Run the report again.".into());
                }
            }
        }
        if self.mix_job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Analyzing project snapshot. You can keep editing.");
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.checkbox(
            &mut self.mixer_routing,
            "Show inserts, sends and outputs on each channel",
        );
        let mut op: Option<(&str, &str, serde_json::Value)> = None;
        egui::CollapsingHeader::new("Mix analysis")
            .default_open(self.mix_readout.is_some())
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for (label, tool, args, hover) in [
                        (
                            "MIX REPORT",
                            "mix_report",
                            json!({}),
                            "levels, peaks and balance for every track",
                        ),
                        (
                            "MASTER CHECK",
                            "check_master",
                            json!({}),
                            "loudness and true peak against the target",
                        ),
                        (
                            "MASKING",
                            "masking_matrix",
                            json!({}),
                            "which tracks fight for the same frequencies",
                        ),
                        ("LEVELS", "level_hints", json!({}), "suggested level moves"),
                        (
                            "BALANCE",
                            "balance_mix",
                            json!({}),
                            "even out the track levels",
                        ),
                        (
                            "MASTER",
                            "master_assistant",
                            json!({}),
                            "master the song to streaming loudness",
                        ),
                        (
                            "CARVE",
                            "carve_mix",
                            json!({}),
                            "give each track its own frequency space",
                        ),
                        (
                            "STEREO",
                            "stereo_image",
                            json!({}),
                            "width and phase per frequency band",
                        ),
                        (
                            "EQ CURVE",
                            "eq_curve",
                            json!({"track": "master"}),
                            "the frequency shape of the master chain",
                        ),
                    ] {
                        if ui
                            .add_enabled(self.mix_job.is_none(), egui::Button::new(label).small())
                            .on_hover_text(hover)
                            .clicked()
                        {
                            op = Some((label, tool, args));
                        }
                    }
                });
            });
        if let Some((label, tool, args)) = op {
            if matches!(
                tool,
                "mix_report"
                    | "check_master"
                    | "masking_matrix"
                    | "level_hints"
                    | "stereo_image"
                    | "eq_curve"
            ) {
                let (project, bank, workdir, revision) = {
                    let e = self.engine.lock().unwrap();
                    (
                        e.project.clone(),
                        e.bank.clone(),
                        e.workdir.clone(),
                        e.revision,
                    )
                };
                let (tx, rx) = std::sync::mpsc::channel();
                let ctx = ui.ctx().clone();
                self.mix_job = Some((revision, rx));
                self.mix_readout = None;
                self.mix_readout_revision = None;
                std::thread::spawn(move || {
                    let mut own = crate::engine::Engine::new(workdir);
                    own.project = project;
                    own.bank = bank;
                    let result = own
                        .call_from(tool, &args, "you")
                        .map(|v| format_readout(label, tool, &v))
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(result);
                    ctx.request_repaint();
                });
            } else {
                let r = self.engine.lock().unwrap().call_from(tool, &args, "you");
                match r {
                    Ok(v) => {
                        self.mix_readout = Some(format_readout(label, tool, &v));
                        self.mix_readout_revision = Some(self.engine.lock().unwrap().revision);
                    }
                    Err(e) => {
                        self.toast = Some((format!("{e:#}"), 0.0, false));
                    }
                }
            }
        }
        if self.mix_readout.is_some()
            && ui
                .small_button("CLEAR ANSWER")
                .on_hover_text("hide the helper answer")
                .clicked()
        {
            self.mix_readout = None;
        }
        if let Some(readout) = self.mix_readout.clone() {
            egui::ScrollArea::vertical()
                .id_salt("mix_readout")
                .max_height((ui.available_height() * 0.22).clamp(48.0, 110.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(readout)
                            .size(12.0)
                            .monospace()
                            .color(tk.text_dim),
                    );
                });
        }
        let h = (ui.available_height() - 18.0).clamp(260.0, 500.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let scroll_w = ui.available_width() - MASTER_W - 8.0;
            let strip_w = ((scroll_w - strips.len().saturating_sub(1) as f32)
                / strips.len().max(1) as f32)
                .clamp(STRIP_W, STRIP_W * 1.5);
            ui.allocate_ui(Vec2::new(scroll_w, h + 18.0), |ui| {
                ui.style_mut().spacing.scroll.floating = false;
                egui::ScrollArea::horizontal()
                    .max_height(h + 18.0)
                    .id_salt("mixer")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            for s in &strips {
                                self.strip(ui, p, h, s, &bus_names, pos, strip_w);
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
            .filter(|r| r.rev == self.engine.lock().unwrap().revision)
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
        strip_width: f32,
    ) {
        let t = Tokens::DARK;
        let (max_inserts, max_sends) = slot_rows(h);
        let insert_rows = max_inserts;
        let send_rows = max_sends.min(1);
        let is_bus = matches!(s.kind, StripKind::Bus);
        let selected = matches!(s.kind, StripKind::Track(i) if i == self.selected);
        let name = s.name.as_str();
        let ((lv_l, lv_r), peak) = if name == "master" {
            ((-120.0, -120.0), -120.0)
        } else {
            self.strip_meter_levels(name, pos)
        };
        let (r, _) = ui.allocate_exact_size(Vec2::new(strip_width, h), Sense::hover());
        let pt = ui.painter().clone();
        pt.rect_filled(
            r,
            0.0,
            if selected {
                super::theme::Tokens::DARK.button_hi
            } else {
                t.strip_bg
            },
        );
        pt.rect_filled(
            Rect::from_min_size(r.min, Vec2::new(strip_width, 4.0)),
            0.0,
            s.color,
        );
        let x0 = r.min.x + 4.0;
        let iw = strip_width - 8.0;
        let mut y = r.min.y + 8.0;
        // kind line
        pt.text(
            Pos2::new(r.center().x, y + 5.0),
            Align2::CENTER_CENTER,
            &s.sub,
            FontId::monospace(10.5),
            if is_bus { ACCENT2 } else { t.text_dim },
        );
        y += 14.0;
        if self.mixer_routing {
            // INSERTS
            let sec = Rect::from_min_size(
                Pos2::new(x0, y),
                Vec2::new(iw, 16.0 + insert_rows as f32 * 17.0),
            );
            let area = section(&pt, sec, "INSERTS");
            let insert_start = slot_page(
                ui,
                sec,
                egui::Id::new(("insert_page", name)),
                s.inserts.len(),
                insert_rows,
            );
            for row in 0..insert_rows {
                let k = insert_start + row;
                let sr = slot_rect(area, row);
                let ins = s.inserts.get(k);
                let resp = ui.interact(sr, ui.id().with(("ins", name, k)), Sense::click());
                let fill = match ins {
                    Some((_, true)) => Color32::from_rgb(70, 56, 30),
                    Some(_) => super::theme::Tokens::DARK.button,
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
                    Stroke::new(1.0_f32, super::theme::Tokens::DARK.border),
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
            y = sec.max.y + 4.0;
            // SENDS (drag horizontally to change)
            let sec = Rect::from_min_size(
                Pos2::new(x0, y),
                Vec2::new(iw, 16.0 + send_rows as f32 * 17.0),
            );
            let area = section(&pt, sec, "SENDS");
            let send_start = slot_page(
                ui,
                sec,
                egui::Id::new(("send_page", name)),
                s.sends.len(),
                send_rows,
            );
            for row in 0..send_rows {
                let sr = slot_rect(area, row);
                let Some((bus, db)) = s.sends.get(send_start + row) else {
                    pt.rect(
                        sr,
                        2.0,
                        t.slot_bg,
                        Stroke::new(1.0_f32, super::theme::Tokens::DARK.border),
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
                    Stroke::new(1.0_f32, super::theme::Tokens::DARK.border),
                );
                let frac = fader_db_to_pos(v);
                pt.rect_filled(
                    Rect::from_min_size(sr.min, Vec2::new(sr.width() * frac, sr.height()))
                        .shrink(1.0),
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
        }
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
        counter_box(
            &pt,
            pr,
            &if pv.abs() < 0.005 {
                "C".into()
            } else {
                format!("{:+.0}", pv * 100.0)
            },
            9.5,
        );
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
            // keep M in the same column as on track strips (item spacing adds the gap)
            row.add_space(bw);
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
            if self
                .rendered
                .as_ref()
                .is_some_and(|r| r.rev == self.engine.lock().unwrap().revision)
            {
                format!("pk {}", fmt_db(peak))
            } else {
                "pk --".into()
            },
            FontId::monospace(10.5),
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
        let current = self
            .rendered
            .as_ref()
            .filter(|r| r.rev == self.engine.lock().unwrap().revision);
        let loud = current
            .filter(|r| !p.tracks.is_empty() && r.report.master.peak_dbfs >= -69.0)
            .map(|r| r.report.loudness.clone());
        let (r, _) = ui.allocate_exact_size(Vec2::new(MASTER_W, h), Sense::hover());
        let pt = ui.painter().clone();
        pt.rect_filled(r, 0.0, super::theme::Tokens::DARK.strip_bg);
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
            FontId::monospace(10.5),
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
                    FontId::monospace(10.0),
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
        if loud.is_none() {
            pt.text(
                Pos2::new(r.center().x, y + 9.0),
                Align2::CENTER_CENTER,
                if current.is_none() {
                    "UPDATING"
                } else {
                    "SILENCE"
                },
                FontId::monospace(10.0),
                t.text_dim,
            );
            y += 24.0;
        }
        // the master lift (MixProfile: presence bell + air shelf): toggle it
        // and drag its two gains; each change is one master_lift call (one
        // undo step), so a song keeps it on re-mix
        {
            let cur = crate::mix_rules::current_master_lift(p);
            let on = cur.is_some();
            let prof = cur.unwrap_or_default();
            let br = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(iw, 16.0));
            let label = match (on, prof.extra_db.abs() >= 0.05) {
                (false, _) => "LIFT off".to_string(),
                (true, false) => format!("LIFT {:+.1} / {:+.1}", prof.presence_db, prof.air_db),
                (true, true) => format!(
                    "LIFT {:+.1} / {:+.1} / {:+.1}",
                    prof.presence_db, prof.air_db, prof.extra_db
                ),
            };
            let b = ui.put(
                br,
                egui::Button::new(egui::RichText::new(label).size(9.0)).selected(on),
            );
            if b.on_hover_text("Master lift: presence bell + air shelf (MixProfile). Click to turn it on/off; drag P (presence dB), A (air dB) and N (the extra bell nudge, dB at its centre, default 3.5 kHz) below.").clicked() {
                self.call("master_lift", json!({"preset": if on { "off" } else { "default" }}));
            }
            y += 19.0;
            if on {
                let hw = iw / 3.0 - 3.0;
                for (k, key, val, lim, pre) in [
                    (0usize, "presence_db", prof.presence_db, 6.0f32, "P "),
                    (1, "air_db", prof.air_db, 6.0, "A "),
                    (2, "extra_db", prof.extra_db, 3.0, "N "),
                ] {
                    let id = egui::Id::new(("master-lift", k));
                    let mut val_now: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(val);
                    let cell = Rect::from_min_size(
                        Pos2::new(x0 + k as f32 * (hw + 4.0), y),
                        Vec2::new(hw, 16.0),
                    );
                    let resp = ui.put(
                        cell,
                        egui::DragValue::new(&mut val_now)
                            .range(-lim..=lim)
                            .speed(0.05)
                            .fixed_decimals(1)
                            .prefix(pre),
                    );
                    if resp.changed() {
                        ui.ctx().data_mut(|d| d.insert_temp(id, val_now));
                    }
                    if resp.drag_stopped() || resp.lost_focus() {
                        ui.ctx().data_mut(|d| d.remove::<f32>(id));
                        if (val_now - val).abs() > 0.05 {
                            let mut a = serde_json::Map::new();
                            a.insert(key.to_string(), json!(val_now));
                            self.call("master_lift", serde_json::Value::Object(a));
                        }
                    }
                }
                y += 19.0;
            }
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
            if self
                .rendered
                .as_ref()
                .is_some_and(|r| r.rev == self.engine.lock().unwrap().revision)
            {
                format!("pk {}", fmt_db(peak))
            } else {
                "pk --".into()
            },
            FontId::monospace(10.5),
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
                ui.set_width((avail.x * 0.32).clamp(150.0,214.0));
                ui.set_height(avail.y - 22.0);
                egui::ScrollArea::vertical().id_salt("lanes").auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(RichText::new("LANES").size(10.5).color(DIM).strong().extra_letter_spacing(1.2));
                    if lanes.is_empty() {
                        ui.label(RichText::new("No automation yet. Add a lane below, then draw on it or pick a shape.").size(11.0).color(DIM));
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
                                if ui.add(b).on_hover_text(format!("draw a {label} curve on this lane")).clicked() {
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
        let every = ((bars as f32 * 36.0 / plot.width().max(1.0)).ceil() as usize).max(1);
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
                "Select a lane",
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
        let pb = automation_playhead_beat(p, ppos);
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

/// Automation lanes use quarter-note song beats, not elapsed base-BPM beats.
fn automation_playhead_beat(p: &Project, seconds: f32) -> f32 {
    if p.tempo_points.is_empty() {
        seconds / (4.0 * p.step_secs())
    } else {
        super::playlist::marker_beat(p, seconds)
    }
}

#[cfg(test)]
mod automation_cursor_tests {
    use super::*;
    #[test]
    fn cursor_value_follows_tempo_jumps_ramps_and_constant_compatibility() {
        let mut e = crate::engine::Engine::new(std::env::temp_dir().join("automation-cursor-test"));
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_automation",
            &json!({"track":"keys","param":"volume","points":[[0,-12],[16,0]]}),
        )
        .unwrap();
        for bpm in [60., 90., 120., 172.] {
            e.project.bpm = bpm;
            for i in 0..100 {
                let secs = i as f32 * 0.123;
                assert_eq!(
                    automation_playhead_beat(&e.project, secs),
                    secs / (4.0 * e.project.step_secs())
                );
            }
        }
        e.project.bpm = 120.;
        e.call("add_tempo_point", &json!({"beat":4,"bpm":60}))
            .unwrap();
        let b = automation_playhead_beat(&e.project, 6.);
        assert!((b - 8.).abs() < 1e-5);
        assert!((e.project.automation[0].value_at(b).unwrap() + 6.).abs() < 1e-5);
        e.call("add_tempo_point", &json!({"beat":12,"bpm":100,"ramp":true}))
            .unwrap();
        let curve = crate::tempo_curve::TempoCurve::new(&e.project);
        for beat in [0., 2., 4., 7., 12., 16.] {
            let seconds = curve.secs_at_beat(beat) as f32;
            assert!((automation_playhead_beat(&e.project, seconds) - beat as f32).abs() < 1e-4);
        }
    }
}
