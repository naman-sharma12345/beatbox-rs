//! Music theory: note names, scales, chords, roman-numeral progressions,
//! voice leading, plus generators for drums, basslines, chords and melodies.

use crate::dsp::Rng;
use crate::project::{Note, STEPS_PER_BAR};
use anyhow::{anyhow, bail, Result};

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub const SCALES: &[(&str, &[u8])] = &[
    ("major", &[0, 2, 4, 5, 7, 9, 11]),
    ("minor", &[0, 2, 3, 5, 7, 8, 10]),
    ("dorian", &[0, 2, 3, 5, 7, 9, 10]),
    ("phrygian", &[0, 1, 3, 5, 7, 8, 10]),
    ("lydian", &[0, 2, 4, 6, 7, 9, 11]),
    ("mixolydian", &[0, 2, 4, 5, 7, 9, 10]),
    ("locrian", &[0, 1, 3, 5, 6, 8, 10]),
    ("harmonic_minor", &[0, 2, 3, 5, 7, 8, 11]),
    ("melodic_minor", &[0, 2, 3, 5, 7, 9, 11]),
    ("phrygian_dominant", &[0, 1, 4, 5, 7, 8, 10]),
    ("pentatonic_major", &[0, 2, 4, 7, 9]),
    ("pentatonic_minor", &[0, 3, 5, 7, 10]),
    ("blues", &[0, 3, 5, 6, 7, 10]),
    ("hirajoshi", &[0, 2, 3, 7, 8]),
    ("bhairav", &[0, 1, 4, 5, 7, 8, 11]),
    // raag-flavoured thaats (the parent scales of the raags)
    ("kafi", &[0, 2, 3, 5, 7, 9, 10]),
    ("asavari", &[0, 2, 3, 5, 7, 8, 10]),
    ("bhairavi", &[0, 1, 3, 5, 7, 8, 10]),
    ("todi", &[0, 1, 3, 6, 7, 8, 11]),
];

pub const CHORD_QUALITIES: &[(&str, &[u8])] = &[
    ("maj", &[0, 4, 7]),
    ("min", &[0, 3, 7]),
    ("dim", &[0, 3, 6]),
    ("aug", &[0, 4, 8]),
    ("sus2", &[0, 2, 7]),
    ("sus4", &[0, 5, 7]),
    ("maj7", &[0, 4, 7, 11]),
    ("min7", &[0, 3, 7, 10]),
    ("dom7", &[0, 4, 7, 10]),
    ("dim7", &[0, 3, 6, 9]),
    ("m7b5", &[0, 3, 6, 10]),
    ("add9", &[0, 4, 7, 14]),
    ("madd9", &[0, 3, 7, 14]),
    ("maj9", &[0, 4, 7, 11, 14]),
    ("min9", &[0, 3, 7, 10, 14]),
    ("dom9", &[0, 4, 7, 10, 14]),
    ("min11", &[0, 3, 7, 10, 14, 17]),
    ("power", &[0, 7, 12]),
];

pub fn scale_intervals(name: &str) -> Result<&'static [u8]> {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    let n = match n.as_str() {
        "maj" | "ionian" => "major",
        "min" | "aeolian" | "natural_minor" => "minor",
        other => other,
    };
    SCALES
        .iter()
        .find(|(k, _)| *k == n)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            anyhow!(
                "unknown scale '{name}'. Options: {}",
                SCALES.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")
            )
        })
}

fn quality(name: &str) -> Result<&'static [u8]> {
    CHORD_QUALITIES
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .ok_or_else(|| anyhow!("unknown chord quality '{name}'"))
}

/// Parse a pitch class like "C", "F#", "Bb".
pub fn pitch_class(s: &str) -> Result<u8> {
    let mut chars = s.trim().chars();
    let letter = chars
        .next()
        .ok_or_else(|| anyhow!("empty note"))?
        .to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => bail!("bad note letter in '{s}'"),
    };
    let rest: String = chars.collect();
    let acc = if rest.starts_with('#') || rest.starts_with('♯') {
        1
    } else if rest.starts_with('b') || rest.starts_with('♭') {
        -1
    } else {
        0
    };
    Ok((base + acc).rem_euclid(12) as u8)
}

