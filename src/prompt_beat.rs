//! Prompt-to-beat and lyrics-to-beat: free text ("Bohemia type beat with a
//! beat switch and a quiet section", or a page of lyrics) becomes a
//! producer plan (genre, tempo, key, mood, palette, purposeful contrasts,
//! length) that `produce_track` builds, listens to and revises.

use serde::Serialize;
use serde_json::{json, Map, Value};

/// An artist / scene reference: what "<artist> type beat" means.
pub struct ArtistRef {
    pub names: &'static [&'static str],
    pub genre: &'static str,
    pub bpm: f32,
    pub scale: &'static str,
    pub mood: &'static str,
    /// (role, preset)
    pub palette: &'static [(&'static str, &'static str)],
    pub feel: &'static [&'static str],
    pub why: &'static str,
}

pub const ARTISTS: &[ArtistRef] = &[
    ArtistRef { names: &["bohemia", "raja hip hop", "desi hip hop", "punjabi rap"], genre: "desi_hiphop", bpm: 90.0, scale: "asavari", mood: "dark", palette: &[("lead", "bansuri"), ("counter", "sitar"), ("bass", "808")], feel: &["backbeat"], why: "Bohemia: Punjabi desi hip-hop, ~90 BPM, dark Asavari/minor, flute and sitar melodies over a heavy 808 and boom-bap backbeat" },
    ArtistRef { names: &["sidhu", "moose wala", "moosewala", "punjabi drill"], genre: "drill", bpm: 142.0, scale: "minor", mood: "dark", palette: &[("lead", "sitar"), ("counter", "strings")], feel: &["half_time"], why: "Sidhu Moose Wala: Punjabi drill, sliding 808s, half-time drill hats, a sitar/tumbi-style hook" },
    ArtistRef { names: &["divine", "naezy", "gully"], genre: "boom_bap", bpm: 92.0, scale: "minor", mood: "dark", palette: &[("lead", "sitar")], feel: &["backbeat"], why: "Divine / gully rap: Mumbai boom-bap, punchy drums, gritty minor loops" },
    ArtistRef { names: &["seedhe maut", "krsna", "kr$na", "raftaar", "emiway"], genre: "trap", bpm: 140.0, scale: "minor", mood: "hype", palette: &[], feel: &["half_time"], why: "Indian rap (Seedhe Maut / KR$NA / Raftaar): hard trap, half-time snares, rolling hats" },
    ArtistRef { names: &["karan aujla", "ap dhillon", "shubh", "diljit"], genre: "melodic_rap", bpm: 95.0, scale: "minor", mood: "smooth", palette: &[("lead", "sitar"), ("harmony", "felt_piano")], feel: &["bounce"], why: "Karan Aujla / AP Dhillon / Shubh: melodic Punjabi R&B-rap, warm keys, a desi lead, bouncy 808" },
    ArtistRef { names: &["drake", "pnd", "partynextdoor", "6lack"], genre: "rnb", bpm: 76.0, scale: "minor", mood: "smooth", palette: &[], feel: &["half_time", "sparse_hats"], why: "Drake / PND: moody R&B-rap, sparse drums, filtered pads, deep 808" },
    ArtistRef { names: &["travis scott", "travis", "metro boomin", "metro", "future", "21 savage", "southside"], genre: "trap", bpm: 144.0, scale: "minor", mood: "dark", palette: &[], feel: &["half_time"], why: "Travis / Metro / Future / 21: dark trap, haunting pads and bells, gliding 808s" },
    ArtistRef { names: &["pop smoke", "central cee", "headie one", "fivio", "uk drill", "ny drill"], genre: "drill", bpm: 142.0, scale: "minor", mood: "dark", palette: &[], feel: &["half_time"], why: "Pop Smoke / Central Cee: drill, sliding 808s, syncopated hats and snares" },
    ArtistRef { names: &["kendrick", "j cole", "j. cole", "nas", "joey bada", "mf doom", "griselda", "alchemist"], genre: "boom_bap", bpm: 90.0, scale: "minor", mood: "jazzy", palette: &[], feel: &["swing", "backbeat"], why: "Kendrick / Cole / Nas: boom-bap, swung drums, dusty jazzy loops" },
    ArtistRef { names: &["lofi girl", "nujabes", "jinsang", "study beat"], genre: "lofi", bpm: 82.0, scale: "major", mood: "chill", palette: &[], feel: &["swing"], why: "lo-fi: swung dusty drums, jazzy 7th chords, tape warmth" },
    ArtistRef { names: &["burna", "wizkid", "rema", "davido", "tems", "asake"], genre: "afrobeats", bpm: 104.0, scale: "minor", mood: "smooth", palette: &[], feel: &["bounce"], why: "Burna / Wizkid / Rema: afrobeats, log-drum style bass, shakers and syncopated percussion" },
    ArtistRef { names: &["juice wrld", "lil peep", "iann dior", "the kid laroi"], genre: "melodic_rap", bpm: 150.0, scale: "minor", mood: "sad", palette: &[], feel: &["half_time"], why: "Juice WRLD: emo melodic rap, guitar/piano loops, half-time trap drums" },
    ArtistRef { names: &["kordhell", "dvrst", "phonk", "memphis"], genre: "phonk", bpm: 130.0, scale: "minor", mood: "dark", palette: &[], feel: &["driving"], why: "phonk: cowbell melodies, distorted 808s, Memphis-style drums" },
    ArtistRef { names: &["the weeknd", "weeknd", "frank ocean", "sza", "brent faiyaz"], genre: "rnb", bpm: 84.0, scale: "minor", mood: "smooth", palette: &[], feel: &["bounce"], why: "Weeknd / Frank / SZA: modern R&B, lush chords, soft drums" },
];

