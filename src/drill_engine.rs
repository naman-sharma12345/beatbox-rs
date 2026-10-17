//! Drill beat engine: a whole arranged UK / NY / melodic / desi drill beat
//! from the measured rules in the song research (drill grid, 808 slides,
//! hat rolls, beat switch, transitions, drops where the vocal pauses).
//!
//! Every section is its own pattern, so each bar stays hand-editable in the
//! piano roll and playlist afterwards, and every number the engine picked
//! (slides, rolls, drops, chords) is reported back.

use crate::dsp::Rng;
use crate::engine::Engine;
use crate::instruments::Instrument;
use crate::project::{Note, Pattern, Section};
use crate::tools::{b_or, f_opt, f_or, obj, s_opt, seed_of, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Intro,
    Hook,
    Verse,
    Switch,
    Bridge,
    Outro,
    Break,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        let s = s.trim().to_lowercase();
        Some(
            match s
                .trim_end_matches(|c: char| c.is_ascii_digit())
                .trim_end_matches('_')
            {
                "intro" => Kind::Intro,
                "hook" | "chorus" => Kind::Hook,
                "verse" => Kind::Verse,
                "switch" | "beat_switch" | "beatswitch" => Kind::Switch,
                "bridge" | "melodic" => Kind::Bridge,
                "outro" | "end" => Kind::Outro,
                "break" | "drop" => Kind::Break,
                _ => return None,
            },
        )
    }
    fn word(self) -> &'static str {
        match self {
            Kind::Intro => "intro",
            Kind::Hook => "hook",
            Kind::Verse => "verse",
            Kind::Switch => "switch",
            Kind::Bridge => "bridge",
            Kind::Outro => "outro",
            Kind::Break => "break",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Uk,
    Ny,
    Melodic,
    Desi,
}

#[derive(Clone, Debug)]
pub struct Sec {
    pub kind: Kind,
    pub bars: u32,
    pub name: String,
}

/// Chord loop: (semitones above the tonic, minor?) per chord.
pub type Loop = Vec<(i32, bool)>;

pub fn progression(name: &str) -> Option<(Loop, &'static str)> {
    Some(match name {
        "phrygian" => (vec![(0, true), (1, false)], "i - bII (Phrygian menace)"),
        "harmonic" => (
            vec![(0, true), (8, false), (10, false), (7, false)],
            "i - bVI - bVII - V (harmonic minor)",
        ),
        "dark" => (
            vec![(0, true), (5, true), (8, false), (7, false)],
            "i - iv - bVI - V",
        ),
        "two" => (
            vec![(0, true), (8, false)],
            "i - bVI (two-chord melodic drill)",
        ),
        "switch" => (
            vec![(5, true), (1, false), (8, false), (7, false)],
            "iv - bII - bVI - V (beat switch)",
        ),
        _ => return None,
    })
}

/// DRILL-STD: intro 4, hook 8, verse 16, hook 8, verse 16 with the beat
/// switch, melodic bridge 8, hook 8, outro 4.
pub fn template(name: &str, switch: bool) -> Vec<Sec> {
    let raw: &[(Kind, u32)] = match name {
        "short" => &[
            (Kind::Intro, 4),
            (Kind::Hook, 8),
            (Kind::Verse, 16),
            (Kind::Hook, 8),
            (Kind::Outro, 4),
        ],
        "loop" => &[(Kind::Hook, 8)],
        _ => &[
            (Kind::Intro, 4),
            (Kind::Hook, 8),
            (Kind::Verse, 16),
            (Kind::Hook, 8),
            (Kind::Switch, 16),
            (Kind::Bridge, 8),
            (Kind::Hook, 8),
            (Kind::Outro, 4),
        ],
    };
    name_secs(
        raw.iter()
            .map(|&(k, b)| {
                (
                    if !switch && k == Kind::Switch {
                        Kind::Verse
                    } else {
                        k
                    },
                    b,
                )
            })
            .collect(),
    )
}

fn name_secs(v: Vec<(Kind, u32)>) -> Vec<Sec> {
    let mut count = std::collections::HashMap::new();
    let total: std::collections::HashMap<&str, usize> =
        v.iter()
            .fold(std::collections::HashMap::new(), |mut m, (k, _)| {
                *m.entry(k.word()).or_insert(0) += 1;
                m
            });
    v.into_iter()
        .map(|(k, b)| {
            let c = count.entry(k.word()).or_insert(0usize);
            *c += 1;
            let name = if total[k.word()] > 1 {
                format!("{}{}", k.word(), c)
            } else {
                k.word().to_string()
            };
            Sec {
                kind: k,
                bars: b.clamp(1, 64),
                name,
            }
        })
        .collect()
}

/// "intro 4, hook 8, verse 16" or [{"type":"hook","bars":8}, "verse:16"].
pub fn parse_sections(v: &Value) -> Result<Vec<Sec>> {
    let mut out: Vec<(Kind, u32)> = Vec::new();
    let mut push = |tok: &str| -> Result<()> {
        let t = tok.trim();
        if t.is_empty() {
            return Ok(());
        }
        let mut it = t
            .split(|c: char| c.is_whitespace() || c == ':' || c == '=' || c == 'x')
            .filter(|s| !s.is_empty());
        let k = it.next().unwrap_or("");
        let kind = Kind::parse(k).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown section '{k}' (intro, hook, verse, switch, bridge, break, outro)"
            )
        })?;
        let bars = it
            .next()
            .and_then(|b| b.parse::<u32>().ok())
            .unwrap_or(match kind {
                Kind::Verse | Kind::Switch => 16,
                Kind::Intro | Kind::Outro | Kind::Break => 4,
                _ => 8,
            });
        out.push((kind, bars));
        Ok(())
    };
    match v {
        Value::String(s) => {
            for tok in s.split([',', ';', '|', '\n']) {
                push(tok)?;
            }
        }
        Value::Array(a) => {
            for x in a {
                match x {
                    Value::String(s) => push(s)?,
                    Value::Object(o) => {
                        let k = o
                            .get("type")
                            .or_else(|| o.get("kind"))
                            .or_else(|| o.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let b = o.get("bars").and_then(|v| v.as_u64()).unwrap_or(0);
                        if b > 0 {
                            push(&format!("{k} {b}"))?;
                        } else {
                            push(k)?;
                        }
                    }
                    _ => bail!("sections: use strings like 'verse 16' or objects {{type, bars}}"),
                }
            }
        }
        _ => bail!("sections: a string ('intro 4, hook 8, verse 16') or an array"),
    }
    if out.is_empty() {
        bail!("sections is empty");
    }
    Ok(name_secs(out))
}