/// Parse "C4", "F#2", "Bb-1" or a plain MIDI number "60". C4 = 60.
pub fn parse_note(s: &str) -> Result<u8> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).map_err(|e| anyhow!(e));
    }
    let pc = pitch_class(t)? as i32;
    let mut idx = 1;
    let b = t.as_bytes();
    if b.len() > 1 && (b[1] == b'#' || b[1] == b'b') {
        idx = 2;
    }
    let oct: i32 = t[idx..]
        .parse()
        .map_err(|_| anyhow!("bad octave in note '{s}' (use e.g. C4, F#2)"))?;
    let m = (oct + 1) * 12 + pc;
    if !(0..=127).contains(&m) {
        bail!("note '{s}' out of MIDI range");
    }
    Ok(m as u8)
}

pub fn note_name(m: u8) -> String {
    format!("{}{}", NOTE_NAMES[(m % 12) as usize], m as i32 / 12 - 1)
}

/// A resolved chord: root pitch class + intervals, and a display label.
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    pub root_pc: u8,
    pub intervals: Vec<u8>,
    pub label: String,
}

fn roman_value(s: &str) -> Option<(usize, usize)> {
    // returns (degree 1..7, chars consumed) for the longest roman prefix
    const R: [&str; 7] = ["VII", "III", "VI", "IV", "II", "V", "I"];
    const D: [usize; 7] = [7, 3, 6, 4, 2, 5, 1];
    let up = s.to_uppercase();
    for (r, d) in R.iter().zip(D.iter()) {
        if up.starts_with(r) {
            return Some((*d, r.len()));
        }
    }
    None
}

/// Parse one chord token: roman numeral relative to the key ("i", "bVII", "V7",
/// "iv9", "ii°") or an absolute chord name ("Cm7", "F#", "Bbmaj7", "Gsus4").
pub fn parse_chord(token: &str, key_pc: u8, scale: &str) -> Result<Chord> {
    let t = token.trim();
    if t.is_empty() {
        bail!("empty chord");
    }
    let first = t.chars().next().unwrap();
    // absolute chord name
    if "CDEFGAB".contains(first) {
        let pc = pitch_class(t)?;
        let mut rest = &t[1..];
        if rest.starts_with('#') || rest.starts_with('b') {
            rest = &rest[1..];
        }
        let q = match rest {
            "" | "maj" | "M" => "maj",
            "m" | "min" | "-" => "min",
            "dim" | "°" | "o" => "dim",
            "aug" | "+" => "aug",
            "7" => "dom7",
            "maj7" | "M7" => "maj7",
            "m7" | "min7" => "min7",
            "dim7" | "°7" => "dim7",
            "m7b5" | "ø" => "m7b5",
            "9" => "dom9",
            "maj9" => "maj9",
            "m9" | "min9" => "min9",
            "m11" => "min11",
            "add9" => "add9",
            "madd9" => "madd9",
            "sus2" => "sus2",
            "sus4" | "sus" => "sus4",
            "5" => "power",
            other => bail!("unknown chord suffix '{other}' in '{t}'"),
        };
        return Ok(Chord {
            root_pc: pc,
            intervals: quality(q)?.to_vec(),
            label: t.to_string(),
        });
    }
    // roman numeral
    let mut s = t;
    let mut acc: i32 = 0;
    if let Some(r) = s.strip_prefix('b') {
        acc = -1;
        s = r;
    } else if let Some(r) = s.strip_prefix('#') {
        acc = 1;
        s = r;
    }
    let (deg, n) = roman_value(s).ok_or_else(|| {
        anyhow!(
            "can't parse chord '{t}' (use roman numerals like i VI III VII or names like Cm Ab)"
        )
    })?;
    let numeral = &s[..n];
    let upper = numeral.chars().all(|c| c.is_ascii_uppercase());
    let suffix = &s[n..];
    let q = match (upper, suffix) {
        (_, "°") | (_, "dim") | (_, "o") => "dim",
        (_, "°7") | (_, "dim7") => "dim7",
        (_, "ø") | (_, "ø7") | (_, "m7b5") => "m7b5",
        (_, "+") | (_, "aug") => "aug",
        (_, "sus2") => "sus2",
        (_, "sus4") | (_, "sus") => "sus4",
        (_, "maj7") => "maj7",
        (_, "maj9") => "maj9",
        (_, "add9") => {
            if upper {
                "add9"
            } else {
                "madd9"
            }
        }
        (_, "5") => "power",
        (true, "") => "maj",
        (false, "") => "min",
        (true, "7") => "dom7",
        (false, "7") => "min7",
        (true, "9") => "dom9",
        (false, "9") => "min9",
        (false, "11") => "min11",
        (_, other) => bail!("unknown chord suffix '{other}' in '{t}'"),
    };
    let iv = scale_intervals(scale).unwrap_or(&[0, 2, 3, 5, 7, 8, 10]);
    let iv7: &[u8] = if iv.len() == 7 {
        iv
    } else if scale.contains("major") {
        &[0, 2, 4, 5, 7, 9, 11]
    } else {
        &[0, 2, 3, 5, 7, 8, 10]
    };
    let root = (key_pc as i32 + iv7[deg - 1] as i32 + acc).rem_euclid(12) as u8;
    Ok(Chord {
        root_pc: root,
        intervals: quality(q)?.to_vec(),
        label: t.to_string(),
    })
}