const GENRE_WORDS: &[(&str, &str)] = &[
    ("uk drill", "drill"), ("drill", "drill"), ("boom bap", "boom_bap"), ("boombap", "boom_bap"), ("boom-bap", "boom_bap"),
    ("lo-fi", "lofi"), ("lofi", "lofi"), ("r&b", "rnb"), ("rnb", "rnb"), ("phonk", "phonk"), ("afro", "afrobeats"),
    ("desi", "desi_hiphop"), ("punjabi", "desi_hiphop"), ("bhangra", "desi_hiphop"), ("indian", "desi_hiphop"), ("bollywood", "desi_hiphop"),
    ("melodic", "melodic_rap"), ("emo", "melodic_rap"), ("trap", "trap"), ("rap", "trap"), ("hip hop", "boom_bap"), ("hip-hop", "boom_bap"),
];

const CONTRASTS: &[(&[&str], &str)] = &[
    (&["quiet", "breakdown", "stripped", "calm part", "calm section", "soft part", "ambient part", "break section"], "quiet_section"),
    (&["drums drop out", "no drums", "drop out the drums", "dropout"], "drum_dropout"),
    (&["beat switch", "switch up", "switchup", "beat change", "flip the beat", "second half", "tempo change"], "beat_switch"),
    (&["drop", "silence before"], "silence_before_drop"),
    (&["half time", "half-time", "halftime"], "half_time_hook"),
    (&["key change", "modulat", "change key"], "key_change"),
    (&["build", "builds up", "grows", "sparse to dense", "escalat"], "sparse_to_dense"),
    (&["call and response", "call & response", "call-and-response"], "bass_kick_call_response"),
    (&["odd", "weird phrase", "unexpected"], "odd_phrase"),
];

const MOOD_WORDS: &[(&[&str], &str)] = &[
    (&["dark", "evil", "sinister", "menac", "eerie", "horror", "cold"], "dark"),
    (&["sad", "emotional", "heartbreak", "pain", "melanchol", "lonely", "cry"], "sad"),
    (&["hype", "aggressive", "hard", "energetic", "banger", "angry", "gym", "turn up"], "hype"),
    (&["chill", "relax", "calm", "study", "sleep", "mellow", "smooth vibe"], "chill"),
    (&["jazz", "soul", "dusty"], "jazzy"),
    (&["happy", "uplift", "hopeful", "bright", "summer", "inspir"], "hopeful"),
    (&["spiritual", "devotional", "prayer", "sufi", "god"], "devotional"),
    (&["romantic", "love", "smooth", "sensual", "late night"], "smooth"),
];