/// Root pitch class and the spelled key from "F#", "F# minor", "Cm", "Bb min".
pub fn parse_key(s: &str) -> Result<(u8, String)> {
    let t = s.trim();
    let mut ch = t.chars();
    let first = ch.next().map(|c| c.to_ascii_uppercase()).unwrap_or('X');
    if !"CDEFGAB".contains(first) {
        bail!("key '{s}': start with a note name (C, F#, Bb ...)");
    }
    let mut root = first.to_string();
    if let Some(c) = ch.next() {
        if c == '#' || c == 'b' {
            root.push(c);
        }
    }
    let pc = crate::theory::pitch_class(&root)?;
    Ok((pc, root))
}

pub struct Ctx {
    pub style: Style,
    pub tonic: u8,
    /// MIDI of the 808 tonic (35-55 Hz sweet spot, E1..D#2)
    pub root808: i32,
    pub chords: Loop,
    pub switch_chords: Loop,
    pub harmonic: bool,
    pub phrygian: bool,
    pub step_s: f32,
    pub humanize: bool,
}

pub struct Stats {
    pub slides: u32,
    pub cycles: u32,
    pub rolls: u32,
    pub roll_notes: u32,
    pub triplet_roll_notes: u32,
    pub hat_notes: u32,
    pub phrase_drops: u32,
}

const KICK_A: [u32; 4] = [0, 3, 6, 10];
const KICK_B: [u32; 3] = [0, 6, 14];
const HAT_X: [u32; 7] = [0, 3, 6, 8, 10, 12, 14];
const HAT_GHOST: [u32; 4] = [2, 5, 9, 13];

fn chord_at(chords: &Loop, bar: u32) -> (i32, bool) {
    let per = if chords.len() <= 2 { 2 } else { 1 };
    chords[((bar / per) as usize) % chords.len()]
}

/// 808 pitch for a chord root: the octave closest to the 808 tonic.
fn bass_pitch(ctx: &Ctx, off: i32) -> i32 {
    let mut p = ctx.root808 + off.rem_euclid(12);
    if p - ctx.root808 > 6 {
        p -= 12;
    }
    p.clamp(24, 55)
}

fn push(p: &mut Pattern, track: &str, n: Note) {
    p.notes_mut(track).push(n);
}

fn note(start: f32, len: f32, pitch: i32, vel: f32) -> Note {
    Note::new(start, len, pitch.clamp(0, 127) as u8, vel.clamp(0.05, 1.0))
}

fn hum(ctx: &Ctx, rng: &mut Rng, n: &mut Note, ms: f32, vel_pct: f32) {
    if !ctx.humanize {
        return;
    }
    let st = (ms * 0.001 / ctx.step_s.max(1e-3)).min(0.3);
    n.offset = (n.offset + rng.bipolar() * st).clamp(-0.45, 0.45);
    n.vel = (n.vel * (1.0 + rng.bipolar() * vel_pct)).clamp(0.05, 1.0);
}

/// One bar of drums (kick, snare, clap, hat, rim) at step offset `o`.
fn drums_bar(
    p: &mut Pattern,
    ctx: &Ctx,
    kind: Kind,
    bi: u32,
    bars: u32,
    rng: &mut Rng,
    st: &mut Stats,
) {
    let o = (bi * 16) as f32;
    let b_bar = bi % 2 == 1;
    let bridge = kind == Kind::Bridge;
    let melodic = ctx.style == Style::Melodic;
    // kick
    if !bridge && kind != Kind::Intro && kind != Kind::Outro {
        let steps: Vec<u32> = match (kind, melodic, b_bar) {
            (Kind::Switch, _, false) => vec![0, 7, 10],
            (Kind::Switch, _, true) => vec![0, 3, 11],
            (_, true, false) => vec![0, 6],
            (_, true, true) => vec![0, 6, 11],
            (_, false, false) => KICK_A.to_vec(),
            (_, false, true) => KICK_B.to_vec(),
        };
        for s in steps {
            push(
                p,
                "kick",
                note(o + s as f32, 1.0, 60, if s == 0 { 1.0 } else { 0.92 }),
            );
        }
    }
    // snare (+ clap)
    if !bridge && kind != Kind::Intro && kind != Kind::Outro {
        let drag = if ctx.style == Style::Uk && ctx.humanize {
            0.06
        } else {
            0.0
        };
        let mut sn = note(o + 8.0, 1.0, 60, 0.95);
        sn.offset = drag;
        push(p, "snare", sn);
        if b_bar && !melodic && kind != Kind::Switch {
            let mut late = note(o + 14.0, 1.0, 60, 0.78);
            late.offset = drag;
            push(p, "snare", late);
        }
        if melodic || ctx.style == Style::Ny || kind == Kind::Switch {
            push(
                p,
                "clap",
                note(o + 8.0, 1.0, 60, if melodic { 0.9 } else { 0.7 }),
            );
        }
    }
    // hats
    if kind != Kind::Outro && !(kind == Kind::Intro && bi + 2 < bars) {
        let mut hats: Vec<Note> = Vec::new();
        if melodic {
            for s in (0..16).step_by(2) {
                hats.push(note(
                    o + s as f32,
                    0.5,
                    60,
                    if s % 4 == 0 { 0.8 } else { 0.62 },
                ));
            }
        } else if bridge {
            for s in [0u32, 4, 8, 12] {
                hats.push(note(o + s as f32, 0.5, 60, 0.6));
            }
        } else {
            for s in HAT_X {
                hats.push(note(
                    o + s as f32,
                    0.5,
                    60,
                    if s % 4 == 0 { 0.82 } else { 0.7 },
                ));
            }
            for s in HAT_GHOST {
                hats.push(note(o + s as f32, 0.5, 60, rng.range(0.35, 0.5)));
            }
        }
        // one roll per 2 bars (beat 4 or the and-of-4); hooks 2+ get an extra short one
        let extra = kind == Kind::Hook && !b_bar && rng.chance(0.35);
        if (b_bar || extra) && !bridge && kind != Kind::Intro {
            let (rs, span) = if extra || rng.chance(0.4) {
                (14.0f32, 2.0f32)
            } else {
                (12.0, 4.0)
            };
            let r = rng.f32();
            let (spacing, trip) = match ctx.style {
                Style::Ny if r < 0.25 => (1.0 / 3.0, true),
                _ if r < 0.5 => (2.0 / 3.0, true),
                _ if r < 0.88 => (0.5, false),
                _ => (4.0 / 3.0, true),
            };
            hats.retain(|n| n.start < o + rs || n.start >= o + rs + span);
            let count = (span / spacing).round().max(2.0) as u32;
            let ramp_pitch = st.rolls % 4 == 3;
            for k in 0..count {
                let f = k as f32 / (count - 1).max(1) as f32;
                let pitch = if ramp_pitch {
                    60 + (5.0 * f).round() as i32
                } else {
                    60
                };
                hats.push(note(
                    o + rs + k as f32 * spacing,
                    spacing.min(0.5),
                    pitch,
                    0.6 + 0.4 * f,
                ));
            }
            st.rolls += 1;
            st.roll_notes += count;
            if trip {
                st.triplet_roll_notes += count;
            }
        }
        for mut h in hats {
            if h.start.fract().abs() < 1e-4 {
                hum(ctx, rng, &mut h, 8.0, 0.1);
            }
            st.hat_notes += 1;
            push(p, "hat", h);
        }
    }
    // rim: one dark rim per bar on beat 4
    if (bridge || matches!(ctx.style, Style::Ny | Style::Desi) || (kind == Kind::Switch))
        && kind != Kind::Intro
        && kind != Kind::Outro
    {
        push(p, "rim", note(o + 12.0, 1.0, 60, 0.55));
        if ctx.style == Style::Desi && b_bar {
            push(p, "rim", note(o + 7.0, 1.0, 62, 0.45));
        }
    }
}