pub fn parse_progression(prog: &str, key_pc: u8, scale: &str) -> Result<Vec<Chord>> {
    let chords: Result<Vec<Chord>> = prog
        .split(|c: char| c.is_whitespace() || c == ',' || c == '|' || c == '-')
        .filter(|s| !s.is_empty())
        .map(|tok| parse_chord(tok, key_pc, scale))
        .collect();
    let chords = chords?;
    if chords.is_empty() {
        bail!("progression is empty");
    }
    Ok(chords)
}

/// Voice chords around `octave`, choosing inversions for smooth voice leading.
pub fn voice_chords(chords: &[Chord], octave: i32, smooth: bool) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut prev_center: Option<f32> = None;
    for c in chords {
        let base = (octave + 1) * 12 + c.root_pc as i32;
        let tones: Vec<i32> = c.intervals.iter().map(|&i| base + i as i32).collect();
        let mut best = tones.clone();
        if smooth {
            if let Some(pc) = prev_center {
                let mut candidates = Vec::new();
                for inv in 0..tones.len() {
                    let mut v: Vec<i32> = tones.clone();
                    for x in v.iter_mut().take(inv) {
                        *x += 12;
                    }
                    for shift in [-12, 0, 12] {
                        candidates.push(v.iter().map(|x| x + shift).collect::<Vec<i32>>());
                    }
                }
                best = candidates
                    .into_iter()
                    .min_by(|a, b| {
                        let ca = a.iter().sum::<i32>() as f32 / a.len() as f32;
                        let cb = b.iter().sum::<i32>() as f32 / b.len() as f32;
                        (ca - pc).abs().partial_cmp(&(cb - pc).abs()).unwrap()
                    })
                    .unwrap();
            }
        }
        best.sort();
        prev_center = Some(best.iter().sum::<i32>() as f32 / best.len() as f32);
        out.push(best.into_iter().map(|x| x.clamp(0, 127) as u8).collect());
    }
    out
}

/// Open voicings for a mix with a bass part: the bass owns the root and the
/// low end, so the chord sits above `floor` (MIDI; 59 = B3, ~247 Hz), never
/// stacks a third (or second) at its bottom below G4, spreads close triads
/// (drop-2 style) and leaves the root out of four-note chords. Voice leading
/// stays smooth: each chord takes the candidate whose centre is closest to the
/// previous one. Falls back to the close voicing lifted above `floor`.
pub fn voice_chords_open(chords: &[Chord], octave: i32, floor: i32) -> Vec<Vec<u8>> {
    let ceil = floor + 26;
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut prev_center: Option<f32> = None;
    for c in chords {
        let base = (octave + 1) * 12 + c.root_pc as i32;
        let all: Vec<i32> = c.intervals.iter().map(|&i| base + i as i32).collect();
        // rootless when there are enough other tones (the bass plays the root)
        let tones: Vec<i32> = if all.len() >= 4 {
            all.iter()
                .copied()
                .filter(|t| (t - base) % 12 != 0)
                .collect()
        } else {
            all.clone()
        };
        let mut cands: Vec<Vec<i32>> = Vec::new();
        for inv in 0..tones.len() {
            let mut v: Vec<i32> = tones.clone();
            for x in v.iter_mut().take(inv) {
                *x += 12;
            }
            v.sort();
            let mut spread = v.clone();
            if spread.len() >= 3 {
                // drop-2 style spread: the second voice from the bottom up an octave
                spread[1] += 12;
                spread.sort();
            }
            for shift in [-24, -12, 0, 12, 24] {
                cands.push(v.iter().map(|x| x + shift).collect());
                cands.push(spread.iter().map(|x| x + shift).collect());
            }
        }
        let ok = |v: &Vec<i32>| {
            let lo = v[0];
            let hi = *v.last().unwrap();
            lo >= floor && hi <= ceil && (v.len() < 2 || lo >= 67 || v[1] - lo >= 5)
        };
        let target = prev_center.unwrap_or((floor + 8) as f32);
        let center = |v: &Vec<i32>| v.iter().sum::<i32>() as f32 / v.len() as f32;
        let best = cands
            .iter()
            .filter(|v| ok(v))
            .min_by(|a, b| {
                let da = (center(a) - target).abs() + 0.15 * (a.last().unwrap() - a[0]) as f32;
                let db = (center(b) - target).abs() + 0.15 * (b.last().unwrap() - b[0]) as f32;
                da.partial_cmp(&db).unwrap()
            })
            .cloned()
            .unwrap_or_else(|| {
                let mut v = all.clone();
                v.sort();
                while v[0] < floor {
                    for x in v.iter_mut() {
                        *x += 12;
                    }
                }
                v
            });
        prev_center = Some(center(&best));
        out.push(best.into_iter().map(|x| x.clamp(0, 127) as u8).collect());
    }
    out
}

