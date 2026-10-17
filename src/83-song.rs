//! Songs as data. One [`SongSpec`] JSON in, one finished song out:
//! composed, vocal placed, mixed to the targets in [`crate::mix_rules`],
//! delivered (wav/mp3/flac) and checked, in ONE engine call. The spec is kept
//! in the project (`Project.song`), so any part can be changed later by its
//! JSON path (`song_patch`) and only what that path touches is redone:
//!
//! * `/mix/...`, `/master/...`, `/export/...`, `/vocal/gain_db` -> re-mix and
//!   re-deliver (the beat and the vocal clips stay; unchanged tracks come
//!   from the render cache);
//! * `/vocal/...` -> the vocal track is rebuilt, then re-mixed;
//! * anything else (`/bpm`, `/structure/...`, `/instruments/...`, `/seed`...)
//!   -> the beat is recomposed with the SAME seed (unchanged choices stay
//!   the same), then the vocal and the mix.
//!
//! Every problem in a spec is reported at once, each with its JSON path and
//! the fix. A song call is one engine tool call: one undo step, and any
//! failure rolls the whole project back.

use crate::engine::Engine;
use crate::mix_rules::{self, Targets, VocalChain};
use crate::tools::{obj, Tool};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SECTION_KINDS: &[&str] = &[
    "intro",
    "verse",
    "pre",
    "hook",
    "chorus",
    "drop",
    "bridge",
    "breakdown",
    "outro",
];
const VOCAL_SAMPLE: &str = "song_vocal";

fn d_track() -> String {
    "vocal".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct SongSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Free words about the song (style, references, feel); optional.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f32>,
    /// "D", "D minor", "F#m".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<String>,
    /// sparse | balanced | dense (rap usually wants sparse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structure: Vec<SectionSpec>,
    /// role -> instrument (roles: lead, harmony, counter, texture, bass,
    /// kick, snare, hat, open_hat, perc, tabla, bayan): a preset name, a
    /// recorded instrument "catalog:<id>" (search_instruments), or one-shot
    /// samples {"samples": [files], "gain_db": 0} (e.g. real dhol hits).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instruments: BTreeMap<String, InstrumentSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocal: Option<VocalSpec>,
    #[serde(default)]
    pub mix: MixSpec,
    #[serde(default)]
    pub master: MasterSpec,
    #[serde(default)]
    pub export: ExportSpec,
}

/// What plays a role.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum InstrumentSpec {
    /// A preset name, or "catalog:<id>" for a recorded (SFZ) instrument.
    Name(String),
    /// One-shot recordings played round-robin at the role's own pitch.
    Samples {
        samples: Vec<String>,
        #[serde(default)]
        gain_db: f32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SectionSpec {
    pub kind: String,
    pub bars: u32,
    /// Default: kind + its count ("verse1", "hook2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The section's beat energy in dB (-9..6, default 0): the arrangement
    /// layers (everything but the vocal, kick, bass and sub) play this much
    /// softer/louder in this section only. Thinner verses (-2..-4) make the
    /// hook lift without master automation, so a loud master keeps its
    /// lift. song_improve turns it for a flat hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VocalSpec {
    /// A wav/mp3/flac on disk: the raw take.
    pub file: String,
    #[serde(default = "d_track")]
    pub track: String,
    /// Where each piece of the take goes. Empty = cut at the pauses and
    /// placed in take order from `start_section` (default: the first verse).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<PhraseSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_section: Option<String>,
    /// The words (kept with the song; placement uses `phrases`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    /// Extra fader dB on top of the automatic balance.
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub chain: VocalChain,
    /// Beat follows the vocal (on unless false): when the take is cut at its
    /// pauses, each phrase starts where its syllables land on the 1/16 grid,
    /// and with no "bpm" in the spec the tempo is picked from the take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<bool>,
    /// How hard syllables inside each phrase are pulled onto the grid after
    /// the lock (0..1, default 0.5; 0 keeps every phrase untouched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flex: Option<f32>,
}

/// One piece of the take: from `src_s` for `len_s` seconds (or to `end_s`),
/// placed at `section` + `bar` (1-based) + `beat` (1-based, fractions ok:
/// 4.5 = the "and" of 4), or at an absolute `start_beat` / `at_s`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct PhraseSpec {
    #[serde(alias = "src")]
    pub src_s: f32,
    #[serde(default, alias = "len", skip_serializing_if = "Option::is_none")]
    pub len_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_beat: Option<f32>,
    #[serde(default, alias = "start_s", skip_serializing_if = "Option::is_none")]
    pub at_s: Option<f32>,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_ms: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MixSpec {
    pub vocal_over_beat_db: f32,
    pub vocal_over_beat_max_db: f32,
    /// Least hooks/drops sit over verses on the delivered file (dB).
    pub drop_lift_db: f32,
    /// Render-measure-correct passes for the vocal balance.
    pub balance_passes: u32,
    /// Role -> fader offset (dB, -12..12) on top of the genre's balance,
    /// e.g. {"bass": 2, "hat": -1}. Applied when the beat is composed, so a
    /// change recomposes (same seed) and re-mixes. song_improve writes here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub levels: BTreeMap<String, f32>,
    /// The 5-10 kHz floor on the delivered file (dB minus LUFS; default
    /// -26): raise it toward a reference's top end (song_improve with a
    /// reference sets it). The delivery loop gives the bright parts back
    /// what the level rule took until the floor is met.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_floor_db: Option<f32>,
    /// The 2-5 kHz ceiling on the delivered file (dB minus LUFS; default
    /// the genre window's top): lower it toward a reference's presence so
    /// the delivery's beat-bus tone aims darker there (song_improve sets it
    /// when a take comes out harsh).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_ceiling_db: Option<f32>,
    /// The master's tone lift (presence bell + air shelf + optional extra
    /// bell nudge) for this song; unset = `MixProfile::default()`,
    /// `{"presence_db": 0, "air_db": 0}` = none (the extra bell is 0 by default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_lift: Option<mix_rules::MixProfile>,
    /// Deeper builds into each hook (off by default so earlier renders stay
    /// comparable): the melodic beds' filter closes to ~500 Hz two bars out
    /// and sweeps open into the hook, the drum bus drops out for the last
    /// beat (a pre-drop gap), and lead/chords throw their reverb send up
    /// over that beat so the tail fills the gap. Applied when the beat is
    /// composed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub builds: bool,
    /// Section gain staging (dB, -6..6) by section kind or name, e.g.
    /// {"verse": -1.2, "hook": 1.5}: stepped volume on every bus that feeds
    /// the master (the beat bus and the effect returns), so hooks sit over
    /// verses before the master chain. The vocal keeps its own ride. Empty
    /// (off) by default, so earlier renders stay comparable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub section_gain_db: BTreeMap<String, f32>,
    /// Per-part EQ keyed by exact track name (e.g. "chords") or by role (as
    /// /mix/levels, e.g. "harmony"), or "vocal"; when a track name and a
    /// role both name one track, the track name's EQ is used. E.g.
    /// {"bass": {"low_shelf": {"hz": 150, "db": -2}, "bells": [{"hz": 3000,
    /// "db": -1.5, "q": 1.2}]}}: one parametric EQ (low shelf, high shelf,
    /// up to 6 bells) at the end of the part's own chain, before any mix
    /// rule measures, so the level and tone rules hear the EQ'd part. The
    /// vocal's goes after its vocal chain (/vocal/chain/presence_db stays
    /// the vocal's own presence bell). Empty (off) by default: nothing is
    /// added, so earlier renders stay bit-identical.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub part_eq: BTreeMap<String, PartEq>,
    /// Masking-aware auto-EQ (see `crate::auto_eq`): after the first vocal
    /// balance, measure where the vocal collides with each instrument (and
    /// the kick with the bass) and cut the other part there: at most 2 bells
    /// per part, -1..-3 dB, never the vocal, only into free slots of the
    /// part's EQ and never near a band the user set. The cuts are written
    /// into the stored spec's /mix/part_eq as bells marked "auto": true
    /// (each build drops and re-measures them) and listed in the result's
    /// mix.auto_eq. Off (false) by default: nothing is measured or added.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto_eq: bool,
    /// Vocal-keyed band ducks (see `apply_auto_duck`): every instrument whose
    /// overlap with the vocal is 5% or more gets one keyed dynamic EQ band at
    /// its best clash band (150 Hz-1 kHz, from the auto-EQ measure) that
    /// dips up to 4 dB while the vocal sounds (its level through an envelope
    /// follower: attack 12 ms, release 100 ms) and stays flat when the vocal
    /// is silent. Written into the stored spec's /mix/part_eq/<part>/ducks
    /// marked "auto": true (each build drops and places them again). Never on the vocal; not
    /// within a third of an octave of a user part EQ band. Off by default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vocal_duck: bool,
    /// Kick-keyed duck on the bass: one keyed dynamic EQ band at the
    /// kick/bass clash band (30-250 Hz) that dips up to 4 dB on each kick
    /// hit and takes nothing between hits (written into /mix/part_eq like
    /// the vocal ducks). Off by default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub kick_duck: bool,
    /// Vocal resonance tamer (see `apply_vocal_resonance`): up to 4 narrow
    /// self-keyed dynamic EQ bands (Q5, at most -4 dB, attack 5 ms, release
    /// 80 ms) on the vocal's lasting resonances (700 Hz-8 kHz, found in its
    /// long-term spectrum), each acting only when its peak pokes out over
    /// its usual level. Never boosts, never on the beat; not within a third
    /// of an octave of the de-esser or a user's vocal EQ band. Written into
    /// /mix/part_eq/vocal/ducks marked "auto" (key "self"). Off by default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vocal_resonance: bool,
}

/// A shelf of a part EQ: corner (Hz) and gain (dB).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EqShelf {
    pub hz: f32,
    pub db: f32,
}

/// A bell of a part EQ: centre (Hz), gain (dB) and Q (default 1).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EqBell {
    pub hz: f32,
    pub db: f32,
    #[serde(default = "default_bell_q")]
    pub q: f32,
    /// Written by the auto-EQ (/mix/auto_eq) into the delivered spec, not by
    /// a person: every build drops these and measures again (with auto_eq
    /// off they are dropped and nothing replaces them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

fn default_bell_q() -> f32 {
    1.0
}

/// Most bells one part EQ takes (the parametric EQ has 8 bands; two are
/// the shelves).
pub const PART_EQ_MAX_BELLS: usize = 6;
/// The slope of a part EQ's shelves (the parametric EQ's shelf Q).
const PART_EQ_SHELF_Q: f32 = 0.707;

/// One part's EQ (/mix/part_eq/<part>).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PartEq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_shelf: Option<EqShelf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_shelf: Option<EqShelf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bells: Vec<EqBell>,
    /// Sidechain band ducks the build placed on this part (/mix/vocal_duck,
    /// /mix/kick_duck), all marked "auto": true: a record of what the build
    /// did, dropped and placed again by every build (a duck a person writes
    /// here is refused; turn the mode on instead).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ducks: Vec<EqDuck>,
}

/// One auto duck of a part (see `PartEq::ducks`): a keyed dynamic EQ band at
/// `hz` (Q `q`) that dips up to `max_db` while `source` sounds; `key` says
/// how it follows the source ("level": its level through an envelope
/// follower; "hits": its note starts).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct EqDuck {
    pub hz: f32,
    pub max_db: f32,
    pub q: f32,
    pub source: String,
    pub key: String,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub auto: bool,
}

impl PartEq {
    /// The parametric EQ bands this part EQ adds, in order (low shelf, high
    /// shelf, bells); bands at 0 dB are left out, so an all-zero part EQ
    /// adds nothing.
    pub fn bands(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (kind, sh) in [
            ("low_shelf", self.low_shelf),
            ("high_shelf", self.high_shelf),
        ] {
            if let Some(sh) = sh.filter(|x| x.db.abs() > 1e-3) {
                out.push(
                    json!({"kind": kind, "freq": sh.hz, "gain_db": sh.db, "q": PART_EQ_SHELF_Q}),
                );
            }
        }
        for b in self.bells.iter().filter(|b| b.db.abs() > 1e-3) {
            if b.auto {
                out.push(
                    json!({"kind": "bell", "freq": b.hz, "gain_db": b.db, "q": b.q, "auto": true}),
                );
            } else {
                out.push(json!({"kind": "bell", "freq": b.hz, "gain_db": b.db, "q": b.q}));
            }
        }
        out
    }

    /// Problems with this part EQ, each with the fix.
    fn problems(&self, base: &str, out: &mut Vec<Problem>) {
        for (i, d) in self.ducks.iter().enumerate().filter(|(_, d)| !d.auto) {
            out.push(p(format!("{base}/ducks/{i}"), format!("a duck at {} Hz", d.hz), "no duck here: ducks are placed by the build (set /mix/vocal_duck or /mix/kick_duck to true)"));
        }
        let hz_ok = |hz: f32| hz.is_finite() && (20.0..=20000.0).contains(&hz);
        let db_ok = |db: f32| db.is_finite() && (-12.0..=12.0).contains(&db);
        for (k, sh) in [
            ("low_shelf", self.low_shelf),
            ("high_shelf", self.high_shelf),
        ] {
            if let Some(sh) = sh {
                if !hz_ok(sh.hz) {
                    out.push(p(
                        format!("{base}/{k}/hz"),
                        format!("{} Hz", sh.hz),
                        "a corner in 20..20000 Hz",
                    ));
                }
                if !db_ok(sh.db) {
                    out.push(p(
                        format!("{base}/{k}/db"),
                        format!("{} dB", sh.db),
                        "a gain in -12..12 dB",
                    ));
                }
            }
        }
        if self.bells.len() > PART_EQ_MAX_BELLS {
            out.push(p(
                format!("{base}/bells"),
                format!("{} bells", self.bells.len()),
                format!("at most {PART_EQ_MAX_BELLS} bells per part"),
            ));
        }
        for (i, b) in self.bells.iter().enumerate() {
            if !hz_ok(b.hz) {
                out.push(p(
                    format!("{base}/bells/{i}/hz"),
                    format!("{} Hz", b.hz),
                    "a centre in 20..20000 Hz",
                ));
            }
            if !db_ok(b.db) {
                out.push(p(
                    format!("{base}/bells/{i}/db"),
                    format!("{} dB", b.db),
                    "a gain in -12..12 dB",
                ));
            }
            if !(b.q.is_finite() && (0.1..=10.0).contains(&b.q)) {
                out.push(p(
                    format!("{base}/bells/{i}/q"),
                    format!("Q {}", b.q),
                    "a Q in 0.1..10",
                ));
            }
        }
    }
}

