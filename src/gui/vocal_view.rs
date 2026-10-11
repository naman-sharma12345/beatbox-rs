//! The vocal view: a sung take and everything vocal_to_song heard, on the
//! song grid. Sections, chords per slot, lyrics per word, the sung melody as
//! a pitch lane and the (warped) take's waveform, all aligned to bars.

use super::theme::Tokens;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};

const HEAD_W: f32 = 110.0;

fn kind_color(kind: &str) -> Color32 {
    match kind {
        "hook" => Color32::from_rgb(232, 120, 60),
        "verse" => Color32::from_rgb(86, 140, 220),
        "intro" | "outro" => Color32::from_rgb(120, 120, 135),
        _ => Color32::from_rgb(150, 110, 200),
    }
}

impl Studio {
    pub(super) fn vocal_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        let Some(m) = p.vocal_map.clone() else {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No vocal yet").size(18.0).color(tk.text));
                ui.label(
                    egui::RichText::new("A mapped vocal take shows here: its sections, chords, words and sung notes on the song grid.")
                        .size(12.0)
                        .color(tk.text_dim),
                );
            });
            return;
        };
        // waveform peaks of the clip's sample, cached by sample name
        let clip = p.audio_clips.iter().find(|c| c.sample == m.sample).cloned();
        if self.vocal_wave.as_ref().map(|w| &w.0) != Some(&m.sample) {
            let e = self.engine.lock().unwrap();
            let peaks = e
                .bank
                .get(&m.sample)
                .map(|d| crate::analysis::waveform_peaks(d, 4000))
                .unwrap_or_default();
            let secs = e.bank.get(&m.sample).map(|d| d.len() as f32 / crate::dsp::SR).unwrap_or(0.0);
            drop(e);
            self.vocal_wave = Some((m.sample.clone(), peaks, secs));
        }
        let (_, peaks, wave_secs) = self.vocal_wave.clone().unwrap_or_default();
        let total_beats = p.song_beats().max(4.0);

        let avail = ui.available_size();
        // zoomed so every chord reads (14 px per beat), scrolled sideways
        let want_w = (HEAD_W + total_beats * 14.0 + 8.0).max(avail.x);
        egui::ScrollArea::horizontal()
            .id_salt("vocal_view")
            .auto_shrink([false, false])
            .show(ui, |ui| self.vocal_canvas(ui, p, &m, clip.clone(), &peaks, wave_secs, want_w, avail.y.max(420.0)));
    }

    #[allow(clippy::too_many_arguments)]
    fn vocal_canvas(
        &self,
        ui: &mut egui::Ui,
        p: &Project,
        m: &crate::project::VocalMap,
        clip: Option<crate::project::AudioClip>,
        peaks: &[(f32, f32)],
        wave_secs: f32,
        w: f32,
        hgt: f32,
    ) {
        let tk = Tokens::DARK;
        let total_beats = p.song_beats().max(4.0);
        let beat_s = 60.0 / p.bpm.max(1.0);
        let (full, _) = ui.allocate_exact_size(vec2(w, hgt), Sense::hover());
        let pt = ui.painter_at(full);
        pt.rect_filled(full, 0.0, tk.playlist_bg);
        let tl = Rect::from_min_max(pos2(full.min.x + HEAD_W, full.min.y), full.max);
        let px_per_beat = (tl.width() - 8.0) / total_beats;
        let x_of = |b: f32| tl.min.x + b * px_per_beat;
        // lane layout
        let h = full.height();
        let ruler = 18.0;
        let sec_h = 26.0;
        let chord_h = 30.0;
        let lyr_h = 46.0;
        let rest = (h - ruler - sec_h - chord_h - lyr_h - 10.0).max(160.0);
        let mel_h = rest * 0.58;
        let wav_h = rest - mel_h;
        let mut y = full.min.y;
        let mut lane = |name: &str, hh: f32, alt: bool| -> Rect {
            let r = Rect::from_min_max(pos2(tl.min.x, y), pos2(tl.max.x, y + hh));
            let head = Rect::from_min_max(pos2(full.min.x, y), pos2(tl.min.x, y + hh));
            pt.rect_filled(r, 0.0, if alt { tk.playlist_alt } else { tk.playlist_bg });
            pt.rect_filled(head, 0.0, tk.panel_bg2);
            pt.text(pos2(head.min.x + 8.0, head.center().y), Align2::LEFT_CENTER, name, FontId::proportional(11.0), tk.text_dim);
            pt.line_segment([pos2(full.min.x, y + hh), pos2(full.max.x, y + hh)], Stroke::new(1.0_f32, tk.border));
            y += hh;
            r
        };
        let ruler_r = lane("BAR", ruler, false);
        let sec_r = lane("SECTIONS", sec_h, true);
        let chord_r = lane("CHORDS", chord_h, false);
        let lyr_r = lane("WORDS", lyr_h, true);
        let mel_r = lane("MELODY", mel_h, false);
        let wav_r = lane(if m.warped { "VOCAL (warped)" } else { "VOCAL" }, wav_h, true);
        // grid + bar numbers
        let bars = (total_beats / 4.0).ceil() as i32;
        let every = if px_per_beat * 4.0 > 34.0 { 1 } else if px_per_beat * 4.0 > 14.0 { 4 } else { 8 };
        for b in 0..=bars {
            let x = x_of(b as f32 * 4.0);
            let major = b % every == 0;
            pt.line_segment([pos2(x, ruler_r.max.y), pos2(x, full.max.y)], Stroke::new(1.0_f32, if major { tk.bar_line } else { tk.grid_line }));
            if major && b < bars {
                pt.text(pos2(x + 3.0, ruler_r.center().y), Align2::LEFT_CENTER, (b + 1).to_string(), FontId::proportional(9.5), tk.ruler_text);
            }
        }
        // sections
        for (name, kind, b0, n) in &m.sections {
            let r = Rect::from_min_max(pos2(x_of(*b0 as f32 * 4.0) + 1.0, sec_r.min.y + 3.0), pos2(x_of((b0 + n) as f32 * 4.0) - 1.0, sec_r.max.y - 3.0));
            let c = kind_color(kind);
            pt.rect_filled(r, 3.0, c.gamma_multiply(0.75));
            pt.text(pos2(r.min.x + 5.0, r.center().y), Align2::LEFT_CENTER, name.to_uppercase(), FontId::proportional(10.5), Color32::WHITE);
        }
        // chords
        for (b, len, label) in &m.chords {
            let r = Rect::from_min_max(pos2(x_of(*b) + 1.0, chord_r.min.y + 4.0), pos2(x_of(b + len) - 1.0, chord_r.max.y - 4.0));
            pt.rect_filled(r, 2.0, tk.slot_bg);
            pt.rect_stroke(r, 2.0, Stroke::new(1.0_f32, tk.border_light));
            if r.width() > 14.0 {
                pt.text(r.center(), Align2::CENTER_CENTER, label, FontId::monospace(11.0), tk.accent);
            }
        }
        // lyrics: words staggered on two rows so neighbours do not collide
        let mut last_x = [f32::NEG_INFINITY; 2];
        for (b, _, w) in &m.words {
            let x = x_of(*b);
            let width = w.len() as f32 * 5.6 + 4.0;
            let row = if x > last_x[0] { 0 } else if x > last_x[1] { 1 } else { continue };
            last_x[row] = x + width;
            let yy = lyr_r.min.y + 13.0 + row as f32 * 19.0;
            pt.circle_filled(pos2(x, yy - 8.0), 1.6, tk.accent);
            pt.text(pos2(x, yy), Align2::LEFT_TOP, w, FontId::proportional(10.0), tk.text);
        }
        // melody lane
        if !m.notes.is_empty() {
            let lo = m.notes.iter().map(|n| n.2).min().unwrap_or(48) as f32 - 2.0;
            let hi = m.notes.iter().map(|n| n.2).max().unwrap_or(72) as f32 + 2.0;
            let y_of = |pitch: f32| mel_r.max.y - 6.0 - (pitch - lo) / (hi - lo).max(1.0) * (mel_r.height() - 12.0);
            let row_h = ((mel_r.height() - 12.0) / (hi - lo).max(1.0)).clamp(2.0, 10.0);
            for k in (lo as i32)..=(hi as i32) {
                if [1, 3, 6, 8, 10].contains(&(k.rem_euclid(12))) {
                    let yy = y_of(k as f32);
                    pt.rect_filled(Rect::from_min_max(pos2(mel_r.min.x, yy - row_h / 2.0), pos2(mel_r.max.x, yy + row_h / 2.0)), 0.0, Color32::from_black_alpha(40));
                }
                if k.rem_euclid(12) == 0 {
                    pt.text(pos2(full.min.x + HEAD_W - 6.0, y_of(k as f32)), Align2::RIGHT_CENTER, crate::theory::note_name(k as u8), FontId::proportional(9.0), tk.text_dim);
                }
            }
            for (b, len, pitch) in &m.notes {
                let yy = y_of(*pitch as f32);
                let r = Rect::from_min_max(pos2(x_of(*b), yy - row_h / 2.0), pos2(x_of(b + len).max(x_of(*b) + 2.0), yy + row_h / 2.0));
                pt.rect_filled(r, 1.5, Color32::from_rgb(236, 168, 72));
            }
        }
        // waveform of the take, placed by its clip
        if let Some(c) = clip {
            let mid = wav_r.center().y;
            let amp = wav_r.height() * 0.46;
            let n = peaks.len().max(1);
            for (i, (mn, mx)) in peaks.iter().enumerate() {
                let t = i as f32 / n as f32 * wave_secs;
                let b = c.start_beat + t / beat_s;
                if b < 0.0 || b > total_beats {
                    continue;
                }
                let x = x_of(b);
                pt.line_segment([pos2(x, mid - mx.abs().min(1.0) * amp), pos2(x, mid + mn.abs().min(1.0) * amp)], Stroke::new(1.0_f32, Color32::from_rgb(110, 200, 170)));
            }
        }
    }
}