/// Parse a step string: X accent, x hit, o ghost, '.' or '-' rest. Spaces and | ignored.
pub fn parse_steps(s: &str) -> Vec<Option<f32>> {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .map(|c| match c {
            'X' => Some(1.0),
            'x' | '1' => Some(0.85),
            'o' | 'O' => Some(0.45),
            _ => None,
        })
        .collect()
}

pub fn steps_to_notes(
    steps: &[Option<f32>],
    total_steps: u32,
    repeat: bool,
    pitch: u8,
    len: f32,
) -> Vec<Note> {
    let mut notes = Vec::new();
    if steps.is_empty() {
        return notes;
    }
    let count = if repeat {
        total_steps as usize
    } else {
        steps.len().min(total_steps as usize)
    };
    for i in 0..count {
        if let Some(v) = steps[i % steps.len()] {
            notes.push(Note {
                start: i as f32,
                len,
                pitch,
                vel: v,
                ..Default::default()
            });
        }
    }
    notes
}

/// Drum grooves: (genre, bpm, swing, [(track, preset, steps)])
pub struct Groove {
    pub name: &'static str,
    pub bpm: f32,
    pub swing: f32,
    pub parts: &'static [(&'static str, &'static str, &'static str)],
}

pub const GROOVES: &[Groove] = &[
    Groove {
        name: "house",
        bpm: 124.0,
        swing: 0.1,
        parts: &[
            ("kick", "kick", "X...X...X...X..."),
            ("clap", "clap", "....x.......x..."),
            ("hat", "hat", "o.o.o.o.o.o.o.o."),
            ("open_hat", "open_hat", "..x...x...x...x."),
        ],
    },
    Groove {
        name: "techno",
        bpm: 130.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X...X...X...X..."),
            ("hat", "hat", "ooxoooxoooxoooxo"),
            ("open_hat", "open_hat", "..x...x...x...x."),
            ("rim", "rim", "...x..x....x.x.."),
            ("clap", "clap", "....x.......x..."),
        ],
    },
    Groove {
        name: "trap",
        bpm: 140.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X......x..x....."),
            ("snare", "clap", "........X......."),
            ("hat", "hat", "x.x.x.x.x.xox.xxx.x.x.x.x.x.xoxo"),
            ("open_hat", "open_hat", "..............x."),
        ],
    },
    Groove {
        name: "drill",
        bpm: 142.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X.....x...x.....X.......x.x....."),
            ("snare", "snare", "......X........X......X......X.."),
            ("hat", "hat", "x..x..x.x..x..x.x..x..x.x.x..x.."),
            ("rim", "rim", "...o.......o...."),
        ],
    },
    Groove {
        name: "boom_bap",
        bpm: 90.0,
        swing: 0.3,
        parts: &[
            ("kick", "kick", "X.....x...x.x..."),
            ("snare", "snare", "....X.......X..o"),
            ("hat", "hat", "x.x.x.x.x.x.x.x."),
        ],
    },
    Groove {
        name: "lofi",
        bpm: 82.0,
        swing: 0.35,
        parts: &[
            ("kick", "kick", "X......x..x....."),
            ("snare", "snare", "....x.......x..."),
            ("hat", "hat", "x.xox.x.x.xox.xo"),
            ("shaker", "shaker", "..o...o...o...o."),
        ],
    },
    Groove {
        name: "dnb",
        bpm: 174.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X.........X.....X.........x....."),
            ("snare", "snare", "....X.......X.......X.......X..o"),
            ("hat", "hat", "x.x.x.x.x.x.x.x."),
            ("shaker", "shaker", "oxoxoxoxoxoxoxox"),
        ],
    },
    Groove {
        name: "reggaeton",
        bpm: 95.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X...X...X...X..."),
            ("snare", "snare", "...x..x....x..x."),
            ("hat", "hat", "x.x.x.x.x.x.x.x."),
            ("rim", "rim", "......o.......o."),
        ],
    },
    Groove {
        name: "afrobeats",
        bpm: 105.0,
        swing: 0.15,
        parts: &[
            ("kick", "kick", "X..x..x...x..x.."),
            ("rim", "rim", "...x..x....x..x."),
            ("shaker", "shaker", "xoxxxoxxxoxxxoxx"),
            ("clap", "clap", "....x.......x..."),
            ("tom", "tom", "..............o."),
        ],
    },
    Groove {
        name: "phonk",
        bpm: 130.0,
        swing: 0.0,
        parts: &[
            ("kick", "kick", "X..x..X...x.x..."),
            ("clap", "clap", "....X.......X..."),
            ("hat", "hat", "x.x.x.x.x.x.xxxx"),
            ("cowbell", "cowbell", "x..x..x...x.x.x."),
        ],
    },
    Groove {
        // desi hip-hop: a boom-bap pocket under a keherwa-style tabla and a
        // dholak (treble slaps + bass-head booms)
        name: "desi_hiphop",
        bpm: 92.0,
        swing: 0.12,
        parts: &[
            ("kick", "kick", "X.....x...x.....X.....x.x......."),
            ("snare", "snare", "....X.......X.......X.......X..o"),
            ("hat", "hat", "x.x.x.x.x.x.x.x."),
            ("tabla", "tabla", "X.xoX.x.X.xox.x.X.xoX.x.Xoxox.xo"),
            ("dholak", "dholak", "..x..x.x..x..xx...x..x.x..x..x.x"),
            (
                "dholak_bass",
                "dholak_bass",
                "X......xX.....x.X......xX..x....",
            ),
        ],
    },
    Groove {
        name: "garage",
        bpm: 132.0,
        swing: 0.4,
        parts: &[
            ("kick", "kick", "X.......x.X....."),
            ("snare", "snare", "....X.......X..."),
            ("hat", "hat", "..x...x...x..xx."),
            ("shaker", "shaker", "oxoxoxoxoxoxoxox"),
        ],
    },
];