impl Default for MixSpec {
    fn default() -> Self {
        let t = Targets::default();
        MixSpec {
            vocal_over_beat_db: t.vocal_over_beat_db,
            vocal_over_beat_max_db: t.vocal_over_beat_max_db,
            drop_lift_db: t.drop_lift_db[0],
            balance_passes: 3,
            levels: BTreeMap::new(),
            air_floor_db: None,
            presence_ceiling_db: None,
            master_lift: None,
            builds: false,
            section_gain_db: BTreeMap::new(),
            part_eq: BTreeMap::new(),
            auto_eq: false,
            vocal_duck: false,
            kick_duck: false,
            vocal_resonance: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MasterSpec {
    pub lufs: f32,
    pub true_peak: f32,
    pub preserve_dynamics: f32,
}

impl Default for MasterSpec {
    fn default() -> Self {
        let t = Targets::default();
        MasterSpec {
            lufs: t.lufs,
            true_peak: t.true_peak_max_db,
            preserve_dynamics: mix_rules::SONG_PRESERVE_DYNAMICS,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ExportSpec {
    /// Output file; default renders/<title>.<format>.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// mp3 | wav | flac.
    pub format: String,
    pub bitrate_kbps: u32,
}

impl Default for ExportSpec {
    fn default() -> Self {
        ExportSpec {
            path: None,
            format: "mp3".into(),
            bitrate_kbps: 320,
        }
    }
}

impl SongSpec {
    pub fn targets(&self) -> Targets {
        let d = Targets::default();
        Targets {
            vocal_over_beat_db: self.mix.vocal_over_beat_db,
            vocal_over_beat_max_db: self.mix.vocal_over_beat_max_db,
            drop_lift_db: [
                self.mix.drop_lift_db,
                self.mix.drop_lift_db.max(d.drop_lift_db[1]),
            ],
            lufs: self.master.lufs,
            lufs_tolerance: d.lufs_tolerance,
            true_peak_max_db: self.master.true_peak,
            air_rel_lufs_db: [
                self.mix.air_floor_db.unwrap_or(d.air_rel_lufs_db[0]),
                d.air_rel_lufs_db[1],
            ],
            presence_rel_lufs_db: match self.mix.presence_ceiling_db {
                Some(hi) => [d.presence_rel_lufs_db[0].min(hi - 1.5), hi],
                None => d.presence_rel_lufs_db,
            },
            ..d
        }
    }

    /// Section names in order (given names, else kind + count).
    pub fn section_names(&self) -> Vec<String> {
        let mut count: BTreeMap<String, u32> = BTreeMap::new();
        self.structure
            .iter()
            .map(|s| {
                let k = canon_kind(&s.kind);
                let n = count.entry(k.clone()).or_insert(0);
                *n += 1;
                s.name.clone().unwrap_or_else(|| format!("{k}{n}"))
            })
            .collect()
    }
}

/// The song as the project keeps it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SongState {
    /// The spec as last given/patched (what song_get returns).
    pub spec: Value,
    /// The seed the beat was composed with (patches reuse it).
    pub seed: u64,
    pub sections: Vec<mix_rules::Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(default)]
    pub checks: Value,
}

fn canon_kind(k: &str) -> String {
    match k.trim().to_lowercase().as_str() {
        "chorus" | "drop" => "hook".into(),
        "prechorus" | "pre_chorus" | "pre-chorus" => "pre".into(),
        x => x.into(),
    }
}

// ----------------------------------------------------------------- validation

#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    pub path: String,
    pub problem: String,
    pub fix: String,
}

fn p(path: impl Into<String>, problem: impl Into<String>, fix: impl Into<String>) -> Problem {
    Problem {
        path: path.into(),
        problem: problem.into(),
        fix: fix.into(),
    }
}

fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + (ca != *cb) as usize)
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest known word, if any is close.
fn closest<'a>(w: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let w = w.to_lowercase();
    known
        .into_iter()
        .map(|k| (lev(&w, k), k))
        .filter(|(d, k)| *d <= (k.len() / 3).max(2) || k.contains(w.as_str()) || w.contains(*k))
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn did_you_mean(w: &str, known: &[&str], alias: &[(&str, &str)]) -> String {
    if let Some((_, to)) = alias.iter().find(|(a, _)| a.eq_ignore_ascii_case(w)) {
        return format!("use '{to}'");
    }
    match closest(w, known.iter().copied()) {
        Some(k) => format!("did you mean '{k}'? (known: {})", known.join(", ")),
        None => format!("remove it (known: {})", known.join(", ")),
    }
}

const TOP_KEYS: &[&str] = &[
    "title",
    "prompt",
    "genre",
    "bpm",
    "key",
    "scale",
    "mood",
    "density",
    "seed",
    "structure",
    "instruments",
    "vocal",
    "mix",
    "master",
    "export",
];
const TOP_ALIAS: &[(&str, &str)] = &[
    ("tempo", "bpm"),
    ("sections", "structure"),
    ("form", "structure"),
    ("arrangement", "structure"),
    ("vocals", "vocal"),
    ("voice", "vocal"),
    ("instrumentation", "instruments"),
    ("palette", "instruments"),
    ("style", "genre"),
    ("lufs", "master.lufs"),
    ("name", "title"),
    ("brief", "prompt"),
];
const VOCAL_KEYS: &[&str] = &[
    "file",
    "track",
    "phrases",
    "start_section",
    "lyrics",
    "gain_db",
    "chain",
    "lock",
    "flex",
];
const PHRASE_KEYS: &[&str] = &[
    "src_s",
    "src",
    "len_s",
    "len",
    "end_s",
    "section",
    "bar",
    "beat",
    "start_beat",
    "at_s",
    "start_s",
    "gain_db",
    "name",
    "fade_in_ms",
    "fade_out_ms",
];
const SECTION_KEYS: &[&str] = &["kind", "bars", "name", "energy"];
const MIX_KEYS: &[&str] = &[
    "vocal_over_beat_db",
    "vocal_over_beat_max_db",
    "drop_lift_db",
    "balance_passes",
    "levels",
    "air_floor_db",
    "presence_ceiling_db",
    "master_lift",
    "builds",
    "section_gain_db",
    "part_eq",
    "auto_eq",
    "vocal_duck",
    "kick_duck",
    "vocal_resonance",
];
const MASTER_KEYS: &[&str] = &["lufs", "true_peak", "preserve_dynamics"];
const EXPORT_KEYS: &[&str] = &["path", "format", "bitrate_kbps"];
const CHAIN_KEYS: &[&str] = &[
    "hpf_hz",
    "deess_freq_hz",
    "deess_range_db",
    "comp_ratio",
    "comp_threshold_db",
    "comp_makeup_db",
    "reverb_send_db",
    "delay_send_db",
    "brighten",
    "presence_db",
    "presence_hz",
];

fn unknown_keys(
    v: &Value,
    path: &str,
    known: &[&str],
    alias: &[(&str, &str)],
    out: &mut Vec<Problem>,
) {
    if let Some(m) = v.as_object() {
        for k in m.keys() {
            if !known.contains(&k.as_str()) {
                out.push(p(
                    format!("{path}/{k}"),
                    format!("'{k}' is not a field here"),
                    did_you_mean(k, known, alias),
                ));
            }
        }
    }
}

/// Split "D minor" / "Dm" / "F#" into (root, scale).
pub fn split_key(k: &str) -> (String, Option<String>) {
    let k = k.trim();
    if let Some((r, s)) = k.split_once(char::is_whitespace) {
        return (
            r.trim().to_string(),
            Some(s.trim().to_lowercase().replace(' ', "_")),
        );
    }
    if k.len() >= 2 && k.ends_with('m') && !k.ends_with("dim") {
        return (k[..k.len() - 1].to_string(), Some("minor".into()));
    }
    (k.to_string(), None)
}

/// Every problem in a spec, with paths and fixes; the parsed spec when the
/// shape is right (semantic problems are still listed).
pub fn validate(v: &Value) -> (Option<SongSpec>, Vec<Problem>) {
    let mut out = Vec::new();
    if !v.is_object() {
        out.push(p("", "the song spec must be a JSON object", "send {\"genre\": \"desi_hiphop\", \"bpm\": 100, \"structure\": [...], ...}; song_schema shows every field"));
        return (None, out);
    }
    unknown_keys(v, "", TOP_KEYS, TOP_ALIAS, &mut out);
    for (i, s) in v["structure"].as_array().into_iter().flatten().enumerate() {
        unknown_keys(
            s,
            &format!("/structure/{i}"),
            SECTION_KEYS,
            &[("type", "kind"), ("length", "bars"), ("label", "name")],
            &mut out,
        );
    }
    unknown_keys(
        &v["vocal"],
        "/vocal",
        VOCAL_KEYS,
        &[
            ("path", "file"),
            ("take", "file"),
            ("phrase_map", "phrases"),
            ("gain", "gain_db"),
        ],
        &mut out,
    );
    for (i, s) in v["vocal"]["phrases"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(m) = s.as_object() {
            // phrase maps from other tools carry their own notes (err, words...): kept, ignored
            for k in m.keys() {
                if !PHRASE_KEYS.contains(&k.as_str())
                    && closest(k, PHRASE_KEYS.iter().copied())
                        .map(|c| lev(k, c) <= 2 && k.len() > 3)
                        .unwrap_or(false)
                {
                    out.push(p(
                        format!("/vocal/phrases/{i}/{k}"),
                        format!("'{k}' looks like a typo"),
                        did_you_mean(k, PHRASE_KEYS, &[]),
                    ));
                }
            }
        }
    }
    unknown_keys(
        &v["vocal"]["chain"],
        "/vocal/chain",
        CHAIN_KEYS,
        &[],
        &mut out,
    );
    unknown_keys(
        &v["mix"],
        "/mix",
        MIX_KEYS,
        &[("vocal_db", "vocal_over_beat_db")],
        &mut out,
    );
    unknown_keys(
        &v["master"],
        "/master",
        MASTER_KEYS,
        &[
            ("tp", "true_peak"),
            ("ceiling", "true_peak"),
            ("loudness", "lufs"),
        ],
        &mut out,
    );
    unknown_keys(
        &v["export"],
        "/export",
        EXPORT_KEYS,
        &[
            ("file", "path"),
            ("out", "path"),
            ("bitrate", "bitrate_kbps"),
        ],
        &mut out,
    );
    if !out.is_empty() {
        return (None, out);
    }
    let spec: SongSpec = match serde_json::from_value(v.clone()) {
        Ok(s) => s,
        Err(e) => {
            out.push(p("", format!("wrong type: {e}"), "numbers as numbers (\"bpm\": 100), lists as lists; song_schema shows each field's type"));
            return (None, out);
        }
    };
    // genre
    if let Some(g) = &spec.genre {
        if crate::producer::playbook(g).is_err() {
            let names: Vec<&str> = crate::producer::playbooks()
                .iter()
                .map(|b| b.name.as_str())
                .collect();
            out.push(p(
                "/genre",
                format!("'{g}' is not a genre this engine knows"),
                did_you_mean(
                    g,
                    &names,
                    &[
                        ("desi", "desi_hiphop"),
                        ("punjabi", "punjabi_party"),
                        ("bhangra", "punjabi_party"),
                        ("honey", "punjabi_party"),
                        ("party", "punjabi_party"),
                        ("hiphop", "boom_bap"),
                        ("hip_hop", "boom_bap"),
                    ],
                ),
            ));
        }
    }
    if spec.genre.is_none() && spec.prompt.trim().is_empty() {
        out.push(p("/genre", "no genre and no prompt", "give \"genre\" (e.g. punjabi_party, desi_hiphop, trap, boom_bap, lofi, drill, rnb, afrobeats) or a \"prompt\""));
    }
    if let Some(b) = spec.bpm {
        if !(50.0..=200.0).contains(&b) {
            out.push(p(
                "/bpm",
                format!("{b} is outside 50..200"),
                "use the song's tempo, e.g. 100 for desi hip-hop or 140 for trap",
            ));
        }
    }
    let mut scale = spec.scale.clone();
    if let Some(k) = &spec.key {
        let (root, sc) = split_key(k);
        if crate::theory::pitch_class(&root).is_err() {
            out.push(p(
                "/key",
                format!("'{k}' has no readable root note"),
                "use a note name: \"D\", \"F#\", \"Bb\", or \"D minor\"",
            ));
        }
        if scale.is_none() {
            scale = sc;
        }
    }
    if let Some(s) = &scale {
        if crate::theory::scale_intervals(s).is_err() {
            let names: Vec<&str> = crate::theory::SCALES.iter().map(|x| x.0).collect();
            out.push(p(
                if spec.scale.is_some() {
                    "/scale"
                } else {
                    "/key"
                },
                format!("'{s}' is not a scale"),
                did_you_mean(s, &names, &[("min", "minor"), ("maj", "major")]),
            ));
        }
    }
    if let Some(d) = &spec.density {
        if !["sparse", "balanced", "dense"].contains(&d.as_str()) {
            out.push(p(
                "/density",
                format!("'{d}' is not a density"),
                "use sparse, balanced or dense",
            ));
        }
    }
    // structure
    let names = spec.section_names();
    for (i, s) in spec.structure.iter().enumerate() {
        if !SECTION_KINDS.contains(&s.kind.trim().to_lowercase().as_str()) {
            out.push(p(
                format!("/structure/{i}/kind"),
                format!("'{}' is not a section kind", s.kind),
                did_you_mean(
                    &s.kind,
                    SECTION_KINDS,
                    &[
                        ("refrain", "hook"),
                        ("break", "breakdown"),
                        ("end", "outro"),
                        ("start", "intro"),
                    ],
                ),
            ));
        }
        if s.bars == 0 || s.bars > 64 {
            out.push(p(
                format!("/structure/{i}/bars"),
                format!("{} bars", s.bars),
                "use 1..64 (4, 8 or 16 are usual)",
            ));
        }
        if names.iter().filter(|n| **n == names[i]).count() > 1 {
            out.push(p(
                format!("/structure/{i}/name"),
                format!("the name '{}' is used twice", names[i]),
                "give each section its own name, or leave names out",
            ));
        }
    }
    if !spec.structure.is_empty() && spec.structure.len() < 2 {
        out.push(p("/structure", "a song needs at least two sections", "e.g. [{\"kind\":\"intro\",\"bars\":4},{\"kind\":\"verse\",\"bars\":16},{\"kind\":\"hook\",\"bars\":8}]"));
    }
    // instruments
    for (role, ins) in &spec.instruments {
        if !crate::creative::ROLES.contains(&role.as_str()) {
            out.push(p(
                format!("/instruments/{role}"),
                format!("'{role}' is not a role"),
                did_you_mean(
                    role,
                    crate::creative::ROLES,
                    &[
                        ("melody", "lead"),
                        ("chords", "harmony"),
                        ("keys", "harmony"),
                        ("pad", "texture"),
                        ("808", "bass"),
                        ("hats", "hat"),
                        ("drums", "kick"),
                        ("dhol", "bayan"),
                        ("dagga", "bayan"),
                        ("tilli", "tabla"),
                    ],
                ),
            ));
            continue;
        }
        match ins {
            InstrumentSpec::Name(n) => {
                if let Some(id) = n.strip_prefix("catalog:") {
                    if crate::sound_catalog::entry(id).is_none() {
                        out.push(p(format!("/instruments/{role}"), format!("no recorded instrument '{id}' in the catalog"), "search_instruments (instrument category) lists the ids, e.g. catalog:euterpea_harmonium"));
                    }
                } else if crate::instruments::preset(n).is_none() {
                    let names: Vec<&str> =
                        crate::instruments::PRESETS.iter().map(|x| x.0).collect();
                    out.push(p(
                        format!("/instruments/{role}"),
                        format!("'{n}' is not an instrument preset"),
                        did_you_mean(n, &names, &[]),
                    ));
                }
            }
            InstrumentSpec::Samples { samples, gain_db } => {
                if samples.is_empty() {
                    out.push(p(
                        format!("/instruments/{role}/samples"),
                        "no files",
                        "give one or more wav/mp3 one-shots (several = round robin)",
                    ));
                }
                for (i, f) in samples.iter().enumerate() {
                    if !Path::new(f).is_file() {
                        out.push(p(
                            format!("/instruments/{role}/samples/{i}"),
                            format!("'{f}' is not a file on this machine"),
                            "give the full path of a wav/mp3 one-shot",
                        ));
                    }
                }
                if !(-24.0..=24.0).contains(gain_db) {
                    out.push(p(
                        format!("/instruments/{role}/gain_db"),
                        format!("{gain_db} dB"),
                        "use -24..24",
                    ));
                }
            }
        }
    }
    // vocal
    if let Some(vs) = &spec.vocal {
        if !Path::new(&vs.file).is_file() {
            out.push(p(
                "/vocal/file",
                format!("'{}' is not a file on this machine", vs.file),
                "give the full path of a wav/mp3/flac take",
            ));
        }
        if vs.track.trim().is_empty() {
            out.push(p(
                "/vocal/track",
                "empty track name",
                "leave it out (default 'vocal')",
            ));
        }
        for (path, prob, fix) in vs.chain.problems("/vocal/chain") {
            out.push(p(path, prob, fix));
        }
        if !(-24.0..=24.0).contains(&vs.gain_db) {
            out.push(p(
                "/vocal/gain_db",
                format!("{} dB", vs.gain_db),
                "use -24..24; the balance loop already sets the level",
            ));
        }
        if let Some(ss) = &vs.start_section {
            if !spec.structure.is_empty() && !names.contains(ss) {
                out.push(p(
                    "/vocal/start_section",
                    format!("no section '{ss}'"),
                    format!("use one of: {}", names.join(", ")),
                ));
            }
        }
        for (i, ph) in vs.phrases.iter().enumerate() {
            let base = format!("/vocal/phrases/{i}");
            if ph.src_s < 0.0 {
                out.push(p(
                    format!("{base}/src_s"),
                    "negative start in the take",
                    "seconds from the start of the take, >= 0",
                ));
            }
            let len = ph.len_s.or(ph.end_s.map(|e| e - ph.src_s));
            match len {
                None => out.push(p(
                    base.clone(),
                    "no length",
                    "give len_s (seconds) or end_s (seconds in the take)",
                )),
                Some(l) if l <= 0.05 => out.push(p(
                    format!("{base}/len_s"),
                    format!("{l:.2} s is too short"),
                    "len_s > 0.05, or end_s after src_s",
                )),
                _ => {}
            }
            let placed = ph.section.is_some() || ph.start_beat.is_some() || ph.at_s.is_some();
            if !placed {
                out.push(p(
                    base.clone(),
                    "no position in the song",
                    "give section (+ bar, beat), or start_beat, or at_s",
                ));
            }
            if let Some(sec) = &ph.section {
                if spec.structure.is_empty() {
                    out.push(p(
                        format!("{base}/section"),
                        "phrases name sections but the spec has no structure",
                        "add \"structure\", or place phrases with start_beat / at_s",
                    ));
                } else if let Err(e) = resolve_section(&spec, &names, sec) {
                    out.push(p(
                        format!("{base}/section"),
                        e.to_string(),
                        format!("use one of: {}", names.join(", ")),
                    ));
                } else if let Ok(j) = resolve_section(&spec, &names, sec) {
                    let bars = spec.structure[j].bars as f32;
                    let bar = ph.bar.unwrap_or(1.0);
                    if bar < 0.0 || bar > bars {
                        out.push(p(
                            format!("{base}/bar"),
                            format!("bar {bar} but '{}' has {bars} bars", names[j]),
                            format!("use 1..{bars} (0 = the bar before, for a pickup)"),
                        ));
                    }
                    let beat = ph.beat.unwrap_or(1.0);
                    if !(0.0..5.0).contains(&beat) {
                        out.push(p(
                            format!("{base}/beat"),
                            format!("beat {beat}"),
                            "use 1..4.99 in 4/4 (1 = the downbeat, 4.5 = the and of 4)",
                        ));
                    }
                }
            }
        }
    }
    for (role, db) in &spec.mix.levels {
        if !crate::creative::ROLES.contains(&role.as_str()) {
            out.push(p(
                format!("/mix/levels/{role}"),
                format!("'{role}' is not a role"),
                did_you_mean(
                    role,
                    crate::creative::ROLES,
                    &[
                        ("melody", "lead"),
                        ("chords", "harmony"),
                        ("808", "bass"),
                        ("hats", "hat"),
                        ("dhol", "bayan"),
                    ],
                ),
            ));
        } else if !(-12.0..=12.0).contains(db) || !db.is_finite() {
            out.push(p(
                format!("/mix/levels/{role}"),
                format!("{db} dB"),
                "a fader offset in -12..12 dB",
            ));
        }
    }
    // which parts a key names (a track name or a role) is known only once
    // the beat is composed: `apply_part_eq` checks it there
    for (part, pe) in &spec.mix.part_eq {
        if part.trim().is_empty() {
            out.push(p(
                "/mix/part_eq",
                "an empty part name",
                "a track name (e.g. chords) or a role (e.g. harmony), or vocal",
            ));
        } else {
            pe.problems(&format!("/mix/part_eq/{part}"), &mut out);
        }
    }
    for (sec, db) in &spec.mix.section_gain_db {
        if sec.trim().is_empty() {
            out.push(p(
                "/mix/section_gain_db",
                "an empty section name",
                "a section kind (verse, hook, ...) or name (hook2)",
            ));
        } else if !(-6.0..=6.0).contains(db) || !db.is_finite() {
            out.push(p(
                format!("/mix/section_gain_db/{sec}"),
                format!("{db} dB"),
                "a section gain in -6..6 dB",
            ));
        }
    }
    if let Some(f) = spec.mix.air_floor_db {
        let ceil = Targets::default().air_rel_lufs_db[1];
        if !(-32.0..=ceil - 0.4).contains(&f) {
            out.push(p(
                "/mix/air_floor_db",
                format!("{f} dB"),
                format!(
                    "a 5-10 kHz floor in -32..{} dB (minus LUFS; the ceiling is {ceil})",
                    ceil - 0.4
                ),
            ));
        }
    }
    if let Some(ml) = &spec.mix.master_lift {
        for (k, why) in ml.problems() {
            out.push(p(format!("/mix/master_lift/{k}"), why, "the master lift: presence_hz 500..5000, presence_db/air_db -6..6, presence_q 0.3..3, air_hz 4000..16000, extra_hz 500..8000, extra_db -3..3, extra_q 0.3..4"));
        }
    }
    if !(1..=6).contains(&spec.mix.balance_passes) {
        out.push(p(
            "/mix/balance_passes",
            format!("{}", spec.mix.balance_passes),
            "use 1..6",
        ));
    }
    if !(0.0..=20.0).contains(&spec.mix.vocal_over_beat_db)
        || spec.mix.vocal_over_beat_max_db <= spec.mix.vocal_over_beat_db + 1.0
    {
        out.push(p("/mix/vocal_over_beat_db", format!("{}..{} dB", spec.mix.vocal_over_beat_db, spec.mix.vocal_over_beat_max_db), "a floor in 0..20 and a max at least 2 dB above it (default 11..17: a user vocal sits on top)"));
    }
    if !(-20.0..=-6.0).contains(&spec.master.lufs) {
        out.push(p(
            "/master/lufs",
            format!("{} LUFS", spec.master.lufs),
            "use -20..-6 (-14 streaming-safe, -10 loud rap, -8 very loud)",
        ));
    }
    if !(-6.0..=-0.1).contains(&spec.master.true_peak) {
        out.push(p(
            "/master/true_peak",
            format!("{} dBTP", spec.master.true_peak),
            "use -6..-0.1 (-1 is the usual delivery ceiling)",
        ));
    }
    if !(0.0..=1.0).contains(&spec.master.preserve_dynamics) {
        out.push(p(
            "/master/preserve_dynamics",
            format!("{}", spec.master.preserve_dynamics),
            "use 0..1 (0.8 default)",
        ));
    }
    if !["mp3", "wav", "flac"].contains(&spec.export.format.as_str()) {
        out.push(p(
            "/export/format",
            format!("'{}'", spec.export.format),
            "use mp3, wav or flac",
        ));
    }
    if let Some(path) = &spec.export.path {
        let parent = Path::new(path)
            .parent()
            .filter(|x| !x.as_os_str().is_empty());
        if let Some(d) = parent {
            if !d.is_dir() {
                out.push(p(
                    "/export/path",
                    format!("the folder {} does not exist", d.display()),
                    "use an existing folder, or leave path out",
                ));
            }
        }
    }
    (Some(spec), out)
}

/// A section by name ("hook2"), or by kind when it occurs once ("hook").
fn resolve_section(spec: &SongSpec, names: &[String], key: &str) -> Result<usize> {
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(key)) {
        return Ok(i);
    }
    let k = canon_kind(key);
    let idx: Vec<usize> = (0..spec.structure.len())
        .filter(|&i| canon_kind(&spec.structure[i].kind) == k)
        .collect();
    match idx.len() {
        1 => Ok(idx[0]),
        0 => bail!("no section '{key}'"),
        _ => bail!("'{key}' occurs {} times; name which one", idx.len()),
    }
}

fn problems_err(probs: &[Problem]) -> anyhow::Error {
    let lines: Vec<String> = probs
        .iter()
        .map(|x| {
            format!(
                "{}: {} -> {}",
                if x.path.is_empty() { "/" } else { &x.path },
                x.problem,
                x.fix
            )
        })
        .collect();
    anyhow!(
        "the song spec has {} problem(s), nothing was changed:\n- {}",
        probs.len(),
        lines.join("\n- ")
    )
}

// ------------------------------------------------------------------- building

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Scope {
    /// re-mix and re-deliver only
    Mix,
    /// rebuild the vocal track, then mix
    Vocal,
    /// recompose the beat (same seed), then vocal and mix
    Full,
}

/// What a JSON path in the spec touches.
pub fn scope_of(path: &str) -> Scope {
    let p = path.trim_start_matches('/');
    let head = p.split('/').next().unwrap_or("");
    match head {
        // role levels are set when the beat is composed
        "mix" if p == "mix/levels" || p.starts_with("mix/levels/") || p == "mix/builds" => {
            Scope::Full
        }
        // part EQs go on the parts when they are built (the vocal's after its chain)
        "mix" if p == "mix/part_eq" || p.starts_with("mix/part_eq/") => Scope::Full,
        "mix" | "master" | "export" | "title" => Scope::Mix,
        "vocal" if p == "vocal/gain_db" => Scope::Mix,
        "vocal" => Scope::Vocal,
        // section energy is section automation, set at the mix
        "structure" if p.ends_with("/energy") => Scope::Mix,
        _ => Scope::Full,
    }
}

fn seed_for(spec: &SongSpec) -> u64 {
    spec.seed.unwrap_or_else(crate::creative::fresh_seed)
}

/// Beat follows the vocal: with a vocal and no "bpm", the tempo where the
/// take's phrases lock best, searched over 70-140 (vocal_flex::TEMPO_RANGE);
/// half/double-time ties fold toward the genre's range (96 without a genre).
fn vocal_tempo(spec: &SongSpec, log: &mut Vec<String>) -> Option<f32> {
    let vs = spec.vocal.as_ref()?;
    if vs.lock == Some(false) {
        return None;
    }
    let x = crate::samples::decode_file(Path::new(&vs.file)).ok()?;
    // search the wide range (a sung take's real pulse can sit anywhere in
    // it); the genre's own range only decides between half/double-time ties
    let genre = spec
        .genre
        .as_deref()
        .and_then(|g| crate::producer::playbook(g).ok())
        .map(|pb| (pb.bpm[0], pb.bpm[1]));
    let (lo, hi) = crate::vocal_flex::TEMPO_RANGE;
    let pref = genre.map(|(a, b)| (a * b).sqrt()).unwrap_or(96.0);
    let ph = crate::vocal_flex::phrases(&x, 0.3, None);
    let on = crate::vocal_flex::syllable_onsets(&x, 0.5, 0.09);
    let groups: Vec<Vec<f32>> = ph
        .iter()
        .map(|p| {
            on.iter()
                .filter(|&&t| t >= p.start_s && t < p.end_s)
                .map(|&t| t - p.start_s)
                .collect()
        })
        .collect();
    let (c, folded) =
        crate::vocal_flex::fold_tempo(crate::vocal_flex::lock_tempo(&groups, lo, hi, 0.25), pref);
    let (b, s) = *c.first()?;
    let b = (b * 4.0).round() / 4.0;
    let fold = folded
        .map(|f| format!("; {f} scored the same, folded to the {pref:.0} bpm side"))
        .unwrap_or_default();
    log.push(format!("tempo {b} bpm picked from the vocal (lock score {:.2} over {} phrases, searched {lo:.0}-{hi:.0}{fold}); give \"bpm\" to override", s, ph.len()));
    Some(b)
}

fn compose_beat(e: &mut Engine, spec: &SongSpec, seed: u64, log: &mut Vec<String>) -> Result<()> {
    let names = spec.section_names();
    let mut intent = Map::new();
    if !spec.structure.is_empty() {
        intent.insert(
            "form".into(),
            json!(spec
                .structure
                .iter()
                .map(|s| json!({"kind": canon_kind(&s.kind), "bars": s.bars}))
                .collect::<Vec<_>>()),
        );
    }
    let presets: BTreeMap<&String, &String> = spec
        .instruments
        .iter()
        .filter_map(|(r, i)| match i {
            InstrumentSpec::Name(n) if !n.starts_with("catalog:") => Some((r, n)),
            _ => None,
        })
        .collect();
    if !presets.is_empty() {
        intent.insert("palette".into(), json!(presets));
    }
    if let Some(d) = &spec.density {
        intent.insert("density".into(), json!(d));
    }
    if spec.vocal.is_some() && spec.density.is_none() {
        intent.insert("density".into(), json!("sparse"));
        intent.insert("hero".into(), json!("groove"));
    }
    if let Some(m) = &spec.mood {
        intent.insert("mood".into(), json!(m));
    }
    intent.insert("target_lufs".into(), json!(spec.master.lufs));
    let (root, kscale) = spec.key.as_deref().map(split_key).unzip();
    let brief = if spec.prompt.trim().is_empty() {
        format!(
            "{} song",
            spec.genre.clone().unwrap_or_default().replace('_', " ")
        )
    } else {
        spec.prompt.clone()
    };
    let args = crate::producer::PlanArgs {
        brief,
        genre: spec.genre.clone(),
        bpm: spec.bpm.or_else(|| vocal_tempo(spec, log)),
        key: root,
        scale: spec.scale.clone().or(kscale.flatten()),
        mood: spec.mood.clone(),
        duration_s: None,
        seed,
        use_samples: true,
        flip_sample: None,
        seed_source: "explicit".into(),
        method: None,
        reference: None,
        authored: Default::default(),
        intent: Value::Object(intent),
    };
    let mut plan = crate::producer::plan_track(&args)?;
    // the structure is a contract: the creative wildcards may not move it
    if !spec.structure.is_empty() {
        if plan.sections.len() != spec.structure.len() {
            // a wildcard inserted a section (a breakdown, a switch): the
            // structure asked for wins; keep the planned sections that match
            // it in order, drop the rest
            let mut kept = Vec::new();
            let mut j = 0;
            for s in plan.sections.drain(..) {
                if j < spec.structure.len() && s.kind == canon_kind(&spec.structure[j].kind) {
                    kept.push(s);
                    j += 1;
                } else {
                    log.push(format!(
                        "left out a planned extra '{}' section: the structure is the one asked for",
                        s.kind
                    ));
                }
            }
            if kept.len() != spec.structure.len() {
                bail!("the producer could not keep the asked structure ({} of {} sections matched); try another seed, and please report it", kept.len(), spec.structure.len());
            }
            plan.sections = kept;
        }
        for (i, s) in plan.sections.iter_mut().enumerate() {
            if s.bars != spec.structure[i].bars {
                log.push(format!(
                    "kept {} at {} bars (a wildcard wanted {})",
                    names[i], spec.structure[i].bars, s.bars
                ));
                s.bars = spec.structure[i].bars;
            }
            s.name = names[i].clone();
        }
    }
    if !spec.title.is_empty() {
        plan.title = spec.title.clone();
    }
    crate::producer::prepare_kit(e, &mut plan);
    crate::producer::compose(e, &plan)?;
    swap_instruments(e, spec, log)?;
    crate::producer::balance_beat(e, &plan)?;
    apply_role_levels(e, spec, log)?;
    apply_part_eq(e, spec, log)?;
    if spec.mix.builds {
        let b = apply_builds(e)?;
        log.push(format!("builds: {b}"));
    }
    // loudness and peaks belong to delivery: a limiter in the project would
    // flatten the drops before the mix rules see them
    e.project.master_effects.retain(|f| {
        let t = serde_json::to_value(f)
            .ok()
            .and_then(|v| v["type"].as_str().map(String::from))
            .unwrap_or_default();
        !["limiter", "gain", "maximizer", "soft_clipper", "clipper"].contains(&t.as_str())
    });
    log.push(format!(
        "beat: {} {} bpm {} {}, {} sections, seed {seed}",
        plan.genre,
        plan.bpm,
        plan.key,
        plan.scale,
        plan.sections.len()
    ));
    Ok(())
}

/// The spec's role level offsets (/mix/levels), after the genre balance.
/// Pre-delivery lift from the arrangement: verses short of the lift thin
/// their layers (into `spec`'s energy, which every delivery round re-applies),
/// down to -6 dB. Nothing is asked of the master's volume.
fn arrangement_lift(
    e: &mut Engine,
    spec: &mut SongSpec,
    voice: Option<&str>,
    t: &Targets,
) -> Result<Value> {
    let r1 = |x: f32| (x * 10.0).round() / 10.0;
    let m = mix_rules::measure_sections(e, voice)?;
    let Some(l) = mix_rules::lift_db(&m) else {
        return Ok(json!({"skipped": "no hook/drop or no verse in the structure"}));
    };
    // a bit over the floor: delivery limiting takes some back
    let want = t.drop_lift_db[0] + 1.0;
    if l >= want {
        return Ok(json!({"lift_db": r1(l), "changed": false}));
    }
    // layers only (vocal, kick and bass stay), so a dB of thinning lifts less
    let need = ((want - l) * 1.5).min(4.0);
    // a song whose form came from the producer plan has no spec structure:
    // its sections are the arrangement's (the energy lanes need them)
    let derived = spec.structure.is_empty();
    if derived {
        spec.structure = structure_from_arrangement(&e.project);
    }
    let from = if derived { "arrangement" } else { "spec" };
    if !spec
        .structure
        .iter()
        .any(|s| canon_kind(&s.kind) == "verse")
    {
        return Ok(
            json!({"lift_db": r1(l), "changed": false, "why": "no verse section to thin", "sections_from": from}),
        );
    }
    let mut moved = Vec::new();
    for s in spec
        .structure
        .iter_mut()
        .filter(|s| canon_kind(&s.kind) == "verse")
    {
        let cur = s.energy.unwrap_or(0.0);
        let next = (cur - need).max(-6.0);
        if next < cur - 0.05 {
            s.energy = Some(next);
            moved.push(r1(next));
        }
    }
    if moved.is_empty() {
        return Ok(
            json!({"lift_db": r1(l), "changed": false, "why": "the verses are as thin as allowed (-6 dB)", "sections_from": from}),
        );
    }
    let applied = apply_section_energy(e, spec, voice)?;
    let thinned = applied.as_array().map_or(0, |a| a.len());
    let m2 = mix_rules::measure_sections(e, voice)?;
    Ok(
        json!({"lift_db_before": r1(l), "verse_energy_db": moved, "sections_thinned": thinned, "sections_from": from, "lift_db": mix_rules::lift_db(&m2).map(r1), "changed": thinned > 0}),
    )
}

/// The song's form as the arrangement plays it (one section per entry,
/// named as the arrangement names it), for songs whose spec gave none.
fn structure_from_arrangement(p: &crate::project::Project) -> Vec<SectionSpec> {
    mix_rules::section_spans(p)
        .into_iter()
        .map(|sp| SectionSpec {
            kind: sp.kind.clone(),
            bars: ((sp.end_beat - sp.start_beat) / 4.0).round().max(1.0) as u32,
            name: Some(sp.name),
            energy: None,
        })
        .collect()
}

/// The vocal's share of `mix.section_gain_db`: added to its ride (the
/// per-section fader lane the balance wrote) as an offset, so the ride keeps
/// its shape and each listed section sits that many dB over or under it.
/// `state` holds (ride, ride + offset) across calls: a lane still equal to
/// what we wrote is not offset twice, and a lane the balance re-wrote from
/// ours has the offset taken out first.
fn apply_vocal_section_offset(
    e: &mut Engine,
    spec: &SongSpec,
    voice: Option<&str>,
    state: &mut Option<(
        Vec<crate::automation::AutoPoint>,
        Vec<crate::automation::AutoPoint>,
    )>,
) -> Result<Value> {
    use crate::automation::{AutoPoint, AutomationLane, Curve};
    let Some(v) = voice else { return Ok(json!([])) };
    if spec.mix.section_gain_db.is_empty() {
        return Ok(json!([]));
    }
    let ti = e.project.track_index(v)?;
    let spans = mix_rules::section_spans(&e.project);
    let gain_for = |name: &str, kind: &str| -> f32 {
        spec.mix
            .section_gain_db
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .or_else(|| {
                spec.mix
                    .section_gain_db
                    .iter()
                    .find(|(k, _)| canon_kind(k) == kind)
            })
            .map(|(_, g)| g.clamp(-6.0, 6.0))
            .unwrap_or(0.0)
    };
    // a ride point is stepped one beat before its section
    let off_at = |beat: f32| -> f32 {
        spans
            .iter()
            .rfind(|s| s.start_beat <= beat + 1.0 + 1e-3)
            .map(|s| gain_for(&s.name, &s.kind))
            .unwrap_or(0.0)
    };
    let cur = e
        .project
        .automation
        .iter()
        .find(|l| l.target == v && l.param == "volume")
        .map(|l| l.points.clone());
    let ride: Vec<AutoPoint> = match (cur, state.as_ref()) {
        (Some(c), Some((base, wrote))) if &c == wrote => base.clone(),
        (Some(c), Some(_)) => c
            .iter()
            .map(|p| AutoPoint {
                value: p.value - off_at(p.beat),
                ..p.clone()
            })
            .collect(),
        (Some(c), None) => c,
        // no ride (a flat vocal): the fader at every section start
        (None, _) => {
            let f = e.project.tracks[ti].volume_db;
            spans
                .iter()
                .enumerate()
                .map(|(i, s)| AutoPoint {
                    beat: if i == 0 {
                        0.0
                    } else {
                        (s.start_beat - 1.0).max(0.0)
                    },
                    value: f,
                    curve: Curve::Step,
                })
                .collect()
        }
    };
    let wrote: Vec<AutoPoint> = ride
        .iter()
        .map(|p| AutoPoint {
            value: (p.value + off_at(p.beat)).clamp(-60.0, 24.0),
            ..p.clone()
        })
        .collect();
    e.project
        .automation
        .retain(|l| !(l.target == v && l.param == "volume"));
    let mut lane = AutomationLane::new(v, "volume");
    lane.points = wrote.clone();
    e.project.automation.push(lane);
    *state = Some((ride, wrote));
    let done: Vec<Value> = spans
        .iter()
        .map(|s| (s, gain_for(&s.name, &s.kind)))
        .filter(|(_, g)| g.abs() >= 0.05)
        .map(|(s, g)| json!({"section": s.name, "offset_db": (g * 10.0).round() / 10.0}))
        .collect();
    Ok(json!({"track": v, "how": "offset on the vocal's ride lane", "sections": done}))
}

/// `mix.section_gain_db`: each listed section (by name, e.g. "hook2", or
/// kind, e.g. "hook") plays that many dB louder or softer on every bus
/// that feeds the master: stepped volume lanes on those buses, rebuilt from
/// scratch on each call (the section energy only rebuilds track lanes, so
/// these survive it). Tracks, the vocal's ride included, are untouched.
fn apply_section_gain(e: &mut Engine, spec: &SongSpec) -> Result<Value> {
    if spec.mix.section_gain_db.is_empty() {
        return Ok(json!([]));
    }
    let buses: Vec<String> = e
        .project
        .buses
        .iter()
        .filter(|b| {
            b.output
                .as_deref()
                .is_none_or(|o| o.is_empty() || o.eq_ignore_ascii_case("master"))
        })
        .map(|b| b.name.clone())
        .collect();
    if buses.is_empty() {
        return Ok(json!({"skipped": "no bus feeds the master"}));
    }
    e.project.automation.retain(|l| {
        !(l.param == "volume" && buses.iter().any(|b| b.eq_ignore_ascii_case(&l.target)))
    });
    let base: Vec<f32> = buses
        .iter()
        .map(|n| {
            e.project
                .buses
                .iter()
                .find(|b| &b.name == n)
                .map(|b| b.volume_db)
                .unwrap_or(0.0)
        })
        .collect();
    let gain_for = |name: &str, kind: &str| -> Option<f32> {
        spec.mix
            .section_gain_db
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .or_else(|| {
                spec.mix
                    .section_gain_db
                    .iter()
                    .find(|(k, _)| canon_kind(k) == kind)
            })
            .map(|(_, g)| g.clamp(-6.0, 6.0))
    };
    let mut done = Vec::new();
    for sp in mix_rules::section_spans(&e.project) {
        let Some(g) = gain_for(&sp.name, &sp.kind).filter(|g| g.abs() >= 0.05) else {
            continue;
        };
        for (b, v) in buses.iter().zip(base.iter()) {
            e.call_nested("set_section_mix", &json!({"section": sp.name, "track": b, "volume_db": v + g, "all_occurrences": false}), "song")?;
        }
        done.push(json!({"section": sp.name, "gain_db": (g * 10.0).round() / 10.0}));
    }
    Ok(json!({"buses": buses, "sections": done}))
}

/// Tracks the section energy leaves alone: the floor of the groove.
fn energy_floor_layer(name: &str) -> bool {
    let n = name.to_lowercase();
    ["kick", "bass", "808", "sub"].iter().any(|k| n.contains(k))
}

/// /structure/N/energy: the arrangement layers play `energy` dB softer or
/// louder in that section (stepped volume lanes built from each track's
/// current fader, so re-running it after fader moves is exact).
pub(crate) fn apply_section_energy(
    e: &mut Engine,
    spec: &SongSpec,
    voice: Option<&str>,
) -> Result<Value> {
    let want: Vec<(usize, f32)> = spec
        .structure
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.energy.map(|x| (i, x.clamp(-9.0, 6.0))))
        .filter(|(_, x)| x.abs() >= 0.05)
        .collect();
    if want.is_empty() {
        return Ok(json!([]));
    }
    let names = spec.section_names();
    let spans = mix_rules::section_spans(&e.project);
    let layers: Vec<(String, f32)> = e
        .project
        .tracks
        .iter()
        .filter(|t| Some(t.name.as_str()) != voice && !energy_floor_layer(&t.name))
        .map(|t| (t.name.clone(), t.volume_db))
        .collect();
    // rebuilt from scratch each time: no lane of ours left over
    e.project
        .automation
        .retain(|l| !(l.param == "volume" && layers.iter().any(|(n, _)| *n == l.target)));
    let mut done = Vec::new();
    for (i, en) in want {
        let sec = match spans.iter().find(|x| x.name == names[i]) {
            Some(x) => x.name.clone(),
            None if spans.len() == spec.structure.len() => spans[i].name.clone(),
            None => continue,
        };
        for (t, base) in &layers {
            e.call_nested("set_section_mix", &json!({"section": sec, "track": t, "volume_db": base + en, "all_occurrences": false}), "song")?;
        }
        done.push(json!({"section": sec, "energy_db": (en * 10.0).round() / 10.0, "layers": layers.len()}));
    }
    Ok(json!(done))
}