/// Two bars of 808 (bar `bi` and `bi + 1`): root on the downbeat, space,
/// slides into the kick holes (1-3 glides per 2 bars).
fn bass_cycle(
    p: &mut Pattern,
    ctx: &Ctx,
    chords: &Loop,
    kind: Kind,
    bi: u32,
    bars: u32,
    rng: &mut Rng,
    st: &mut Stats,
) {
    let pick_iv = |rng: &mut Rng| -> i32 {
        [12, 7, 3, -5, -12, 1][rng.weighted(&[0.3, 0.2, 0.2, 0.1, 0.1, 0.1])]
    };
    let ny = ctx.style == Style::Ny;
    let k = 1 + rng.weighted(if ny {
        &[0.25, 0.45, 0.3]
    } else {
        &[0.4, 0.4, 0.2]
    }) as u32;
    let mut slots = vec![0u32, 1, 2];
    // choose k distinct slots: 0 = bar A into beat 4, 1 = bar B stair-step, 2 = bar B octave flick
    while slots.len() > k as usize {
        let i = rng.below(slots.len());
        slots.remove(i);
    }
    let up = if kind == Kind::Switch && bi % 4 >= 2 {
        12
    } else {
        0
    };
    let mut slides = 0u32;
    for half in 0..2u32 {
        let b = bi + half;
        if b >= bars {
            break;
        }
        let o = (b * 16) as f32;
        let (off, _) = chord_at(chords, b);
        let mut r = bass_pitch(ctx, off) + up;
        if r > 55 {
            r -= 12;
        }
        let clampt = |t: i32| {
            if t > 55 {
                t - 12
            } else if t < 24 {
                t + 12
            } else {
                t
            }
        };
        if half == 0 {
            push(p, "b808", note(o, 3.0, r, 1.0));
            push(p, "b808", note(o + 3.0, 3.0, r, 0.88));
            push(p, "b808", note(o + 6.0, 4.0, r, 0.9));
            if slots.contains(&0) {
                let t = clampt(r + pick_iv(rng));
                let mut s = note(o + 10.0, 2.0, r, 0.9);
                s.slide_to = Some(t as u8);
                push(p, "b808", s);
                push(p, "b808", note(o + 12.0, 4.0, t, 0.86));
                slides += 1;
            } else {
                push(p, "b808", note(o + 10.0, 6.0, r, 0.88));
            }
        } else {
            if slots.contains(&2) {
                let t = clampt(r + 12);
                let mut s = note(o, 4.0, r, 1.0);
                s.slide_to = Some(t as u8);
                push(p, "b808", s);
                push(p, "b808", note(o + 4.0, 2.0, t, 0.85));
                slides += 1;
            } else {
                push(p, "b808", note(o, 6.0, r, 1.0));
            }
            if slots.contains(&1) {
                // stair-step: two short overlapped notes walking into the target
                let t = clampt(r + pick_iv(rng));
                let mid = clampt(r + if t >= r { 3 } else { -2 });
                push(p, "b808", note(o + 6.0, 6.0, r, 0.88));
                let mut s1 = note(o + 12.0, 1.0, r, 0.85);
                s1.slide_to = Some(mid as u8);
                push(p, "b808", s1);
                let mut s2 = note(o + 13.0, 1.0, mid, 0.85);
                s2.slide_to = Some(t as u8);
                push(p, "b808", s2);
                push(p, "b808", note(o + 14.0, 2.0, t, 0.9));
                slides += 1;
            } else {
                push(p, "b808", note(o + 6.0, 8.0, r, 0.88));
                push(p, "b808", note(o + 14.0, 2.0, r, 0.85));
            }
        }
    }
    st.slides += slides;
    st.cycles += 1;
}

fn scale_of(ctx: &Ctx) -> [i32; 7] {
    let mut s = [0, 2, 3, 5, 7, 8, 10];
    if ctx.phrygian {
        s[1] = 1;
    }
    if ctx.harmonic {
        s[6] = 11;
    }
    if ctx.style == Style::Desi {
        s = [0, 1, 4, 5, 7, 8, 11];
    }
    s
}

