//! Offline renderer: arrangement -> per-track audio -> FX -> mix -> master.

use crate::dsp::*;
use crate::fx::FxContext;
use crate::instruments::render_note;
use crate::project::Project;
use crate::samples::SampleBank;
use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// One scheduled note.
#[derive(Clone, Debug)]
pub struct Event {
    pub start: usize,
    pub gate: f32,
    pub pitch: f32,
    pub vel: f32,
}

pub struct Stem {
    pub name: String,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

pub struct Mix {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub stems: Vec<Stem>,
    pub seconds: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Play the whole song this many times.
    pub loops: u32,
    /// Seconds of tail after the last bar (reverb/delay ring-out).
    pub tail: f32,
    pub keep_stems: bool,
    /// Render only this section range of the song (in steps), if set.
    pub step_range: Option<(u32, u32)>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            loops: 1,
            tail: 1.5,
            keep_stems: false,
            step_range: None,
        }
    }
}

/// Build the event list for every track (index aligned with project.tracks).
pub fn schedule(p: &Project, opts: &RenderOptions) -> (Vec<Vec<Event>>, usize) {
    let step = p.step_secs();
    let swing_steps = p.swing.clamp(0.0, 1.0) * 0.5;
    let mut events: Vec<Vec<Event>> = vec![Vec::new(); p.tracks.len()];
    let mut offset: u32 = 0;
    let (r0, r1) = opts.step_range.unwrap_or((0, u32::MAX));
    for _ in 0..opts.loops.max(1) {
        for sec in p.song_sections() {
            let Ok(pi) = p.pattern_index(&sec.pattern) else {
                continue;
            };
            let pat = &p.patterns[pi];
            for _ in 0..sec.repeats.max(1) {
                for (ti, track) in p.tracks.iter().enumerate() {
                    for n in pat.notes(&track.name) {
                        let mut s = n.start;
                        if s.fract() == 0.0 && (s as i64) % 2 == 1 {
                            s += swing_steps;
                        }
                        let abs = offset as f32 + s;
                        if abs < r0 as f32 || abs >= r1 as f32 {
                            continue;
                        }
                        let rel = abs - r0 as f32;
                        events[ti].push(Event {
                            start: (rel * step * SR) as usize,
                            gate: (n.len * step).max(0.01),
                            pitch: n.pitch as f32,
                            vel: n.vel,
                        });
                    }
                }
                offset += pat.steps();
            }
        }
    }
    let end_step = offset.min(r1).saturating_sub(r0);
    let len = (end_step as f32 * step * SR) as usize;
    for e in events.iter_mut() {
        e.sort_by_key(|x| x.start);
    }
    (events, len)
}

pub fn render(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> Result<Mix> {
    let (events, body_len) = schedule(p, opts);
    let total = body_len + (opts.tail.max(0.0) * SR) as usize;
    let any_solo = p.tracks.iter().any(|t| t.solo);

    let triggers: HashMap<String, Vec<usize>> = p
        .tracks
        .iter()
        .zip(events.iter())
        .map(|(t, ev)| (t.name.to_lowercase(), ev.iter().map(|e| e.start).collect()))
        .collect();
    let ctx = FxContext {
        step_secs: p.step_secs(),
        triggers: &triggers,
    };

    let mut ml = vec![0.0f32; total];
    let mut mr = vec![0.0f32; total];
    let mut stems = Vec::new();

    for (ti, track) in p.tracks.iter().enumerate() {
        let audible = !track.mute && (!any_solo || track.solo);
        if !audible && !opts.keep_stems {
            continue;
        }
        let mut mono = vec![0.0f32; total];
        let mut cache: HashMap<(i32, u8, u32), Vec<f32>> = HashMap::new();
        for (k, e) in events[ti].iter().enumerate() {
            if e.start >= total {
                continue;
            }
            let key = (
                (e.pitch * 10.0) as i32,
                (e.vel * 127.0) as u8,
                (e.gate * 1000.0) as u32,
            );
            let seed = (ti as u64) << 32 | (k as u64 % 7);
            let buf = cache.entry(key).or_insert_with(|| {
                render_note(&track.instrument, e.pitch, e.vel, e.gate, bank, seed)
            });
            let end = (e.start + buf.len()).min(total);
            for (o, s) in mono[e.start..end].iter_mut().zip(buf.iter()) {
                *o += *s;
            }
        }
        // pan (equal power)
        let angle = (track.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let (gl, gr) = (
            angle.cos() * std::f32::consts::SQRT_2,
            angle.sin() * std::f32::consts::SQRT_2,
        );
        let mut l: Vec<f32> = mono.iter().map(|x| x * gl).collect();
        let mut r: Vec<f32> = mono.iter().map(|x| x * gr).collect();
        drop(mono);
        for fx in &track.effects {
            fx.process(&mut l, &mut r, &ctx);
        }
        let vol = db_to_gain(track.volume_db);
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            *a *= vol;
            *b *= vol;
        }
        if audible {
            for i in 0..total {
                ml[i] += l[i];
                mr[i] += r[i];
            }
        }
        if opts.keep_stems {
            stems.push(Stem {
                name: track.name.clone(),
                left: l,
                right: r,
            });
        }
    }

    let mv = db_to_gain(p.master_volume_db);
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        *a *= mv;
        *b *= mv;
    }
    // DC block before the master chain so the limiter has the last word
    let (mut dl, mut dr) = (DcBlock::default(), DcBlock::default());
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        *a = dl.process(*a);
        *b = dr.process(*b);
    }
    for fx in &p.master_effects {
        fx.process(&mut ml, &mut mr, &ctx);
    }
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        if !a.is_finite() {
            *a = 0.0;
        }
        if !b.is_finite() {
            *b = 0.0;
        }
    }
    Ok(Mix {
        seconds: total as f32 / SR,
        left: ml,
        right: mr,
        stems,
    })
}

/// Write 16-bit stereo PCM WAV.
pub fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let n = l.len().min(r.len());
    let data_len = (n * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&(SR as u32).to_le_bytes());
    out.extend_from_slice(&((SR as u32) * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for s in [l[i], r[i]] {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}