const INSTRUMENTS: &[(&[&str], &str, &str)] = &[
    (&["flute", "bansuri"], "lead", "bansuri"),
    (&["sitar", "tumbi"], "lead", "sitar"),
    (&["santoor"], "lead", "santoor"),
    (&["sarangi", "strings", "violin"], "counter", "strings"),
    (&["piano"], "harmony", "felt_piano"),
    (&["choir", "vocal chop"], "texture", "choir_aah"),
    (&["bell"], "lead", "bell_glass"),
    (&["koto", "japanese"], "lead", "koto"),
    (&["harmonium"], "harmony", "harmonium"),
    (&["guitar"], "lead", "pluck"),
    (&["organ"], "harmony", "organ"),
    (&["brass", "horn"], "counter", "brass_section"),
];

#[derive(Clone, Debug, Serialize, Default)]
pub struct PromptPlan {
    pub genre: Option<String>,
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub scale: Option<String>,
    pub mood: Option<String>,
    pub duration_s: Option<f32>,
    pub contrasts: Vec<String>,
    pub feel: Vec<String>,
    pub palette: Map<String, Value>,
    pub energy: Option<f32>,
    pub density: Option<String>,
    /// Why each choice was made, in words.
    pub reasons: Vec<String>,
}

fn has(text: &str, w: &str) -> bool {
    text.contains(w)
}