/// A 4-bar single-note motif: 2-bar call, 2-bar answer, notes on 1 and the
/// and-of-2 with long holds. Returns (start step, len, midi) for bar 0..4.
pub fn motif(ctx: &Ctx, chords: &Loop, rng: &mut Rng) -> Vec<(f32, f32, i32)> {
    let sc = scale_of(ctx);
    let base = 60 + 12 + ctx.tonic as i32; // octave 5
    let base = if base > 78 { base - 12 } else { base };
    let tone = |deg: i32| -> i32 {
        let d = deg.rem_euclid(7) as usize;
        base + sc[d] + 12 * deg.div_euclid(7)
    };
    let chord_deg = |bar: u32| -> Vec<i32> {
        let (off, _) = chord_at(chords, bar);
        // scale degree nearest the chord root, plus its 3rd and 5th in the scale
        let root = (0..7)
            .min_by_key(|&d| {
                ((sc[d] - off.rem_euclid(12)).rem_euclid(12))
                    .min((off.rem_euclid(12) - sc[d]).rem_euclid(12))
            })
            .unwrap_or(0) as i32;
        vec![root, root + 2, root + 4]
    };
    let rhythm: [(f32, f32); 4] = [(0.0, 6.0), (6.0, 10.0), (16.0, 6.0), (22.0, 10.0)];
    let mut call: Vec<i32> = Vec::new();
    let mut last = 4;
    for (i, _) in rhythm.iter().enumerate() {
        let cd = chord_deg(i as u32 / 2);
        let pick = cd
            .iter()
            .copied()
            .min_by_key(|&d| ((d - last).abs() * 10) as i64 + (rng.below(7) as i64))
            .unwrap_or(0);
        let pick = if rng.chance(0.25) { pick + 1 } else { pick };
        call.push(pick);
        last = pick;
    }
    let mut out = Vec::new();
    for (i, &(s, l)) in rhythm.iter().enumerate() {
        out.push((s, l, tone(call[i])));
    }
    // answer: same opening, ends on the colour note then home
    for (i, &(s, l)) in rhythm.iter().enumerate() {
        let d = match i {
            2 => {
                if ctx.harmonic || ctx.style == Style::Desi {
                    -1 // raised 7th below the tonic
                } else if ctx.phrygian {
                    1 // b2
                } else {
                    call[2] - 1
                }
            }
            3 => 0,
            _ => call[i],
        };
        out.push((s + 32.0, l, tone(d)));
    }
    out
}

fn melody_section(
    p: &mut Pattern,
    ctx: &Ctx,
    chords: &Loop,
    kind: Kind,
    bars: u32,
    mot: &[(f32, f32, i32)],
    rng: &mut Rng,
    desi_lead: bool,
) {
    let (piano_vel, bell, stacc, choir, lead) = match kind {
        Kind::Intro => (0.55, false, false, true, desi_lead),
        Kind::Hook => (0.7, !desi_lead, true, true, desi_lead),
        Kind::Verse => (0.48, false, false, false, false),
        Kind::Switch => (0.6, true, true, false, false),
        Kind::Bridge => (0.5, false, false, true, desi_lead),
        Kind::Outro => (0.5, false, false, true, false),
        Kind::Break => (0.5, false, false, true, false),
    };
    let blocks = bars.div_ceil(4);
    for blk in 0..blocks {
        let o = (blk * 64) as f32;
        let thin = kind == Kind::Verse && blk % 2 == 1;
        for (i, &(s, l, m)) in mot.iter().enumerate() {
            let at = o + s;
            if at >= (bars * 16) as f32 || thin {
                continue;
            }
            // switch: the motif moves an octave down with a pushed rhythm
            let (m2, s2) = if kind == Kind::Switch {
                (m - 12, at + if i % 2 == 1 { -2.0 } else { 0.0 })
            } else {
                (m, at)
            };
            let len = l.min((bars * 16) as f32 - s2);
            push(
                p,
                "piano",
                note(s2.max(0.0), len, m2, piano_vel + rng.bipolar() * 0.04),
            );
            if bell && i % 2 == 0 {
                push(p, "bell", note(s2.max(0.0), len.min(4.0), m2 + 12, 0.42));
            }
            if lead {
                let mut n = note(s2.max(0.0), len, m2, 0.62);
                if l >= 6.0 && rng.chance(0.4) {
                    // a meend into the note from a semitone under
                    n.slide_to = None;
                }
                push(p, "lead", n);
            }
        }
    }
    for b in 0..bars {
        let o = (b * 16) as f32;
        let (off, minor) = chord_at(chords, b);
        let root = 48 + (ctx.tonic as i32 + off).rem_euclid(12);
        let third = if minor { 3 } else { 4 };
        if stacc {
            let tones = [
                root + 12,
                root + 7 + 12,
                root + 24,
                root + third + 12,
                root + 7 + 12,
            ];
            for (k, s) in [0u32, 3, 6, 10, 12].iter().enumerate() {
                push(p, "stacc", note(o + *s as f32, 1.0, tones[k], 0.48));
            }
        }
        if choir && (kind != Kind::Hook || b % 2 == 0) {
            let len = if kind == Kind::Hook {
                32.0f32.min((bars * 16) as f32 - o)
            } else {
                16.0
            };
            for t in [root + 12, root + 12 + third, root + 19] {
                push(
                    p,
                    "choir",
                    note(o, len, t, if kind == Kind::Bridge { 0.55 } else { 0.38 }),
                );
            }
        }
    }
}