pub fn groove(name: &str) -> Result<&'static Groove> {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    let n = match n.as_str() {
        "hiphop" | "hip_hop" | "boombap" => "boom_bap",
        "lo_fi" | "lofi_hiphop" => "lofi",
        "drum_and_bass" | "jungle" => "dnb",
        "uk_garage" | "2step" => "garage",
        "afro" | "amapiano" => "afrobeats",
        "desi" | "dhh" | "desi_hip_hop" | "gully" | "indian" => "desi_hiphop",
        other => other,
    };
    GROOVES.iter().find(|g| g.name == n).ok_or_else(|| {
        anyhow!(
            "unknown style '{name}'. Options: {}",
            GROOVES
                .iter()
                .map(|g| g.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Humanize: random timing (steps) and velocity jitter.
pub fn humanize(notes: &mut [Note], timing: f32, velocity: f32, rng: &mut Rng) {
    for n in notes.iter_mut() {
        n.start = (n.start + rng.bipolar() * timing).max(0.0);
        n.vel = (n.vel * (1.0 + rng.bipolar() * velocity)).clamp(0.05, 1.0);
    }
}

/// Add a fill (snare roll / ghost notes) to the last bar of a drum part.
pub fn add_fill(notes: &mut Vec<Note>, total_steps: u32, rng: &mut Rng, density: f32) {
    let start = total_steps.saturating_sub(4);
    for s in start..total_steps {
        if rng.chance(density) && !notes.iter().any(|n| n.start as u32 == s) {
            let vel = 0.4 + 0.5 * (s - start) as f32 / 4.0;
            notes.push(Note {
                start: s as f32,
                len: 1.0,
                pitch: 60,
                vel,
                ..Default::default()
            });
            if rng.chance(0.4) {
                notes.push(Note {
                    start: s as f32 + 0.5,
                    len: 0.5,
                    pitch: 60,
                    vel: vel * 0.7,
                    ..Default::default()
                });
            }
        }
    }
}

/// Chord comping patterns.
pub fn chord_notes(
    voiced: &[Vec<u8>],
    bars_per_chord: f32,
    total_steps: u32,
    style: &str,
    vel: f32,
) -> Result<Vec<Note>> {
    let span = (bars_per_chord * STEPS_PER_BAR as f32).max(1.0);
    let mut notes = Vec::new();
    let n_chords = (total_steps as f32 / span).ceil() as usize;
    for ci in 0..n_chords {
        let chord = &voiced[ci % voiced.len()];
        let t0 = ci as f32 * span;
        let t1 = (t0 + span).min(total_steps as f32);
        let mut push = |start: f32, len: f32, p: u8, v: f32| {
            if start < t1 {
                notes.push(Note {
                    start,
                    len: len.min(t1 - start),
                    pitch: p,
                    vel: v,
                    ..Default::default()
                });
            }
        };
        match style {
            "block" | "pad" | "hold" => {
                for &p in chord {
                    push(t0, t1 - t0, p, vel);
                }
            }
            "stabs" => {
                let pat = parse_steps("x..x..x...x..x..");
                let mut s = t0;
                while s < t1 {
                    let i = ((s - t0) as usize) % 16;
                    if pat[i].is_some() {
                        for &p in chord {
                            push(s, 1.5, p, vel);
                        }
                    }
                    s += 1.0;
                }
            }
            "offbeat" => {
                let mut s = t0 + 2.0;
                while s < t1 {
                    for &p in chord {
                        push(s, 1.5, p, vel);
                    }
                    s += 4.0;
                }
            }
            "pulse" => {
                let mut s = t0;
                while s < t1 {
                    for &p in chord {
                        push(s, 1.6, p, if ((s - t0) as u32) % 4 == 0 { vel } else { vel * 0.75 });
                    }
                    s += 2.0;
                }
            }
            "arp_up" | "arp_down" | "arp_updown" | "arp" => {
                let mut seq: Vec<u8> = chord.clone();
                seq.push(chord[0] + 12);
                if style == "arp_down" {
                    seq.reverse();
                } else if style == "arp_updown" {
                    let mut down: Vec<u8> = seq.iter().rev().skip(1).take(seq.len().saturating_sub(2)).copied().collect();
                    seq.append(&mut down);
                }
                let mut s = t0;
                let mut k = 0;
                while s < t1 {
                    push(s, 0.9, seq[k % seq.len()], if k % 4 == 0 { vel } else { vel * 0.8 });
                    s += 1.0;
                    k += 1;
                }
            }
            other => bail!("unknown chord style '{other}'. Options: block, stabs, offbeat, pulse, arp_up, arp_down, arp_updown"),
        }
    }
    Ok(notes)
}

/// Basslines that follow a progression.
pub fn bass_notes(
    chords: &[Chord],
    bars_per_chord: f32,
    total_steps: u32,
    octave: i32,
    style: &str,
    rng: &mut Rng,
) -> Result<Vec<Note>> {
    let span = (bars_per_chord * STEPS_PER_BAR as f32).max(1.0);
    let n_chords = (total_steps as f32 / span).ceil() as usize;
    let mut notes = Vec::new();
    for ci in 0..n_chords {
        let c = &chords[ci % chords.len()];
        let root = ((octave + 1) * 12 + c.root_pc as i32).clamp(0, 127) as u8;
        let fifth = root.saturating_add(c.intervals.get(2).copied().unwrap_or(7));
        let t0 = ci as f32 * span;
        let t1 = (t0 + span).min(total_steps as f32);
        let mut push = |start: f32, len: f32, p: u8, v: f32| {
            if start < t1 {
                notes.push(Note {
                    start,
                    len: len.min(t1 - start),
                    pitch: p,
                    vel: v,
                    ..Default::default()
                });
            }
        };
        match style {
            "root" | "whole" => push(t0, t1 - t0, root, 0.9),
            "eighths" => {
                let mut s = t0;
                while s < t1 {
                    push(s, 1.6, root, if ((s - t0) as u32) % 4 == 0 { 0.95 } else { 0.75 });
                    s += 2.0;
                }
            }
            "octave" => {
                let mut s = t0;
                let mut k = 0;
                while s < t1 {
                    push(s, 1.5, if k % 2 == 0 { root } else { root + 12 }, 0.85);
                    s += 2.0;
                    k += 1;
                }
            }
            "offbeat" => {
                let mut s = t0 + 2.0;
                while s < t1 {
                    push(s, 1.6, root, 0.9);
                    s += 4.0;
                }
            }
            "syncopated" | "funk" => {
                let pat = parse_steps("x..x..x...x..x..");
                let mut s = t0;
                while s < t1 {
                    let i = ((s - t0) as usize) % 16;
                    if pat[i].is_some() {
                        let p = if rng.chance(0.25) { fifth } else if rng.chance(0.15) { root + 12 } else { root };
                        push(s, 1.5, p, 0.85);
                    }
                    s += 1.0;
                }
            }
            "808" | "trap" => {
                // long notes on a trap kick-ish rhythm, occasional octave jumps
                let pat = [0.0, 7.0, 10.0];
                let mut bar = t0;
                while bar < t1 {
                    for (k, off) in pat.iter().enumerate() {
                        let s = bar + off;
                        let next = pat.get(k + 1).map(|n| bar + n).unwrap_or(bar + 16.0);
                        let p = if k == 2 && rng.chance(0.35) { root + 12 } else { root };
                        push(s, next - s, p, 0.95);
                    }
                    bar += 16.0;
                }
            }
            "walking" => {
                let iv = [0u8, 2, 4, 5, 7, 9, 7, 4];
                let mut s = t0;
                let mut k = 0;
                while s < t1 {
                    push(s, 3.6, root + iv[k % iv.len()].min(12), 0.85);
                    s += 4.0;
                    k += 1;
                }
            }
            other => bail!("unknown bass style '{other}'. Options: root, eighths, octave, offbeat, syncopated, 808, walking"),
        }
    }
    Ok(notes)
}

/// Scale-constrained melody with motif repetition and chord-tone targeting.
pub struct MelodySpec<'a> {
    pub key_pc: u8,
    pub scale: &'a [u8],
    pub octave: i32,
    pub range_semitones: i32,
    pub density: f32,
    pub total_steps: u32,
    pub motif_bars: u32,
    pub chords: Option<(&'a [Chord], f32)>,
    pub note_len: f32,
}

pub fn melody_notes(spec: &MelodySpec, rng: &mut Rng) -> Vec<Note> {
    let low = (spec.octave + 1) * 12 + spec.key_pc as i32;
    let high = low + spec.range_semitones.max(5);
    let pool: Vec<i32> = (low - 12..=high)
        .filter(|p| {
            p >= &low
                && spec
                    .scale
                    .contains(&(((p - spec.key_pc as i32).rem_euclid(12)) as u8))
        })
        .collect();
    let motif_steps = (spec.motif_bars.max(1) * STEPS_PER_BAR).min(spec.total_steps);
    let weights_for = |i: u32| -> f32 {
        let pos = i % 16;
        let w = if pos % 4 == 0 {
            1.0
        } else if pos % 2 == 0 {
            0.65
        } else {
            0.35
        };
        (spec.density * w * 1.6).min(0.98)
    };
    let gen = |from: u32, to: u32, rng: &mut Rng, start_idx: usize| -> Vec<Note> {
        let mut out = Vec::new();
        let mut idx = start_idx.min(pool.len().saturating_sub(1));
        let mut s = from;
        while s < to {
            if rng.chance(weights_for(s)) {
                // random walk with small steps preferred
                let step = [-2i32, -1, -1, 0, 1, 1, 2, 3, -3]
                    [rng.weighted(&[1.0, 3.0, 3.0, 1.0, 3.0, 3.0, 1.0, 0.4, 0.4])];
                idx = (idx as i32 + step).clamp(0, pool.len() as i32 - 1) as usize;
                let mut pitch = pool[idx];
                // strong beats: snap to nearest chord tone
                if let Some((chords, bpc)) = spec.chords {
                    if s % 4 == 0 {
                        let ci = (s as f32 / (bpc * 16.0)) as usize % chords.len();
                        let c = &chords[ci];
                        let tones: Vec<i32> = c
                            .intervals
                            .iter()
                            .map(|i| (c.root_pc as i32 + *i as i32) % 12)
                            .collect();
                        if let Some((bi, bp)) = pool
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| tones.contains(&(*p).rem_euclid(12)))
                            .min_by_key(|(_, p)| (*p - pitch).abs())
                        {
                            idx = bi;
                            pitch = *bp;
                        }
                    }
                }
                out.push(Note {
                    start: s as f32,
                    len: spec.note_len,
                    pitch: pitch.clamp(0, 127) as u8,
                    vel: if s % 4 == 0 { 0.9 } else { 0.72 },
                    ..Default::default()
                });
            }
            s += 1;
        }
        // extend notes up to the next onset (legato-ish), capped
        for i in 0..out.len() {
            let next = out.get(i + 1).map(|n| n.start).unwrap_or(to as f32);
            out[i].len = (next - out[i].start)
                .min(spec.note_len.max(1.0) * 2.0)
                .max(0.5);
        }
        out
    };
    let start_idx = pool.len() / 3;
    let motif = gen(0, motif_steps, rng, start_idx);
    let mut notes = motif.clone();
    let mut offset = motif_steps;
    let mut rep = 1;
    while offset < spec.total_steps {
        let end = (offset + motif_steps).min(spec.total_steps);
        if rep % 2 == 1 {
            // answer phrase: repeat first half of motif, vary the second
            let half = motif_steps / 2;
            for n in motif.iter().filter(|n| (n.start as u32) < half) {
                let mut m = n.clone();
                m.start += offset as f32;
                if m.start < end as f32 {
                    notes.push(m);
                }
            }
            notes.extend(gen(offset + half, end, rng, start_idx));
        } else {
            for n in &motif {
                let mut m = n.clone();
                m.start += offset as f32;
                if m.start < end as f32 {
                    notes.push(m);
                }
            }
        }
        offset = end;
        rep += 1;
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_parse() {
        assert_eq!(parse_note("C4").unwrap(), 60);
        assert_eq!(parse_note("A4").unwrap(), 69);
        assert_eq!(parse_note("F#2").unwrap(), 42);
        assert_eq!(parse_note("Bb1").unwrap(), 34);
        assert_eq!(parse_note("64").unwrap(), 64);
        assert_eq!(note_name(61), "C#4");
    }

    #[test]
    fn roman_numerals_in_minor() {
        let c = parse_progression("i VI III VII", 0, "minor").unwrap();
        let roots: Vec<u8> = c.iter().map(|c| c.root_pc).collect();
        assert_eq!(roots, vec![0, 8, 3, 10]); // C Ab Eb Bb
        assert_eq!(c[0].intervals, vec![0, 3, 7]);
        assert_eq!(c[1].intervals, vec![0, 4, 7]);
        let d = parse_progression("ii7 V7 Imaj7", 0, "major").unwrap();
        assert_eq!(d[0].intervals, vec![0, 3, 7, 10]);
        assert_eq!(d[1].root_pc, 7);
        assert_eq!(d[2].intervals, vec![0, 4, 7, 11]);
    }

    #[test]
    fn chord_names() {
        let c = parse_progression("Cm7 Ab Ebmaj7 Bbsus4", 0, "minor").unwrap();
        assert_eq!(c[0].intervals, vec![0, 3, 7, 10]);
        assert_eq!(c[1].root_pc, 8);
        assert_eq!(c[3].intervals, vec![0, 5, 7]);
    }

    #[test]
    fn voice_leading_stays_close() {
        let c = parse_progression("i VI III VII", 0, "minor").unwrap();
        let v = voice_chords(&c, 4, true);
        for w in v.windows(2) {
            let a = w[0].iter().map(|x| *x as f32).sum::<f32>() / w[0].len() as f32;
            let b = w[1].iter().map(|x| *x as f32).sum::<f32>() / w[1].len() as f32;
            assert!((a - b).abs() <= 6.0, "{a} {b}");
        }
    }

    #[test]
    fn all_grooves_parse() {
        for g in GROOVES {
            for (_, preset, steps) in g.parts {
                assert!(crate::instruments::preset(preset).is_some());
                assert!(!parse_steps(steps).is_empty());
            }
        }
    }

    #[test]
    fn melody_in_scale() {
        let scale = scale_intervals("minor").unwrap();
        let spec = MelodySpec {
            key_pc: 9,
            scale,
            octave: 4,
            range_semitones: 14,
            density: 0.5,
            total_steps: 64,
            motif_bars: 2,
            chords: None,
            note_len: 1.0,
        };
        let notes = melody_notes(&spec, &mut Rng::new(3));
        assert!(!notes.is_empty());
        for n in notes {
            assert!(scale.contains(&(((n.pitch as i32 - 9).rem_euclid(12)) as u8)));
            assert!(n.start < 64.0);
        }
    }
    #[test]
    fn open_voicings_stay_above_the_bass_without_low_thirds() {
        for prog in [
            "i7 iv7 VII7 IIImaj7",
            "i VI III VII",
            "ii7 V7 Imaj7 vi7",
            "I bII iv I",
        ] {
            for key in 0..12u8 {
                let ch = parse_progression(prog, key, "minor").unwrap();
                for v in voice_chords_open(&ch, 3, 59) {
                    assert!(v[0] >= 59, "{prog} {key}: {v:?}");
                    assert!(
                        v.len() < 2 || v[0] >= 67 || v[1] - v[0] >= 5,
                        "{prog} {key}: {v:?}"
                    );
                }
            }
        }
        // a seventh chord drops its root (the bass has it)
        let ch = parse_progression("i7", 0, "minor").unwrap();
        let v = voice_chords_open(&ch, 4, 59);
        assert!(v[0].iter().all(|p| p % 12 != 0), "{:?}", v[0]);
    }
}