fn number_before(text: &str, unit: &str) -> Option<f32> {
    let i = text.find(unit)?;
    let head = text[..i].trim_end();
    let digits: String = head.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// Read a free-text brief.
pub fn parse_prompt(prompt: &str) -> PromptPlan {
    let t = format!(" {} ", prompt.to_lowercase().replace(['\n', ','], " "));
    let mut p = PromptPlan::default();
    // artist references first (the most specific)
    for a in ARTISTS {
        if let Some(n) = a.names.iter().find(|n| has(&t, n)) {
            p.genre = Some(a.genre.into());
            p.bpm = Some(a.bpm);
            p.scale = Some(a.scale.into());
            p.mood = Some(a.mood.into());
            for (role, preset) in a.palette {
                p.palette.insert(role.to_string(), json!(preset));
            }
            p.feel.extend(a.feel.iter().map(|s| s.to_string()));
            p.reasons.push(format!("'{n}' -> {}", a.why));
            break;
        }
    }
    if p.genre.is_none() {
        if let Some((w, g)) = GENRE_WORDS.iter().find(|(w, _)| has(&t, w)) {
            p.genre = Some(g.to_string());
            p.reasons.push(format!("'{w}' -> genre {g}"));
        }
    }
    for (words, c) in CONTRASTS {
        if let Some(w) = words.iter().find(|w| has(&t, w)) {
            if !p.contrasts.iter().any(|x| x == c) {
                p.contrasts.push(c.to_string());
                p.reasons.push(format!("'{w}' -> contrast {c}"));
            }
        }
    }
    for (words, m) in MOOD_WORDS {
        if let Some(w) = words.iter().find(|w| has(&t, w)) {
            p.mood = Some(m.to_string());
            p.reasons.push(format!("'{w}' -> mood {m}"));
            break;
        }
    }
    for (words, role, preset) in INSTRUMENTS {
        if let Some(w) = words.iter().find(|w| has(&t, w)) {
            p.palette.insert(role.to_string(), json!(preset));
            p.reasons.push(format!("'{w}' -> {role}: {preset}"));
        }
    }
    if let Some(b) = number_before(&t, "bpm") {
        if (50.0..=200.0).contains(&b) {
            p.bpm = Some(b);
            p.reasons.push(format!("{b} BPM as asked"));
        }
    }
    // "in F# minor", "key of D"
    for kw in [" in ", " key of ", " key "] {
        if let Some(i) = t.find(kw) {
            let rest: Vec<&str> = t[i + kw.len()..].split_whitespace().take(2).collect();
            if let Some(k) = rest.first() {
                let k = k.trim_end_matches('m');
                if crate::theory::pitch_class(k).is_ok() && k.len() <= 2 && k.chars().next().map_or(false, |c| ('a'..='g').contains(&c)) {
                    let mut kk = k.to_uppercase();
                    if kk.len() == 2 {
                        kk = format!("{}{}", &kk[..1], &k[1..]);
                    }
                    p.key = Some(kk.clone());
                    if let Some(s) = rest.get(1) {
                        if ["minor", "major", "dorian", "phrygian"].contains(s) {
                            p.scale = Some(s.to_string());
                        }
                    }
                    p.reasons.push(format!("key {kk} as asked"));
                    break;
                }
            }
        }
    }
    for (unit, mult) in [("seconds", 1.0), ("sec", 1.0), ("minutes", 60.0), ("minute", 60.0), ("min", 60.0)] {
        if let Some(n) = number_before(&t, unit) {
            p.duration_s = Some((n * mult).clamp(20.0, 420.0));
            break;
        }
    }
    // a switch and a quiet part need room to be heard
    if p.duration_s.is_none() && !p.contrasts.is_empty() {
        p.duration_s = Some(if p.contrasts.len() >= 2 { 90.0 } else { 75.0 });
    }
    if has(&t, "minimal") || has(&t, "sparse") || has(&t, "space for") {
        p.density = Some("sparse".into());
    } else if has(&t, "busy") || has(&t, "dense") || has(&t, "epic") {
        p.density = Some("dense".into());
    }
    if p.mood.as_deref() == Some("hype") {
        p.energy = Some(0.85);
    } else if matches!(p.mood.as_deref(), Some("chill") | Some("smooth")) {
        p.energy = Some(0.4);
    }
    p
}

// ------------------------------------------------------------ lyrics

#[derive(Clone, Debug, Serialize, Default)]
pub struct LyricSection {
    pub kind: String,
    pub lines: usize,
    pub bars: u32,
    pub first_line: String,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct LyricsReport {
    pub delivery: String,
    pub syllables_per_line: f32,
    pub rhyme_density: f32,
    pub mood: String,
    pub mood_scores: Map<String, Value>,
    pub sections: Vec<LyricSection>,
    pub bpm: f32,
    pub genre: String,
    pub total_bars: u32,
    pub reasons: Vec<String>,
}

pub fn syllables(word: &str) -> usize {
    let w: Vec<char> = word.to_lowercase().chars().filter(|c| c.is_alphabetic()).collect();
    if w.is_empty() {
        return 0;
    }
    let vowel = |c: char| "aeiouy".contains(c);
    let mut n = 0;
    let mut prev = false;
    for &c in &w {
        let v = vowel(c);
        if v && !prev {
            n += 1;
        }
        prev = v;
    }
    if w.len() > 2 && w[w.len() - 1] == 'e' && !vowel(w[w.len() - 2]) && n > 1 {
        n -= 1;
    }
    n.max(1)
}

fn rhyme_key(line: &str) -> String {
    let last = line.split_whitespace().last().unwrap_or("").to_lowercase();
    let w: String = last.chars().filter(|c| c.is_alphabetic()).collect();
    let n = w.chars().count();
    w.chars().skip(n.saturating_sub(2)).collect()
}

const LEX: &[(&str, &[&str])] = &[
    ("dark", &["dark", "blood", "gun", "kill", "death", "die", "grave", "street", "war", "night", "devil", "cold", "smoke", "enemy", "knife", "shadow"]),
    ("sad", &["tears", "cry", "alone", "lonely", "miss", "broken", "gone", "goodbye", "pain", "hurt", "lost", "empty", "sorry"]),
    ("hype", &["money", "boss", "king", "fire", "win", "hustle", "grind", "racks", "drip", "flex", "run", "top", "crown", "power"]),
    ("smooth", &["love", "heart", "baby", "kiss", "forever", "touch", "eyes", "dil", "pyaar", "jaan", "ishq", "mohabbat", "tere", "sajna"]),
    ("devotional", &["god", "rab", "prayer", "lord", "allah", "waheguru", "soul", "heaven", "faith"]),
    ("hopeful", &["dream", "rise", "shine", "light", "sun", "hope", "fly", "free", "tomorrow"]),
];

/// Read lyrics: rap or sung, mood, and the song's sections (stanzas; a
/// stanza or line that repeats is the hook).
pub fn analyze_lyrics(text: &str) -> LyricsReport {
    let stanzas: Vec<Vec<&str>> = text
        .split("\n\n")
        .map(|s| s.lines().map(|l| l.trim()).filter(|l| !l.is_empty() && !l.starts_with('[')).collect::<Vec<_>>())
        .filter(|s| !s.is_empty())
        .collect();
    let lines: Vec<&str> = stanzas.iter().flatten().copied().collect();
    let mut r = LyricsReport::default();
    if lines.is_empty() {
        return r;
    }
    let syl: Vec<usize> = lines.iter().map(|l| l.split_whitespace().map(syllables).sum()).collect();
    r.syllables_per_line = syl.iter().sum::<usize>() as f32 / syl.len() as f32;
    // end rhymes between neighbouring lines
    let keys: Vec<String> = lines.iter().map(|l| rhyme_key(l)).collect();
    let rhymes = keys.windows(2).filter(|w| !w[0].is_empty() && w[0] == w[1]).count() + keys.windows(3).filter(|w| !w[0].is_empty() && w[0] == w[2]).count();
    r.rhyme_density = rhymes as f32 / lines.len() as f32;
    let all = text.to_lowercase();
    let melodic_marks = ["oh", "ooh", "yeah", "baby", "la la", "na na", "hmm", "aaj", "tere", "dil"].iter().filter(|w| all.contains(*w)).count();
    let rap = r.syllables_per_line >= 10.5 || (r.syllables_per_line >= 8.5 && r.rhyme_density > 0.5 && melodic_marks < 3);
    r.delivery = if rap { "rap".into() } else { "melodic".into() };
    r.reasons.push(format!("{:.1} syllables per line, rhyme density {:.2}{} -> {}", r.syllables_per_line, r.rhyme_density, if melodic_marks > 0 { format!(", {melodic_marks} sung-style words") } else { String::new() }, r.delivery));
    // mood
    let words: Vec<String> = all.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    let mut best = ("dark", 0usize);
    for (m, lex) in LEX {
        let n = words.iter().filter(|w| lex.contains(&w.as_str())).count();
        r.mood_scores.insert(m.to_string(), json!(n));
        if n > best.1 {
            best = (m, n);
        }
    }
    r.mood = if best.1 == 0 { if rap { "dark".into() } else { "smooth".into() } } else { best.0.into() };
    // sections: repeated stanzas are hooks
    let norm = |s: &[&str]| s.join(" ").to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>();
    let ns: Vec<String> = stanzas.iter().map(|s| norm(s)).collect();
    let line_bars = if rap { 1 } else { 2 };
    for (i, s) in stanzas.iter().enumerate() {
        let repeated = ns.iter().enumerate().any(|(j, o)| j != i && crate::vocal::lyric_similarity(o, &ns[i]) > 0.6);
        let short_and_repetitive = s.len() <= 4 && { let mut u: Vec<String> = s.iter().map(|l| l.to_lowercase()).collect(); u.sort(); u.dedup(); u.len() < s.len() };
        let kind = if repeated || short_and_repetitive { "hook" } else { "verse" };
        let bars = ((s.len() as u32 * line_bars).div_ceil(4) * 4).clamp(4, 32);
        r.sections.push(LyricSection { kind: kind.into(), lines: s.len(), bars, first_line: s[0].to_string() });
    }
    if !r.sections.iter().any(|s| s.kind == "hook") && r.sections.len() > 1 {
        // no repeat: the shortest stanza reads as the hook
        if let Some(i) = (0..r.sections.len()).min_by_key(|&i| r.sections[i].lines) {
            r.sections[i].kind = "hook".into();
        }
    }
    r.total_bars = r.sections.iter().map(|s| s.bars).sum::<u32>() + 8;
    // tempo and genre from delivery, density and mood
    let (genre, bpm) = if rap {
        if r.syllables_per_line > 15.0 {
            ("trap", 140.0) // dense flows ride half-time trap
        } else if r.mood == "dark" {
            ("boom_bap", 90.0)
        } else if r.mood == "hype" {
            ("trap", 142.0)
        } else {
            ("boom_bap", 92.0)
        }
    } else {
        match r.mood.as_str() {
            "sad" => ("melodic_rap", 150.0),
            "smooth" => ("rnb", 82.0),
            "devotional" => ("desi_hiphop", 86.0),
            "hopeful" => ("afrobeats", 102.0),
            "hype" => ("melodic_rap", 140.0),
            _ => ("rnb", 84.0),
        }
    };
    r.genre = genre.into();
    r.bpm = bpm;
    r.reasons.push(format!("{} delivery + {} mood -> {} at {} BPM", r.delivery, r.mood, genre, bpm));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_bohemia_brief() {
        let p = parse_prompt("Bohemia type beat with a beat switch and a quiet section");
        assert_eq!(p.genre.as_deref(), Some("desi_hiphop"));
        assert!(p.contrasts.contains(&"beat_switch".to_string()));
        assert!(p.contrasts.contains(&"quiet_section".to_string()));
        // the quiet section is placed first so the switch can slam in out of it
        let qi = p.contrasts.iter().position(|c| c == "quiet_section").unwrap();
        let si = p.contrasts.iter().position(|c| c == "beat_switch").unwrap();
        assert!(qi < si);
        assert!(p.duration_s.unwrap() >= 75.0);
        let q = parse_prompt("dark uk drill 144 bpm in F# minor with piano, 1 minute");
        assert_eq!(q.genre.as_deref(), Some("drill"));
        assert_eq!(q.bpm, Some(144.0));
        assert_eq!(q.key.as_deref(), Some("F#"));
        assert_eq!(q.scale.as_deref(), Some("minor"));
        assert_eq!(q.duration_s, Some(60.0));
        assert_eq!(q.palette.get("harmony").and_then(|v| v.as_str()), Some("felt_piano"));
    }

    #[test]
    fn tells_rap_from_singing_and_finds_the_hook() {
        let rap = "Started from the bottom with a dream and a pen in my hand\nEvery single night I was writing like I'm already the man\nThey ain't believe it now they see it when the money expand\nI got the city on my back and I'm the voice of the land\n\nRun it up, run it up\nRun it up, run it up\n\nCold streets taught me how to move without a sound in the dark\nEvery enemy I had just lit the fire and the spark\nNow I'm sitting on the top, every verse is a mark\nI was hungry from the start, now I'm eating with the sharks\n\nRun it up, run it up\nRun it up, run it up";
        let r = analyze_lyrics(rap);
        assert_eq!(r.delivery, "rap", "{r:?}");
        assert_eq!(r.sections.len(), 4);
        assert_eq!(r.sections[1].kind, "hook");
        assert_eq!(r.sections[0].kind, "verse");
        let sung = "Oh baby stay\nDon't go away\nYour love is light\nStay through the night\n\nOh baby stay\nDon't go away\nYour love is light\nStay through the night";
        let s = analyze_lyrics(sung);
        assert_eq!(s.delivery, "melodic", "{s:?}");
        assert_eq!(s.mood, "smooth");
    }
}