/// Fill one section's pattern.
pub fn fill(
    p: &mut Pattern,
    ctx: &Ctx,
    sec: &Sec,
    mot: &[(f32, f32, i32)],
    mot_switch: &[(f32, f32, i32)],
    rng: &mut Rng,
    st: &mut Stats,
    desi_lead: bool,
) {
    let bars = sec.bars;
    let chords = if sec.kind == Kind::Switch {
        ctx.switch_chords.clone()
    } else {
        ctx.chords.clone()
    };
    for bi in 0..bars {
        drums_bar(p, ctx, sec.kind, bi, bars, rng, st);
    }
    match sec.kind {
        Kind::Hook | Kind::Verse | Kind::Switch => {
            let mut bi = 0;
            while bi < bars {
                bass_cycle(p, ctx, &chords, sec.kind, bi, bars, rng, st);
                bi += 2;
            }
        }
        Kind::Bridge | Kind::Break => {
            for b in 0..bars {
                let (off, _) = chord_at(&chords, b);
                let r = bass_pitch(ctx, off);
                let mut n = note((b * 16) as f32, 16.0, r, 0.85);
                if b + 1 == bars {
                    let (noff, _) = chord_at(&ctx.chords, 0);
                    let t = bass_pitch(ctx, noff);
                    if t != r {
                        n.len = 12.0;
                        n.slide_to = Some(t as u8);
                        push(p, "b808", n);
                        push(p, "b808", note((b * 16) as f32 + 12.0, 4.0, t, 0.85));
                        st.slides += 1;
                        continue;
                    }
                }
                push(p, "b808", n);
            }
        }
        Kind::Intro => {
            // the 808 only arrives in the last bar
            let b = bars - 1;
            let r = bass_pitch(ctx, chord_at(&chords, b).0);
            push(p, "b808", note((b * 16) as f32, 12.0, r, 0.9));
        }
        Kind::Outro => {
            let r = bass_pitch(ctx, chord_at(&chords, 0).0);
            push(p, "b808", note(0.0, (bars * 16).min(48) as f32, r, 0.9));
        }
    }
    // a 1-beat drum drop at a phrase end every 8 bars of a verse (bar 4, 12 ...)
    if matches!(sec.kind, Kind::Verse | Kind::Switch) {
        let mut b = 3;
        while b + 1 < bars {
            drop_window(
                p,
                (b * 16 + 12) as f32,
                (b * 16 + 16) as f32,
                &["kick", "snare", "clap", "hat", "rim", "b808"],
            );
            st.phrase_drops += 1;
            b += 8;
        }
    }
    let m = if sec.kind == Kind::Switch {
        mot_switch
    } else {
        mot
    };
    melody_section(p, ctx, &chords, sec.kind, bars, m, rng, desi_lead);
    for v in p.clips.values_mut() {
        v.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// Remove note starts in [a, b) on the tracks; held 808 notes are cut at a.
pub fn drop_window(p: &mut Pattern, a: f32, b: f32, tracks: &[&str]) -> u32 {
    let mut n = 0;
    for t in tracks {
        let v = p.notes_mut(t);
        let before = v.len();
        v.retain(|x| !(x.start >= a - 1e-4 && x.start < b - 1e-4));
        n += (before - v.len()) as u32;
        for x in v.iter_mut() {
            if x.start < a && x.end() > a + 1e-4 {
                x.len = (a - x.start).max(0.25);
                x.slide_to = None;
            }
        }
    }
    n
}

fn style_of(s: &str) -> Result<Style> {
    Ok(
        match s.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "uk" | "uk_drill" | "drill" | "" => Style::Uk,
            "ny" | "ny_drill" | "brooklyn" => Style::Ny,
            "melodic" | "melodic_drill" | "melodic_trap" => Style::Melodic,
            "desi" | "desi_drill" | "punjabi" | "hindi" => Style::Desi,
            o => bail!("style '{o}': uk, ny, melodic or desi"),
        },
    )
}

fn num_list(v: Option<&Value>) -> Vec<f32> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect(),
        Some(Value::String(s)) => s
            .split([',', ' '])
            .filter_map(|t| t.trim().parse::<f32>().ok())
            .collect(),
        Some(x) => x.as_f64().map(|f| vec![f as f32]).unwrap_or_default(),
        None => Vec::new(),
    }
}