fn apply_role_levels(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<()> {
    for (role, db) in &spec.mix.levels {
        let track = crate::producer::track_name(role);
        let Ok(ti) = e.project.track_index(&track) else {
            log.push(format!(
                "{}; its level offset was not applied",
                no_part(e, role)
            ));
            continue;
        };
        e.project.tracks[ti].volume_db += db;
        log.push(format!("level: {role} {db:+.1} dB (/mix/levels)"));
    }
    Ok(())
}

/// The vocal track's name in this spec (default "vocal").
fn vocal_track_name(spec: &SongSpec) -> Option<&str> {
    spec.vocal.as_ref().map(|v| v.track.as_str())
}

/// Which beat track each /mix/part_eq key names: an exact track name
/// first, else a role (its track). Vocal keys ("vocal" and the vocal
/// track's name) are left to `place_vocal`. When a track name and a role
/// both land on one track, the track name's EQ is used and the role's is
/// dropped with a log line. A role this arrangement does not play is
/// skipped with a log line (as /mix/levels does); a key that is neither a
/// track nor a role is an error with a did-you-mean.
fn resolve_part_eq_keys(
    e: &Engine,
    spec: &SongSpec,
    log: &mut Vec<String>,
) -> Result<Vec<(String, String)>> {
    let vocal = vocal_track_name(spec);
    let mut by_track: Vec<(String, String, bool)> = Vec::new(); // (track, key, by name)
    for key in spec.mix.part_eq.keys() {
        if key == "vocal" || Some(key.as_str()) == vocal {
            continue;
        }
        let (track, by_name) = if e.project.track_index(key).is_ok()
            && e.project.tracks.iter().any(|t| t.name == *key)
        {
            (key.clone(), true)
        } else if crate::creative::ROLES.contains(&key.as_str()) {
            let t = crate::producer::track_name(key);
            if e.project.track_index(&t).is_err() {
                log.push(format!("{}; its part EQ was not applied", no_part(e, key)));
                continue;
            }
            (t, false)
        } else {
            let mut known: Vec<&str> = crate::creative::ROLES.to_vec();
            known.extend(e.project.tracks.iter().map(|t| t.name.as_str()));
            known.push(vocal.unwrap_or("vocal"));
            return Err(anyhow!(
                "/mix/part_eq/{key}: '{key}' is neither a track nor a role here ({}); tracks: {}",
                did_you_mean(
                    key,
                    &known,
                    &[
                        ("melody", "lead"),
                        ("808", "bass"),
                        ("hats", "hat"),
                        ("dhol", "bayan"),
                        ("vox", "vocal")
                    ]
                ),
                e.project
                    .tracks
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        match by_track.iter_mut().find(|(t, _, _)| *t == track) {
            Some(slot) if by_name && !slot.2 => {
                log.push(format!("part EQ: /mix/part_eq/{} (role) and /mix/part_eq/{key} (track) both name {track}; the track name's is used", slot.1));
                *slot = (track, key.clone(), true);
            }
            Some(slot) if slot.2 && !by_name => {
                log.push(format!("part EQ: /mix/part_eq/{key} (role) and /mix/part_eq/{} (track) both name {track}; the track name's is used", slot.1));
            }
            Some(slot) => {
                log.push(format!("part EQ: /mix/part_eq/{key} and /mix/part_eq/{} both name {track}; /mix/part_eq/{} is used", slot.1, slot.1));
            }
            None => by_track.push((track, key.clone(), by_name)),
        }
    }
    Ok(by_track.into_iter().map(|(t, k, _)| (t, k)).collect())
}

/// The spec's part EQs (/mix/part_eq) on the beat's parts: one parametric
/// EQ at the end of each named part's chain (the vocal's is added in
/// `place_vocal`). Keys resolve as `resolve_part_eq_keys` says.
fn apply_part_eq(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<()> {
    if spec.mix.part_eq.is_empty() {
        return Ok(());
    }
    for (track, key) in resolve_part_eq_keys(e, spec, log)? {
        add_part_eq(e, &track, &spec.mix.part_eq[&key], log)?;
    }
    Ok(())
}

/// The vocal's part EQ onto the vocal track, after its vocal chain (so
/// after /vocal/chain/presence_db's bell): /mix/part_eq/<vocal track name>
/// if given (the exact track name wins), else /mix/part_eq/vocal.
fn apply_vocal_part_eq(
    e: &mut Engine,
    spec: &SongSpec,
    track: &str,
    log: &mut Vec<String>,
) -> Result<Option<Value>> {
    match spec
        .mix
        .part_eq
        .get(track)
        .or_else(|| spec.mix.part_eq.get("vocal"))
    {
        Some(pe) => add_part_eq(e, track, pe, log),
        None => Ok(None),
    }
}

/// One part EQ onto `track` (nothing when every band is 0 dB).
fn add_part_eq(
    e: &mut Engine,
    track: &str,
    pe: &PartEq,
    log: &mut Vec<String>,
) -> Result<Option<Value>> {
    let bands = pe.bands();
    if bands.is_empty() {
        return Ok(None);
    }
    e.call_nested(
        "add_effect",
        &json!({"track": track, "type": "parametric_eq", "params": {"bands": bands}}),
        "song",
    )?;
    log.push(format!(
        "part EQ: {track} {} band(s) (/mix/part_eq)",
        bands.len()
    ));
    Ok(Some(json!({"track": track, "bands": bands})))
}

/// Auto bands (/mix/auto_eq) an earlier build left in the project come
/// out: each build measures again, and with auto_eq off none stay. A
/// parametric EQ that held only auto bands goes with them. A project
/// without auto bands is not touched. Returns how many came out.
fn strip_auto_bands(e: &mut Engine) -> usize {
    use crate::fx::Effect;
    let has = |f: &Effect| matches!(f, Effect::ParametricEq(p) if p.bands.iter().any(|b| b.auto));
    let mut n = 0;
    for t in e
        .project
        .tracks
        .iter_mut()
        .filter(|t| t.effects.iter().any(has))
    {
        t.effects.retain_mut(|f| match f {
            Effect::ParametricEq(p) if p.bands.iter().any(|b| b.auto) => {
                let all = p.bands.iter().all(|b| b.auto);
                let before = p.bands.len();
                p.bands.retain(|b| !b.auto);
                n += before - p.bands.len();
                !all
            }
            _ => true,
        });
    }
    n
}

/// Bells marked "auto" in a spec's /mix/part_eq (written by an earlier
/// build's auto-EQ) come out; part EQs left empty go too. Returns how many.
fn strip_auto_bells(v: &mut Value) -> usize {
    let Some(pe) = v
        .pointer_mut("/mix/part_eq")
        .and_then(|x| x.as_object_mut())
    else {
        return 0;
    };
    let mut n = 0;
    for entry in pe.values_mut() {
        for k in ["bells", "ducks"] {
            if let Some(bells) = entry.get_mut(k).and_then(|b| b.as_array_mut()) {
                let before = bells.len();
                bells.retain(|b| b.get("auto") != Some(&Value::Bool(true)));
                n += before - bells.len();
                if bells.is_empty() && before > 0 {
                    entry.as_object_mut().map(|o| o.remove(k));
                }
            }
        }
    }
    if n > 0 {
        pe.retain(|_, x| x.as_object().is_none_or(|o| !o.is_empty()));
        if pe.is_empty() {
            v.pointer_mut("/mix")
                .and_then(|m| m.as_object_mut())
                .map(|m| m.remove("part_eq"));
        }
    }
    n
}

/// The pairs the auto-EQ measures: (protected part, part that takes the
/// cut, band range). The vocal against every other track; the kick against
/// the bass, cutting the bass (see `KICK_BASS_WHY`).
fn auto_eq_pairs(e: &Engine, voice: Option<&str>) -> Vec<(String, String, (f32, f32))> {
    use crate::auto_eq as ae;
    let mut out = Vec::new();
    if let Some(v) = voice {
        for t in e.project.tracks.iter().filter(|t| t.name != v) {
            out.push((v.to_string(), t.name.clone(), ae::VOCAL_RANGE));
        }
    }
    let (k, b) = (
        crate::producer::track_name("kick"),
        crate::producer::track_name("bass"),
    );
    if e.project.track_index(&k).is_ok()
        && e.project.track_index(&b).is_ok()
        && Some(b.as_str()) != voice
    {
        out.push((k, b, ae::LOW_RANGE));
    }
    out
}

/// Why the bass, not the kick, takes the kick/bass cut.
const KICK_BASS_WHY: &str = "the bass is cut, the kick kept: the kick is the short transient that carries the hook's punch (drop_lift); the bass is sustained and already ducked by the kick, so a bell on the bass at the kick's band clears the kick without thinning it";

/// Each pair's collision on the stems as the project renders now, and
/// (with a vocal) the vocal's against the whole beat (every other track
/// summed): the aggregate.
fn auto_eq_measure(
    e: &mut Engine,
    pairs: &[(String, String, (f32, f32))],
    voice: Option<&str>,
) -> Result<(
    Vec<Option<crate::auto_eq::PairStat>>,
    Option<crate::auto_eq::PairStat>,
)> {
    use crate::auto_eq as ae;
    use rayon::prelude::*;
    let p = e.project.clone();
    let m = e.render_stems_all(&p)?;
    let want: std::collections::BTreeSet<&str> = pairs
        .iter()
        .flat_map(|(a, b, _)| [a.as_str(), b.as_str()])
        .collect();
    let frames: BTreeMap<String, Vec<Vec<f32>>> = m
        .stems
        .par_iter()
        .filter(|s| want.contains(s.name.as_str()) && !s.left.is_empty())
        .map(|s| {
            (
                s.name.clone(),
                ae::band_frames(&ae::mono(&s.left, &s.right)),
            )
        })
        .collect();
    let beat = voice.and_then(|v| {
        let parts: Vec<_> = m
            .stems
            .iter()
            .filter(|s| s.name != v && !s.left.is_empty())
            .collect();
        let n = parts.iter().map(|s| s.left.len()).max()?;
        let mut sum = vec![0f32; n];
        for s in parts {
            for (i, (l, r)) in s.left.iter().zip(&s.right).enumerate() {
                sum[i] += 0.5 * (l + r);
            }
        }
        Some(ae::band_frames(&sum))
    });
    drop(m);
    let stats = pairs
        .iter()
        .map(|(a, b, r)| match (frames.get(a), frames.get(b)) {
            (Some(fa), Some(fb)) => Some(ae::pair_stat(fa, fb, *r)),
            _ => None,
        })
        .collect();
    let agg = match (voice.and_then(|v| frames.get(v)), beat) {
        (Some(fv), Some(fb)) => Some(ae::pair_stat(fv, &fb, ae::VOCAL_RANGE)),
        _ => None,
    };
    Ok((stats, agg))
}

/// /mix/auto_eq: measure, cut, measure again (see `crate::auto_eq`). The
/// vocal is never cut; a user's part EQ bands are never moved (auto bells
/// join that EQ after them, in its free slots).
fn apply_auto_eq(
    e: &mut Engine,
    spec: &SongSpec,
    voice: Option<&str>,
    log: &mut Vec<String>,
) -> Result<Value> {
    use crate::auto_eq as ae;
    use crate::fx::{Effect, EqBand, EqBandKind};
    let pairs = auto_eq_pairs(e, voice);
    if pairs.is_empty() {
        log.push("auto EQ: nothing to measure (no vocal, and no kick and bass)".into());
        return Ok(json!({"enabled": true, "pairs": [], "cuts": []}));
    }
    let (before, agg0) = auto_eq_measure(e, &pairs, voice)?;
    // candidates per part that takes a cut (never the vocal)
    let mut cands: BTreeMap<String, Vec<ae::Cand>> = BTreeMap::new();
    for ((a, b, r), st) in pairs.iter().zip(&before) {
        if Some(b.as_str()) == voice {
            continue;
        }
        if let Some(st) = st {
            let (q, cut) = if *r == ae::LOW_RANGE {
                (ae::LOW_Q, ae::LOW_RANGE)
            } else {
                (ae::AUTO_Q, ae::VOCAL_CUT_RANGE)
            };
            cands.entry(b.clone()).or_default().extend(ae::candidates(
                st,
                &format!("{a}/{b}"),
                q,
                cut,
            ));
        }
    }
    let mut scratch = Vec::new();
    let user_keys: BTreeMap<String, String> = resolve_part_eq_keys(e, spec, &mut scratch)?
        .into_iter()
        .collect();
    let mut cuts = Vec::new();
    for (track, cs) in &cands {
        if cs.is_empty() || Some(track.as_str()) == voice {
            continue;
        }
        let key = user_keys.get(track).cloned();
        let user = key.as_ref().and_then(|k| spec.mix.part_eq.get(k));
        let (bells, skipped) = ae::plan_part(track, cs, user);
        log.extend(skipped);
        if bells.is_empty() {
            continue;
        }
        let bands: Vec<EqBand> = bells
            .iter()
            .map(|b| EqBand {
                kind: EqBandKind::Bell,
                freq: b.hz,
                gain_db: b.db,
                q: b.q,
                auto: true,
                ..Default::default()
            })
            .collect();
        // the user's own EQ on this part (as `add_part_eq` put it), if any
        let i = e.project.track_index(track)?;
        let mine = user
            .map(|u| u.bands())
            .filter(|b| !b.is_empty())
            .and_then(|want| {
                e.project.tracks[i].effects.iter().rposition(|f| match f {
                    Effect::ParametricEq(p) => {
                        p.bands.len() == want.len()
                            && p.bands.iter().zip(&want).all(|(b, w)| {
                                serde_json::to_value(b.kind).ok().as_ref() == w.get("kind")
                                    && Some(b.freq as f64) == w["freq"].as_f64()
                                    && Some(b.gain_db as f64) == w["gain_db"].as_f64()
                                    && Some(b.q as f64) == w["q"].as_f64()
                            })
                    }
                    _ => false,
                })
            });
        let merged = match mine {
            Some(j) => {
                if let Effect::ParametricEq(p) = &mut e.project.tracks[i].effects[j] {
                    p.bands.extend(bands);
                }
                true
            }
            None => {
                e.call_nested(
                    "add_effect",
                    &json!({"track": track, "type": "parametric_eq", "params": {"bands": bands}}),
                    "song",
                )?;
                false
            }
        };
        for b in &bells {
            let pair = cs
                .iter()
                .find(|c| c.hz.round() == b.hz)
                .map(|c| c.pair.clone())
                .unwrap_or_default();
            log.push(format!(
                "auto EQ: {track} {:.0} Hz {:+.1} dB Q{} ({pair}){}",
                b.hz,
                b.db,
                b.q,
                if merged {
                    ", in the user's part EQ after its bands"
                } else {
                    ""
                }
            ));
            let r1 = |x: f32| (x as f64 * 10.0).round() / 10.0;
            cuts.push(json!({"part": track, "key": key.clone().unwrap_or_else(|| track.clone()), "hz": r1(b.hz), "db": r1(b.db), "q": r1(b.q), "pair": pair, "merged_into_user_eq": merged}));
        }
    }
    let (after, agg1) = if cuts.is_empty() {
        (before.clone(), agg0.clone())
    } else {
        auto_eq_measure(e, &pairs, voice)?
    };
    let pct = |x: f32| (x * 1000.0).round() / 10.0;
    let mut rows = Vec::new();
    for (((a, b, r), st0), st1) in pairs.iter().zip(&before).zip(&after) {
        let (Some(st0), Some(st1)) = (st0, st1) else {
            continue;
        };
        let mut row = json!({"pair": format!("{a}/{b}"), "cut_part": b, "range_hz": [r.0, r.1], "overlap_before_pct": pct(st0.overlap), "overlap_after_pct": pct(st1.overlap), "frames_both_active": st0.frames});
        if a == &crate::producer::track_name("kick") {
            row["why"] = json!(KICK_BASS_WHY);
        }
        if st0.overlap > 0.0 && cuts.iter().any(|c| c["pair"] == row["pair"]) {
            log.push(format!(
                "auto EQ: {a}/{b} overlap {:.1}% -> {:.1}%",
                pct(st0.overlap),
                pct(st1.overlap)
            ));
        }
        rows.push(row);
    }
    log.push(format!(
        "auto EQ: {} cut(s) on {} part(s)",
        cuts.len(),
        cuts.iter()
            .map(|c| c["part"].as_str().unwrap_or(""))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    ));
    let mut out = json!({"enabled": true, "metric": "overlap = sum(min(La, Lb)) / sum(La): 1/3-octave band loudness (power^0.3) over frames where both parts play; a = the protected part", "min_pair_overlap_pct": pct(ae::MIN_PAIR_OVERLAP), "pairs": rows, "cuts": cuts});
    if let (Some(a0), Some(a1)) = (&agg0, &agg1) {
        log.push(format!(
            "auto EQ: vocal/beat (every other track summed) overlap {:.1}% -> {:.1}%",
            pct(a0.overlap),
            pct(a1.overlap)
        ));
        out["aggregate"] = json!({"pair": "vocal/beat", "what": "the vocal against every other track summed", "overlap_before_pct": pct(a0.overlap), "overlap_after_pct": pct(a1.overlap)});
    }
    Ok(out)
}

/// The ids the sidechain ducks carry in a track's chain (one of each per
/// track at most): how a rebuild finds and takes them out.
const AUTO_DUCK_VOCAL_ID: &str = "auto_duck_vocal";
const AUTO_DUCK_KICK_ID: &str = "auto_duck_kick";
/// The resonance tamer's bands on the vocal ("auto_resonance1".."4").
const AUTO_RESONANCE_ID: &str = crate::resonance::AUTO_RESONANCE_ID;

/// Ducks (/mix/vocal_duck, /mix/kick_duck) an earlier build left come out:
/// each build measures again, and off leaves none. Returns how many.
fn strip_auto_ducks(e: &mut Engine) -> usize {
    let is = |f: &crate::fx::Effect| {
        let id = f.id();
        id == AUTO_DUCK_VOCAL_ID || id == AUTO_DUCK_KICK_ID || id.starts_with(AUTO_RESONANCE_ID)
    };
    let mut n = 0;
    for t in e
        .project
        .tracks
        .iter_mut()
        .filter(|t| t.effects.iter().any(is))
    {
        let before = t.effects.len();
        t.effects.retain(|f| !is(f));
        n += before - t.effects.len();
    }
    n
}

/// How much of the vocal's active time a vocal duck holds its dip: the
/// share of 10 ms frames of the vocal stem within `ACTIVE_RANGE_DB` of its
/// loudest frame (the auto-EQ's activity rule) where the duck's envelope
/// (`fx_extra::level_duck_curve` over the vocal's clips, as the render keys
/// it) is past half its dip. None without a vocal stem.
fn vocal_duck_coverage(e: &mut Engine, voice: &str) -> Result<Option<f32>> {
    use crate::dsp::SR;
    let p = e.project.clone();
    let m = e.render_stems_all(&p)?;
    let Some(v) = m
        .stems
        .iter()
        .find(|s| s.name == voice && !s.left.is_empty())
    else {
        return Ok(None);
    };
    let x = crate::auto_eq::mono(&v.left, &v.right);
    let step = p.step_secs();
    let mut key = vec![0.0f32; x.len()];
    for c in p
        .audio_clips
        .iter()
        .filter(|c| c.track.eq_ignore_ascii_case(voice))
    {
        let Some(data) = e.bank.get(&c.sample) else {
            continue;
        };
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(data.len());
        let avail = data.len() - off;
        let len = c
            .length_s
            .map(|l| ((l.max(0.0) * SR) as usize).min(avail))
            .unwrap_or(avail);
        let start = (c.start_beat * 4.0 * step * SR).round() as i64;
        for (k, &s) in data[off..off + len].iter().enumerate() {
            if let Some(slot) = usize::try_from(start + k as i64)
                .ok()
                .and_then(|i| key.get_mut(i))
            {
                *slot += s;
            }
        }
    }
    let duck = crate::fx::DynamicEqFx {
        attack_ms: crate::auto_eq::VOCAL_DUCK_ATTACK_MS,
        release_ms: crate::auto_eq::VOCAL_DUCK_RELEASE_MS,
        ..Default::default()
    };
    let env = crate::fx_extra::level_duck_curve(&duck, &key, x.len());
    let hop = (0.01 * SR) as usize;
    let db: Vec<f32> = x
        .chunks(hop)
        .map(|s| {
            10.0 * (s.iter().map(|a| a * a).sum::<f32>() / s.len() as f32)
                .max(1e-12)
                .log10()
        })
        .collect();
    let top = db.iter().cloned().fold(f32::MIN, f32::max);
    let (mut active, mut held) = (0usize, 0usize);
    for (i, d) in db.iter().enumerate() {
        if *d >= top - crate::auto_eq::ACTIVE_RANGE_DB {
            active += 1;
            let w = &env[i * hop..((i + 1) * hop).min(env.len())];
            if w.iter().sum::<f32>() / w.len().max(1) as f32 > 0.5 {
                held += 1;
            }
        }
    }
    Ok((active > 0).then(|| held as f32 / active as f32))
}

/// /mix/vocal_resonance: the vocal's lasting resonances (see
/// `resonance::find`, on the vocal stem as its chain leaves it) each get a
/// self-keyed dynamic EQ band at the end of the vocal's chain: Q5, at most
/// -4 dB, ratio 3, attack 5 ms, release 80 ms, threshold at the band's
/// median active level + 2 dB, so it only dips when the ring pokes out.
/// Bands within a third of an octave of the de-esser or of a non-auto EQ
/// band on the vocal (the user's) are skipped.
fn apply_vocal_resonance(
    e: &mut Engine,
    voice: Option<&str>,
    log: &mut Vec<String>,
) -> Result<Value> {
    let Some(v) = voice else {
        log.push("vocal resonance: no vocal here".into());
        return Ok(json!({"bands": []}));
    };
    let p = e.project.clone();
    let m = e.render_stems_all(&p)?;
    let Some(st) = m.stems.iter().find(|s| s.name == v && !s.left.is_empty()) else {
        log.push("vocal resonance: the vocal stem is silent".into());
        return Ok(json!({"bands": []}));
    };
    let x = crate::auto_eq::mono(&st.left, &st.right);
    drop(m);
    let vi = e.project.track_index(v)?;
    let exclude = crate::resonance::exclusions(&e.project.tracks[vi].effects);
    Ok(crate::resonance::tame_track(
        &mut e.project.tracks[vi],
        &x,
        &exclude,
        log,
    ))
}

/// /mix/vocal_duck and /mix/kick_duck: keyed dynamic EQ bands (the
/// project's `dynamic_eq` with a `source`), placed from the auto-EQ's
/// measure and measured again after. A duck dips only while its source
/// plays (the vocal's level through an envelope follower, attack 12 ms,
/// release 100 ms; the kick's note starts), so instrument-only passages and
/// the gaps between kick hits keep their level.
fn apply_auto_duck(
    e: &mut Engine,
    spec: &SongSpec,
    voice: Option<&str>,
    log: &mut Vec<String>,
) -> Result<Value> {
    use crate::auto_eq as ae;
    use crate::fx::{DynamicEqFx, Effect};
    let kick = crate::producer::track_name("kick");
    let pairs: Vec<(String, String, (f32, f32))> = auto_eq_pairs(e, voice)
        .into_iter()
        .filter(|(a, _, r)| {
            if *r == ae::LOW_RANGE {
                spec.mix.kick_duck && *a == kick
            } else {
                spec.mix.vocal_duck && Some(a.as_str()) == voice
            }
        })
        .collect();
    if spec.mix.vocal_duck && voice.is_none() {
        log.push("auto duck: no vocal here, so no vocal duck".into());
    }
    if pairs.is_empty() {
        log.push("auto duck: nothing to duck".into());
        return Ok(json!({"pairs": [], "ducks": []}));
    }
    let (before, agg0) = auto_eq_measure(e, &pairs, voice.filter(|_| spec.mix.vocal_duck))?;
    let mut scratch = Vec::new();
    let user_keys: BTreeMap<String, String> = resolve_part_eq_keys(e, spec, &mut scratch)?
        .into_iter()
        .collect();
    // the kick's hits (for the kick duck's release): onsets in its stem
    let kick_release = if spec.mix.kick_duck && e.project.track_index(&kick).is_ok() {
        let p = e.project.clone();
        let m = e.render_stems_all(&p)?;
        m.stems
            .iter()
            .find(|s| s.name == kick && !s.left.is_empty())
            .map(|s| {
                ae::kick_release_ms(&crate::render::audio_onsets(&ae::mono(&s.left, &s.right)))
            })
            .unwrap_or(ae::DUCK_RELEASE_MS)
    } else {
        ae::DUCK_RELEASE_MS
    };
    let mut ducks = Vec::new();
    for ((a, b, r), st) in pairs.iter().zip(&before) {
        let Some(st) = st else { continue };
        if Some(b.as_str()) == voice {
            continue; // never the vocal
        }
        let low = *r == ae::LOW_RANGE;
        let (q, cut) = if low {
            (ae::LOW_Q, ae::LOW_RANGE)
        } else {
            (ae::AUTO_Q, ae::VOCAL_CUT_RANGE)
        };
        let pair = format!("{a}/{b}");
        let Some(c) = ae::duck_band(st, &pair, q, cut) else {
            continue;
        };
        let user = user_keys.get(b).and_then(|k| spec.mix.part_eq.get(k));
        if let Some(u) = ae::user_band_near(user, c.hz) {
            log.push(format!("auto duck: skipped {b} {:.0} Hz ({pair}): a user band sits at {u:.0} Hz; the user's band wins", c.hz));
            continue;
        }
        // the kick duck follows the kick's hits; the vocal duck follows the
        // vocal's level (an envelope follower), so it holds through a vowel
        let (key_mode, attack, release) = if low {
            ("hits", ae::DUCK_ATTACK_MS, kick_release)
        } else {
            ("level", ae::VOCAL_DUCK_ATTACK_MS, ae::VOCAL_DUCK_RELEASE_MS)
        };
        let mut fx = Effect::DynamicEq(DynamicEqFx {
            freq: c.hz,
            q,
            range_db: -ae::DUCK_DB,
            attack_ms: attack,
            release_ms: release,
            source: a.clone(),
            key: if low { String::new() } else { "level".into() },
            ..Default::default()
        });
        fx.set_id(if low {
            AUTO_DUCK_KICK_ID
        } else {
            AUTO_DUCK_VOCAL_ID
        });
        let i = e.project.track_index(b)?;
        e.project.tracks[i].effects.push(fx);
        log.push(format!("auto duck: {b} {:.0} Hz -{} dB Q{q} keyed by {a}'s {key_mode}, attack {attack} ms, release {release:.0} ms", c.hz, ae::DUCK_DB));
        let key = user_keys.get(b).cloned().unwrap_or_else(|| b.clone());
        ducks.push(json!({"part": b, "key": key, "source": a, "key_mode": key_mode, "pair": pair, "hz": c.hz.round(), "max_db": -ae::DUCK_DB, "q": q, "attack_ms": attack, "release_ms": (release * 10.0).round() / 10.0}));
    }
    let (after, agg1) = if ducks.is_empty() {
        (before.clone(), agg0.clone())
    } else {
        auto_eq_measure(e, &pairs, voice.filter(|_| spec.mix.vocal_duck))?
    };
    let pct = |x: f32| (x * 1000.0).round() / 10.0;
    let mut rows = Vec::new();
    for (((a, b, _), s0), s1) in pairs.iter().zip(&before).zip(&after) {
        if let (Some(s0), Some(s1)) = (s0, s1) {
            let row = json!({"pair": format!("{a}/{b}"), "overlap_before_pct": pct(s0.overlap), "overlap_after_pct": pct(s1.overlap)});
            if ducks.iter().any(|d| d["pair"] == row["pair"]) {
                log.push(format!(
                    "auto duck: {a}/{b} overlap {:.1}% -> {:.1}%",
                    pct(s0.overlap),
                    pct(s1.overlap)
                ));
            }
            rows.push(row);
        }
    }
    let mut out = json!({"pairs": rows, "ducks": ducks, "note": "a duck dips only while its source plays; the overlap is measured on the stems with the ducks working"});
    if let Some(v) = voice
        .filter(|v| spec.mix.vocal_duck && ducks.iter().any(|d| d["source"].as_str() == Some(*v)))
    {
        if let Some(c) = vocal_duck_coverage(e, v)? {
            log.push(format!(
                "auto duck: the vocal ducks hold over {:.1}% of the vocal's active time",
                c * 100.0
            ));
            out["vocal_active_time_covered_pct"] = json!((c as f64 * 1000.0).round() / 10.0);
        }
    }
    if let (Some(a0), Some(a1)) = (&agg0, &agg1) {
        out["aggregate"] = json!({"pair": "vocal/beat", "overlap_before_pct": pct(a0.overlap), "overlap_after_pct": pct(a1.overlap)});
    }
    Ok(out)
}

/// Recorded instruments for the roles that asked for them (after compose,
/// before the level balance, so the balance hears the real sound).
fn swap_instruments(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<()> {
    for (role, ins) in &spec.instruments {
        let track = crate::producer::track_name(role);
        let InstrumentSpec::Name(n) = ins else {
            if let InstrumentSpec::Samples { samples, gain_db } = ins {
                // a role the arrangement does not play (a perc kit on a beat
                // with no perc part) is skipped with a note, not a failed song
                let Ok(ti) = e.project.track_index(&track) else {
                    log.push(format!(
                        "{}; your samples for it were not used",
                        no_part(e, role)
                    ));
                    continue;
                };
                // the pitch the part plays: the hits sound at their own pitch there
                let mut count: BTreeMap<u8, usize> = BTreeMap::new();
                for pt in &e.project.patterns {
                    for nt in pt.clips.get(&track.to_lowercase()).into_iter().flatten() {
                        *count.entry(nt.pitch).or_default() += 1;
                    }
                }
                let root = count
                    .iter()
                    .max_by_key(|(_, c)| **c)
                    .map(|(p, _)| *p)
                    .unwrap_or(60);
                let mut zones = Vec::new();
                for (i, f) in samples.iter().enumerate() {
                    let name = format!("song_{role}_{i}");
                    e.project.samples.retain(|s| s.name != name);
                    e.call_nested("import_sample", &json!({"path": f, "name": name, "source": format!("song instrument for {role}")}), "song")?;
                    zones.push(json!({"sample": name, "root": root, "lo_note": 0, "hi_note": 127, "vel_lo": 0.0, "vel_hi": 1.0, "tune_cents": 0.0, "gain_db": gain_db, "rr": i}));
                }
                let inst = json!({"type": "multisample", "zones": zones, "attack": 0.002, "release": 0.35, "one_shot": true, "round_robin": samples.len() > 1, "vel_crossfade": true, "vel_to_gain": 0.6, "gain": 0.9, "source": ""});
                e.project.tracks[ti].instrument = serde_json::from_value(inst)
                    .map_err(|err| anyhow!("instrument for {role}: {err}"))?;
                e.bank.sync(&e.project.samples);
                log.push(format!(
                    "{role}: {} recorded one-shot(s) at the part's pitch",
                    samples.len()
                ));
            }
            continue;
        };
        let Some(id) = n.strip_prefix("catalog:") else {
            continue;
        };
        let Ok(ti) = e.project.track_index(&track) else {
            log.push(format!(
                "{}; instrument {id} was not loaded",
                no_part(e, role)
            ));
            continue;
        };
        let tmp = format!("__song_{role}");
        e.call_nested(
            "load_instrument",
            &json!({"track": tmp, "catalog_id": id}),
            "song",
        )?;
        let tj = e.project.track_index(&tmp)?;
        let inst = e.project.tracks[tj].instrument.clone();
        e.project.tracks.remove(tj);
        let ti = if tj < ti { ti - 1 } else { ti };
        e.project.tracks[ti].instrument = inst;
        e.bank.sync(&e.project.samples);
        log.push(format!("{role}: recorded instrument {id}"));
    }
    Ok(())
}

/// Beats before a hook the build starts closing the filter (two bars).
pub const BUILD_BARS_BEATS: f32 = 8.0;
/// Where the build's filter starts (Hz).
pub const BUILD_FROM_HZ: f32 = 500.0;
/// The reverb throw's lift over the send's level (dB, capped at -2 dB).
pub const BUILD_THROW_DB: f32 = 12.0;

/// `mix.builds`: deepen the build into every hook that follows a non-hook
/// section of at least 6 beats (a one-bar riser when it is under 12). Rewrites the producer's hook-lift cutoff
/// lanes on chords/counter/texture over the two bars before the hook,
/// adds a gain stage on the drum bus with a one-beat pre-drop gap, and a
/// `send.<reverb bus>` throw on lead and chords over that beat.
fn apply_builds(e: &mut Engine) -> Result<Value> {
    use crate::automation::{AutoPoint, Curve};
    let spans = mix_rules::section_spans(&e.project);
    // (hook start, riser length): two bars out when the section before is
    // at least 12 beats, one bar when it is 6..12 (a short intro), none below
    let built: Vec<(f32, f32)> = spans
        .windows(2)
        .filter(|w| w[1].kind == "hook" && w[0].kind != "hook")
        .filter_map(|w| {
            let len = w[0].end_beat - w[0].start_beat;
            let riser = if len >= 12.0 {
                BUILD_BARS_BEATS
            } else if len >= 6.0 {
                4.0
            } else {
                return None;
            };
            Some((w[1].start_beat, riser))
        })
        .collect();
    let hooks: Vec<f32> = built.iter().map(|b| b.0).collect();
    if hooks.is_empty() {
        return Ok(json!({"hooks": 0}));
    }
    let pt = |beat: f32, value: f32, curve: Curve| AutoPoint { beat, value, curve };
    // 1) risers: the existing cutoff lanes, rewritten over the two bars out
    let mut risers = Vec::new();
    for t in ["chords", "counter", "texture"] {
        let Ok(ti) = e.project.track_index(t) else {
            continue;
        };
        let filt: Vec<usize> = e.project.tracks[ti]
            .effects
            .iter()
            .enumerate()
            .filter(|(_, f)| f.type_name() == "filter")
            .map(|(i, _)| i)
            .collect();
        let Some(lane) = e.project.automation.iter_mut().find(|l| {
            l.target.eq_ignore_ascii_case(t)
                && filt.iter().any(|i| l.param == format!("fx.{i}.cutoff"))
        }) else {
            continue;
        };
        for &(s, riser) in &built {
            let a = s - riser;
            let before = lane.value_at(a - 1.0).unwrap_or(18000.0);
            lane.points.retain(|p| p.beat < a - 1.0 || p.beat >= s);
            lane.points.push(pt(a - 1.0, before, Curve::Linear));
            lane.points.push(pt(a, BUILD_FROM_HZ, Curve::Smooth));
            lane.points.push(pt(s - 0.01, 16000.0, Curve::Step));
            if !lane.points.iter().any(|p| (p.beat - s).abs() < 1e-3) {
                lane.points.push(pt(s, 18000.0, Curve::Step));
            }
        }
        lane.sort();
        risers.push(t);
    }
    // 2) pre-drop gap: a gain stage on the drum bus, -60 dB for the last beat
    let mut gap = Value::Null;
    if let Ok(bi) = e.project.bus_index("drums") {
        let idx = e.project.buses[bi].effects.len();
        e.project.buses[bi]
            .effects
            .push(crate::fx::Effect::Gain(crate::fx::GainFx::default()));
        let mut points = vec![pt(0.0, 0.0, Curve::Step)];
        for &s in &hooks {
            points.push(pt(s - 1.05, 0.0, Curve::Linear));
            points.push(pt(s - 1.0, -60.0, Curve::Step));
            points.push(pt(s - 0.02, -60.0, Curve::Linear));
            points.push(pt(s, 0.0, Curve::Step));
        }
        let name = e.project.buses[bi].name.clone();
        let mut lane = crate::automation::AutomationLane::new(&name, &format!("fx.{idx}.db"));
        lane.points = points;
        lane.sort();
        e.project.automation.push(lane);
        gap = json!({"bus": name, "fx": idx});
    }
    // 3) reverb throws on lead/chords: their send to a reverb bus, lifted
    // over the gap beat and back at the downbeat
    let mut throws = Vec::new();
    for t in ["lead", "chords"] {
        let Ok(ti) = e.project.track_index(t) else {
            continue;
        };
        let send = e.project.tracks[ti].sends.iter().find(|s| {
            e.project.bus_index(&s.bus).ok().is_some_and(|bi| {
                e.project.buses[bi]
                    .effects
                    .iter()
                    .any(|f| f.type_name() == "reverb")
            })
        });
        let Some(s) = send.cloned() else { continue };
        let up = (s.db + BUILD_THROW_DB).min(-2.0).max(s.db);
        let mut points = vec![pt(0.0, s.db, Curve::Step)];
        for &h in &hooks {
            points.push(pt(h - 1.25, s.db, Curve::Linear));
            points.push(pt(h - 1.0, up, Curve::Step));
            points.push(pt(h - 0.05, up, Curve::Linear));
            points.push(pt(h, s.db, Curve::Step));
        }
        let tn = e.project.tracks[ti].name.clone();
        e.project.automation.retain(|l| {
            !(l.target.eq_ignore_ascii_case(&tn)
                && l.param == format!("send.{}", s.bus.to_lowercase()))
        });
        let mut lane =
            crate::automation::AutomationLane::new(&tn, &format!("send.{}", s.bus.to_lowercase()));
        lane.points = points;
        lane.sort();
        e.project.automation.push(lane);
        throws.push(json!({"track": tn, "bus": s.bus, "from_db": s.db, "to_db": up}));
    }
    Ok(
        json!({"hooks": hooks, "riser_beats": built.iter().map(|b| b.1).collect::<Vec<_>>(), "risers": risers, "gap": gap, "throws": throws}),
    )
}

fn no_part(e: &Engine, role: &str) -> anyhow::Error {
    let have: Vec<&str> = crate::creative::ROLES
        .iter()
        .copied()
        .filter(|r| {
            e.project
                .track_index(&crate::producer::track_name(r))
                .is_ok()
        })
        .collect();
    anyhow!(
        "/instruments/{role}: this arrangement has no {role} part; roles that play here: {}",
        have.join(", ")
    )
}

fn remove_vocal(e: &mut Engine, track: &str) {
    e.project
        .audio_clips
        .retain(|c| !c.track.eq_ignore_ascii_case(track));
    e.project.automation.retain(|l| l.target != track);
    if let Ok(i) = e.project.track_index(track) {
        e.project.tracks.remove(i);
    }
    for t in e.project.tracks.iter_mut() {
        t.effects.retain(|f| {
            let v = serde_json::to_value(f).unwrap_or(Value::Null);
            !(v["type"] == "sidechain"
                && v["source"]
                    .as_str()
                    .map(|s| s.eq_ignore_ascii_case(track))
                    .unwrap_or(false))
        });
    }
}

fn place_vocal(
    e: &mut Engine,
    spec: &SongSpec,
    vs: &VocalSpec,
    log: &mut Vec<String>,
) -> Result<Value> {
    remove_vocal(e, &vs.track);
    let file = std::fs::canonicalize(&vs.file)
        .map_err(|err| anyhow!("/vocal/file: {} ({err})", vs.file))?;
    let fresh = e
        .project
        .samples
        .iter()
        .find(|s| s.name == VOCAL_SAMPLE)
        .map(|s| Path::new(&s.path) != file)
        .unwrap_or(true);
    if fresh {
        e.project.samples.retain(|s| s.name != VOCAL_SAMPLE);
        e.call_nested("import_sample", &json!({"path": file.to_string_lossy(), "name": VOCAL_SAMPLE, "source": "the song's vocal take"}), "song")?;
    }
    e.bank.sync(&e.project.samples);
    let take_s = e
        .bank
        .get(VOCAL_SAMPLE)
        .map(|d| d.len() as f32 / crate::dsp::SR)
        .unwrap_or(0.0);
    e.call_nested("add_track", &json!({"name": vs.track, "instrument": {"type": "sampler", "sample": VOCAL_SAMPLE, "one_shot": true}, "volume_db": vs.gain_db}), "song")?;
    let spans = mix_rules::section_spans(&e.project);
    let names = spec.section_names();
    let bs = 60.0 / e.project.bpm;
    let mut placed = 0;
    let mut lock = Value::Null;
    if vs.phrases.is_empty() {
        let start = match &vs.start_section {
            Some(s) => spans
                .iter()
                .find(|x| &x.name == s)
                .map(|x| x.start_beat)
                .ok_or_else(|| anyhow!("/vocal/start_section: no section '{s}'"))?,
            None => spans
                .iter()
                .find(|x| x.kind == "verse")
                .or(spans.first())
                .map(|x| x.start_beat)
                .unwrap_or(0.0),
        };
        if vs.lock != Some(false) {
            let r = e.call_nested("lock_vocal_to_beat", &json!({"sample": VOCAL_SAMPLE, "track": vs.track, "start_beat": start, "bpm": e.project.bpm, "flex": vs.flex.unwrap_or(0.5)}), "song")?;
            placed = r["phrases"].as_u64().unwrap_or(0) as usize;
            let l = &r["lock"];
            log.push(format!(
                "vocal: {placed} phrases locked to the beat from beat {start}: {}% of syllables within 25 ms of the 1/16 grid (median {} ms); laid free-time it would be {}% (median {} ms)",
                l["locked"]["within_25ms_pct"], l["locked"]["median_ms"], l["free_time"]["within_25ms_pct"], l["free_time"]["median_ms"]
            ));
            lock = l.clone();
        } else {
            let r = e.call_nested("place_vocal_phrases", &json!({"sample": VOCAL_SAMPLE, "track": vs.track, "start_beat": start, "grid": "beat", "keep_gaps": true}), "song")?;
            placed = r["phrases"].as_u64().unwrap_or(0) as usize;
            log.push(format!(
                "vocal: {placed} phrases cut at the pauses, in take order from beat {start}"
            ));
        }
    } else {
        let mut problems = Vec::new();
        for (i, ph) in vs.phrases.iter().enumerate() {
            let len = ph.len_s.or(ph.end_s.map(|x| x - ph.src_s)).unwrap_or(0.0);
            if take_s > 0.0 && ph.src_s + len > take_s + 0.05 {
                problems.push(p(
                    format!("/vocal/phrases/{i}"),
                    format!(
                        "{:.2}..{:.2} s runs past the end of the take ({take_s:.2} s)",
                        ph.src_s,
                        ph.src_s + len
                    ),
                    "shorten len_s or move src_s",
                ));
                continue;
            }
            let start_beat = if let Some(sec) = &ph.section {
                let j = resolve_section(spec, &names, sec)?;
                let sp = spans
                    .get(j)
                    .ok_or_else(|| anyhow!("section {sec} is not in the arrangement"))?;
                sp.start_beat + (ph.bar.unwrap_or(1.0) - 1.0) * 4.0 + (ph.beat.unwrap_or(1.0) - 1.0)
            } else if let Some(b) = ph.start_beat {
                b
            } else {
                ph.at_s.unwrap_or(0.0) / bs
            };
            if start_beat < -0.01 {
                problems.push(p(
                    format!("/vocal/phrases/{i}"),
                    format!("lands before the song starts (beat {start_beat:.2})"),
                    "move it later",
                ));
                continue;
            }
            e.call_nested("add_audio_clip", &json!({
                "track": vs.track, "sample": VOCAL_SAMPLE, "start_beat": start_beat, "offset_s": ph.src_s, "length_s": len,
                "gain_db": ph.gain_db, "fade_in_ms": ph.fade_in_ms.unwrap_or(8.0), "fade_out_ms": ph.fade_out_ms.unwrap_or(40.0)
            }), "song")?;
            placed += 1;
        }
        if !problems.is_empty() {
            return Err(problems_err(&problems));
        }
        log.push(format!(
            "vocal: {placed} phrases placed whole (no time-stretch)"
        ));
    }
    let chain = mix_rules::apply_vocal_chain(e, &vs.track, &vs.chain, "song")?;
    let part_eq = apply_vocal_part_eq(e, spec, &vs.track, log)?;
    let mut out = json!({"track": vs.track, "phrases": placed, "take_s": (take_s * 10.0).round() / 10.0, "chain": chain, "lock": lock});
    if let Some(x) = part_eq {
        out["part_eq"] = x;
    }
    Ok(out)
}

fn out_path(e: &Engine, spec: &SongSpec) -> PathBuf {
    match &spec.export.path {
        Some(p) => e.resolve(p),
        None => {
            let t = if spec.title.is_empty() {
                "song".to_string()
            } else {
                crate::samples::sample_name(&spec.title)
            };
            e.renders_dir().join(format!("{t}.{}", spec.export.format))
        }
    }
}

/// Mix to the targets, deliver, check. The vocal (if any) is balanced
/// section by section; hooks are lifted over verses.
fn mix_deliver(
    e: &mut Engine,
    spec: &SongSpec,
    log: &mut Vec<String>,
) -> Result<(Value, PathBuf, Value)> {
    // every mix rule and delivery round after this moves faders, buses,
    // fader lanes or the master only: synthesise each track once and re-sum
    // (dropped when we return)
    let prev = e.track_cache.replace(Default::default());
    let t0 = std::time::Instant::now();
    let r = mix_deliver_inner(e, spec, log);
    e.track_cache = prev;
    log.push(format!(
        "mix + delivery: {:.0} s",
        t0.elapsed().as_secs_f32()
    ));
    r
}

fn lap(log: &mut Vec<String>, t: &mut std::time::Instant, what: &str) {
    log.push(format!("{what}: {:.1} s", t.elapsed().as_secs_f32()));
    *t = std::time::Instant::now();
}

/// Most deliveries one song build makes (deliver -> check -> fix): lift,
/// loudness and both tone bands each own a knob, and the top end can need a
/// round from each side.
const DELIVERY_ROUNDS: usize = 5;

fn mix_deliver_inner(
    e: &mut Engine,
    spec: &SongSpec,
    log: &mut Vec<String>,
) -> Result<(Value, PathBuf, Value)> {
    let t = spec.targets();
    let mut tl = std::time::Instant::now();
    let voice = spec
        .vocal
        .as_ref()
        .map(|v| v.track.clone())
        .filter(|v| e.project.track_index(v).is_ok());
    let mut mix = Map::new();
    // auto bands an earlier build left are re-measured (or, off, dropped)
    let stale = strip_auto_bands(e);
    if stale > 0 {
        log.push(format!(
            "auto EQ: took out {stale} auto band(s) from the last build"
        ));
    }
    let stale = strip_auto_ducks(e);
    if stale > 0 {
        log.push(format!(
            "auto duck: took out {stale} duck(s) from the last build"
        ));
    }
    // the beat's bright parts are voiced softer before anything is measured
    mix.insert(
        "source_tone".into(),
        mix_rules::soften_beat_source_with(e, voice.as_deref(), &spec.mix.levels)?,
    );
    mix.insert("peaks".into(), mix_rules::tame_peaks(e)?);
    if let Some(v) = &voice {
        let i = e.project.track_index(v)?;
        e.project.tracks[i].volume_db = spec.vocal.as_ref().map(|x| x.gain_db).unwrap_or(0.0);
        // level first (the top-end rule compares the beat to the vocal as
        // it will sit), then the top end, then the level again
        mix_rules::balance_vocal(e, v, &t, 2)?;
    }
    // masking cuts with the faders set, before the tone rules and the
    // delivery loop (which then hear them)
    // the vocal's own rings first, so the masking measure hears it tamed
    if spec.mix.vocal_resonance {
        mix.insert(
            "vocal_resonance".into(),
            apply_vocal_resonance(e, voice.as_deref(), log)?,
        );
        lap(log, &mut tl, "vocal resonance");
    }
    if spec.mix.auto_eq {
        mix.insert(
            "auto_eq".into(),
            apply_auto_eq(e, spec, voice.as_deref(), log)?,
        );
        lap(log, &mut tl, "auto EQ");
    }
    if spec.mix.vocal_duck || spec.mix.kick_duck {
        mix.insert(
            "duck".into(),
            apply_auto_duck(e, spec, voice.as_deref(), log)?,
        );
        lap(log, &mut tl, "auto duck");
    }
    mix.insert(
        "top_end".into(),
        mix_rules::tame_top(e, voice.as_deref(), &t, 0.0)?,
    );
    if let Some(v) = &voice {
        let b = mix_rules::balance_vocal(e, v, &t, spec.mix.balance_passes as usize)?;
        log.push(format!("vocal balance: {}", b["fader_offsets_db"]));
        mix.insert("vocal_balance".into(), b);
    }
    mix.insert("glue".into(), mix_rules::set_glue(e)?);
    // the default lift (presence bell + air shelf) after the glue: every
    // tone rule after it only cuts, so a dark song had no way up
    mix.insert(
        "master_lift".into(),
        mix_rules::set_master_lift(e, &spec.mix.master_lift.clone().unwrap_or_default())?,
    );
    // the top end: the beat darkens on its own bus first (the vocal keeps
    // its top), the vocal is re-balanced over the darker beat, and the
    // master tone only takes what the beat could not (small, capped)
    lap(log, &mut tl, "peaks, vocal balance, top end, glue");
    let (mut top_p, mut top_a) = (0.0f32, 0.0f32);
    let m0 = mix_rules::TOP_DELIVERY_MARGIN_DB;
    mix.insert(
        "top_tone".into(),
        mix_rules::shape_top_with(e, voice.as_deref(), &t, m0, m0)?,
    );
    lap(log, &mut tl, "beat tone");
    // the energy the arrangement plays at: the spec's, plus what the lift rule thins
    let mut spec_e = spec.clone();
    mix.insert(
        "section_energy".into(),
        apply_section_energy(e, &spec_e, voice.as_deref())?,
    );
    mix.insert("section_gain".into(), apply_section_gain(e, &spec_e)?);
    let mut vocal_ride = None;
    mix.insert(
        "vocal_section_offset".into(),
        apply_vocal_section_offset(e, &spec_e, voice.as_deref(), &mut vocal_ride)?,
    );
    // the hook's lift comes from the arrangement (verses thin their layers),
    // never the master's volume, which the limiter would give back
    let lift = arrangement_lift(e, &mut spec_e, voice.as_deref(), &t)?;
    mix.insert("drop_lift".into(), lift);
    mix.insert(
        "quiet_sections".into(),
        mix_rules::lift_quiet_sections(e, &t)?,
    );
    let pre = mix_rules::measure_sections(e, voice.as_deref())?;
    let path = out_path(e, spec);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // the file's extension names its format: "song.wav" with the default
    // format used to get mp3 bytes under a .wav name, which the delivered-file
    // check then could not decode ("unsupported wave format")
    let format = path
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| x.to_ascii_lowercase())
        .filter(|x| ["mp3", "wav", "flac"].contains(&x.as_str()))
        .unwrap_or_else(|| spec.export.format.clone());
    let o = crate::tools_delivery::DeliverOpts {
        format,
        target_lufs: Some(spec.master.lufs),
        ceiling_dbtp: spec.master.true_peak - mix_rules::DELIVERY_CEILING_MARGIN_DB,
        soft_clip_db: mix_rules::DELIVERY_SOFT_CLIP_DB,
        preserve_dynamics: spec.master.preserve_dynamics,
        bitrate_kbps: spec.export.bitrate_kbps,
        ..Default::default()
    };
    let spans = mix_rules::section_spans(&e.project);
    // deliver -> check the delivered file -> fix each failed target with
    // the knob that owns it -> deliver again (at most DELIVERY_ROUNDS deliveries):
    // lift -> verses step down on the master automation (undone if the
    // limiter took it back); loudness -> less of the contrast is kept
    // (only while the lift has room); harshness -> the top-end rule again,
    // aimed lower by what delivery added
    let mut o = o;
    let mut d = Value::Null;
    let mut checks;
    let mut lift_log = Vec::new();
    let mut tone_rounds = 0usize;
    // what the level rule took from each bright part (the air floor may
    // give some of it back)
    let mut air_owed: Vec<(String, f32)> = mix["source_tone"]["levels"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((
                        x["part"].as_str()?.to_string(),
                        x["down_db"].as_f64()? as f32,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    // preserve_dynamics is searched between where the hook went flat (lift
    // short) and where the lift had room to spare (loudness may take some)
    let (mut pd_flat, mut pd_room): (f32, Option<f32>) = (0.0, None);
    for round in 0..DELIVERY_ROUNDS {
        // faders may have moved since: the energy lanes follow them
        apply_section_energy(e, &spec_e, voice.as_deref())?;
        apply_section_gain(e, &spec_e)?;
        apply_vocal_section_offset(e, &spec_e, voice.as_deref(), &mut vocal_ride)?;
        let full = e.mix()?;
        d = crate::tools_delivery::deliver(&full.left, &full.right, &path, &o)?;
        checks = mix_rules::check(&path, &pre, &spans, e.project.bpm, &t)?;
        lap(log, &mut tl, &format!("delivery round {}", round + 1));
        let get = |name: &str| {
            checks["checks"]
                .as_array()
                .and_then(|c| c.iter().find(|x| x["check"] == name))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let lift = get("drop_lift")["measured"].as_f64().map(|x| x as f32);
        let lufs = get("loudness")["measured"].as_f64().map(|x| x as f32);
        let harsh = get("not_harsh");
        if round + 1 == DELIVERY_ROUNDS {
            break;
        }
        let mut changed = false;
        // a flat hook on the delivered file: the limiter took the contrast,
        // so the delivery keeps more of the song's own (gentler limiting)
        let pd = o.preserve_dynamics;
        if let Some(l) = lift.filter(|l| *l < t.drop_lift_db[0]) {
            pd_flat = pd_flat.max(pd);
            let next = match pd_room {
                Some(h) => 0.5 * (pd + h),
                None => pd + ((t.drop_lift_db[0] + 0.3 - l) * 0.2).clamp(0.1, 0.3),
            }
            .min(0.9);
            if next > pd + 0.03 {
                o.preserve_dynamics = next;
                lift_log.push(json!({"delivered_lift_db": (l * 10.0).round() / 10.0, "preserve_dynamics": (next * 100.0).round() / 100.0, "why": "the limiter flattened the hook"}));
                changed = true;
            }
        } else if let (Some(l), Some(lu)) = (lift, lufs) {
            let room = l >= t.drop_lift_db[0] + 0.3;
            if lu < t.lufs - t.lufs_tolerance && room && pd > 0.3 {
                pd_room = Some(pd_room.map_or(pd, |h| h.min(pd)));
                let next = if pd_flat > 0.0 {
                    0.5 * (pd + pd_flat)
                } else {
                    pd - 0.15
                }
                .max(0.3);
                if next < pd - 0.03 {
                    o.preserve_dynamics = next;
                    lift_log.push(json!({"delivered_lufs": (lu * 10.0).round() / 10.0, "delivered_lift_db": (l * 10.0).round() / 10.0, "preserve_dynamics": (next * 100.0).round() / 100.0}));
                    changed = true;
                }
            }
        }
        if harsh["pass"] == false && tone_rounds < 2 {
            // each band aims at its own window on the delivered file: too
            // bright -> the top rule again aimed lower; too dark -> eased
            let pr = harsh["measured"]["presence_rel_lufs_db"]
                .as_f64()
                .map(|x| x as f32);
            let ar = harsh["measured"]["air_rel_lufs_db"]
                .as_f64()
                .map(|x| x as f32);
            let [lo, hi] = t.presence_rel_lufs_db;
            let mut moved = Vec::new();
            if let Some(pr) = pr.filter(|p| *p > hi + 0.15 || *p < lo - 0.15) {
                let d = pr - t.presence_rel_aim_db();
                top_p = (top_p + d).clamp(-4.0, 8.0);
                moved.push(json!({"band": "2-5k", "delivered_rel_lufs_db": (pr * 10.0).round() / 10.0, "aim_db": t.presence_rel_aim_db()}));
            }
            let [a_lo, a_hi] = t.air_rel_lufs_db;
            if let Some(ar) = ar.filter(|a| *a > a_hi + 0.15) {
                top_a = (top_a + ar - (a_hi - 0.2)).clamp(-4.0, 8.0);
                moved.push(json!({"band": "5-10k", "delivered_rel_lufs_db": (ar * 10.0).round() / 10.0, "max_db": a_hi}));
            }
            // too dull up top: first the beat bus's 7.5k shelf eases (it is
            // the cut that made it dull), then the bright parts get back some
            // of what the level rule took (a fader move: the cache re-sums it)
            let air_aim = 0.5 * (a_lo + a_hi);
            let shelf = mix_rules::beat_shelf_db(&e.project);
            if let Some(ar) = ar.filter(|a| *a < a_lo - 0.15 && shelf < -0.5) {
                top_a = (top_a + ar - air_aim).clamp(-6.0, 8.0);
                moved.push(json!({"band": "5-10k", "delivered_rel_lufs_db": (ar * 10.0).round() / 10.0, "floor_db": a_lo, "beat_shelf_7k5_db": (shelf * 10.0).round() / 10.0}));
            } else if let Some(ar) = ar.filter(|a| *a < a_lo - 0.15) {
                let up = mix_rules::give_back_air(e, &mut air_owed, ar, &t)?;
                if !up.is_empty() {
                    lift_log.push(json!({"delivered_air_rel_lufs_db": (ar * 10.0).round() / 10.0, "floor_db": a_lo, "air_given_back": up}));
                    changed = true;
                }
            }
            if !moved.is_empty() {
                tone_rounds += 1;
                let r = mix_rules::shape_top_with(e, voice.as_deref(), &t, m0 + top_p, m0 + top_a)?;
                lift_log.push(json!({"delivered_tone": moved, "top_tone": r}));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let pre = mix_rules::measure_sections(e, voice.as_deref())?;
    checks = mix_rules::check(&path, &pre, &spans, e.project.bpm, &t)?;
    if let Some(c) = checks["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|x| x["check"] == "drop_lift"))
    {
        log.push(format!(
            "drop_lift {} dB (delivered): {}",
            c["measured"],
            c["note"].as_str().unwrap_or("")
        ));
    }
    if !lift_log.is_empty() {
        log.push(format!(
            "delivery loop: {} fix(es) after checking the delivered file",
            lift_log.len()
        ));
        mix.insert("delivery_fixes".into(), json!(lift_log));
    }
    mix.insert("delivery".into(), json!({"format": spec.export.format, "lufs_before": d.get("before").cloned().unwrap_or(Value::Null), "notes": d.get("notes").cloned().unwrap_or(Value::Null)}));
    Ok((Value::Object(mix), path, checks))
}

/// Build (or rebuild from `scope`) the whole song from a valid spec.
/// Where a spec's vocal take is on this machine. In order: the path as
/// given; `BEATBOX_VOCAL_PATH` (the take itself, or a folder holding a file
/// of the take's name); then the path as given (when relative) or the
/// take's file name, under the working folder, the current folder, the repo
/// and the repo's `presets/` (so a preset's take travels with a clone).
/// None = not found anywhere.
pub fn find_vocal_take(file: &str, workdir: Option<&Path>) -> Option<std::path::PathBuf> {
    let given = Path::new(file);
    if given.is_file() {
        return Some(given.to_path_buf());
    }
    let name = given.file_name().map(std::path::PathBuf::from);
    if let Some(env) = std::env::var_os("BEATBOX_VOCAL_PATH").filter(|v| !v.is_empty()) {
        let env = std::path::PathBuf::from(env);
        if env.is_file() {
            return Some(env);
        }
        if let Some(n) = &name {
            if env.join(n).is_file() {
                return Some(env.join(n));
            }
        }
    }
    let repo = option_env!("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from);
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    roots.extend(workdir.map(Path::to_path_buf));
    roots.extend(std::env::current_dir().ok());
    if let Some(r) = repo {
        roots.push(r.join("presets"));
        roots.push(r);
    }
    for root in &roots {
        if given.is_relative() && root.join(given).is_file() {
            return Some(root.join(given));
        }
        if let Some(n) = &name {
            if root.join(n).is_file() {
                return Some(root.join(n));
            }
        }
    }
    None
}

/// The spec with its vocal take found on this machine (`find_vocal_take`),
/// and a warning when it moved or is missing. A missing take does not stop
/// the song: the vocal is left out (the beat renders with the vocal muted)
/// and the warning says so.
pub fn resolve_vocal_take(spec_v: &Value, workdir: Option<&Path>) -> (Value, Option<String>) {
    let Some(file) = spec_v.pointer("/vocal/file").and_then(|f| f.as_str()) else {
        return (spec_v.clone(), None);
    };
    if Path::new(file).is_file() {
        return (spec_v.clone(), None);
    }
    let mut v = spec_v.clone();
    match find_vocal_take(file, workdir) {
        Some(found) => {
            let found = found.to_string_lossy().to_string();
            v["vocal"]["file"] = json!(found);
            (
                v,
                Some(format!(
                    "vocal take '{file}' is not on this machine; using '{found}'"
                )),
            )
        }
        None => {
            if let Value::Object(m) = &mut v {
                m.remove("vocal");
            }
            (v, Some(format!("vocal take '{file}' is not on this machine (also looked in BEATBOX_VOCAL_PATH, the working folder, the current folder and the repo's presets/): rendering with the vocal muted. Set BEATBOX_VOCAL_PATH to the take (or its folder) to bring it back")))
        }
    }
}

pub fn build(e: &mut Engine, spec_v: &Value, scope: Scope) -> Result<Value> {
    let t0 = std::time::Instant::now();
    // auto bells from an earlier build are measured again (or, with
    // /mix/auto_eq off, dropped): the build starts from the user's bands
    let mut cleaned = spec_v.clone();
    let stale_bells = strip_auto_bells(&mut cleaned);
    let spec_v = &cleaned;
    let (resolved, vocal_warning) = resolve_vocal_take(spec_v, Some(&e.workdir));
    if let Some(w) = &vocal_warning {
        eprintln!("warning: {w}");
    }
    let stored_spec = spec_v;
    let spec_v = &resolved;
    let (spec, probs) = validate(spec_v);
    if !probs.is_empty() {
        return Err(problems_err(&probs));
    }
    let spec = spec.expect("valid");
    let prev = e.project.song.clone();
    let scope = if prev.is_none() { Scope::Full } else { scope };
    let seed = match (&prev, spec.seed) {
        (_, Some(s)) => s,
        (Some(p), None) if scope != Scope::Full || spec_v.get("seed").is_none() => p.seed,
        _ => seed_for(&spec),
    };
    let mut log = Vec::new();
    if stale_bells > 0 {
        log.push(format!(
            "auto EQ: dropped {stale_bells} auto bell(s) the last build wrote into /mix/part_eq"
        ));
    }
    if scope == Scope::Full {
        compose_beat(e, &spec, seed, &mut log)?;
    }
    let mut vocal = Value::Null;
    if scope >= Scope::Vocal {
        if let Some(vs) = &spec.vocal {
            vocal = place_vocal(e, &spec, vs, &mut log)?;
        } else if let Some(p) = &prev {
            // a vocal removed from the spec leaves the song
            if let Some(t) = p.spec["vocal"]["track"]
                .as_str()
                .or(p.spec.get("vocal").map(|_| "vocal"))
            {
                remove_vocal(e, t);
            }
        }
    }
    let (mix, path, checks) = mix_deliver(e, &spec, &mut log)?;
    let spans = mix_rules::section_spans(&e.project);
    let bs = 60.0 / e.project.bpm;
    // the spec as given is kept (a take found elsewhere, or missing now,
    // is looked for again on the next build)
    let mut stored = stored_spec.clone();
    if let Value::Object(m) = &mut stored {
        m.remove("seed");
    }
    // the auto cuts go into the stored spec, marked auto
    write_auto_cuts(&mut stored, &mix)?;
    e.project.song = Some(SongState {
        spec: stored,
        seed,
        sections: spans.clone(),
        audio: Some(path.to_string_lossy().to_string()),
        checks: checks.clone(),
    });
    if !spec.title.is_empty() {
        e.project.name = spec.title.clone();
    }
    Ok(json!({
        "audio": path.to_string_lossy(),
        "warnings": vocal_warning.iter().collect::<Vec<_>>(),
        "scope": format!("{scope:?}").to_lowercase(),
        "seed": seed,
        "bpm": e.project.bpm,
        "key": format!("{} {}", e.project.key_root, e.project.scale),
        "duration_s": spans.last().map(|s| ((s.end_beat * bs) * 10.0).round() / 10.0),
        "sections": spans.iter().map(|s| json!({"name": s.name, "kind": s.kind, "bars": ((s.end_beat - s.start_beat) / 4.0).round(), "start_s": ((s.start_beat * bs) * 100.0).round() / 100.0})).collect::<Vec<_>>(),
        "vocal": vocal,
        "checks": checks,
        "mix": mix,
        "log": log,
        "wall_seconds": (t0.elapsed().as_secs_f32() * 10.0).round() / 10.0,
        "next": ["song_patch {set: {\"/mix/vocal_over_beat_db\": 8}} re-mixes only", "song_patch {set: {\"/vocal/phrases/0/beat\": 2}} rebuilds only the vocal", "song_get shows the spec; undo restores the song before this call"],
    }))
}

/// The auto-EQ's cuts (a build result's mix.auto_eq.cuts) into a stored
/// spec's /mix/part_eq, each bell marked "auto": true.
fn write_auto_cuts(stored: &mut Value, mix: &Value) -> Result<()> {
    for c in mix["auto_eq"]["cuts"].as_array().into_iter().flatten() {
        let key = c["key"]
            .as_str()
            .unwrap_or_default()
            .replace('~', "~0")
            .replace('/', "~1");
        let bell = json!({"hz": c["hz"], "db": c["db"], "q": c["q"], "auto": true});
        let bells = format!("/mix/part_eq/{key}/bells");
        if !stored.pointer(&bells).is_some_and(|b| b.is_array()) {
            set_pointer(stored, &bells, json!([]))?;
        }
        set_pointer(stored, &format!("{bells}/-"), bell)?;
    }
    for d in mix["duck"]["ducks"].as_array().into_iter().flatten() {
        let key = d["key"]
            .as_str()
            .unwrap_or_default()
            .replace('~', "~0")
            .replace('/', "~1");
        let duck = json!({"hz": d["hz"], "max_db": d["max_db"], "q": d["q"], "source": d["source"], "key": d["key_mode"], "attack_ms": d["attack_ms"], "release_ms": d["release_ms"], "auto": true});
        let ducks = format!("/mix/part_eq/{key}/ducks");
        if !stored.pointer(&ducks).is_some_and(|b| b.is_array()) {
            set_pointer(stored, &ducks, json!([]))?;
        }
        set_pointer(stored, &format!("{ducks}/-"), duck)?;
    }
    let bands = mix["vocal_resonance"]["bands"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !bands.is_empty() {
        let ducks = "/mix/part_eq/vocal/ducks";
        if !stored.pointer(ducks).is_some_and(|b| b.is_array()) {
            set_pointer(stored, ducks, json!([]))?;
        }
        for b in bands {
            let d = json!({"hz": b["hz"], "max_db": b["max_db"], "q": b["q"], "source": b["track"], "key": "self", "attack_ms": b["attack_ms"], "release_ms": b["release_ms"], "auto": true});
            set_pointer(stored, &format!("{ducks}/-"), d)?;
        }
    }
    Ok(())
}

/// Set (or remove with null) one value by JSON pointer, creating objects on
/// the way; "-" appends to an array.
pub fn set_pointer(root: &mut Value, ptr: &str, val: Value) -> Result<()> {
    if ptr.is_empty() || ptr == "/" {
        bail!("give a path like /mix/vocal_over_beat_db");
    }
    if !ptr.starts_with('/') {
        bail!("'{ptr}' is not a JSON path: start it with '/', e.g. /vocal/gain_db");
    }
    let parts: Vec<String> = ptr[1..]
        .split('/')
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect();
    let mut cur = root;
    for (i, k) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if cur.is_null() {
            *cur = json!({});
        }
        match cur {
            Value::Object(m) => {
                if last {
                    if val.is_null() {
                        m.remove(k);
                    } else {
                        m.insert(k.clone(), val);
                    }
                    return Ok(());
                }
                cur = m.entry(k.clone()).or_insert(Value::Null);
            }
            Value::Array(a) => {
                let idx = if k == "-" {
                    a.len()
                } else {
                    k.parse::<usize>()
                        .map_err(|_| anyhow!("{ptr}: '{k}' is not a list index"))?
                };
                if last {
                    if val.is_null() {
                        if idx < a.len() {
                            a.remove(idx);
                        }
                    } else if idx == a.len() {
                        a.push(val);
                    } else if idx < a.len() {
                        a[idx] = val;
                    } else {
                        bail!("{ptr}: index {idx} but the list has {} items", a.len());
                    }
                    return Ok(());
                }
                if idx >= a.len() {
                    bail!("{ptr}: index {idx} but the list has {} items", a.len());
                }
                cur = &mut a[idx];
            }
            _ => bail!("{ptr}: '{}' is not an object or list", parts[..i].join("/")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- tools

fn spec_arg(a: &Value) -> Value {
    match a.get("spec") {
        Some(s) if s.is_object() => s.clone(),
        _ => {
            let mut m = a.as_object().cloned().unwrap_or_default();
            m.remove("spec");
            Value::Object(m)
        }
    }
}

/// The genres this engine knows, from the playbooks (never a hand list that
/// drifts): "trap, ..., punjabi_party (honey_singh, badshah, bhangra), ...".
fn genre_help() -> String {
    let names: Vec<String> = crate::producer::playbooks()
        .iter()
        .map(|b| match b.name.as_str() {
            "punjabi_party" => "punjabi_party (Honey Singh / Badshah party rap: dhol, tumbi, 808; also 'bhangra', 'badshah', 'honey_singh')".to_string(),
            "desi_hiphop" => "desi_hiphop (tabla, sitar, tanpura, 808)".to_string(),
            n => n.to_string(),
        })
        .collect();
    names.join(", ")
}

fn spec_props() -> Value {
    json!({
        "spec": {"type": "object", "description": "The whole SongSpec (or give its fields flat next to action)."},
        "title": {"type": "string"},
        "prompt": {"type": "string", "description": "Free words about the sound (optional)."},
        "genre": {"type": "string", "description": genre_help()},
        "bpm": {"type": "number"},
        "key": {"type": "string", "description": "\"D\", \"D minor\", \"F#m\""},
        "scale": {"type": "string"},
        "mood": {"type": "string"},
        "density": {"type": "string", "enum": ["sparse", "balanced", "dense"]},
        "seed": {"type": "integer", "description": "Same spec + same seed = same song. Omitted: a fresh one, returned and kept for patches."},
        "structure": {"type": "array", "items": {"type": "object", "properties": {"kind": {"type": "string", "enum": SECTION_KINDS}, "bars": {"type": "integer"}, "name": {"type": "string"}}, "required": ["kind", "bars"]}},
        "instruments": {"type": "object", "description": "role -> preset name | \"catalog:<id>\" (a recorded instrument, e.g. catalog:euterpea_harmonium) | {\"samples\": [one-shot files], \"gain_db\": 0} (e.g. real dhol hits on tabla/bayan)"},
        "vocal": {"type": "object", "description": "{file, track?, phrases?: [{src_s, len_s|end_s, section, bar, beat, gain_db?}], start_section?, lyrics?, gain_db?, chain?, lock? (default true: beat follows the vocal; no bpm = tempo picked from the take, each phrase locked to the grid), flex? (0..1 syllable pull, default 0.5)}"},
        "mix": {"type": "object", "description": "{vocal_over_beat_db: 6, vocal_over_beat_max_db: 12, drop_lift_db: 2, balance_passes: 3, levels: {role: dB offset, e.g. \"bass\": 2}}"},
        "master": {"type": "object", "description": "{lufs: -10, true_peak: -1, preserve_dynamics: 0.8}"},
        "export": {"type": "object", "description": "{path?, format: mp3|wav|flac, bitrate_kbps: 320}"},
    })
}

pub fn example() -> Value {
    json!({
        "title": "nikal ja", "genre": "desi_hiphop", "bpm": 100, "key": "D minor",
        "structure": [{"kind": "intro", "bars": 4}, {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8}, {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8}, {"kind": "outro", "bars": 4}],
        "instruments": {"lead": "indian_flute", "harmony": "harmonium"},
        "vocal": {"file": "/path/take.wav", "phrases": [{"src_s": 3.34, "len_s": 4.3, "section": "verse1", "bar": 1, "beat": 1}, {"src_s": 50.2, "len_s": 3.9, "section": "hook1", "bar": 1, "beat": 1, "gain_db": 1}]},
        "export": {"path": "/tmp/song.mp3", "format": "mp3"}
    })
}

fn create_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let mut spec = spec_arg(a);
    // {"improve": N} beside the spec: N self-critique rounds after the build
    let improve = spec
        .as_object_mut()
        .and_then(|m| m.remove("improve"))
        .or_else(|| a.get("improve").cloned());
    let mut r = build(e, &spec, Scope::Full)?;
    // {"improve": 2} or {"improve": {"rounds": 2, "reference": "song2.mp3"}}
    let (rounds, reference) = match &improve {
        Some(Value::Object(o)) => (
            o.get("rounds").and_then(|v| v.as_u64()).unwrap_or(2),
            o.get("reference").cloned(),
        ),
        Some(v) => (v.as_u64().unwrap_or(0), None),
        None => (0, None),
    };
    if rounds > 0 {
        let rf = crate::song_critic::reference_metrics(e, &json!({"reference": reference}))?;
        let imp = crate::song_critic::improve(e, (rounds as usize).min(4), 3, None, rf)?;
        r["checks"] = e
            .project
            .song
            .as_ref()
            .map(|s| s.checks.clone())
            .unwrap_or(Value::Null);
        r["improve"] = imp;
    }
    Ok(r)
}

fn patch_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let set = a
        .get("set")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let remove: Vec<String> = a
        .get("remove")
        .and_then(|v| v.as_array())
        .map(|x| {
            x.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    patch_song(e, &set, &remove)
}

/// Set / remove spec values by JSON pointer and rebuild only what they touch
/// (the same seed unless /seed is set).
pub fn patch_song(e: &mut Engine, set: &Map<String, Value>, remove: &[String]) -> Result<Value> {
    let st = e.project.song.clone().ok_or_else(|| {
        anyhow!("this project has no song yet: song_create builds one from a spec first")
    })?;
    if set.is_empty() && remove.is_empty() {
        bail!("nothing to change: give set {{\"/path\": value}} and/or remove [\"/path\"]; song_get shows the spec and its paths");
    }
    let mut spec = st.spec.clone();
    let mut scope = Scope::Mix;
    for (k, v) in set {
        set_pointer(&mut spec, k, v.clone())?;
        scope = scope.max_by(scope_of(k));
    }
    for k in remove {
        set_pointer(&mut spec, k, Value::Null)?;
        scope = scope.max_by(scope_of(k));
    }
    if set.contains_key("/seed") {
        scope = Scope::Full;
    }
    let mut spec_with_seed = spec.clone();
    if !set.contains_key("/seed") {
        spec_with_seed["seed"] = json!(st.seed);
    }
    let mut r = build(e, &spec_with_seed, scope)?;
    if let Some(s) = e.project.song.as_mut() {
        // as `build` stores it: the last auto cuts out, this build's in
        strip_auto_bells(&mut spec);
        write_auto_cuts(&mut spec, &r["mix"])?;
        s.spec = spec;
    }
    r["changed_paths"] = json!(set.keys().chain(remove.iter()).collect::<Vec<_>>());
    Ok(r)
}

trait MaxBy {
    fn max_by(self, o: Self) -> Self;
}
impl MaxBy for Scope {
    fn max_by(self, o: Self) -> Self {
        if o > self {
            o
        } else {
            self
        }
    }
}

fn get_tool(e: &mut Engine, _a: &Value) -> Result<Value> {
    match &e.project.song {
        Some(s) => Ok(
            json!({"spec": s.spec, "seed": s.seed, "sections": s.sections, "audio": s.audio, "checks": s.checks}),
        ),
        None => Ok(
            json!({"song": null, "note": "no song in this project; song_create builds one", "example": example()}),
        ),
    }
}

fn validate_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (spec, vocal_warning) = resolve_vocal_take(&spec_arg(a), Some(&e.workdir));
    let (s, probs) = validate(&spec);
    let mut out = json!({
        "valid": probs.is_empty(),
        "problems": probs,
        "warnings": vocal_warning.iter().collect::<Vec<_>>(),
        "sections": s.as_ref().map(|s| s.section_names()),
        "targets": s.as_ref().map(|s| s.targets()),
    });
    // say what a genre word resolved to ("honey_singh" -> punjabi_party), and
    // without a structure, the sections the genre's own form names, so
    // phrases can be placed before anything is built
    if let Some(pb) = s
        .as_ref()
        .and_then(|s| s.genre.as_deref())
        .and_then(|g| crate::producer::playbook(g).ok())
    {
        out["genre"] = json!({"asked": s.as_ref().and_then(|s| s.genre.clone()), "resolved": pb.name, "bpm_range": pb.bpm, "about": pb.description});
        if s.as_ref().is_some_and(|s| s.structure.is_empty()) {
            let mut count: BTreeMap<String, usize> = BTreeMap::new();
            let names: Vec<String> = pb
                .arrangement
                .iter()
                .map(|t| {
                    let k = canon_kind(&t.kind);
                    let c = count.entry(k.clone()).or_insert(0);
                    *c += 1;
                    format!("{k}{c}")
                })
                .collect();
            out["sections"] = json!(names);
            out["sections_from"] = json!(format!(
                "the {} template form ({} bars); a fresh seed may vary it, so give \"structure\" when phrases must land in named sections",
                pb.name,
                pb.arrangement.iter().map(|t| t.bars).sum::<u32>()
            ));
        }
    }
    Ok(out)
}

fn schema_tool(_e: &mut Engine, _a: &Value) -> Result<Value> {
    Ok(json!({
        "fields": spec_props(),
        "section_kinds": SECTION_KINDS,
        "roles": crate::creative::ROLES,
        "genres": crate::producer::playbooks().iter().map(|b| b.name.clone()).collect::<Vec<_>>(),
        "defaults": {"mix": MixSpec::default(), "master": MasterSpec::default(), "export": ExportSpec::default(), "vocal_chain": VocalChain::default()},
        "example": example(),
        "patch_scopes": {"mix": "/mix (except /mix/levels), /master, /export, /title, /vocal/gain_db: re-mix and deliver", "vocal": "/vocal/...: rebuild the vocal track", "full": "anything else: recompose the beat with the same seed"},
    }))
}

fn render_tool(e: &mut Engine, _a: &Value) -> Result<Value> {
    let st = e.project.song.clone().ok_or_else(|| {
        anyhow!("this project has no song yet: song_create builds one from a spec first")
    })?;
    let mut spec = st.spec.clone();
    spec["seed"] = json!(st.seed);
    let mut r = build(e, &spec, Scope::Mix)?;
    if let Some(s) = e.project.song.as_mut() {
        s.spec = st.spec;
    }
    r["note"] = json!(
        "re-mixed and delivered the project as it is now (edits made with other actions are kept)"
    );
    Ok(r)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "song_create",
            description: "START HERE for a whole song. One SongSpec in, one finished song out: the beat composed to your structure and instruments, your vocal take cut and placed (phrases at section/bar/beat, or cut at its pauses), the vocal chain (HPF, light de-ess, 3:1, small sends), the vocal balanced over the beat section by section, hooks lifted over verses, delivered as mp3/wav/flac at -10 LUFS / -1 dBTP, and every target CHECKED on the delivered file (checks[] with pass/fail and the fix). One undo step. song_schema shows every field; song_validate checks a spec without building.",
            mutates: true,
            schema: || {
                let mut p = spec_props();
                p["improve"] = json!({"anyOf": [{"type": "integer"}, {"type": "object", "properties": {"rounds": {"type": "integer"}, "reference": {"type": "string"}}}], "description": "self-critique after the build (song_improve: judge against the genre profile, fix, rebuild, keep the best take): a number of rounds (max 4), or {\"rounds\": 2, \"reference\": \"approved.mp3\"} to also aim at a reference file's top end, low end and loudness"});
                obj(p, &[])
            },
            run: create_tool,
        },
        Tool {
            name: "song_patch",
            description: "Change the song by JSON path and redo only what the path touches: set {\"/mix/vocal_over_beat_db\": 8} re-mixes, {\"/vocal/phrases/3/beat\": 2} rebuilds the vocal, {\"/bpm\": 96} or {\"/instruments/lead\": \"sitar\"} recomposes the beat with the same seed. remove [\"/vocal\"] drops a part. Returns the new checks. One undo step.",
            mutates: true,
            schema: || obj(json!({"set": {"type": "object", "description": "JSON path -> new value"}, "remove": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: patch_tool,
        },
        Tool {
            name: "song_get",
            description: "The song's spec (every path song_patch can change), seed, sections with start times, the delivered file and its last checks.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: get_tool,
        },
        Tool {
            name: "song_validate",
            description: "Check a SongSpec without building anything: every problem at once, each with its JSON path and the fix; the section names phrases can refer to; the mix targets that will be checked.",
            mutates: false,
            schema: || obj(spec_props(), &[]),
            run: validate_tool,
        },
        Tool {
            name: "song_schema",
            description: "Every SongSpec field with its type and default, section kinds, instrument roles, genres, a full example and what each patch path redoes.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: schema_tool,
        },
        Tool {
            name: "song_render",
            description: "Re-mix, deliver and check the song as the project is now (after edits with other actions): vocal balance, hook lift, loudness and peak targets, checks[] in the result.",
            mutates: true,
            schema: || obj(json!({}), &[]),
            run: render_tool,
        },
        Tool {
            name: "song_critique",
            description: "Judge the song as delivered against its genre's target profile (data: low end, hook lift, loudness range, width, groove density, the hook's build, top end, vocal over beat) and name each weakness in plain words, with the spec change that fixes it. Changes nothing. Give {file, genre} to judge any audio file.",
            mutates: false,
            schema: || obj(json!({"genre": {"type": "string", "description": "judge against this genre's profile (default: the song's genre)"}, "file": {"type": "string", "description": "judge this audio file instead of the song (needs genre for its profile)"}, "reference": {"type": "string", "description": "also hold the song to this audio file: top end within 1 dB under it, 40-120 Hz no heavier than 0.5 dB over it, loudness within 0.6 LU"}}), &[]),
            run: crate::song_critic::critique_tool,
        },
        Tool {
            name: "song_improve",
            description: "Make the song better on its own: critique it against its genre profile, apply the fixes for the biggest weaknesses (spec knobs: /mix/levels, /mix/drop_lift_db, /density, /master/preserve_dynamics, /mix/vocal_over_beat_db), rebuild, critique again and keep the best take, for up to `rounds` rebuilds (default 2, max 4; each is a full render). The song's file ends as the best take. song_create {..., \"improve\": 2} does the same in one call.",
            mutates: true,
            schema: || obj(json!({"rounds": {"type": "integer", "description": "rebuilds at most (1..4, default 2)"}, "max_fixes": {"type": "integer", "description": "fixes per round (default 3)"}, "genre": {"type": "string", "description": "judge against this genre's profile (default: the song's genre)"}, "reference": {"type": "string", "description": "an approved mix to aim at: top end within 1 dB under it, 40-120 Hz no heavier, loudness within 0.6 LU (sets /mix/air_floor_db, /mix/levels, /master/lufs)"}}), &[]),
            run: crate::song_critic::improve_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_vocal_take_is_found_by_env_or_relative_path_else_muted_with_a_warning() {
        let d = std::env::temp_dir().join(format!("song_vox_find_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("takes")).unwrap();
        std::fs::write(d.join("takes").join("b40_take.wav"), b"x").unwrap();
        std::fs::write(d.join("rel_take.wav"), b"x").unwrap();
        // BEATBOX_VOCAL_PATH: a folder holding the take's file name
        std::env::set_var("BEATBOX_VOCAL_PATH", d.join("takes"));
        assert_eq!(
            find_vocal_take("/gone/box/b40_take.wav", None),
            Some(d.join("takes").join("b40_take.wav"))
        );
        std::env::remove_var("BEATBOX_VOCAL_PATH");
        // relative to the working folder, by file name
        assert_eq!(
            find_vocal_take("/gone/box/rel_take.wav", Some(&d)),
            Some(d.join("rel_take.wav"))
        );
        assert_eq!(find_vocal_take("/gone/box/never_b40.wav", Some(&d)), None);
        // the d11 preset's take path resolves or is muted, never an error
        let preset: Value =
            serde_json::from_str(include_str!("../presets/d11_baseline.json")).unwrap();
        let (v, w) = resolve_vocal_take(&preset, Some(&d));
        assert!(
            v.get("vocal").is_none() == w.as_deref().is_some_and(|w| w.contains("muted")),
            "{w:?}"
        );
        // a song whose take is missing renders, without its vocal, and warns
        let mut e = Engine::new(d.clone());
        let spec = json!({"genre": "boom_bap", "bpm": 90, "seed": 3,
            "structure": [{"kind": "verse", "bars": 2}, {"kind": "hook", "bars": 2}],
            "vocal": {"file": "/gone/box/never_b40.wav"},
            "export": {"path": d.join("t.wav").to_string_lossy(), "format": "wav"}});
        let r = e.call("song_create", &spec).unwrap();
        assert!(Path::new(r["audio"].as_str().unwrap()).is_file());
        let w = r["warnings"][0].as_str().unwrap();
        assert!(
            w.contains("never_b40.wav") && w.contains("vocal muted"),
            "{w}"
        );
        assert!(e.project.track_index("vocal").is_err(), "no vocal track");
        // the spec as given is kept, so the take is looked for again next time
        assert_eq!(
            e.project.song.as_ref().unwrap().spec["vocal"]["file"],
            "/gone/box/never_b40.wav"
        );
        let v = validate_tool(&mut e, &spec).unwrap();
        assert!(
            v["valid"] == true && v["warnings"][0].as_str().unwrap().contains("muted"),
            "{v}"
        );
    }

    #[test]
    fn the_d11_baseline_preset_is_a_valid_spec_with_its_locked_values() {
        let v: Value = serde_json::from_str(include_str!("../presets/d11_baseline.json")).unwrap();
        let (spec, probs) = validate(&v);
        // the vocal take and export folder are local paths on the render box
        let other: Vec<&Problem> = probs
            .iter()
            .filter(|p| p.path != "/vocal/file" && p.path != "/export/path")
            .collect();
        assert!(other.is_empty(), "{other:?}");
        if probs.is_empty() {
            assert!(spec.is_some());
        }
        assert_eq!(v["seed"], json!(6625524094895247u64));
        assert_eq!(v["vocal"]["chain"]["presence_db"], json!(1.2));
        let ml = &v["mix"]["master_lift"];
        assert_eq!(
            (
                ml["air_db"].clone(),
                ml["extra_db"].clone(),
                ml["presence_db"].clone()
            ),
            (json!(5.0), json!(0.0), json!(5.5))
        );
        assert_eq!(
            v["mix"]["section_gain_db"],
            json!({"verse": -1.2, "hook": 1.5})
        );
    }

    #[test]
    fn a_role_the_beat_does_not_play_is_skipped_not_fatal() {
        // the known bug: perc samples on a beat with no perc part failed the song
        let (spec, probs) = validate(
            &json!({"genre": "trap", "instruments": {"perc": {"samples": ["/nonexistent/perc.wav"]}, "counter": "catalog:vcsl_pipe_organ_soft"}, "mix": {"levels": {"perc": 2.0}}}),
        );
        let spec = spec.unwrap_or_else(|| panic!("{probs:?}"));
        let d = std::env::temp_dir().join(format!("song_noperc_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        let mut log = Vec::new();
        swap_instruments(&mut e, &spec, &mut log).unwrap();
        apply_role_levels(&mut e, &spec, &mut log).unwrap();
        assert!(
            log.iter()
                .any(|l| l.contains("no perc part") && l.contains("not used")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|l| l.contains("level offset was not applied")),
            "{log:?}"
        );
    }

    #[test]
    fn part_eq_specs_round_trip_and_bad_ones_name_the_field() {
        let v = json!({"genre": "trap", "seed": 7, "mix": {"part_eq": {
            "harmony": {"low_shelf": {"hz": 150, "db": -2}, "high_shelf": {"hz": 9000, "db": 1.5}, "bells": [{"hz": 3000, "db": -1.5, "q": 1.25}, {"hz": 500, "db": 1}]},
            "vocal": {"bells": [{"hz": 4000, "db": 1}]}}}});
        let (spec, probs) = validate(&v);
        let spec = spec.unwrap_or_else(|| panic!("{probs:?}"));
        let h = &spec.mix.part_eq["harmony"];
        assert_eq!(
            h.low_shelf,
            Some(EqShelf {
                hz: 150.0,
                db: -2.0
            })
        );
        assert_eq!(h.bells[1].q, 1.0, "a bell's Q defaults to 1");
        assert_eq!(h.bands().len(), 4);
        let back = serde_json::to_value(&spec.mix).unwrap();
        assert_eq!(
            back["part_eq"]["harmony"]["bells"][0],
            json!({"hz": 3000.0, "db": -1.5, "q": 1.25})
        );
        // off by default and not written when empty
        let plain = validate(&json!({"genre": "trap", "seed": 7})).0.unwrap();
        assert!(plain.mix.part_eq.is_empty());
        assert!(serde_json::to_value(&plain.mix)
            .unwrap()
            .get("part_eq")
            .is_none());
        // a band at 0 dB adds nothing
        assert!(PartEq {
            low_shelf: Some(EqShelf { hz: 100.0, db: 0.0 }),
            high_shelf: None,
            bells: vec![EqBell {
                hz: 1000.0,
                db: 0.0,
                q: 1.0,
                auto: false
            }],
            ducks: vec![]
        }
        .bands()
        .is_empty());
        let bad = |pe: Value| {
            validate(&json!({"genre": "trap", "mix": {"part_eq": pe}}))
                .1
                .into_iter()
                .map(|p| p.path)
                .collect::<Vec<_>>()
        };
        assert!(bad(json!({"": {"bells": [{"hz": 1000, "db": 1}]}}))
            .contains(&"/mix/part_eq".to_string()));
        // which part a key names is checked when the beat exists (track names are known then)
        assert!(bad(json!({"chords": {"bells": [{"hz": 1000, "db": 1}]}})).is_empty());
        assert!(bad(json!({"bass": {"low_shelf": {"hz": 100, "db": 15}}}))
            .contains(&"/mix/part_eq/bass/low_shelf/db".to_string()));
        assert!(bad(json!({"bass": {"high_shelf": {"hz": 30000, "db": 1}}}))
            .contains(&"/mix/part_eq/bass/high_shelf/hz".to_string()));
        assert!(
            bad(json!({"lead": {"bells": [{"hz": 1000, "db": 1, "q": 0}]}}))
                .contains(&"/mix/part_eq/lead/bells/0/q".to_string())
        );
        let seven: Vec<Value> = (0..7)
            .map(|i| json!({"hz": 200 * (i + 1), "db": 1}))
            .collect();
        assert!(
            bad(json!({"lead": {"bells": seven}})).contains(&"/mix/part_eq/lead/bells".to_string())
        );
        assert!(!validate(&json!({"genre": "trap", "mix": {"part_eq": {"bass": {"shelf": {"hz": 100, "db": 1}}}}})).1.is_empty(), "unknown keys are refused");
        assert_eq!(scope_of("/mix/part_eq/bass"), Scope::Full);
        assert_eq!(scope_of("/mix/part_eq"), Scope::Full);
    }

    /// Keys name a part by exact track name or by role; when both land on
    /// one track the track name's EQ is used; a role the beat does not play
    /// is skipped; a key that is neither is an error with a did-you-mean.
    #[test]
    fn part_eq_keys_are_track_names_or_roles_and_the_track_name_wins() {
        let base = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]});
        let compose = |pe: Value, tag: &str| {
            let mut v = base.clone();
            v["mix"] = json!({"part_eq": pe});
            let spec = validate(&v).0.unwrap();
            let mut e = Engine::new(
                std::env::temp_dir().join(format!("song_parteq_keys_{tag}_{}", std::process::id())),
            );
            let mut log = Vec::new();
            let r = compose_beat(&mut e, &spec, 7, &mut log);
            (r, e, log)
        };
        let eqs = |e: &Engine, t: &str| -> Vec<Value> {
            let i = e.project.track_index(t).unwrap();
            serde_json::to_value(&e.project.tracks[i].effects)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| {
                    f["type"] == "parametric_eq"
                        && f["bands"].as_array().is_some_and(|b| {
                            b.iter().any(|x| {
                                (x["freq"].as_f64().unwrap_or(0.0) - 1234.0).abs() < 1.0
                                    || (x["freq"].as_f64().unwrap_or(0.0) - 4321.0).abs() < 1.0
                            })
                        })
                })
                .cloned()
                .collect()
        };
        let by_role = json!({"bells": [{"hz": 1234, "db": 2}]});
        let by_name = json!({"bells": [{"hz": 4321, "db": -2}]});
        // the role "harmony" plays on the track "chords"
        let (r, e, _) = compose(json!({"harmony": by_role}), "role");
        r.unwrap();
        assert_eq!(eqs(&e, "chords").len(), 1);
        assert_eq!(eqs(&e, "chords")[0]["bands"][0]["freq"], 1234.0);
        let (r, e, _) = compose(json!({"chords": by_name}), "name");
        r.unwrap();
        assert_eq!(eqs(&e, "chords")[0]["bands"][0]["freq"], 4321.0);
        // both: one EQ, the track name's
        let (r, e, log) = compose(json!({"harmony": by_role, "chords": by_name}), "both");
        r.unwrap();
        let got = eqs(&e, "chords");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0]["bands"][0]["freq"], 4321.0);
        assert!(
            log.iter().any(|l| l.contains("the track name's is used")),
            "{log:?}"
        );
        // a role this beat does not play: skipped, not fatal
        let (r, _, log) = compose(json!({"tabla": by_role}), "noplay");
        r.unwrap();
        assert!(
            log.iter()
                .any(|l| l.contains("its part EQ was not applied")),
            "{log:?}"
        );
        // neither a track nor a role: an error that names the nearest and the tracks
        let (r, _, _) = compose(json!({"chordz": by_role}), "typo");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("/mix/part_eq/chordz") && err.contains("chords"),
            "{err}"
        );
    }

    /// Commit 6: a part EQ is one parametric EQ at the end of that part's
    /// chain; the other parts' chains are untouched, and in the render its
    /// low shelf and bell move that part's bands while every other stem is
    /// sample-identical.
    #[test]
    fn a_part_eq_shapes_its_part_and_nothing_else() {
        let base = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]});
        let mut with = base.clone();
        with["mix"] = json!({"part_eq": {"bass": {"low_shelf": {"hz": 200, "db": -6}, "bells": [{"hz": 1000, "db": 6, "q": 1}]}}});
        let mut zero = base.clone();
        zero["mix"] = json!({"part_eq": {"bass": {"low_shelf": {"hz": 200, "db": 0}}}});
        let build = |v: &Value, tag: &str| {
            let spec = validate(v).0.unwrap();
            let mut e = Engine::new(
                std::env::temp_dir().join(format!("song_parteq_{tag}_{}", std::process::id())),
            );
            let mut log = Vec::new();
            compose_beat(&mut e, &spec, 7, &mut log).unwrap();
            (e, log)
        };
        let (e0, _) = build(&base, "plain");
        let (e1, log1) = build(&with, "eq");
        let (ez, _) = build(&zero, "zero");
        let fx = |e: &Engine| {
            e.project
                .tracks
                .iter()
                .map(|t| (t.name.clone(), serde_json::to_value(&t.effects).unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(fx(&e0), fx(&ez), "an all-zero part EQ adds nothing");
        assert!(
            log1.iter().any(|l| l.contains("part EQ: bass 2 band(s)")),
            "{log1:?}"
        );
        for ((n0, f0), (n1, f1)) in fx(&e0).into_iter().zip(fx(&e1)) {
            assert_eq!(n0, n1);
            if n0 == "bass" {
                let (a, b) = (f0.as_array().unwrap(), f1.as_array().unwrap());
                assert_eq!(b.len(), a.len() + 1);
                assert_eq!(
                    &b[..a.len()],
                    &a[..],
                    "the part's own chain is kept, the EQ goes at its end"
                );
                let last = &b[a.len()];
                assert_eq!(last["type"], "parametric_eq");
                assert_eq!(last["bands"][0]["kind"], "low_shelf");
                assert_eq!(last["bands"][1]["kind"], "bell");
            } else {
                assert_eq!(f0, f1, "{n0}: untouched");
            }
        }
        let stems = |e: &Engine| {
            crate::render::render(
                &e.project,
                &e.bank,
                &crate::render::RenderOptions {
                    keep_stems: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let (r0, r1) = (stems(&e0), stems(&e1));
        fn stem<'a>(r: &'a crate::render::Mix, n: &str) -> &'a crate::render::Stem {
            r.stems
                .iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("no {n} stem"))
        }
        let (b0, b1) = (stem(&r0, "bass"), stem(&r1, "bass"));
        let low = b1.band_db(30.0, 100.0) - b0.band_db(30.0, 100.0);
        let mid = b1.band_db(800.0, 1250.0) - b0.band_db(800.0, 1250.0);
        eprintln!("part eq on bass: 30-100 Hz {low:+.2} dB, 800-1250 Hz {mid:+.2} dB");
        assert!(low < -4.0 && low > -8.0, "low shelf -6 dB: {low}");
        assert!(mid > 3.0 && mid < 7.0, "bell +6 dB at 1 kHz: {mid}");
        for s in &r0.stems {
            if s.name != "bass" {
                let o = stem(&r1, &s.name);
                assert!(
                    s.left == o.left && s.right == o.right,
                    "{}: sample-identical",
                    s.name
                );
            }
        }
    }

    #[test]
    fn the_vocal_part_eq_goes_after_the_vocal_chain_and_presence_db_stays_its_own_bell() {
        let v = json!({"genre": "trap", "vocal": {"file": "/tmp/x.wav", "chain": {"presence_db": 1.2, "brighten": false}}, "mix": {"part_eq": {"vocal": {"high_shelf": {"hz": 8000, "db": 2}}}}});
        let spec = validate(&v).0.unwrap();
        let d = std::env::temp_dir().join(format!("song_vox_parteq_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "vocal", "preset": "epiano"}))
            .unwrap();
        let chain = spec.vocal.as_ref().unwrap().chain.clone();
        mix_rules::apply_vocal_chain(&mut e, "vocal", &chain, "test").unwrap();
        let before = serde_json::to_value(
            &e.project.tracks[e.project.track_index("vocal").unwrap()].effects,
        )
        .unwrap();
        let mut log = Vec::new();
        let out = apply_vocal_part_eq(&mut e, &spec, "vocal", &mut log)
            .unwrap()
            .expect("applied");
        let after = serde_json::to_value(
            &e.project.tracks[e.project.track_index("vocal").unwrap()].effects,
        )
        .unwrap();
        let (a, b) = (before.as_array().unwrap(), after.as_array().unwrap());
        assert_eq!(
            &b[..a.len()],
            &a[..],
            "the vocal chain, presence bell included, is unchanged: {out}"
        );
        assert_eq!(b.len(), a.len() + 1);
        let sh = &b[a.len()]["bands"][0];
        assert_eq!(
            (
                sh["kind"].as_str(),
                sh["freq"].as_f64(),
                sh["gain_db"].as_f64()
            ),
            (Some("high_shelf"), Some(8000.0), Some(2.0)),
            "{sh}"
        );
        let bell = a.iter().any(|f| {
            f["bands"].as_array().is_some_and(|bs| {
                bs.iter().any(|x| {
                    x["kind"] == "bell"
                        && (x["freq"].as_f64().unwrap() - 2500.0).abs() < 1.0
                        && (x["gain_db"].as_f64().unwrap() - 1.2).abs() < 1e-3
                })
            })
        });
        assert!(bell, "presence_db is still its own Q1 bell at 2.5 kHz");
        // no vocal part EQ: nothing added
        let plain = validate(&json!({"genre": "trap", "vocal": {"file": "/tmp/x.wav"}}))
            .0
            .unwrap();
        assert!(apply_vocal_part_eq(&mut e, &plain, "vocal", &mut log)
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_vocal_chain_takes_a_presence_bell_on_the_vocal_alone() {
        let (spec, probs) = validate(
            &json!({"genre": "trap", "vocal": {"file": "/tmp/x.wav", "chain": {"presence_db": 0.7, "brighten": false}}}),
        );
        assert!(
            probs.iter().all(|p| !p.path.starts_with("/vocal/chain")),
            "{probs:?}"
        );
        let chain = spec.unwrap().vocal.unwrap().chain;
        assert_eq!((chain.presence_db, chain.presence_hz), (0.7, 2500.0));
        let (_, probs) = validate(
            &json!({"genre": "trap", "vocal": {"file": "/tmp/x.wav", "chain": {"presence_db": 9.0}}}),
        );
        assert!(
            probs.iter().any(|p| p.path == "/vocal/chain/presence_db"),
            "{probs:?}"
        );
        let d = std::env::temp_dir().join(format!("song_vox_bell_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "vocal", "preset": "epiano"}))
            .unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        let out = mix_rules::apply_vocal_chain(&mut e, "vocal", &chain, "test").unwrap();
        let has_bell = |e: &Engine, t: &str| {
            let i = e.project.track_index(t).unwrap();
            serde_json::to_value(&e.project.tracks[i].effects)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|f| {
                    f["bands"].as_array().is_some_and(|b| {
                        b.iter().any(|x| {
                            x["kind"] == "bell"
                                && (x["freq"].as_f64().unwrap_or(0.0) - 2500.0).abs() < 1.0
                                && (x["gain_db"].as_f64().unwrap_or(0.0) - 0.7).abs() < 1e-3
                        })
                    })
                })
        };
        assert!(has_bell(&e, "vocal"), "{out}");
        assert!(!has_bell(&e, "keys"), "the beat is untouched");
        // in the render: the vocal stem changes, every other stem is identical
        let notes: Vec<_> = (0..4)
            .map(|i| json!({"start": i * 16, "len": 16, "pitch": "C5"}))
            .collect();
        e.call("add_notes", &json!({"track": "vocal", "notes": notes}))
            .unwrap();
        let notes: Vec<_> = (0..4)
            .map(|i| json!({"start": i * 16, "len": 16, "pitch": "C4"}))
            .collect();
        e.call("add_notes", &json!({"track": "keys", "notes": notes}))
            .unwrap();
        let stems = |e: &Engine| {
            crate::render::render(
                &e.project,
                &e.bank,
                &crate::render::RenderOptions {
                    keep_stems: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let with_bell = stems(&e);
        let mut flat = chain.clone();
        flat.presence_db = 0.0;
        mix_rules::apply_vocal_chain(&mut e, "vocal", &flat, "test").unwrap();
        let without = stems(&e);
        let stem = |m: &crate::render::Mix, t: &str| {
            m.stems
                .iter()
                .find(|s| s.name == t)
                .map(|s| s.left.clone())
                .unwrap()
        };
        let (kv, kn) = (stem(&with_bell, "keys"), stem(&without, "keys"));
        assert!(
            kv.iter().zip(&kn).all(|(a, b)| (a - b).abs() < 1e-7),
            "keys stem must be untouched"
        );
        let (vv, vn) = (stem(&with_bell, "vocal"), stem(&without, "vocal"));
        let d: f32 = vv
            .iter()
            .zip(&vn)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        let base: f32 = vn.iter().map(|a| a * a).sum::<f32>().sqrt();
        assert!(
            d > base * 1e-3 && d < base * 0.2,
            "the vocal stem moves a little: diff {d} of {base}"
        );
        // 0 dB (the default) adds no bell
        let mut plain = chain.clone();
        plain.presence_db = 0.0;
        mix_rules::apply_vocal_chain(&mut e, "vocal", &plain, "test").unwrap();
        assert!(!has_bell(&e, "vocal"));
    }

    #[test]
    fn mix_builds_write_a_riser_a_pre_drop_gap_and_a_reverb_throw() {
        let v = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}], "mix": {"builds": true}});
        let (spec, probs) = validate(&v);
        let spec = spec.unwrap_or_else(|| panic!("{probs:?}"));
        assert!(spec.mix.builds);
        assert_eq!(scope_of("/mix/builds"), Scope::Full);
        let d = std::env::temp_dir().join(format!("song_builds_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        let mut log = Vec::new();
        compose_beat(&mut e, &spec, 7, &mut log).unwrap();
        let built = log
            .iter()
            .find(|l| l.starts_with("builds:"))
            .unwrap_or_else(|| panic!("{log:?}"));
        let s = mix_rules::section_spans(&e.project)
            .iter()
            .find(|x| x.kind == "hook")
            .map(|x| x.start_beat)
            .unwrap();
        assert_eq!(s, 16.0, "{built}");
        // the drum bus: silent through the last beat, back on the downbeat
        let gap = e
            .project
            .automation
            .iter()
            .find(|l| l.target == "drums" && l.param.ends_with(".db"))
            .unwrap_or_else(|| panic!("{built}"));
        assert_eq!(gap.value_at(s - 2.0), Some(0.0));
        assert_eq!(gap.value_at(s - 0.5), Some(-60.0));
        assert_eq!(gap.value_at(s + 0.5), Some(0.0));
        // the riser: ~500 Hz two bars out, open by the hook
        let riser = e
            .project
            .automation
            .iter()
            .find(|l| l.target == "chords" && l.param.ends_with(".cutoff"))
            .unwrap_or_else(|| panic!("{built}"));
        assert!(
            (riser.value_at(s - 8.0).unwrap() - 500.0).abs() < 1.0,
            "{built}"
        );
        assert!(riser.value_at(s - 0.01).unwrap() >= 16000.0);
        assert!(riser.value_at_in(s - 4.0, true).unwrap() > 1000.0);
        // the throw: a send lane on lead or chords, up over the gap
        let throw = e
            .project
            .automation
            .iter()
            .find(|l| l.param.starts_with("send."))
            .unwrap_or_else(|| panic!("{built}"));
        assert!(
            throw.value_at(s - 0.5).unwrap() > throw.value_at(s - 2.0).unwrap() + 6.0,
            "{built}"
        );
        assert!(crate::validate::structural(&e.project, &e.bank)
            .iter()
            .all(|x| !(x.check == "automation" && x.status == crate::validate::Status::Fail)));
        // a short (2-bar) intro before a hook gets a one-bar riser
        let (short, _) = validate(
            &json!({"genre": "trap", "seed": 7, "structure": [{"kind": "intro", "bars": 2}, {"kind": "hook", "bars": 4}], "mix": {"builds": true}}),
        );
        let mut e3 = Engine::new(
            std::env::temp_dir().join(format!("song_builds_short_{}", std::process::id())),
        );
        let mut log3 = Vec::new();
        compose_beat(&mut e3, &short.unwrap(), 7, &mut log3).unwrap();
        let r3 = e3
            .project
            .automation
            .iter()
            .find(|l| l.target == "chords" && l.param.ends_with(".cutoff"));
        if let Some(r3) = r3 {
            assert!((r3.value_at(4.0).unwrap() - 500.0).abs() < 1.0, "{log3:?}");
        }
        assert!(
            e3.project
                .automation
                .iter()
                .any(|l| l.target == "drums" && l.value_at(7.5) == Some(-60.0)),
            "{log3:?}"
        );
        // off by default: the same song without builds has none of it
        let (plain, _) = validate(
            &json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]}),
        );
        let mut e2 =
            Engine::new(std::env::temp_dir().join(format!("song_nobuilds_{}", std::process::id())));
        compose_beat(&mut e2, &plain.unwrap(), 7, &mut Vec::new()).unwrap();
        assert!(e2
            .project
            .automation
            .iter()
            .all(|l| l.target != "drums" && !l.param.starts_with("send.")));
    }

    #[test]
    fn section_gain_stages_hooks_over_verses_on_the_master_buses() {
        let v = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}], "mix": {"section_gain_db": {"verse": -1.2, "hook": 1.5}}});
        let (spec, probs) = validate(&v);
        let spec = spec.unwrap_or_else(|| panic!("{probs:?}"));
        assert_eq!(spec.mix.section_gain_db.get("hook"), Some(&1.5));
        let (_, bad) = validate(
            &json!({"genre": "trap", "seed": 7, "structure": [{"kind": "hook", "bars": 4}], "mix": {"section_gain_db": {"hook": 9}}}),
        );
        assert!(
            bad.iter().any(|p| p.path == "/mix/section_gain_db/hook"),
            "{bad:?}"
        );
        let d = std::env::temp_dir().join(format!("song_secgain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        compose_beat(&mut e, &spec, 7, &mut Vec::new()).unwrap();
        // the mix stage routes the beat into its bus before section gain runs
        mix_rules::route_beat_bus(&mut e, None);
        let rms_db = |m: &crate::render::Mix, b0: f32, b1: f32, bpm: f32| {
            let sr = crate::dsp::SR;
            let (a, b) = (
                (b0 * 60.0 / bpm * sr) as usize,
                ((b1 * 60.0 / bpm * sr) as usize).min(m.left.len()),
            );
            let s: f32 = m.left[a..b]
                .iter()
                .chain(m.right[a..b].iter())
                .map(|x| x * x)
                .sum();
            10.0 * (s / (2 * (b - a)).max(1) as f32 + 1e-12).log10()
        };
        let bpm = e.project.bpm;
        let before = e.mix().unwrap();
        let lift0 = rms_db(&before, 17.0, 31.0, bpm) - rms_db(&before, 1.0, 15.0, bpm);
        let out = apply_section_gain(&mut e, &spec).unwrap();
        // the buses that feed the master carry it; tracks and the drum sub-bus do not
        let beat = e
            .project
            .automation
            .iter()
            .find(|l| l.target == "beat" && l.param == "volume")
            .unwrap_or_else(|| panic!("{out}"));
        let base = e
            .project
            .buses
            .iter()
            .find(|b| b.name == "beat")
            .unwrap()
            .volume_db;
        assert!(
            (beat.value_at(8.0).unwrap() - (base - 1.2)).abs() < 0.01,
            "{out}"
        );
        assert!(
            (beat.value_at(24.0).unwrap() - (base + 1.5)).abs() < 0.01,
            "{out}"
        );
        assert!(
            e.project
                .automation
                .iter()
                .all(|l| !(l.target == "drums" && l.param == "volume")),
            "{out}"
        );
        let after = e.mix().unwrap();
        let lift1 = rms_db(&after, 17.0, 31.0, bpm) - rms_db(&after, 1.0, 15.0, bpm);
        eprintln!(
            "section gain: hook over verse {lift0:.2} -> {lift1:.2} dB (through the master chain)"
        );
        assert!(
            lift1 - lift0 > 1.0 && lift1 - lift0 < 3.2,
            "hook over verse moved {lift0} -> {lift1}"
        );
        // rebuilt, not stacked, on a second call
        let n = e.project.automation.len();
        apply_section_gain(&mut e, &spec).unwrap();
        assert_eq!(e.project.automation.len(), n);
        // off by default: no bus lane
        let (plain, _) = validate(
            &json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]}),
        );
        let mut e2 = Engine::new(
            std::env::temp_dir().join(format!("song_nosecgain_{}", std::process::id())),
        );
        let plain = plain.unwrap();
        compose_beat(&mut e2, &plain, 7, &mut Vec::new()).unwrap();
        assert_eq!(apply_section_gain(&mut e2, &plain).unwrap(), json!([]));
        assert!(e2
            .project
            .automation
            .iter()
            .all(|l| !(l.target == "beat" && l.param == "volume")));
    }

    #[test]
    fn section_gain_offsets_the_vocal_ride_without_flattening_it() {
        use crate::automation::{AutoPoint, AutomationLane, Curve};
        let v = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}], "mix": {"section_gain_db": {"verse": -1.2, "hook": 1.5}}});
        let spec = validate(&v).0.unwrap();
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("song_vocoff_{}", std::process::id())));
        compose_beat(&mut e, &spec, 7, &mut Vec::new()).unwrap();
        // "lead" stands in for the vocal: a ride the balance wrote (verse -3, hook -1)
        let mut lane = AutomationLane::new("lead", "volume");
        lane.points = vec![
            AutoPoint {
                beat: 0.0,
                value: -3.0,
                curve: Curve::Step,
            },
            AutoPoint {
                beat: 15.0,
                value: -1.0,
                curve: Curve::Step,
            },
        ];
        e.project.automation.push(lane);
        let vals = |e: &Engine| {
            e.project
                .automation
                .iter()
                .find(|l| l.target == "lead" && l.param == "volume")
                .unwrap()
                .points
                .iter()
                .map(|p| p.value)
                .collect::<Vec<_>>()
        };
        let close = |a: &[f32], b: &[f32]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
        };
        let mut st = None;
        let out = apply_vocal_section_offset(&mut e, &spec, Some("lead"), &mut st).unwrap();
        // the ride's shape (hook 2 dB over verse) stays; verse -1.2, hook +1.5 on top
        assert!(close(&vals(&e), &[-4.2, 0.5]), "{:?} {out}", vals(&e));
        // every delivery round calls it again: not stacked
        apply_vocal_section_offset(&mut e, &spec, Some("lead"), &mut st).unwrap();
        assert!(close(&vals(&e), &[-4.2, 0.5]), "{:?}", vals(&e));
        // the balance re-writes the lane from ours (+0.5 in the hook): that move is kept, the offset not doubled
        e.project
            .automation
            .iter_mut()
            .find(|l| l.target == "lead" && l.param == "volume")
            .unwrap()
            .points[1]
            .value += 0.5;
        apply_vocal_section_offset(&mut e, &spec, Some("lead"), &mut st).unwrap();
        assert!(close(&vals(&e), &[-4.2, 1.0]), "{:?}", vals(&e));
        assert!(close(
            &st.as_ref()
                .unwrap()
                .0
                .iter()
                .map(|p| p.value)
                .collect::<Vec<_>>(),
            &[-3.0, -0.5]
        ));
        // off by default: the ride is left alone
        let plain = validate(&json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]})).0.unwrap();
        let mut st2 = None;
        assert_eq!(
            apply_vocal_section_offset(&mut e, &plain, Some("lead"), &mut st2).unwrap(),
            json!([])
        );
        assert!(close(&vals(&e), &[-4.2, 1.0]));
    }

    #[test]
    fn verse_thinning_works_when_the_form_comes_from_the_arrangement() {
        let v = json!({"genre": "trap", "seed": 7, "structure": [{"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}]});
        let spec = validate(&v).0.unwrap();
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("song_thin_{}", std::process::id())));
        compose_beat(&mut e, &spec, 7, &mut Vec::new()).unwrap();
        // the producer plan's form: no spec structure (as in a song built without one)
        let mut spec_e = spec.clone();
        spec_e.structure.clear();
        let mut t = spec.targets();
        t.drop_lift_db = [20.0, 21.0];
        let out = arrangement_lift(&mut e, &mut spec_e, None, &t).unwrap();
        assert_eq!(out["changed"], true, "{out}");
        assert_eq!(out["sections_from"], "arrangement", "{out}");
        assert!(out["sections_thinned"].as_u64().unwrap() >= 1, "{out}");
        let verse = spec_e
            .structure
            .iter()
            .find(|s| canon_kind(&s.kind) == "verse")
            .unwrap();
        assert!(verse.energy.unwrap() <= -3.9, "{out}");
        // a layer actually plays softer in the verse than in the hook
        let thin = e
            .project
            .automation
            .iter()
            .filter(|l| {
                l.param == "volume"
                    && !energy_floor_layer(&l.target)
                    && e.project.tracks.iter().any(|t| t.name == l.target)
            })
            .any(|l| l.value_at(8.0).unwrap() < l.value_at(24.0).unwrap() - 3.0);
        assert!(thin, "{out}");
        assert!(
            out["lift_db"].as_f64().unwrap() > out["lift_db_before"].as_f64().unwrap(),
            "{out}"
        );
        // kick and bass are never thinned
        assert!(e
            .project
            .automation
            .iter()
            .all(|l| !(l.param == "volume" && energy_floor_layer(&l.target))));
    }

    #[test]
    fn every_problem_comes_with_a_path_and_a_fix() {
        let v = json!({"genre": "desi", "tempo": 100, "structure": [{"kind": "hok", "bars": 8}, {"kind": "verse", "bars": 99}], "master": {"lufs": -3}});
        let (_, probs) = validate(&v);
        assert!(
            probs
                .iter()
                .any(|x| x.path == "/tempo" && x.fix.contains("bpm")),
            "{probs:?}"
        );
        let v = json!({"genre": "dezi_hiphopp", "bpm": 300, "structure": [{"kind": "hok", "bars": 8}, {"kind": "verse", "bars": 99}], "master": {"lufs": -3}});
        let (_, probs) = validate(&v);
        let paths: Vec<&str> = probs.iter().map(|x| x.path.as_str()).collect();
        for want in [
            "/genre",
            "/bpm",
            "/structure/0/kind",
            "/structure/1/bars",
            "/master/lufs",
        ] {
            assert!(paths.contains(&want), "{want} missing in {paths:?}");
        }
        assert!(probs
            .iter()
            .find(|x| x.path == "/structure/0/kind")
            .unwrap()
            .fix
            .contains("hook"));
        assert!(probs
            .iter()
            .find(|x| x.path == "/genre")
            .unwrap()
            .fix
            .contains("desi_hiphop"));
    }

    #[test]
    fn a_good_spec_validates_and_names_sections() {
        let mut v = example();
        v.as_object_mut().unwrap().remove("vocal");
        v["export"] = json!({"format": "wav"});
        let (s, probs) = validate(&v);
        assert!(probs.is_empty(), "{probs:?}");
        assert_eq!(
            s.unwrap().section_names(),
            vec!["intro1", "verse1", "hook1", "verse2", "hook2", "outro1"]
        );
    }

    #[test]
    fn phrases_are_checked_against_the_structure() {
        let mut v = example();
        v["vocal"] = json!({"file": "/etc/hostname", "phrases": [{"src_s": 0, "len_s": 2, "section": "hook", "bar": 1}, {"src_s": 0, "len_s": 2, "section": "hook1", "bar": 12}, {"src_s": 1}]});
        let (_, probs) = validate(&v);
        let paths: Vec<&str> = probs.iter().map(|x| x.path.as_str()).collect();
        assert!(paths.contains(&"/vocal/phrases/0/section"), "{paths:?}"); // hook occurs twice
        assert!(paths.contains(&"/vocal/phrases/1/bar"), "{paths:?}");
        assert!(paths.contains(&"/vocal/phrases/2"), "{paths:?}");
    }

    #[test]
    fn patch_paths_pick_the_smallest_redo() {
        assert_eq!(scope_of("/mix/vocal_over_beat_db"), Scope::Mix);
        assert_eq!(scope_of("/vocal/gain_db"), Scope::Mix);
        assert_eq!(scope_of("/structure/1/energy"), Scope::Mix);
        assert_eq!(scope_of("/structure/1/bars"), Scope::Full);
        assert_eq!(scope_of("/mix/presence_ceiling_db"), Scope::Mix);
        assert_eq!(scope_of("/mix/levels/bass"), Scope::Full);
        assert_eq!(scope_of("/mix/levels"), Scope::Full);
        assert_eq!(scope_of("/vocal/phrases/3/beat"), Scope::Vocal);
        assert_eq!(scope_of("/bpm"), Scope::Full);
        assert_eq!(scope_of("/structure/1/bars"), Scope::Full);
        let mut v = json!({"a": [1, 2]});
        set_pointer(&mut v, "/b/c", json!(3)).unwrap();
        set_pointer(&mut v, "/a/-", json!(9)).unwrap();
        set_pointer(&mut v, "/a/0", Value::Null).unwrap();
        assert_eq!(v, json!({"a": [2, 9], "b": {"c": 3}}));
        assert!(set_pointer(&mut v, "a", json!(1)).is_err());
    }

    #[test]
    fn keys_split() {
        assert_eq!(split_key("D minor"), ("D".into(), Some("minor".into())));
        assert_eq!(split_key("F#m"), ("F#".into(), Some("minor".into())));
        assert_eq!(split_key("Bb"), ("Bb".into(), None));
    }

    #[test]
    fn song_create_takes_improve_beside_the_spec() {
        let tool = tools()
            .into_iter()
            .find(|t| t.name == "song_create")
            .unwrap();
        assert!(
            (tool.schema)()["properties"]["improve"].is_object(),
            "improve must pass the argument check"
        );
    }

    #[test]
    fn validate_names_the_genre_a_word_resolved_to_and_its_sections() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_song_validate_test"));
        let v = validate_tool(&mut e, &json!({"genre": "honey_singh"})).unwrap();
        assert_eq!(v["valid"], true);
        assert_eq!(v["genre"]["resolved"], "punjabi_party");
        let secs: Vec<String> = serde_json::from_value(v["sections"].clone()).unwrap();
        assert_eq!(secs.first().map(String::as_str), Some("intro1"));
        assert!(secs.iter().any(|s| s == "hook2"));
        assert!(v["sections_from"].as_str().unwrap().contains("structure"));
        // a given structure is the contract: its names, no template note
        let w = validate_tool(
            &mut e,
            &json!({"genre": "badshah", "structure": [{"kind": "verse", "bars": 8}]}),
        )
        .unwrap();
        assert_eq!(w["sections"], json!(["verse1"]));
        assert!(w.get("sections_from").is_none());
    }

    fn auto_spec(dir: &Path, extra: Value) -> Value {
        let mut v = json!({"genre": "boom_bap", "bpm": 90, "seed": 3,
            "structure": [{"kind": "intro", "bars": 2}, {"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}],
            "export": {"path": dir.join("t.wav").to_string_lossy(), "format": "wav"}});
        if let Value::Object(m) = extra {
            for (k, x) in m {
                v[k] = x;
            }
        }
        v
    }

    fn auto_bands(e: &Engine) -> Vec<(String, f32, f32, f32)> {
        let mut out = Vec::new();
        for t in &e.project.tracks {
            for f in &t.effects {
                if let crate::fx::Effect::ParametricEq(p) = f {
                    out.extend(
                        p.bands
                            .iter()
                            .filter(|b| b.auto)
                            .map(|b| (t.name.clone(), b.freq, b.gain_db, b.q)),
                    );
                }
            }
        }
        out
    }

    #[test]
    fn auto_eq_off_is_the_same_song_as_no_auto_eq_key() {
        let dir = std::env::temp_dir().join(format!("bb_autoeq_off_{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut e1 = Engine::new(a.clone());
        let r1 = e1.call("song_create", &auto_spec(&a, json!({}))).unwrap();
        let mut e2 = Engine::new(b.clone());
        let r2 = e2.call("song_create", &auto_spec(&b, json!({"mix": {"auto_eq": false, "vocal_duck": false, "kick_duck": false, "vocal_resonance": false}}))).unwrap();
        assert_eq!(
            std::fs::read(r1["audio"].as_str().unwrap()).unwrap(),
            std::fs::read(r2["audio"].as_str().unwrap()).unwrap(),
            "auto_eq false renders the same bytes"
        );
        assert_eq!(
            serde_json::to_value(&e1.project.tracks).unwrap(),
            serde_json::to_value(&e2.project.tracks).unwrap()
        );
        assert!(r2["mix"].get("auto_eq").is_none());
        assert!(auto_bands(&e2).is_empty());
        assert!(!r2["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("auto EQ")));
        let sp: SongSpec =
            serde_json::from_value(json!({"genre": "trap", "mix": {"auto_eq": false}})).unwrap();
        assert!(
            serde_json::to_value(&sp.mix)
                .unwrap()
                .get("auto_eq")
                .is_none(),
            "false is not written back"
        );
        assert!(r2["mix"].get("duck").is_none());
        assert!(
            r2["mix"].get("vocal_resonance").is_none(),
            "off: no resonance bands"
        );
        let sp: SongSpec =
            serde_json::from_value(json!({"genre": "trap", "mix": {"vocal_resonance": false}}))
                .unwrap();
        assert!(serde_json::to_value(&sp.mix)
            .unwrap()
            .get("vocal_resonance")
            .is_none());
        let sp: SongSpec = serde_json::from_value(
            json!({"genre": "trap", "mix": {"vocal_duck": false, "kick_duck": false}}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&sp.mix)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| k.contains("duck"))
                .count(),
            0
        );
        assert!(
            serde_json::to_value(crate::fx::EqBand::default())
                .unwrap()
                .get("auto")
                .is_none(),
            "a band not written by the auto-EQ is stored as before"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_eq_cuts_the_bass_not_the_kick_writes_auto_bells_and_rebuilds_clean() {
        let dir = std::env::temp_dir().join(format!("bb_autoeq_on_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let r = e
            .call(
                "song_create",
                &auto_spec(&dir, json!({"mix": {"auto_eq": true}})),
            )
            .unwrap();
        let ae = &r["mix"]["auto_eq"];
        let pair = ae["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["pair"] == "kick/bass")
            .cloned()
            .expect("kick/bass measured");
        assert!(
            pair["why"].as_str().unwrap().contains("the bass is cut"),
            "{pair}"
        );
        let cuts = ae["cuts"].as_array().unwrap().clone();
        assert!(!cuts.is_empty(), "the kick and bass collide here: {ae}");
        assert!(cuts.len() <= crate::auto_eq::MAX_AUTO_BELLS, "{ae}");
        for c in &cuts {
            assert_eq!(c["part"], "bass", "{c}");
            let db = c["db"].as_f64().unwrap();
            assert!((-3.0..=-1.0).contains(&db), "{c}");
        }
        assert!(
            pair["overlap_after_pct"].as_f64().unwrap()
                < pair["overlap_before_pct"].as_f64().unwrap(),
            "{pair}"
        );
        let bands = auto_bands(&e);
        assert_eq!(bands.len(), cuts.len());
        assert!(
            bands
                .iter()
                .all(|b| b.0 == "bass" && b.2 <= -1.0 && b.2 >= -3.0),
            "{bands:?}"
        );
        // the delivered spec carries them, marked auto
        let stored = |e: &Engine| e.project.song.as_ref().unwrap().spec.clone();
        let bells = stored(&e)["mix"]["part_eq"]["bass"]["bells"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(bells.len(), cuts.len());
        assert!(bells.iter().all(|b| b["auto"] == true), "{bells:?}");
        assert!(
            r["log"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l.as_str().unwrap().starts_with("auto EQ: bass ")),
            "listed in the log"
        );
        // a mix-only rebuild measures again: no pile-up
        e.call("song_patch", &json!({"set": {"/master/lufs": -12}}))
            .unwrap();
        assert_eq!(auto_bands(&e).len(), cuts.len());
        assert_eq!(
            stored(&e)["mix"]["part_eq"]["bass"]["bells"]
                .as_array()
                .unwrap()
                .len(),
            cuts.len()
        );
        // switched off: the auto bands and bells go
        let r3 = e
            .call("song_patch", &json!({"set": {"/mix/auto_eq": false}}))
            .unwrap();
        assert!(auto_bands(&e).is_empty(), "{:?}", auto_bands(&e));
        assert!(
            stored(&e).pointer("/mix/part_eq").is_none(),
            "{}",
            stored(&e)
        );
        assert!(r3["mix"].get("auto_eq").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_eq_never_cuts_the_protected_part_and_keeps_user_bands_first() {
        let dir = std::env::temp_dir().join(format!("bb_autoeq_user_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let spec_v = auto_spec(
            &dir,
            json!({"mix": {"part_eq": {"bass": {"bells": [{"hz": 3000, "db": 1.0, "q": 1.0}]}}}}),
        );
        e.call("song_create", &spec_v).unwrap();
        let voice = ["chords", "lead", "counter"]
            .into_iter()
            .find(|n| e.project.track_index(n).is_ok())
            .expect("a melodic part")
            .to_string();
        let vi = e.project.track_index(&voice).unwrap();
        let voice_fx = serde_json::to_value(&e.project.tracks[vi].effects).unwrap();
        let mut spec = validate(&spec_v).0.unwrap();
        spec.mix.auto_eq = true;
        let mut log = Vec::new();
        // a melodic part plays the vocal: measured against every part, never cut
        let r = apply_auto_eq(&mut e, &spec, Some(&voice), &mut log).unwrap();
        assert!(
            r["pairs"].as_array().unwrap().iter().any(|p| p["pair"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{voice}/"))),
            "{r}"
        );
        let cuts = r["cuts"].as_array().unwrap();
        assert!(!cuts.is_empty(), "{r}");
        assert!(cuts.iter().all(|c| c["part"] != voice.as_str()), "{r}");
        assert_eq!(
            serde_json::to_value(&e.project.tracks[vi].effects).unwrap(),
            voice_fx,
            "the protected part is untouched"
        );
        let mut per: BTreeMap<String, usize> = BTreeMap::new();
        for c in cuts {
            *per.entry(c["part"].as_str().unwrap().to_string())
                .or_default() += 1;
            assert!((-3.0..=-1.0).contains(&c["db"].as_f64().unwrap()), "{c}");
        }
        assert!(per.values().all(|n| *n <= 2), "{per:?}");
        // the user's bass bell keeps its place and values; auto bells follow it in the same EQ
        if cuts.iter().any(|c| c["part"] == "bass") {
            let bi = e.project.track_index("bass").unwrap();
            let eq = e.project.tracks[bi]
                .effects
                .iter()
                .rev()
                .find_map(|f| match f {
                    crate::fx::Effect::ParametricEq(p) if p.bands.iter().any(|b| b.auto) => {
                        Some(p.clone())
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                (
                    eq.bands[0].freq,
                    eq.bands[0].gain_db,
                    eq.bands[0].q,
                    eq.bands[0].auto
                ),
                (3000.0, 1.0, 1.0, false),
                "{eq:?}"
            );
            assert!(
                eq.bands[1..].iter().all(|b| b.auto) && eq.bands.len() <= 7,
                "{eq:?}"
            );
            assert!(cuts
                .iter()
                .filter(|c| c["part"] == "bass")
                .all(|c| c["merged_into_user_eq"] == true && c["key"] == "bass"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn duck_ids(e: &Engine) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for t in &e.project.tracks {
            for f in &t.effects {
                if f.id().starts_with("auto_duck") {
                    let v = serde_json::to_value(f).unwrap();
                    out.push((
                        t.name.clone(),
                        f.id().to_string(),
                        v["source"].as_str().unwrap_or("").to_string(),
                    ));
                }
            }
        }
        out
    }

    #[test]
    fn the_kick_duck_keys_the_bass_by_the_kick_and_rebuilds_clean() {
        let dir = std::env::temp_dir().join(format!("bb_kickduck_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let r = e
            .call(
                "song_create",
                &auto_spec(&dir, json!({"mix": {"kick_duck": true}})),
            )
            .unwrap();
        let d = &r["mix"]["duck"];
        let ducks = d["ducks"].as_array().unwrap().clone();
        assert_eq!(ducks.len(), 1, "{d}");
        assert_eq!(
            (ducks[0]["part"].as_str(), ducks[0]["source"].as_str()),
            (Some("bass"), Some("kick")),
            "{d}"
        );
        assert_eq!(ducks[0]["max_db"], -4.0);
        assert!(
            ducks[0]["release_ms"].as_f64().unwrap() <= 120.0 && ducks[0]["attack_ms"] == 10.0,
            "{d}"
        );
        let row = d["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["pair"] == "kick/bass")
            .unwrap()
            .clone();
        assert!(
            row["overlap_after_pct"].as_f64().unwrap()
                < row["overlap_before_pct"].as_f64().unwrap(),
            "{row}"
        );
        assert_eq!(
            duck_ids(&e),
            vec![(
                "bass".to_string(),
                AUTO_DUCK_KICK_ID.to_string(),
                "kick".to_string()
            )]
        );
        assert!(
            auto_bands(&e).is_empty(),
            "the duck alone adds no static bells"
        );
        assert_eq!(ducks[0]["key_mode"], "hits");
        let stored = |e: &Engine| e.project.song.as_ref().unwrap().spec.clone();
        // visible in the stored spec, marked auto, and no pile-up on a rebuild
        let sd = stored(&e)["mix"]["part_eq"]["bass"]["ducks"].clone();
        assert_eq!(sd.as_array().map(|a| a.len()), Some(1), "{sd}");
        assert_eq!(
            (
                sd[0]["auto"].clone(),
                sd[0]["source"].clone(),
                sd[0]["max_db"].clone()
            ),
            (json!(true), json!("kick"), json!(-4.0)),
            "{sd}"
        );
        e.call("song_patch", &json!({"set": {"/master/lufs": -12}}))
            .unwrap();
        assert_eq!(duck_ids(&e).len(), 1, "no pile-up");
        assert_eq!(
            stored(&e)["mix"]["part_eq"]["bass"]["ducks"]
                .as_array()
                .map(|a| a.len()),
            Some(1)
        );
        e.call("song_patch", &json!({"set": {"/mix/kick_duck": false}}))
            .unwrap();
        assert!(duck_ids(&e).is_empty());
        assert!(
            stored(&e).pointer("/mix/part_eq/bass/ducks").is_none(),
            "off: the stored ducks go too"
        );
        // a duck written by hand is refused with the fix
        let mut hand = auto_spec(&dir, json!({}));
        hand["mix"]["part_eq"] =
            json!({"bass": {"ducks": [{"hz": 60.0, "max_db": -3.0, "q": 2.0, "source": "kick"}]}});
        let err = e.call("song_create", &hand).unwrap_err().to_string();
        assert!(err.contains("ducks are placed by the build"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vocal_ducks_key_every_clashing_part_by_the_vocal_and_never_duck_it() {
        let dir = std::env::temp_dir().join(format!("bb_vocalduck_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let spec_v = auto_spec(&dir, json!({}));
        e.call("song_create", &spec_v).unwrap();
        let voice = ["chords", "lead", "counter"]
            .into_iter()
            .find(|n| e.project.track_index(n).is_ok())
            .unwrap()
            .to_string();
        let vi = e.project.track_index(&voice).unwrap();
        let voice_fx = serde_json::to_value(&e.project.tracks[vi].effects).unwrap();
        let mut spec = validate(&spec_v).0.unwrap();
        spec.mix.vocal_duck = true;
        let mut log = Vec::new();
        let r = apply_auto_duck(&mut e, &spec, Some(&voice), &mut log).unwrap();
        let ducks = r["ducks"].as_array().unwrap();
        assert!(!ducks.is_empty(), "{r}");
        for d in ducks {
            assert_ne!(d["part"], voice.as_str());
            assert_eq!(d["source"], voice.as_str());
            let hz = d["hz"].as_f64().unwrap();
            assert!(
                (150.0..=1000.0).contains(&hz)
                    && d["max_db"] == -4.0
                    && d["release_ms"] == 100.0
                    && d["attack_ms"] == 12.0
                    && d["key_mode"] == "level",
                "{d}"
            );
            let row = r["pairs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["pair"] == d["pair"])
                .unwrap();
            assert!(
                row["overlap_after_pct"].as_f64().unwrap()
                    <= row["overlap_before_pct"].as_f64().unwrap(),
                "{row}"
            );
        }
        assert_eq!(
            serde_json::to_value(&e.project.tracks[vi].effects).unwrap(),
            voice_fx,
            "the protected part is untouched"
        );
        assert!(duck_ids(&e)
            .iter()
            .all(|(t, id, src)| *t != voice && id == AUTO_DUCK_VOCAL_ID && *src == voice));
        // the vocal ducks follow the vocal's level (an envelope follower)
        for t in &e.project.tracks {
            for f in &t.effects {
                if let crate::fx::Effect::DynamicEq(d) = f {
                    if f.id() == AUTO_DUCK_VOCAL_ID {
                        assert_eq!(
                            (d.key.as_str(), d.attack_ms, d.release_ms),
                            ("level", 12.0, 100.0)
                        );
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resonance_bands_stay_on_the_vocal_and_cut_only() {
        let dir = std::env::temp_dir().join(format!("bb_resonance_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("song_create", &auto_spec(&dir, json!({}))).unwrap();
        let voice = ["chords", "lead", "counter"]
            .into_iter()
            .find(|n| e.project.track_index(n).is_ok())
            .unwrap()
            .to_string();
        let others: Vec<Value> = e
            .project
            .tracks
            .iter()
            .filter(|t| t.name != voice)
            .map(|t| serde_json::to_value(&t.effects).unwrap())
            .collect();
        let mut log = Vec::new();
        let r = apply_vocal_resonance(&mut e, Some(&voice), &mut log).unwrap();
        let bands = r["bands"].as_array().unwrap();
        assert!(bands.len() <= crate::resonance::MAX_BANDS, "{r}");
        for b in bands {
            assert!(
                b["max_db"].as_f64().unwrap() < 0.0 && b["max_db"].as_f64().unwrap() >= -4.0,
                "cuts only, at most 4 dB: {b}"
            );
            assert_eq!(b["track"], voice.as_str());
        }
        let vi = e.project.track_index(&voice).unwrap();
        let n = e.project.tracks[vi]
            .effects
            .iter()
            .filter(|f| f.id().starts_with(AUTO_RESONANCE_ID))
            .count();
        assert_eq!(n, bands.len());
        let now: Vec<Value> = e
            .project
            .tracks
            .iter()
            .filter(|t| t.name != voice)
            .map(|t| serde_json::to_value(&t.effects).unwrap())
            .collect();
        assert_eq!(now, others, "the beat is untouched by the resonance tamer");
        // a rebuild takes every auto band out again
        strip_auto_ducks(&mut e);
        assert!(e.project.tracks.iter().all(|t| t
            .effects
            .iter()
            .all(|f| !f.id().starts_with(AUTO_RESONANCE_ID))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instrumental_song_builds_delivers_and_checks() {
        let dir = std::env::temp_dir().join("bb_song_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let spec = json!({"genre": "boom_bap", "bpm": 90, "seed": 3,
            "structure": [{"kind": "intro", "bars": 2}, {"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}],
            "export": {"path": dir.join("t.wav").to_string_lossy(), "format": "wav"}});
        let r = e.call("song_create", &spec).unwrap();
        assert!(std::path::Path::new(r["audio"].as_str().unwrap()).is_file());
        let names: Vec<&str> = r["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["intro1", "verse1", "hook1"]);
        assert_eq!(r["sections"][1]["bars"], 4.0);
        let checks = r["checks"]["checks"].as_array().unwrap();
        assert!(checks.iter().any(|c| c["check"] == "loudness"));
        assert!(
            checks
                .iter()
                .any(|c| c["check"] == "true_peak" && c["pass"] == true),
            "{checks:?}"
        );
        // a mix-only patch keeps the beat
        let before = e.project.patterns.clone();
        let r2 = e
            .call("song_patch", &json!({"set": {"/master/lufs": -12}}))
            .unwrap();
        assert_eq!(r2["scope"], "mix");
        assert_eq!(e.project.patterns, before);
        assert_eq!(
            e.call("song_get", &json!({})).unwrap()["spec"]["master"]["lufs"],
            -12
        );
        // one undo step back to the first song
        e.call("undo", &json!({})).unwrap();
        assert_ne!(e.project.song.as_ref().unwrap().spec["master"]["lufs"], -12);
    }
}