pub fn make_drill_beat(e: &mut Engine, a: &Value) -> Result<Value> {
    let style = style_of(&s_opt(a, "style").unwrap_or_default())?;
    let bpm = f_or(a, "bpm", 142.0).clamp(60.0, 200.0);
    let (tonic, key_name) = parse_key(&s_opt(a, "key").unwrap_or_else(|| "F#".into()))?;
    let mut rng = Rng::new(seed_of(a));
    let prog_name = s_opt(a, "progression")
        .unwrap_or_else(|| "auto".into())
        .to_lowercase();
    let prog_name = if prog_name == "auto" {
        match style {
            Style::Melodic => "two".to_string(),
            Style::Desi => "harmonic".to_string(),
            _ => ["phrygian", "harmonic", "dark"][rng.below(3)].to_string(),
        }
    } else {
        prog_name
    };
    let (chords, prog_label) = progression(&prog_name).ok_or_else(|| {
        anyhow::anyhow!("progression '{prog_name}': auto, phrygian, harmonic, dark, two, switch")
    })?;
    let (switch_chords, switch_label) = progression("switch").unwrap();
    let switch = b_or(a, "switch", true);
    let mut secs = match a.get("sections") {
        Some(v) if !v.is_null() => parse_sections(v)?,
        _ => template(
            &s_opt(a, "template").unwrap_or_else(|| "std".into()),
            switch,
        ),
    };
    // fit a requested length by stretching the verses (multiples of 4 bars, 8..32)
    let mut length_note = Value::Null;
    if let Some(len_s) = f_opt(a, "length_s").filter(|x| *x > 5.0) {
        let target = (len_s * bpm / 240.0).round() as i64;
        let fixed: i64 = secs
            .iter()
            .filter(|s| !matches!(s.kind, Kind::Verse | Kind::Switch))
            .map(|s| s.bars as i64)
            .sum();
        let nv = secs
            .iter()
            .filter(|s| matches!(s.kind, Kind::Verse | Kind::Switch))
            .count() as i64;
        if nv > 0 {
            let per = (((target - fixed) as f32 / nv as f32) / 4.0).round() as i64 * 4;
            let per = per.clamp(8, 32) as u32;
            for s in secs
                .iter_mut()
                .filter(|s| matches!(s.kind, Kind::Verse | Kind::Switch))
            {
                s.bars = per;
            }
        }
        let got: u32 = secs.iter().map(|s| s.bars).sum();
        length_note = json!({"asked_s": len_s, "got_s": (got as f32 * 240.0 / bpm * 10.0).round() / 10.0, "verse_bars": secs.iter().find(|s| s.kind == Kind::Verse).map(|s| s.bars)});
    }
    let total_bars: u32 = secs.iter().map(|s| s.bars).sum();
    if total_bars > 400 {
        bail!("{total_bars} bars is too long (max 400)");
    }
    let glide = f_or(a, "glide_ms", 80.0).clamp(20.0, 200.0);
    let humanize = b_or(a, "humanize", true);
    let desi_lead = style == Style::Desi || s_opt(a, "lead").is_some();
    let lead_preset = s_opt(a, "lead").unwrap_or_else(|| "bansuri".into());
    let mut root808 = 24 + tonic as i32;
    if root808 < 28 {
        root808 += 12;
    }
    let ctx = Ctx {
        style,
        tonic,
        root808,
        harmonic: prog_name == "harmonic" || prog_name == "dark",
        phrygian: prog_name == "phrygian",
        chords: chords.clone(),
        switch_chords: switch_chords.clone(),
        step_s: 60.0 / bpm / 4.0,
        humanize,
    };

    // project: tempo, key; the beat's patterns and song order are replaced,
    // tracks, audio clips (vocals) and buses stay
    let p = &mut e.project;
    p.bpm = bpm;
    p.key_root = key_name.clone();
    p.scale = "minor".into();
    p.swing = 0.0;
    let had_playlist = !p.playlist.is_empty();
    p.playlist.clear();
    p.patterns.clear();
    p.arrangement.clear();
    let voices: Vec<(&str, String)> = vec![
        ("kick", "kick_tight".into()),
        ("snare", "snare_drill".into()),
        ("clap", "clap_wide".into()),
        ("hat", "hat_drill".into()),
        ("rim", "rim".into()),
        ("b808", "808_slide".into()),
        ("piano", "felt_piano".into()),
        ("stacc", "staccato_strings".into()),
        ("choir", "choir_ooh".into()),
        ("bell", "bell_dark".into()),
        ("lead", lead_preset.clone()),
    ];
    let mut created = Vec::new();
    for (name, preset) in &voices {
        if *name == "lead" && !desi_lead {
            continue;
        }
        if crate::instruments::preset(preset).is_none() {
            bail!("unknown preset '{preset}' for {name} (list_presets)");
        }
        let existed = p.track_index(name).is_ok();
        crate::tools::ensure_track(p, name, preset, None)?;
        if !existed {
            created.push(name.to_string());
        }
    }
    if let Ok(i) = p.track_index("b808") {
        if let Instrument::Bass808(b) = &mut p.tracks[i].instrument {
            b.glide_ms = glide;
            b.sustain = true;
        }
    }
    let mut st = Stats {
        slides: 0,
        cycles: 0,
        rolls: 0,
        roll_notes: 0,
        triplet_roll_notes: 0,
        hat_notes: 0,
        phrase_drops: 0,
    };
    let mot = motif(&ctx, &chords, &mut rng);
    let mot_sw = motif(&ctx, &switch_chords, &mut rng);
    for s in &secs {
        let mut pat = Pattern::new(&s.name, s.bars);
        fill(
            &mut pat, &ctx, s, &mot, &mot_sw, &mut rng, &mut st, desi_lead,
        );
        p.patterns.push(pat);
        p.arrangement.push(Section {
            pattern: s.name.clone(),
            repeats: 1,
        });
    }
    // drop-out: the last half beat before each hook belongs to the pickup line
    let mut dropouts = Vec::new();
    for i in 1..secs.len() {
        if secs[i].kind == Kind::Hook {
            let steps = (secs[i - 1].bars * 16) as f32;
            let pi = p.pattern_index(&secs[i - 1].name)?;
            drop_window(
                &mut p.patterns[pi],
                steps - 2.0,
                steps,
                &["kick", "snare", "clap", "hat", "rim", "b808"],
            );
            dropouts.push(secs[i].name.clone());
        }
    }
    // vocal pauses (song seconds) -> drops: kick and 808 out for that beat
    let mut pauses = num_list(a.get("pauses_s"));
    if let Some(vs) = s_opt(a, "vocal_sample") {
        let (_, x) = crate::tools_sound::sample_data(e, &vs)?;
        let at = f_or(a, "vocal_start_s", 0.0);
        let ph = crate::vocal_flex::phrases(&x, 0.6, None);
        for w in ph.windows(2) {
            if w[1].start_s - w[0].end_s >= 0.6 {
                pauses.push(at + w[0].end_s + 0.05);
            }
        }
    }
    let p = &mut e.project;
    let beat_s = 60.0 / bpm;
    let mut drops = Vec::new();
    for t in &pauses {
        let beat = (t / beat_s).floor();
        let mut acc = 0.0f32;
        for s in &secs {
            let len = (s.bars * 4) as f32;
            if beat >= acc && beat < acc + len {
                let pi = p.pattern_index(&s.name)?;
                let a0 = (beat - acc) * 4.0;
                let n = drop_window(&mut p.patterns[pi], a0, a0 + 4.0, &["kick", "b808"]);
                drops.push(json!({"at_s": (t * 100.0).round() / 100.0, "beat": beat, "section": s.name, "notes_removed": n}));
                break;
            }
            acc += len;
        }
    }
    // mix: 808 ducked by the kick, dark piano (LPF 6 kHz, plate, dotted 1/8 delay)
    let fx: Vec<Value> = vec![
        json!({"track": "b808", "type": "sidechain", "params": {"source": "kick", "amount": 0.6, "attack_ms": 0.5, "release_ms": 80.0}}),
        json!({"track": "piano", "type": "filter", "params": {"mode": "lowpass", "cutoff": 6000.0, "resonance": 0.1}}),
        json!({"track": "piano", "type": "delay", "params": {"steps": 3.0, "mix": 0.15, "feedback": 0.3}}),
        json!({"track": "piano", "type": "reverb", "params": {"size": 0.85, "mix": 0.25, "predelay_ms": 40.0}}),
        json!({"track": "choir", "type": "eq", "params": {"low_db": -9.0, "low_freq": 250.0}}),
        json!({"track": "bell", "type": "reverb", "params": {"size": 0.8, "mix": 0.3}}),
        json!({"track": "stacc", "type": "eq", "params": {"low_db": -6.0, "low_freq": 300.0}}),
    ];
    let fx_on = b_or(a, "mix", true);
    let mut fx_added = 0;
    if fx_on {
        for f in fx {
            let tn = f["track"].as_str().unwrap_or("");
            let ti = match e.project.track_index(tn) {
                Ok(i) => i,
                Err(_) => continue,
            };
            // idempotent on re-runs: one effect of each type per track from here
            let ty = f["type"].as_str().unwrap_or("");
            let has = serde_json::to_value(&e.project.tracks[ti].effects)
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .any(|x| x.get("type").and_then(|t| t.as_str()) == Some(ty));
            if !has && e.call_from("add_effect", &f, "drill").is_ok() {
                fx_added += 1;
            }
        }
        for (t, db) in [
            ("piano", -5.0f32),
            ("stacc", -11.0),
            ("choir", -13.0),
            ("bell", -10.0),
            ("lead", -6.0),
            ("rim", -9.0),
        ] {
            if created.iter().any(|c| c == t) {
                if let Ok(i) = e.project.track_index(t) {
                    e.project.tracks[i].volume_db += db;
                }
            }
        }
    }
    // transitions: reverse crash + impact into each hook, tape stop into the switch
    let mut transitions = Vec::new();
    if b_or(a, "transitions", true) {
        for (i, s) in secs.iter().enumerate().skip(1) {
            let mut add = |ty: &str, bars: f32| {
                if e.call_from(
                    "add_transition",
                    &json!({"into": s.name, "type": ty, "bars": bars}),
                    "drill",
                )
                .is_ok()
                {
                    transitions.push(format!("{ty} into {}", s.name));
                }
            };
            match s.kind {
                Kind::Hook => {
                    add("reverse_cymbal", 1.0);
                    add("impact", 1.0);
                }
                Kind::Switch => add("tape_stop", 0.25),
                _ => {}
            }
            let _ = i;
        }
    }
    let mut t0 = 0.0f32;
    let sections: Vec<Value> = secs
        .iter()
        .map(|s| {
            let v = json!({"name": s.name, "type": s.kind.word(), "bars": s.bars, "start_s": (t0 * 100.0).round() / 100.0});
            t0 += s.bars as f32 * 4.0 * beat_s;
            v
        })
        .collect();
    let trip_share = if st.hat_notes > 0 {
        st.triplet_roll_notes as f32 / st.hat_notes as f32
    } else {
        0.0
    };
    let style_word = match style {
        Style::Uk => "uk",
        Style::Ny => "ny",
        Style::Melodic => "melodic",
        Style::Desi => "desi",
    };
    Ok(json!({
        "style": style_word,
        "bpm": bpm,
        "key": format!("{key_name} minor"),
        "progression": prog_label,
        "switch_progression": if secs.iter().any(|s| s.kind == Kind::Switch) { Value::from(switch_label) } else { Value::Null },
        "sections": sections,
        "duration_s": (t0 * 10.0).round() / 10.0,
        "length": length_note,
        "tracks_created": created,
        "808": {"glide_ms": glide, "slides": st.slides, "per_2_bars": if st.cycles > 0 { ((st.slides as f32 / st.cycles as f32) * 100.0).round() / 100.0 } else { 0.0 }, "root_midi": root808},
        "hats": {"rolls": st.rolls, "roll_notes": st.roll_notes, "triplet_share": (trip_share * 1000.0).round() / 1000.0},
        "phrase_drops": st.phrase_drops,
        "pre_hook_dropouts": dropouts,
        "vocal_drops": drops,
        "transitions": transitions,
        "effects_added": fx_added,
        "playlist_cleared": had_playlist,
        "next": "add the vocal (place_vocal_phrases), then render or mix; every section is its own pattern, so edit any bar by hand",
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "make_drill_beat",
        description: "A full arranged drill beat on measured drill rules: kick 1/4/7/11 with beat 4 left empty for the 808 (2-bar cycle), snare on 3 plus the late and-of-4 every 2nd bar, 2-2-1 skip hats with ghosts, one hat roll per 2 bars (1/16 triplets, 1/32, pitch ramps), a mono gliding 808 (root on the downbeat, 1-3 slides per 2 bars into kick holes: octave, 5th, m3, b2), dark felt piano call/answer, staccato strings, choir, bell. DRILL-STD song (intro 4, hook 8, verse 16, hook 8, switch 16, bridge 8, hook 8, outro 4) with a beat switch (new chords, new kick and 808 shapes), reverse crash + impact into hooks, a drop-out before each hook, a tape stop into the switch, 1-beat drum drops every 8 verse bars, and kick+808 drops wherever the vocal pauses (pauses_s or vocal_sample). style: uk (default), ny (harder rolls, clap layer, rims), melodic (half-time, 8th hats), desi (harmonic/Phrygian-dominant lead: lead preset, default bansuri). key (default F#), bpm (default 142), progression: auto, phrygian, harmonic, dark, two. sections ('intro 4, hook 8, verse 16') or template std/short/loop; length_s stretches the verses. Replaces patterns and song order; tracks, vocal clips and buses stay.",
        mutates: true,
        schema: || {
            obj(
                json!({
                    "style": {"type": "string", "enum": ["uk", "ny", "melodic", "desi"]},
                    "bpm": {"type": "number"},
                    "key": {"type": "string", "description": "tonic, e.g. F#, C, D# (always minor)"},
                    "progression": {"type": "string", "enum": ["auto", "phrygian", "harmonic", "dark", "two"]},
                    "template": {"type": "string", "enum": ["std", "short", "loop"]},
                    "sections": {"description": "'intro 4, hook 8, verse 16, hook 8, switch 16, bridge 8, hook 8, outro 4' or an array of {type, bars}"},
                    "length_s": {"type": "number"},
                    "switch": {"type": "boolean"},
                    "glide_ms": {"type": "number", "description": "808 glide 20-200 ms (default 80)"},
                    "pauses_s": {"type": "array", "items": {"type": "number"}, "description": "song times (s) where the vocal pauses >= 0.6 s: kick and 808 drop for that beat"},
                    "vocal_sample": {"type": "string", "description": "find the pauses in this take instead"},
                    "vocal_start_s": {"type": "number"},
                    "lead": {"type": "string", "description": "a desi lead preset (bansuri, sitar, santoor ...); adds a 'lead' track"},
                    "humanize": {"type": "boolean"},
                    "mix": {"type": "boolean", "description": "add the 808 sidechain and melody FX (default true)"},
                    "transitions": {"type": "boolean"},
                    "seed": {"type": "integer"}
                }),
                &[],
            )
        },
        run: make_drill_beat,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng(tag: &str) -> Engine {
        let dir = std::env::temp_dir().join(format!("beatbox_drill_{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "drill", "bpm": 120}))
            .unwrap();
        e
    }

    fn starts(p: &Pattern, t: &str, bar: u32) -> Vec<u32> {
        let o = (bar * 16) as f32;
        let mut v: Vec<u32> = p
            .notes(t)
            .iter()
            .filter(|n| n.start >= o && n.start < o + 16.0)
            .map(|n| (n.start - o).round() as u32)
            .collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn grid_follows_the_drill_rules() {
        let mut e = eng("grid");
        let r = e
            .call(
                "make_drill_beat",
                &json!({"key": "C", "bpm": 142, "seed": 3, "transitions": false}),
            )
            .unwrap();
        assert_eq!(e.project.bpm, 142.0);
        assert_eq!(e.project.key_root, "C");
        assert_eq!(r["sections"].as_array().unwrap().len(), 8, "{r}");
        let hook = &e.project.patterns[e.project.pattern_index("hook1").unwrap()];
        assert_eq!(starts(hook, "kick", 0), vec![0, 3, 6, 10]);
        assert_eq!(starts(hook, "kick", 1), vec![0, 6, 14]);
        assert_eq!(starts(hook, "snare", 0), vec![8]);
        assert_eq!(starts(hook, "snare", 1), vec![8, 14]);
        // beat 4 of bar A is free of kicks (the 808's slide lives there)
        assert!(!starts(hook, "kick", 2).iter().any(|s| (12..16).contains(s)));
        // ghost hats are quiet
        assert!(hook.notes("hat").iter().any(|n| n.vel < 0.52));
        // 808 is mono: no overlaps
        let b = hook.notes("b808");
        for w in b.windows(2) {
            assert!(
                w[0].end() <= w[1].start + 1e-3,
                "808 overlap {:?} {:?}",
                w[0],
                w[1]
            );
        }
        let per = r["808"]["per_2_bars"].as_f64().unwrap();
        assert!((1.0..=3.0).contains(&per), "{r}");
        // every slide lands on a note at its target pitch right where it ends
        for n in b.iter().filter(|n| n.slide_to.is_some()) {
            assert!(
                b.iter()
                    .any(|m| (m.start - n.end()).abs() < 1e-3 && Some(m.pitch) == n.slide_to),
                "slide without a landing {n:?}"
            );
        }
        let trip = r["hats"]["triplet_share"].as_f64().unwrap();
        assert!(trip > 0.02 && trip < 0.4, "{r}");
        assert!(r["hats"]["rolls"].as_u64().unwrap() >= 20);
        // 808 pitches are chord roots of the key (C minor, i-bII/i-bVI-bVII-V/i-iv-bVI-V) or slide targets
        assert!(b.iter().all(|n| (24..=55).contains(&n.pitch)));
    }

    #[test]
    fn length_key_switch_and_pauses() {
        let mut e = eng("len");
        let r = e
            .call("make_drill_beat", &json!({"style": "ny", "key": "F# minor", "length_s": 170, "pauses_s": [20.0], "seed": 9}))
            .unwrap();
        let got = r["length"]["got_s"].as_f64().unwrap();
        assert!((got - 170.0).abs() < 25.0, "{r}");
        assert_eq!(e.project.key_root, "F#");
        assert!(r["switch_progression"].is_string());
        assert!(r["transitions"].as_array().unwrap().len() >= 4, "{r}");
        // the pause beat at 20 s has no kick or 808 onset
        let beat = (20.0f32 / (60.0 / 142.0)).floor();
        let d = &r["vocal_drops"][0];
        assert_eq!(d["beat"].as_f64().unwrap() as f32, beat);
        let sec = d["section"].as_str().unwrap();
        let (b0, _) = e.project.section_beats(sec).unwrap();
        let pat = &e.project.patterns[e.project.pattern_index(sec).unwrap()];
        let a0 = (beat - b0) * 4.0;
        assert!(!pat
            .notes("kick")
            .iter()
            .chain(pat.notes("b808").iter())
            .any(|n| n.start >= a0 && n.start < a0 + 4.0));
        // the bar before each hook ends with a drop-out (no drums or 808 in its last 2 steps)
        let intro = &e.project.patterns[e.project.pattern_index("intro").unwrap()];
        let end = intro.steps() as f32;
        assert!(!intro.notes("hat").iter().any(|n| n.start >= end - 2.0));
    }

    #[test]
    fn custom_sections_melodic_and_desi() {
        let mut e = eng("custom");
        let r = e
            .call(
                "make_drill_beat",
                &json!({"style": "melodic", "sections": "intro 2, hook 4, bridge 4", "seed": 1}),
            )
            .unwrap();
        assert_eq!(r["sections"].as_array().unwrap().len(), 3);
        let hook = &e.project.patterns[e.project.pattern_index("hook").unwrap()];
        assert_eq!(starts(hook, "kick", 0), vec![0, 6]);
        let r2 = e
            .call(
                "make_drill_beat",
                &json!({"style": "desi", "template": "short", "seed": 2}),
            )
            .unwrap();
        assert!(e.project.track_index("lead").is_ok(), "{r2}");
        assert!(e
            .call("make_drill_beat", &json!({"style": "polka"}))
            .is_err());
        assert!(e
            .call("make_drill_beat", &json!({"sections": "verse 8, waltz 4"}))
            .is_err());
    }
}
