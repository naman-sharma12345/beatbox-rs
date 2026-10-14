//! Tools for the cover/song pipeline (see `cover`): one call `cover_song`
//! (aliases make_song_from_reference, make_song_from_vocal) and every step on
//! its own: identify_song, fetch_reference, analyze_song, align_take,
//! plan_cover, build_cover, mix_cover, master_cover, qa_song.

use crate::analysis;
use crate::cover::{self, Alignment, Features, NoteOut, Plan, PlanSection, SongAnalysis};
use crate::dsp::{Fft, SR};
use crate::engine::Engine;
use crate::instruments::Instrument;
use crate::multisample::{self, MultisampleParams, Zone};
use crate::project::Track;
use crate::render::{self, RenderOptions};
use crate::samples::{self, SampleInfo};
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, Tool};
use crate::tools_delivery::{deliver, DeliverOpts};
use crate::vocal;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn call(e: &mut Engine, name: &str, args: Value) -> Result<Value> {
    e.call(name, &args).with_context(|| format!("{name} {args}"))
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn db(x: f32) -> f32 {
    10.0 * x.max(1e-12).log10()
}

struct Clock {
    t0: Instant,
    last: Instant,
    steps: Vec<Value>,
}

impl Clock {
    fn new() -> Self {
        Clock { t0: Instant::now(), last: Instant::now(), steps: Vec::new() }
    }
    fn mark(&mut self, step: &str) {
        let now = Instant::now();
        self.steps.push(json!({"step": step, "s": r1((now - self.last).as_secs_f32())}));
        self.last = now;
    }
    fn total(&self) -> f32 {
        r1(self.t0.elapsed().as_secs_f32())
    }
}

fn ref_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".beatbox").join("ref_cache")
}

fn ytdlp() -> String {
    std::env::var("BEATBOX_YTDLP").unwrap_or_else(|_| "yt-dlp".into())
}

/// Lyrics of the take (cached next to it).
fn lyrics_of(e: &Engine, take: &Path, model: &str, language: Option<&str>) -> Option<vocal::Lyrics> {
    vocal::transcribe(take, &e.workdir, model, language, None).ok()
}

/// One YouTube search through yt-dlp: (title, duration s, id).
fn yt_search(q: &str, n: usize) -> Vec<(String, f32, String)> {
    let out = std::process::Command::new("timeout")
        .args(["45", &ytdlp(), "--flat-playlist", "--no-warnings", "--print", "%(title)s\t%(duration)s\t%(id)s", &format!("ytsearch{n}:{q}")])
        .output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\t').collect();
            if p.len() < 3 {
                return None;
            }
            Some((p[0].to_string(), p[1].parse().unwrap_or(0.0), p[2].to_string()))
        })
        .collect()
}

/// Lyric lines worth searching: up to 3 lines of >= 4 words from different parts of the take.
fn search_lines(l: &vocal::Lyrics) -> Vec<String> {
    let segs: Vec<&vocal::LyricLine> = l.segments.iter().filter(|s| s.text.split_whitespace().count() >= 4).collect();
    if segs.is_empty() {
        return if l.text.split_whitespace().count() >= 3 { vec![l.text.split_whitespace().take(12).collect::<Vec<_>>().join(" ")] } else { vec![] };
    }
    let mut out: Vec<String> = Vec::new();
    for frac in [0.0f32, 0.4, 0.75] {
        let i = ((segs.len() as f32 * frac) as usize).min(segs.len() - 1);
        let t: String = segs[i].text.split_whitespace().take(12).collect::<Vec<_>>().join(" ");
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Identify the song from the take's lyrics: searches run in parallel and must agree.
pub fn identify(lyr: &vocal::Lyrics) -> Value {
    let lines = search_lines(lyr);
    if lines.is_empty() {
        return json!({"identified": false, "why": "no lyric lines long enough to search", "lyrics": lyr.text});
    }
    let handles: Vec<_> = lines.iter().map(|q| { let q = q.clone(); std::thread::spawn(move || yt_search(&format!("{q} lyrics"), 5)) }).collect();
    let results: Vec<Vec<(String, f32, String)>> = handles.into_iter().map(|h| h.join().unwrap_or_default()).collect();
    // votes per normalised title
    let mut votes: std::collections::BTreeMap<String, (f32, BTreeSet<usize>, String, String, f32)> = std::collections::BTreeMap::new();
    for (qi, rs) in results.iter().enumerate() {
        for (rank, (title, dur, id)) in rs.iter().enumerate() {
            if *dur < 60.0 || *dur > 720.0 {
                continue;
            }
            let k = cover::title_key(title);
            let v = votes.entry(k).or_insert((0.0, BTreeSet::new(), title.clone(), id.clone(), *dur));
            v.0 += 5.0 - rank as f32;
            v.1.insert(qi);
        }
    }
    let mut ranked: Vec<(String, (f32, BTreeSet<usize>, String, String, f32))> = votes.into_iter().collect();
    ranked.sort_by(|a, b| (b.1 .1.len(), b.1 .0).partial_cmp(&(a.1 .1.len(), a.1 .0)).unwrap_or(std::cmp::Ordering::Equal));
    let cands: Vec<Value> = ranked.iter().take(5).map(|(k, v)| json!({"title": v.2, "key": k, "url": format!("https://www.youtube.com/watch?v={}", v.3), "queries_agreeing": v.1.len(), "score": v.0, "duration_s": v.4})).collect();
    let nq = results.iter().filter(|r| !r.is_empty()).count();
    let best = ranked.first();
    let ok = match best {
        Some((_, v)) => (nq >= 2 && v.1.len() >= 2) || (nq == 1 && v.0 >= 9.0),
        None => false,
    };
    json!({
        "identified": ok,
        "best": if ok { cands.first().cloned().unwrap_or(Value::Null) } else { Value::Null },
        "candidates": cands,
        "searched_lines": lines,
        "language": lyr.language,
        "why": if ok { "the same record came back for several lyric lines" } else { "no record agreed across the lyric searches: treating the take as an original" },
    })
}

fn ytdlp_extra() -> Vec<String> {
    std::env::var("BEATBOX_YTDLP_ARGS").map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default()
}

fn ytdlp_fetch(target: &str, dir: &Path) -> Result<PathBuf> {
    let stamp = format!("ref_{}", crate::creative::fresh_seed() % 1_000_000_000);
    let out = std::process::Command::new("timeout")
        .args(["150", &ytdlp(), "-f", "bestaudio/best", "--no-playlist", "--no-warnings", "-x", "--audio-format", "wav", "--postprocessor-args", "ffmpeg:-ac 1 -ar 44100"])
        .args(ytdlp_extra())
        .arg("-o")
        .arg(dir.join(format!("{stamp}.%(ext)s")))
        .arg(target)
        .output()
        .context("running yt-dlp (set BEATBOX_YTDLP)")?;
    let wav = dir.join(format!("{stamp}.wav"));
    if !wav.is_file() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("ERROR")).last().unwrap_or("no audio came back").chars().take(160).collect::<String>());
    }
    Ok(wav)
}

/// Full-length SoundCloud uploads of a title (previews of 30 s skipped), closest duration first.
fn soundcloud_find(title: &str, dur_hint: Option<f32>) -> Vec<String> {
    let out = std::process::Command::new("timeout")
        .args(["45", &ytdlp(), "--flat-playlist", "--no-warnings", "--print", "%(title)s\t%(duration)s\t%(url)s", &format!("scsearch6:{title}")])
        .output();
    let Ok(out) = out else { return vec![] };
    let mut v: Vec<(f32, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\t').collect();
            let d: f32 = p.get(1)?.parse().ok()?;
            if d < 60.0 || d > 720.0 {
                return None;
            }
            Some(((d - dur_hint.unwrap_or(d)).abs(), p.get(2)?.to_string()))
        })
        .collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter().map(|x| x.1).collect()
}

/// Download a reference for analysis only (mono 44.1 kHz WAV in the ref cache).
/// `src` is a file, a URL or search words; when a URL refuses (video sites
/// often ask for a sign-in from servers), the same title is looked up on
/// SoundCloud. Returns (path, downloaded, where it came from, what failed).
pub fn fetch_reference_with(src: &str, title: Option<&str>, dur: Option<f32>) -> Result<(PathBuf, bool, String, Vec<String>)> {
    let p = PathBuf::from(src);
    if p.is_file() {
        return Ok((p, false, src.to_string(), vec![]));
    }
    let dir = ref_cache_dir();
    std::fs::create_dir_all(&dir)?;
    let mut tried: Vec<String> = Vec::new();
    let first = if src.starts_with("http") { src.to_string() } else { format!("ytsearch1:{src}") };
    match ytdlp_fetch(&first, &dir) {
        Ok(w) => return Ok((w, true, first, tried)),
        Err(e) => tried.push(format!("{first}: {e}")),
    }
    let q = title.map(|t| t.to_string()).unwrap_or_else(|| src.to_string());
    if !q.starts_with("http") {
        for url in soundcloud_find(&q, dur).into_iter().take(2) {
            match ytdlp_fetch(&url, &dir) {
                Ok(w) => return Ok((w, true, url, tried)),
                Err(e) => tried.push(format!("{url}: {e}")),
            }
        }
    }
    bail!("could not fetch the reference: {}", tried.join(" | "))
}

pub fn fetch_reference(src: &str) -> Result<(PathBuf, bool)> {
    fetch_reference_with(src, None, None).map(|(p, d, _, _)| (p, d))
}

/// Song analysis of a reference file, cached next to it.
/// The word-timing line sync helper: scripts/align_words.py (faster-whisper, local model only).
pub const ALIGN_WORDS_PY: &str = include_str!("../scripts/align_words.py");

fn align_words(e: &Engine, take: &Path, refv: &Path, a: &Value) -> Result<Value> {
    let script = e.workdir.join(".beatbox_align_words.py");
    std::fs::write(&script, ALIGN_WORDS_PY)?;
    let py = std::env::var("BEATBOX_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut cmd = std::process::Command::new(&py);
    cmd.arg(&script).arg(take).arg(refv)
        .arg("--model").arg(s_opt(a, "model").unwrap_or_else(|| "small".into()))
        .arg("--max-phrase").arg(f_opt(a, "max_phrase_s").unwrap_or(6.0).to_string());
    let o = cmd.output().map_err(|err| anyhow!("running {py}: {err}"))?;
    let txt = String::from_utf8_lossy(&o.stdout);
    let v: Value = serde_json::from_str(txt.lines().find(|l| l.trim_start().starts_with('{')).unwrap_or("").trim())
        .map_err(|_| anyhow!("line sync helper failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("no output")))?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        bail!("{err}");
    }
    Ok(v)
}

pub fn analysis_of(path: &Path) -> Result<(SongAnalysis, Features)> {
    let x = samples::decode_file(path)?;
    let fe = cover::features(&x);
    // the analyser version is in the name: an old cache never answers for new code
    let cache = path.with_extension("song.v4.json");
    if let Ok(s) = std::fs::read_to_string(&cache) {
        if let Ok(a) = serde_json::from_str::<SongAnalysis>(&s) {
            return Ok((a, fe));
        }
    }
    let a = cover::analyze_song(&x, &path.to_string_lossy())?;
    let _ = std::fs::write(&cache, serde_json::to_string(&a)?);
    Ok((a, fe))
}

fn bar_s(bpm: f32) -> f32 {
    240.0 / bpm
}

/// The plan: sync (he sang along to the record) or free (grid fitted to him).

fn norm_word(w: &str) -> String {
    w.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect()
}

/// Longest common subsequence of two word lists: matched index pairs.
fn lcs_pairs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return vec![];
    }
    let mut d = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[i * (m + 1) + j] = if !a[i].is_empty() && a[i] == b[j] {
                d[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                d[(i + 1) * (m + 1) + j].max(d[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if !a[i].is_empty() && a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if d[(i + 1) * (m + 1) + j] >= d[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Lyric sync: his phrases matched word by word to the record's own sung
/// lyrics, so each phrase lands on the record's grid where the record sings
/// that line (he never has to have sung along in time). Phrases keep his own
/// timing inside (no stretch, no tuning); only where each phrase starts moves,
/// then a nudge (<= 110 ms) onto the nearest 16th. Record stretches he skipped
/// (longer than his own pause by 4+ bars) are cut out in whole 4-bar blocks.
/// Returns (plan, info) or None when too few words match.
fn lyric_sync(take_x: &[f32], take_words: &[vocal::Word], rec_words: &[vocal::Word], ra: &SongAnalysis, comp_role: &str, a: &Value) -> Option<(Plan, Value)> {
    let tw: Vec<String> = take_words.iter().map(|w| norm_word(&w.word)).collect();
    let rw: Vec<String> = rec_words.iter().map(|w| norm_word(&w.word)).collect();
    let pairs = lcs_pairs(&tw, &rw);
    let ratio = pairs.len() as f32 / tw.len().max(1) as f32;
    if pairs.len() < 8 || ratio < 0.25 {
        return None;
    }
    let mut rec_of: Vec<Option<f32>> = vec![None; tw.len()];
    for (i, j) in &pairs {
        rec_of[*i] = Some(rec_words[*j].start);
    }
    // phrases: split at pauses
    let mut phrases: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    for i in 1..=take_words.len() {
        if i == take_words.len() || take_words[i].start - take_words[i - 1].end > 0.45 {
            phrases.push((s, i));
            s = i;
        }
    }
    // long lines are split at their widest breath so his own drift against the
    // record's tempo never builds up past ~6 s
    let max_len = f_opt(a, "max_phrase_s").unwrap_or(6.0);
    let mut i = 0;
    while i < phrases.len() {
        let (p0, p1) = phrases[i];
        let len = take_words[p1 - 1].end - take_words[p0].start;
        if len > max_len && p1 - p0 >= 4 {
            // the quietest word boundary (40 ms of the raw take around it)
            let quiet = |t: f32| -> f32 {
                let c = (t * SR) as usize;
                let h = (0.02 * SR) as usize;
                let (a0, a1) = (c.saturating_sub(h).min(take_x.len()), (c + h).min(take_x.len()));
                if a1 <= a0 { return 0.0; }
                -(take_x[a0..a1].iter().map(|x| x * x).sum::<f32>() / (a1 - a0) as f32 + 1e-12).log10()
            };
            let best = (p0 + 1..p1)
                .filter(|&k| take_words[k].start - take_words[p0].start >= 1.5 && take_words[p1 - 1].end - take_words[k].start >= 1.5)
                .map(|k| { let g = (take_words[k].start - take_words[k - 1].end).max(0.0); (k, quiet(take_words[k].start - g * 0.5) + 4.0 * g) })
                .fold(None, |acc: Option<(usize, f32)>, x| match acc { Some(a) if a.1 >= x.1 => Some(a), _ => Some(x) });
            if let Some((k, _)) = best {
                phrases[i] = (p0, k);
                phrases.insert(i + 1, (k, p1));
                continue;
            }
        }
        i += 1;
    }
    let med = |mut v: Vec<f32>| -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        Some(v[v.len() / 2])
    };
    let mut offs: Vec<Option<f32>> = phrases.iter().map(|(p0, p1)| {
        let v: Vec<f32> = (*p0..*p1).filter_map(|i| rec_of[i].map(|r| r - take_words[i].start)).collect();
        if v.len() >= 2 || (v.len() == 1 && p1 - p0 <= 2) { med(v) } else { None }
    }).collect();
    // lone outliers (a repeated line matched to the wrong repeat) follow their neighbours
    for i in 0..offs.len() {
        let prev = offs[..i].iter().rev().flatten().next().copied();
        let next = offs[i + 1..].iter().flatten().next().copied();
        if let (Some(o), Some(p), Some(n)) = (offs[i], prev, next) {
            if (o - p).abs() > 6.0 && (o - n).abs() > 6.0 && (p - n).abs() < 6.0 {
                offs[i] = Some(p);
            }
        }
    }
    let first = offs.iter().flatten().next().copied()?;
    let mut last = first;
    let beat_s = 60.0 / ra.bpm;
    let bs = 4.0 * beat_s;
    let step = beat_s / 4.0;
    let nb = ra.bar_times.len();
    let db0 = ra.downbeat_s;
    // record start of each phrase, monotonic
    let mut place: Vec<(f32, f32, f32, f32)> = Vec::new(); // (take s0, take s1, rec s0, nudge)
    let mut prev_rec_end = f32::MIN;
    for (k, (p0, p1)) in phrases.iter().enumerate() {
        let mut o = offs[k].unwrap_or(last);
        let (t0, t1) = (take_words[*p0].start, take_words[p1 - 1].end);
        if t0 + o < prev_rec_end + 0.05 {
            // a line sung again or out of order: keep his own pause after the last phrase
            let prev = place.last().map(|p| p.2 + (t0 - p.0)).unwrap_or(t0 + o);
            o = prev - t0;
        }
        last = o;
        let r0 = t0 + o;
        let g = ((r0 - db0) / step).round() * step + db0;
        let nudge = if (g - r0).abs() <= 0.11 { g - r0 } else { 0.0 };
        place.push((t0, t1, r0 + nudge, nudge));
        prev_rec_end = r0 + nudge + (t1 - t0);
    }
    let bar_of = |t: f32| (((t - db0) / bs).floor().max(0.0) as usize).min(nb.saturating_sub(1));
    let sec_start = |b: usize| ra.sections.iter().filter(|s| s.bar0 <= b).map(|s| s.bar0).max().unwrap_or(0);
    let intro = a.get("intro_bars").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
    let fv = bar_of(place[0].2);
    let mut lo = fv.saturating_sub(intro);
    if let Some(sec) = ra.sections.iter().filter(|s| s.bar0 <= fv && s.bar0 + 2 >= lo && s.bar0 <= lo + 2).min_by_key(|s| (s.bar0 as i64 - lo as i64).abs()) {
        lo = sec.bar0;
    }
    let lp = place.last().unwrap();
    let outro = a.get("outro_bars").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
    let hi = (bar_of(lp.2 + (lp.1 - lp.0)) + 1 + outro).min(nb);
    // cuts: record stretches he skipped
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for w in place.windows(2) {
        let (a0, b0) = (w[0], w[1]);
        let rec_gap = b0.2 - (a0.2 + (a0.1 - a0.0));
        let take_gap = (b0.0 - a0.1).max(bs);
        let excess = rec_gap - take_gap;
        if excess >= 4.0 * bs {
            let mut n = ((excess / (4.0 * bs)).floor() as usize) * 4;
            let end_bar = bar_of(a0.2 + (a0.1 - a0.0)) + 1;
            let mut cb = end_bar;
            while (cb - sec_start(cb)) % 4 != 0 && cb < end_bar + 3 {
                cb += 1;
            }
            let next_bar = bar_of(b0.2);
            while n > 0 && cb + n > next_bar {
                n -= 4;
            }
            if n > 0 {
                cuts.push((cb, cb + n));
            }
        }
    }
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut b = lo;
    for (c0, c1) in &cuts {
        if *c0 > b && *c0 < hi {
            segs.push((b, *c0));
        }
        b = (*c1).max(b);
    }
    if b < hi {
        segs.push((b, hi));
    }
    // record time -> song time
    let mut rec_map: Vec<[f32; 3]> = Vec::new();
    let mut acc = 0usize;
    for (s0, s1) in &segs {
        rec_map.push([acc as f32 * bs, ra.bar_times[*s0], (s1 - s0) as f32 * bs]);
        acc += s1 - s0;
    }
    let song_t = |rt: f32| -> Option<f32> {
        rec_map.iter().find(|m| rt >= m[1] - 0.5 && rt < m[1] + m[2]).map(|m| m[0] + (rt - m[1]).max(-(m[0])))
    };
    // vocal clips (pre-roll 120 ms for the breath, tail up to 0.8 s)
    let mut clips: Vec<[f32; 3]> = Vec::new();
    for (k, p) in place.iter().enumerate() {
        let Some(st) = song_t(p.2) else { continue };
        let gap_before = if k == 0 { p.0 } else { p.0 - place[k - 1].1 };
        let pre = 0.12f32.min(p.0).min((gap_before * 0.5).max(0.0));
        let start = (st - pre).max(0.0);
        let mut len = (p.1 - p.0) + pre + 0.8;
        if let Some(n) = place.get(k + 1) {
            let npre = 0.12f32.min(n.0).min(((n.0 - p.1) * 0.5).max(0.0));
            // take time up to the next phrase's breath
            len = len.min((n.0 - npre) - (p.0 - pre));
            if let Some(ns) = song_t(n.2) {
                // and never over the next clip in the song
                len = len.min((ns - npre) - start);
            }
        }
        if len > 0.2 {
            clips.push([start / beat_s, p.0 - pre, len]);
        }
    }
    if clips.len() < 3 {
        return None;
    }
    // sections: the record's, clipped to the kept stretches
    let maxe = ra.sections.iter().map(|x| x.energy_db).fold(f32::MIN, f32::max);
    let mut sections: Vec<PlanSection> = Vec::new();
    let mut used = std::collections::BTreeMap::new();
    for (s0, s1) in &segs {
        for s in &ra.sections {
            let a0 = s.bar0.max(*s0);
            let a1 = (s.bar0 + s.bars).min(*s1);
            if a1 <= a0 {
                continue;
            }
            let c = used.entry(s.name.clone()).or_insert(0usize);
            *c += 1;
            let name = if *c == 1 { s.name.clone() } else { format!("{}_{}", s.name, c) };
            sections.push(PlanSection {
                name,
                kind: s.kind.clone(),
                bars: a1 - a0,
                chords: (a0..a1).map(|b| [cover::parse_chord_name(&ra.chords[b][0]), cover::parse_chord_name(&ra.chords[b][1])]).collect(),
                drums: s.drums.clone(),
                bass: s.bass_steps.clone(),
                comp: s.comp_steps.clone(),
                comp_arp: s.comp_density >= 6.0 && comp_role != "piano",
                energy: (1.0 + (s.energy_db - maxe) / 20.0).clamp(0.55, 1.0),
            });
        }
    }
    let nudges: Vec<f32> = place.iter().map(|p| (p.3 * 1000.0).round()).collect();
    let info = json!({
        "lyric_sync": {
            "take_words": tw.len(), "record_words": rw.len(), "matched": pairs.len(), "match_ratio": r1(ratio * 100.0),
            "phrases": place.len(), "placed": clips.len(),
            "offsets_s": offs.iter().map(|o| o.map(|v| (v * 100.0).round() / 100.0)).collect::<Vec<_>>(),
            "nudges_ms": nudges, "max_nudge_ms": nudges.iter().fold(0.0f32, |m, v| m.max(v.abs())),
            "record_bars": [lo, hi], "cut_bars": cuts, "segments": segs,
        }
    });
    let plan = Plan {
        mode: "sync_lyrics".into(),
        bpm: ra.bpm,
        key_root: ra.key_root.clone(),
        scale: ra.scale.clone(),
        sections,
        vocal_start_beat: clips[0][0],
        vocal_offset_s: clips[0][1],
        comp_role: comp_role.to_string(),
        rec_bar0: Some(lo),
        vocal_clips: clips,
        rec_map,
        notes: vec![format!("lyric sync: {} of his {} words found in the record's own lyrics; {} phrases placed on the record's grid ({} bpm) where the record sings them, nudges <= 110 ms, no stretch/tuning; record bars {}..{}{}", pairs.len(), tw.len(), place.len(), ra.bpm, lo, hi, if cuts.is_empty() { String::new() } else { format!(", skipped bars {:?}", cuts) })],
    };
    Some((plan, info))
}

/// cover_song match_tuning: offsets smaller than this stay at A440.
pub const MATCH_TUNING_MIN_CENTS: f32 = 15.0;

pub fn make_plan(take_x: &[f32], take_fe: &Features, words: &[vocal::Word], rec: Option<(&SongAnalysis, &Features)>, rec_words: &[vocal::Word], style: &str, a: &Value) -> Result<(Plan, Value)> {
    let mut info = json!({});
    let comp_role = {
        let s = style.to_lowercase();
        if s.contains("piano") || s.contains("keys") || s.contains("ballad") && !s.contains("guitar") {
            "piano"
        } else if s.contains("acoustic") {
            "acoustic_guitar"
        } else {
            "guitar"
        }
    }
    .to_string();
    if let Some((ra, _)) = rec {
        if !rec_words.is_empty() && !words.is_empty() && !b_or(a, "force_free", false) {
            if let Some((plan, mut li)) = lyric_sync(take_x, words, rec_words, ra, &comp_role, a) {
                li["alignment"] = json!("lyrics");
                return Ok((plan, li));
            }
            info["lyric_sync"] = json!("too few of his words were found in the record's lyrics");
        }
    }
    if let Some((ra, rfe)) = rec {
        let al: Alignment = cover::align_take(take_fe, rfe);
        info["alignment"] = serde_json::to_value(&al)?;
        if al.synced && !b_or(a, "force_free", false) {
            let (s0, s1) = cover::sung_span(take_fe);
            let bs = bar_s(ra.bpm);
            let nb = ra.bar_times.len();
            let bar_of = |t: f32| (((t - ra.downbeat_s) / bs).floor().max(0.0) as usize).min(nb.saturating_sub(1));
            let fv = bar_of(al.lag_s + s0);
            let lv = bar_of(al.lag_s + s1);
            let intro = a.get("intro_bars").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
            let mut lo = fv.saturating_sub(intro);
            // snap to a section start of the record when one is close
            if let Some(sec) = ra.sections.iter().filter(|s| s.bar0 <= fv && s.bar0 + 2 >= lo && s.bar0 <= lo + 2).min_by_key(|s| (s.bar0 as i64 - lo as i64).abs()) {
                lo = sec.bar0;
            }
            let hi = (lv + 1 + 3).min(nb);
            let mut sections: Vec<PlanSection> = Vec::new();
            for s in &ra.sections {
                let a0 = s.bar0.max(lo);
                let a1 = (s.bar0 + s.bars).min(hi);
                if a1 <= a0 {
                    continue;
                }
                let chords: Vec<[cover::Ch; 2]> = (a0..a1).map(|b| [cover::parse_chord_name(&ra.chords[b][0]), cover::parse_chord_name(&ra.chords[b][1])]).collect();
                let maxe = ra.sections.iter().map(|x| x.energy_db).fold(f32::MIN, f32::max);
                sections.push(PlanSection {
                    name: s.name.clone(),
                    kind: s.kind.clone(),
                    bars: a1 - a0,
                    chords,
                    drums: s.drums.clone(),
                    bass: s.bass_steps.clone(),
                    comp: s.comp_steps.clone(),
                    comp_arp: s.comp_density >= 6.0 && comp_role != "piano",
                    energy: (1.0 + (s.energy_db - maxe) / 20.0).clamp(0.55, 1.0),
                });
            }
            let start_beat = (al.lag_s - ra.bar_times[lo]) / (60.0 / ra.bpm);
            let (vsb, voff) = if start_beat >= 0.0 { (start_beat, 0.0) } else { (0.0, -start_beat * 60.0 / ra.bpm) };
            let plan = Plan {
                mode: "sync".into(),
                bpm: ra.bpm,
                key_root: ra.key_root.clone(),
                scale: ra.scale.clone(),
                sections,
                vocal_start_beat: vsb,
                vocal_offset_s: voff,
                comp_role,
                rec_bar0: Some(lo),
                vocal_clips: vec![],
                rec_map: vec![],
                notes: vec![format!("he sang along to the record (lag {:.2} s, z {:.1}): the record's grid, chords and sections are used, bars {}..{}", al.lag_s, al.z, lo, hi)],
            };
            return Ok((plan, info));
        }
    }
    // free: the grid is fitted to the singer
    let hint = rec.map(|(r, _)| r.bpm).or_else(|| f_opt(a, "bpm"));
    // pitch_detect "yin_gated" (default since the flip): the notes are segmented from
    // YIN-gated frames (vocal::pitch_track_gated); "legacy" is the original track, byte-identical
    let pd = vocal::PitchDetect::from_arg(s_opt(a, "pitch_detect").as_deref())?;
    // key_detect "yin_gated": the key from gated YIN frames (vocal::gated_key)
    // instead of the segmented notes; an "unknown" read falls back to the notes
    // default since commit 3: yin_gated; "legacy" (or "notes") is the old note-based read, byte-identical
    let key_detect = s_opt(a, "key_detect").unwrap_or_else(|| "yin_gated".into());
    if !matches!(key_detect.as_str(), "yin_gated" | "legacy" | "notes") {
        bail!("key_detect must be yin_gated (default), legacy or notes, not {key_detect:?}");
    }
    let gated_on = key_detect == "yin_gated" && (s_opt(a, "key").is_none() || s_opt(a, "scale").is_none());
    // pitch_detect yin_gated rounds the notes against the gated key's tuning
    // (read once here); the key and match_tuning still use it only when gated_on
    let gk = if gated_on || pd == vocal::PitchDetect::YinGated { vocal::gated_key(take_x, SR) } else { None };
    let an = match pd {
        vocal::PitchDetect::Legacy => vocal::analyze_with(take_x, SR, words, hint, pd),
        vocal::PitchDetect::YinGated => vocal::analyze_yin_gated(take_x, SR, words, hint, gk.as_ref()),
    };
    let gated = if gated_on { gk } else { None };
    if an.notes.len() < 4 {
        bail!("could not hear a sung melody in the take (only {} notes); is it a dry vocal?", an.notes.len());
    }
    // a record was found but could not be synced: its tempo still rules (never a silent guess)
    let rec_bpm = rec.map(|(r, _)| r.bpm).filter(|b| *b > 60.0);
    let mut bpm = f_opt(a, "bpm").or(rec_bpm).unwrap_or(an.tempo.bpm);
    if bpm < 72.0 && f_opt(a, "bpm").is_none() && rec_bpm.is_none() {
        bpm *= 2.0;
    }
    let beat_s = 60.0 / bpm;
    let bs = 4.0 * beat_s;
    let (heard_root, heard_scale) = match &gated {
        Some(g) if !g.unknown => (g.key_root.clone(), g.scale.clone()),
        _ => (an.key_root.clone(), an.scale.clone()),
    };
    let key_root = s_opt(a, "key").unwrap_or_else(|| heard_root.clone());
    let scale = s_opt(a, "scale").unwrap_or_else(|| heard_scale.clone());
    let db0 = an.tempo.downbeat;
    let first = an.notes[0].start;
    let k = ((first - db0) / bs).floor();
    let first_bar_t = db0 + k * bs;
    let last_end = an.notes.last().map(|n| n.end).unwrap_or(an.duration);
    let vbars = (((last_end - first_bar_t) / bs).ceil().max(1.0)) as usize;
    let intro = a.get("intro_bars").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
    let outro = a.get("outro_bars").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
    let sec_bars = a.get("section_bars").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    // sung notes in song beats from the first vocal bar
    let mel: Vec<(f32, f32, u8)> = an.notes.iter().map(|n| ((n.start - first_bar_t) / beat_s, n.dur() / beat_s, n.pitch)).collect();
    let words_in = |b0: usize, n: usize| -> String {
        words.iter().filter(|w| {
            let b = (w.start - first_bar_t) / bs;
            b >= b0 as f32 && b < (b0 + n) as f32
        }).map(|w| w.word.clone()).collect::<Vec<_>>().join(" ")
    };
    // vocal sections; the most repeated lyrics are the hook
    let mut vs: Vec<(usize, usize, String)> = Vec::new();
    let mut b = 0usize;
    while b < vbars {
        let mut n = sec_bars.min(vbars - b);
        if vbars - (b + n) > 0 && vbars - (b + n) < sec_bars / 2 {
            n = vbars - b;
        }
        vs.push((b, n, words_in(b, n)));
        b += n;
    }
    let mut rep = vec![0.0f32; vs.len()];
    for i in 0..vs.len() {
        for j in 0..vs.len() {
            if i != j {
                let s = vocal::lyric_similarity(&vs[i].2, &vs[j].2);
                if s >= 0.4 {
                    rep[i] += s;
                }
            }
        }
    }
    let best_rep = rep.iter().cloned().fold(0.0f32, f32::max);
    let kinds: Vec<&str> = (0..vs.len()).map(|i| if best_rep > 0.0 && rep[i] >= best_rep * 0.6 { "chorus" } else { "verse" }).collect();
    let kinds: Vec<&str> = if kinds.iter().all(|k| *k == "verse") && vs.len() >= 2 {
        // nothing repeats: the second half rides as the chorus
        (0..vs.len()).map(|i| if i >= vs.len() / 2 { "chorus" } else { "verse" }).collect()
    } else {
        kinds
    };
    // chords
    let shift = rec.map(|(r, _)| cover::key_shift(&r.key_root, &r.scale, &key_root, &scale)).unwrap_or(0);
    let loop_for = |kind: &str| -> Option<Vec<cover::Ch>> {
        let (r, _) = rec?;
        let s = r.sections.iter().find(|s| s.kind == kind).or_else(|| r.sections.iter().find(|s| s.kind == "verse" || s.kind == "chorus"))?;
        let lp = cover::chord_loop(&r.chords, s.bar0, s.bars);
        if lp.iter().all(|c| c.is_none()) {
            return None;
        }
        Some(lp.into_iter().map(|c| cover::transpose_ch(c, shift)).collect())
    };
    let key_pc = crate::theory::pitch_class(&key_root)?;
    let harm = vocal::harmonize(&mel, vbars * 2, 2.0, key_pc, &scale)?;
    let harm_ch: Vec<cover::Ch> = harm.iter().map(|c| {
        let minor = c.intervals.get(1).map(|i| *i == 3).unwrap_or(false);
        Some((c.root_pc, minor))
    }).collect();
    let pattern_for = |kind: &str| -> (cover::DrumPattern, Vec<f32>, Vec<f32>, bool, f32) {
        if let Some((r, _)) = rec {
            let want = if kind == "chorus" { "chorus" } else if kind == "verse" { "verse" } else { "intro" };
            let found = if want == "intro" {
                r.sections.iter().find(|s| s.kind == "intro").or_else(|| r.sections.first().filter(|s| s.bar0 == 0 && s.drums.presence < 0.8))
            } else {
                r.sections.iter().find(|s| s.kind == want).or_else(|| r.sections.iter().find(|s| s.kind == "verse" || s.kind == "chorus"))
            };
            if let Some(s) = found {
                let maxe = r.sections.iter().map(|x| x.energy_db).fold(f32::MIN, f32::max);
                return (s.drums.clone(), s.bass_steps.clone(), s.comp_steps.clone(), s.comp_density >= 6.0, (1.0 + (s.energy_db - maxe) / 20.0).clamp(0.55, 1.0));
            }
        }
        let (d, bb, c, arp) = cover::style_pattern(style, kind);
        let e = match kind { "chorus" => 1.0, "verse" => 0.85, _ => 0.7 };
        (d, bb, c, arp, e)
    };
    let mut sections: Vec<PlanSection> = Vec::new();
    let mk = |name: String, kind: &str, bars: usize, chords: Vec<[cover::Ch; 2]>| -> PlanSection {
        let (d, bb, c, arp, en) = pattern_for(kind);
        PlanSection { name, kind: kind.to_string(), bars, chords, drums: d, bass: bb, comp: c, comp_arp: arp && comp_role != "piano", energy: en }
    };
    let chords_for = |kind: &str, b0: usize, n: usize| -> Vec<[cover::Ch; 2]> {
        match loop_for(kind) {
            Some(lp) => {
                let sung: Vec<(f32, f32, u8)> = mel.iter().filter(|m| m.0 >= (b0 * 4) as f32 && m.0 < ((b0 + n) * 4) as f32).map(|m| (m.0 - (b0 * 4) as f32, m.1, m.2)).collect();
                cover::fit_loop(&lp, n, &sung)
            }
            None => (b0..b0 + n).map(|bb| [harm_ch.get(2 * bb).copied().flatten(), harm_ch.get(2 * bb + 1).copied().flatten()]).collect(),
        }
    };
    let first_kind = kinds.first().copied().unwrap_or("verse");
    if intro > 0 {
        let ch = chords_for(if loop_for("intro").is_some() { "intro" } else { first_kind }, 0, intro.min(vbars)).into_iter().cycle().take(intro).collect();
        sections.push(mk("intro".into(), "intro", intro, ch));
    }
    let mut counts = std::collections::BTreeMap::new();
    for (i, (b0, n, _)) in vs.iter().enumerate() {
        let kind = kinds[i];
        let c = counts.entry(kind).or_insert(0);
        *c += 1;
        sections.push(mk(format!("{kind}{c}"), kind, *n, chords_for(kind, *b0, *n)));
    }
    if outro > 0 {
        let last = sections.last().map(|s| s.chords.clone()).unwrap_or_default();
        let ch: Vec<[cover::Ch; 2]> = if last.is_empty() { vec![[None, None]; outro] } else { last.iter().cycle().take(outro).cloned().collect() };
        sections.push(mk("outro".into(), "outro", outro, ch));
    }
    let start_beat = (intro * 4) as f32 - first_bar_t / beat_s;
    let (vsb, voff) = if start_beat >= 0.0 { (start_beat, 0.0) } else { (0.0, -start_beat * beat_s) };
    info["take"] = json!({"bpm": an.tempo.bpm, "tempo_confidence": an.tempo.confidence, "key": format!("{} {}", an.key_root, an.scale), "notes": an.notes.len(), "range": [an.range.0, an.range.1]});
    if pd == vocal::PitchDetect::YinGated {
        info["take"]["pitch_detect"] = json!("yin_gated");
        info["take"]["note_tuning_cents"] = json!(an.tuning_cents);
        info["take"]["note_tuning_source"] = json!(an.tuning_source);
    }
    // match_tuning (default on with yin_gated): render the backing at the
    // singer's measured tuning when it is at least 15 cents off A440 and the
    // key was not given; 0 = strict A440
    let tuning_applied = match &gated {
        Some(g) if !g.unknown && b_or(a, "match_tuning", true) && s_opt(a, "key").is_none() && g.tuning_cents.abs() >= MATCH_TUNING_MIN_CENTS => g.tuning_cents,
        _ => 0.0,
    };
    if gated_on {
        info["take"]["tuning_applied_cents"] = json!(tuning_applied);
        info["take"]["key_gated"] = match &gated {
            Some(g) => json!({"key": format!("{} {}", g.key_root, g.scale), "used": !g.unknown, "confidence": g.confidence, "margin": g.margin, "runner_up": g.runner_up, "unknown": g.unknown, "frames_used": g.frames_used, "tuning_cents": g.tuning_cents, "histogram": g.histogram}),
            None => json!({"used": false, "note": "too few gated pitch frames; the note-based key is used"}),
        };
    }
    let plan = Plan {
        mode: "free".into(),
        bpm: (bpm * 100.0).round() / 100.0,
        key_root,
        scale,
        sections,
        vocal_start_beat: vsb,
        vocal_offset_s: voff,
        comp_role,
        rec_bar0: None,
        vocal_clips: vec![],
        rec_map: vec![],
        notes: vec![if rec.is_some() {
            format!("free take (not sung along to the record): his tempo {:.1} bpm and key, the record's chord loops moved {shift:+} semitones, its drum and comping patterns", bpm)
        } else {
            format!("no reference: his tempo {:.1} bpm and key, chords harmonised from his melody, '{}' patterns", bpm, if style.is_empty() { "pop" } else { style })
        }],
    };
    Ok((plan, info))
}

// ------------------------------------------------------------------ building

/// Load an SFZ catalog instrument keeping only the zones that play `notes`.
fn load_sfz_pruned(e: &mut Engine, track: &str, cid: &str, notes: &BTreeSet<u8>, one_shot: bool, allow_download: bool) -> Result<usize> {
    let ent = crate::sound_catalog::entry(cid).ok_or_else(|| anyhow!("unknown catalog id {cid}"))?;
    if ent.format != "sfz_git" {
        bail!("{cid} is not an SFZ instrument");
    }
    let repo_dir = crate::soundfont::library_dir().join(ent.repo.replace('/', "__"));
    if !repo_dir.is_dir() && !allow_download {
        bail!("{cid} is not installed (downloads off)");
    }
    let (sfz, _) = crate::sound_catalog::fetch_sfz_git(&ent, 400, 2)?;
    let text = std::fs::read_to_string(&sfz)?;
    let (regions, rel) = multisample::parse_sfz(&text)?;
    let regions = multisample::playable_regions(regions);
    let dir = sfz.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut params = MultisampleParams { release: rel.unwrap_or(0.35), one_shot, round_robin: true, ..Default::default() };
    for (k, r) in regions.iter().enumerate() {
        if !notes.iter().any(|n| r.lo <= *n && *n <= r.hi) {
            continue;
        }
        let sp = dir.join(&r.sample);
        if !sp.is_file() {
            continue;
        }
        let sname = samples::sample_name(&format!("{}_{}", track, sp.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| k.to_string())));
        if !e.project.samples.iter().any(|s| s.name == sname) {
            crate::tools::register_sample(e, SampleInfo { name: sname.clone(), path: sp.to_string_lossy().to_string(), source: format!("{} ({})", ent.name, ent.license), license: ent.license.clone(), author: String::new(), duration: 0.0 })?;
        }
        params.zones.push(Zone { sample: sname, root: r.root, lo_note: r.lo, hi_note: r.hi, vel_lo: r.vel_lo as f32 / 127.0, vel_hi: (r.vel_hi as f32 + 1.0) / 128.0, tune_cents: r.tune, gain_db: r.volume, rr: r.seq.saturating_sub(1) });
    }
    if params.zones.is_empty() {
        bail!("{cid}: no zones cover the notes used");
    }
    // a kit's hi-hat key holds every pedal position (closed .. open, chosen by
    // CC4 in the SFZ, which a sequenced part does not send): keep the closed
    // hits so the 8ths stay tight instead of rotating through open washes
    for hat in [42u8, 44] {
        let fam = |s: &str| { let l = s.to_lowercase(); l.contains("_cl") || l.contains("closed") || l.contains("_tc") };
        let on: Vec<bool> = params.zones.iter().map(|z| z.lo_note <= hat && hat <= z.hi_note && z.hi_note.saturating_sub(z.lo_note) < 12).collect();
        if params.zones.iter().zip(on.iter()).any(|(z, o)| *o && fam(&z.sample)) {
            let mut k = 0;
            params.zones.retain(|z| { let keep = !on[k] || fam(&z.sample); k += 1; keep });
        }
    }
    params.source = format!("{} — {} ({})", ent.name, sfz.to_string_lossy(), ent.license);
    let n = params.zones.len();
    let inst = Instrument::Multisample(params);
    match e.project.track_index(track) {
        Ok(i) => e.project.tracks[i].instrument = inst,
        Err(_) => e.project.tracks.push(Track::new(track, inst)),
    }
    Ok(n)
}

fn gm(e: &mut Engine, track: &str, program: u32, drums: bool) -> Result<Value> {
    call(e, "load_instrument", json!({"track": track, "font": "generaluser_gs", "bank": 0, "program": program, "drums": drums}))
}

/// Instruments per role: installed recorded catalog instruments first
/// (pick_for_role ranking), General MIDI SoundFont (also recorded samples) as
/// the fallback. Returns what each track got.
fn pick_instruments(e: &mut Engine, plan: &Plan, used: &[(String, BTreeSet<u8>)], allow_download: bool, genre: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    for (track, notes) in used {
        let role = match track.as_str() {
            "drums" => "drums",
            "bass" => "bass",
            _ => match plan.comp_role.as_str() {
                "piano" => "piano",
                "acoustic_guitar" => "acoustic_guitar",
                _ => "guitar",
            },
        };
        let mut done = None;
        let picks = crate::sound_catalog::pick_for_role(role, genre, Some(300.0));
        for (_, ent, _) in picks.iter().take(8) {
            if ent.format != "sfz_git" {
                continue;
            }
            // prefer known-good programs
            let ok_name = !(ent.id.contains("keyswitch") || ent.id.contains("noise") || ent.id.contains("_fx"));
            if !ok_name {
                continue;
            }
            match load_sfz_pruned(e, track, &ent.id, notes, role == "drums", allow_download) {
                Ok(z) => {
                    done = Some(json!({"track": track, "role": role, "instrument": ent.name, "catalog_id": ent.id, "license": ent.license, "zones": z}));
                    break;
                }
                Err(_) => continue,
            }
        }
        if done.is_none() {
            let (prog, drums, name) = match role {
                "drums" => (0, true, "GM standard kit"),
                "bass" => (33, false, "GM fingered bass"),
                "piano" => (0, false, "GM grand piano"),
                "acoustic_guitar" => (25, false, "GM steel-string guitar"),
                _ => (27, false, "GM clean electric guitar"),
            };
            if gm(e, track, prog, drums).is_ok() {
                done = Some(json!({"track": track, "role": role, "instrument": name, "font": "generaluser_gs"}));
            }
        }
        out.push(done.unwrap_or(json!({"track": track, "role": role, "error": "no instrument loaded"})));
    }
    // drop decoded samples nothing uses any more
    e.bank = samples::SampleBank::default();
    let _ = e.bank.sync(&e.project.samples);
    out
}

fn notes_json(v: &[NoteOut]) -> Value {
    Value::Array(v.iter().map(|n| json!({"start": (n.start * 100.0).round() / 100.0, "len": (n.len * 100.0).round() / 100.0, "pitch": n.pitch, "vel": (n.vel * 100.0).round() / 100.0})).collect())
}

/// The standing raw-vocal chain (Naman's pick "C", 10:45 AM): HPF ~85 Hz, -4 dB at 300, +3 dB at 3k,
/// light de-ess, 2.5:1 compression, plate + delay sends. No denoise, gate,
/// exciter or tuning on the lead.
fn raw_vocal_chain(e: &mut Engine, track: &str) -> Result<()> {
    call(e, "add_effect", json!({"track": track, "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 85.0, "stages": 2}, {"kind": "bell", "freq": 300.0, "gain_db": -4.0, "q": 0.6}, {"kind": "bell", "freq": 3000.0, "gain_db": 3.0, "q": 0.8}]}}))?;
    // TECH DEBT (known, left as is on purpose; Naman, Oct 14 2026): range_db +4.0
    // has the wrong sign. The de-esser clamps range_db to -24..0, so +4 becomes 0
    // and this de-esser never reduces gain: it is a no-op (the IWBY v5 render is
    // byte-identical with it removed). Not fixed because flipping it to -4 changes
    // every cover golden, and as a 6.5 kHz high split it would cut the 10 kHz+ air
    // along with the sibilance (v5's air gate fails). The fix needs a band-limited
    // dynamic bell in its place AND a coordinated golden migration. The same line
    // is in arrange_tx.rs (the arrange vocal chain). Opt-in alternative today:
    // cover_song's `sibilance_band` (a 7 kHz dynamic bell, off by default).
    call(e, "add_effect", json!({"track": track, "type": "deesser", "params": {"freq": 6500.0, "threshold_db": -30.0, "range_db": 4.0, "release_ms": 60.0}}))?;
    call(e, "add_effect", json!({"track": track, "type": "compressor", "params": {"threshold_db": -20.0, "ratio": 2.5, "attack_ms": 10.0, "release_ms": 100.0, "makeup_db": 3.0, "knee_db": 6.0}}))?;
    call(e, "add_bus", json!({"name": "plate", "preset": "none"}))?;
    call(e, "add_effect", json!({"track": "plate", "type": "reverb", "params": {"size": 0.7, "mix": 1.0, "predelay_ms": 30.0, "mode": "fdn_plate", "decay_s": 1.8, "low_cut_hz": 300.0}}))?;
    call(e, "add_bus", json!({"name": "vdelay", "preset": "none"}))?;
    call(e, "add_effect", json!({"track": "vdelay", "type": "delay", "params": {"steps": 6.0, "feedback": 0.35, "mix": 1.0, "ping_pong": true, "tone": 4500.0}}))?;
    call(e, "add_effect", json!({"track": "vdelay", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 350.0, "stages": 2}, {"kind": "high_cut", "freq": 7000.0}]}}))?;
    // sends drier than v4.2's and a shorter plate: the words have to survive a
    // real kit (ASR on the mix); the tail must not smear consonants into the next word
    call(e, "set_send", json!({"track": track, "bus": "plate", "db": -16.0}))?;
    call(e, "set_send", json!({"track": track, "bus": "vdelay", "db": -25.0}))?;
    Ok(())
}

/// When a drum note's sample starts to sound (s, median over its zones):
/// the first point at 25% of the sample's peak.
fn drum_lag_s(e: &mut Engine, pitch: u8) -> Option<f32> {
    e.bank.sync(&e.project.samples);
    let i = e.project.track_index("drums").ok()?;
    let zones: Vec<String> = match &e.project.tracks[i].instrument {
        Instrument::Multisample(m) => m.zones.iter().filter(|z| z.lo_note <= pitch && pitch <= z.hi_note).map(|z| z.sample.clone()).collect(),
        _ => return None,
    };
    let mut v: Vec<f32> = zones.iter().filter_map(|s| {
        let x = e.bank.get(s)?;
        let pk = x.iter().fold(0.0f32, |m, a| m.max(a.abs()));
        if pk <= 0.0 { return None; }
        x.iter().position(|a| a.abs() >= 0.25 * pk).map(|i| i as f32 / SR)
    }).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// Build the project from a plan: sections, notes, instruments, the take, the
/// mix chains (v4.2's: a narrow duck on the music only while he sings).
pub fn build(e: &mut Engine, plan: &Plan, take: &Path, name: &str, allow_download: bool, genre: Option<&str>) -> Result<Value> {
    let pats: Vec<Value> = plan.sections.iter().map(|s| json!({"name": s.name, "bars": s.bars})).collect();
    call(e, "new_project", json!({"name": name, "bpm": plan.bpm, "key": plan.key_root, "scale": if plan.scale == "minor" { "minor" } else { "major" }, "patterns": pats}))?;
    call(e, "set_arrangement", json!({"sections": plan.sections.iter().map(|s| json!({"pattern": s.name, "repeats": 1})).collect::<Vec<_>>()}))?;
    // notes first (so instruments can be pruned to them)
    let mut per: Vec<(String, Vec<(String, Vec<NoteOut>)>)> = vec![("drums".into(), vec![]), ("bass".into(), vec![]), ("comp".into(), vec![])];
    for s in &plan.sections {
        let (d, b, c) = cover::section_notes(s);
        per[0].1.push((s.name.clone(), d));
        per[1].1.push((s.name.clone(), b));
        per[2].1.push((s.name.clone(), c));
    }
    let used: Vec<(String, BTreeSet<u8>)> = per.iter().map(|(t, v)| (t.clone(), v.iter().flat_map(|(_, ns)| ns.iter().map(|n| n.pitch)).collect())).filter(|(_, s): &(String, BTreeSet<u8>)| !s.is_empty()).collect();
    let picked = pick_instruments(e, plan, &used, allow_download, genre);
    // each drum sample's own lead-in (a recorded hit starts a few ms into its
    // file): snare and hat notes move earlier by how much later than the kick
    // their samples speak, so the backbeat lands where the record's does
    // (Sweater Weather, Big Rusty kit: the snare spoke ~50 ms after the kick)
    let mut lags = serde_json::Map::new();
    if let Some(k) = drum_lag_s(e, 36) {
        let step_s = 60.0 / plan.bpm / 4.0;
        for p in [38u8, 40, 42, 46] {
            if let Some(l) = drum_lag_s(e, p) {
                let d = (l - k).clamp(0.0, 0.08);
                lags.insert(p.to_string(), json!((d * 1000.0).round()));
                if d > 0.003 {
                    for (_, ns) in per[0].1.iter_mut() {
                        for n in ns.iter_mut().filter(|n| n.pitch == p) {
                            n.start = (n.start - d / step_s).max(0.0);
                        }
                    }
                }
            }
        }
    }
    for (track, secs) in &per {
        if e.project.track_index(track).is_err() {
            continue;
        }
        for (pat, ns) in secs {
            if !ns.is_empty() {
                call(e, "add_notes", json!({"pattern": pat, "track": track, "notes": notes_json(ns)}))?;
            }
        }
    }
    // guitar double: same part, hard left/right (v4's wide pair leaves the centre to the voice)
    let guitar = plan.comp_role != "piano";
    if guitar && e.project.track_index("comp").is_ok() {
        let i = e.project.track_index("comp")?;
        let mut t2 = e.project.tracks[i].clone();
        t2.name = "comp2".into();
        e.project.tracks.push(t2);
        for p in e.project.patterns.iter_mut() {
            let ns: Vec<crate::project::Note> = p.notes("comp").to_vec();
            if !ns.is_empty() {
                *p.notes_mut("comp2") = ns.iter().map(|n| { let mut m = n.clone(); m.start += 0.08; m }).collect();
            }
        }
        e.project.ensure_fx_ids();
    }
    // the take
    call(e, "import_sample", json!({"name": "take", "path": take.to_string_lossy()}))?;
    call(e, "add_track", json!({"name": "vox", "preset": "pad", "volume_db": 0.0}))?;
    let dur = e.project.samples.iter().find(|s| s.name == "take").map(|s| s.duration).unwrap_or(0.0);
    if plan.vocal_clips.is_empty() {
        call(e, "add_audio_clip", json!({"sample": "take", "track": "vox", "start_beat": plan.vocal_start_beat, "offset_s": plan.vocal_offset_s, "length_s": (dur - plan.vocal_offset_s).max(0.5), "fade_in_ms": 5, "fade_out_ms": 30}))?;
    } else {
        for c in &plan.vocal_clips {
            let len = c[2].min(dur - c[1]).max(0.1);
            call(e, "add_audio_clip", json!({"sample": "take", "track": "vox", "start_beat": c[0], "offset_s": c[1], "length_s": len, "fade_in_ms": 10, "fade_out_ms": 60}))?;
        }
    }
    raw_vocal_chain(e, "vox")?;
    // music chains
    call(e, "add_bus", json!({"name": "hall", "preset": "none"}))?;
    call(e, "add_effect", json!({"track": "hall", "type": "reverb", "params": {"size": 0.7, "mix": 1.0, "predelay_ms": 20.0, "mode": "fdn_hall", "decay_s": 3.0, "low_cut_hz": 200.0}}))?;
    call(e, "add_effect", json!({"track": "hall", "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 3500.0, "gain_db": -3.0}]}}))?;
    // the music's room makes way for him too (same narrow duck, only while he sings)
    call(e, "add_effect", json!({"track": "hall", "type": "dynamic_eq", "params": {"freq": 2000.0, "q": 0.8, "threshold_db": -45.0, "ratio": 3.0, "range_db": -6.0, "attack_ms": 10.0, "release_ms": 150.0, "source": "vox"}}))?;
    if e.project.track_index("drums").is_ok() {
        call(e, "add_effect", json!({"track": "drums", "type": "compressor", "params": {"threshold_db": -16.0, "ratio": 3.0, "attack_ms": 15.0, "release_ms": 90.0, "makeup_db": 2.0}}))?;
        call(e, "add_effect", json!({"track": "drums", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 35.0}, {"kind": "bell", "freq": 2300.0, "gain_db": -2.5, "q": 0.7}]}}))?;
        call(e, "set_send", json!({"track": "drums", "bus": "hall", "db": -16.0}))?;
        call(e, "add_effect", json!({"track": "drums", "type": "dynamic_eq", "params": {"freq": 2500.0, "q": 0.8, "threshold_db": -45.0, "ratio": 3.0, "range_db": -6.0, "attack_ms": 10.0, "release_ms": 150.0, "source": "vox"}}))?;
    }
    if e.project.track_index("bass").is_ok() {
        call(e, "add_effect", json!({"track": "bass", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 32.0}, {"kind": "low_shelf", "freq": 90.0, "gain_db": 2.0}, {"kind": "bell", "freq": 250.0, "gain_db": -3.0, "q": 0.9}, {"kind": "high_shelf", "freq": 2500.0, "gain_db": -3.0}]}}))?;
        call(e, "add_effect", json!({"track": "bass", "type": "compressor", "params": {"threshold_db": -18.0, "ratio": 3.0, "attack_ms": 10.0, "release_ms": 120.0, "makeup_db": 2.0}}))?;
        if e.project.track_index("drums").is_ok() {
            call(e, "add_effect", json!({"track": "bass", "type": "sidechain", "params": {"source": "drums", "amount": 0.25, "release_ms": 110.0, "attack_ms": 3.0}}))?;
        }
    }
    for (t, pan) in [("comp", if guitar { -0.85 } else { 0.0 }), ("comp2", 0.85)] {
        if e.project.track_index(t).is_err() {
            continue;
        }
        if guitar && plan.comp_role == "guitar" {
            call(e, "add_effect", json!({"track": t, "type": "amp_sim", "params": {"model": "clean", "gain_db": 6.0, "bass": 0.75, "mid": 0.5, "treble": 0.7, "presence": 0.65, "cab": "1x12", "mic": "close"}}))?;
        }
        call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": [
            {"kind": "low_cut", "freq": if guitar { 80.0 } else { 60.0 }},
            {"kind": "bell", "freq": 220.0, "gain_db": 1.5, "q": 0.8},
            {"kind": "bell", "freq": 420.0, "gain_db": -2.0, "q": 1.0},
            {"kind": "bell", "freq": 1800.0, "gain_db": -2.0, "q": 1.4},
            {"kind": "bell", "freq": 5000.0, "gain_db": if guitar { 4.0 } else { 2.0 }, "q": 1.0}]}}))?;
        if guitar {
            call(e, "add_effect", json!({"track": t, "type": "chorus", "params": {"rate_hz": 0.7, "depth_ms": 6.0, "mix": 0.45}}))?;
            call(e, "add_effect", json!({"track": t, "type": "delay", "params": {"steps": 3.0, "feedback": 0.3, "mix": 0.18, "ping_pong": true, "tone": 3500.0}}))?;
        }
        // the duck: narrow, around 2 kHz, only while he sings (never a broad static cut)
        call(e, "add_effect", json!({"track": t, "type": "dynamic_eq", "params": {"freq": 2000.0, "q": 0.8, "threshold_db": -45.0, "ratio": 3.0, "range_db": -6.0, "attack_ms": 10.0, "release_ms": 150.0, "source": "vox"}}))?;
        call(e, "set_mixer", json!({"track": t, "pan": pan}))?;
        call(e, "set_send", json!({"track": t, "bus": "hall", "db": -9.0}))?;
    }
    // master: gentle glue + true-peak limiter (delivery normalises to the target)
    call(e, "add_effect", json!({"track": "master", "type": "compressor", "params": {"threshold_db": -10.0, "ratio": 1.5, "attack_ms": 25.0, "release_ms": 150.0, "makeup_db": 0.0}}))?;
    call(e, "add_effect", json!({"track": "master", "type": "limiter", "params": {"ceiling_db": -1.0, "release_ms": 80.0, "true_peak": true}}))?;
    Ok(json!({"drum_lead_in_ms": lags, "instruments": picked, "sections": plan.sections.iter().map(|s| format!("{} {} bars", s.name, s.bars)).collect::<Vec<_>>(), "vocal_start_beat": plan.vocal_start_beat}))
}

// ------------------------------------------------------------------ measuring

/// Energy per ~46 ms frame inside [lo, hi) Hz of a stereo buffer (mid).
fn band_frames(l: &[f32], r: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    const N: usize = 2048;
    let fft = Fft::new(N);
    let n = l.len().min(r.len());
    let frames = n / N;
    let (b0, b1) = (((lo * N as f32 / SR) as usize).max(1), ((hi * N as f32 / SR) as usize).min(N / 2));
    let w: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        for i in 0..N {
            re[i] = 0.5 * (l[f * N + i] + r[f * N + i]) * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut s = 0.0;
        for k in b0..b1 {
            s += re[k] * re[k] + im[k] * im[k];
        }
        out.push(s);
    }
    out
}

const FRAME_S: f32 = 2048.0 / 44100.0;

pub struct Stems {
    vocal: (Vec<f32>, Vec<f32>),
    music: (Vec<f32>, Vec<f32>),
    roles: Vec<(String, Vec<f32>, Vec<f32>)>,
    mix: (Vec<f32>, Vec<f32>),
}

thread_local! {
    /// Pre-fader track cache for one cover_song run: the mix passes, the air
    /// fix and the master move faders, sends and buses, which never change a
    /// track's own buffer, so those renders reuse it (render_cached is
    /// bit-identical to a fresh render). None outside a run.
    static STEM_CACHE: std::cell::RefCell<Option<render::TrackCache>> = const { std::cell::RefCell::new(None) };
}

/// Holds STEM_CACHE for the length of a cover_song run (cleared on drop,
/// errors included).
struct StemCacheRun;

impl StemCacheRun {
    fn start() -> Self {
        STEM_CACHE.with(|c| *c.borrow_mut() = Some(render::TrackCache::default()));
        StemCacheRun
    }
}

impl Drop for StemCacheRun {
    fn drop(&mut self) {
        STEM_CACHE.with(|c| *c.borrow_mut() = None);
    }
}

/// render::render, through the run's track cache when one is held.
fn render_run(p: &crate::project::Project, e: &Engine, opts: &RenderOptions) -> Result<render::Mix> {
    STEM_CACHE.with(|c| match c.borrow_mut().as_mut() {
        Some(tc) => render::render_cached(p, &e.bank, opts, Some(tc)),
        None => render::render(p, &e.bank, opts),
    })
}

fn render_stems(e: &mut Engine, master: bool) -> Result<Stems> {
    e.bank.sync(&e.project.samples);
    let mut p = e.project.clone();
    if !master {
        p.master_effects.clear();
    }
    let m = render_run(&p, e, &RenderOptions { keep_stems: true, tail: 2.0, ..Default::default() })?;
    let n = m.left.len();
    let mut vocal = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut music = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut roles = Vec::new();
    let vocal_names = ["vox", "plate", "vdelay"];
    let routed: BTreeSet<String> = p.tracks.iter().filter(|t| t.output.is_some()).map(|t| t.name.clone()).collect();
    for s in m.stems.iter().chain(m.bus_stems.iter()) {
        if routed.contains(&s.name) {
            continue;
        }
        let dst = if vocal_names.contains(&s.name.as_str()) { &mut vocal } else { &mut music };
        for i in 0..n.min(s.left.len()) {
            dst.0[i] += s.left[i];
            dst.1[i] += s.right[i];
        }
        if ["drums", "bass", "comp", "comp2"].contains(&s.name.as_str()) {
            roles.push((s.name.clone(), s.left.clone(), s.right.clone()));
        }
    }
    Ok(Stems { vocal, music, roles, mix: (m.left, m.right) })
}

/// Vocal presence (dB, 1-4 kHz) inside sung frames, per section: (name, median, p25, frames).
fn presence(st: &Stems, plan: &Plan) -> Vec<(String, f32, f32, usize)> {
    let v = band_frames(&st.vocal.0, &st.vocal.1, 1000.0, 4000.0);
    let m = band_frames(&st.music.0, &st.music.1, 1000.0, 4000.0);
    let vmax = {
        let mut s = v.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        s.get(s.len() * 98 / 100).copied().unwrap_or(0.0)
    };
    let thr = vmax * 10f32.powf(-3.5);
    let beat_s = 60.0 / plan.bpm;
    let mut out = Vec::new();
    let mut b0 = 0.0f32;
    for s in &plan.sections {
        let (t0, t1) = (b0 * beat_s, (b0 + (s.bars * 4) as f32) * beat_s);
        b0 += (s.bars * 4) as f32;
        let mut d: Vec<f32> = Vec::new();
        for f in ((t0 / FRAME_S) as usize)..((t1 / FRAME_S) as usize).min(v.len()).min(m.len()) {
            if v[f] > thr {
                d.push(db(v[f]) - db(m[f]));
            }
        }
        if d.len() < 10 {
            continue;
        }
        d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.push((s.name.clone(), r1(d[d.len() / 2]), r1(d[d.len() / 4]), d.len()));
    }
    out
}

fn energy_db(l: &[f32], r: &[f32], lo: f32, hi: f32) -> f32 {
    let f = band_frames(l, r, lo, hi);
    // active frames only (the part plays): mean of frames within 30 dB of its loud frames
    let mut s = f.clone();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let loud = s.get(s.len() * 95 / 100).copied().unwrap_or(0.0);
    let act: Vec<f32> = f.into_iter().filter(|x| *x > loud * 1e-3).collect();
    if act.is_empty() {
        return -120.0;
    }
    db(act.iter().sum::<f32>() / act.len() as f32)
}

fn role_band(name: &str) -> (f32, f32) {
    match name {
        "drums" => (30.0, 16000.0),
        "bass" => (30.0, 250.0),
        _ => (250.0, 5000.0),
    }
}

/// Scale a track's instrument output (big corrections; faders stop at +12 dB).
fn scale_instrument(e: &mut Engine, track: &str, g_db: f32) -> Result<()> {
    let i = e.project.track_index(track)?;
    let k = 10f32.powf(g_db / 20.0);
    match &mut e.project.tracks[i].instrument {
        Instrument::Multisample(m) => m.gain *= k,
        Instrument::SoundFont(p) => p.gain *= k,
        _ => e.project.tracks[i].volume_db = (e.project.tracks[i].volume_db + g_db).clamp(-60.0, 12.0),
    }
    Ok(())
}

/// Depth of the narrow duck that works only while he sings (comping full, drums 60%).
fn set_duck(e: &mut Engine, range_db: f32) {
    for t in e.project.tracks.iter_mut() {
        let k = match t.name.as_str() {
            "comp" | "comp2" => 1.0,
            "hall" => 1.0,
            // the kit ducks less: a full-depth duck pumps it and hides the snare
            "drums" => 0.6,
            _ => continue,
        };
        for fx in t.effects.iter_mut() {
            if let crate::fx::Effect::DynamicEq(d) = fx {
                if d.source == "vox" {
                    d.range_db = range_db * k;
                }
            }
        }
    }
}

/// Even out the take's sections (clip gain before the vocal chain), the way an
/// engineer rides a quieter chorus up: the take is split at the quietest
/// moment near each section line and each piece gets its own gain (0..+9 dB).
pub fn ride_vocal(e: &mut Engine, plan: &Plan, take_x: &[f32]) -> Result<Value> {
    let beat_s = 60.0 / plan.bpm;
    let cs = plan.vocal_start_beat * beat_s;
    let off = plan.vocal_offset_s;
    let dur = take_x.len() as f32 / SR;
    let fr = band_frames(take_x, take_x, 1000.0, 4000.0);
    let mut srt = fr.clone();
    srt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let thr = srt.get(srt.len() * 98 / 100).copied().unwrap_or(0.0) * 10f32.powf(-3.5);
    // section starts in take time
    let mut starts = Vec::new();
    let mut b = 0.0f32;
    for s in &plan.sections {
        starts.push(b * beat_s - cs + off);
        b += (s.bars * 4) as f32;
    }
    starts.push(b * beat_s - cs + off);
    let mut meds: Vec<Option<f32>> = Vec::new();
    for i in 0..plan.sections.len() {
        let (t0, t1) = (starts[i].max(off), starts[i + 1].min(dur));
        let mut v: Vec<f32> = Vec::new();
        if t1 > t0 {
            for f in ((t0 / FRAME_S) as usize)..((t1 / FRAME_S) as usize).min(fr.len()) {
                if fr[f] > thr {
                    v.push(db(fr[f]));
                }
            }
        }
        meds.push(if v.len() >= 20 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(v[v.len() / 2])
        } else {
            None
        });
    }
    let top = meds.iter().flatten().cloned().fold(f32::MIN, f32::max);
    let gains: Vec<f32> = meds.iter().map(|m| match m {
        Some(m) if top - m > 1.5 => ((top - m - 1.0) * 10.0).round().clamp(0.0, 90.0) / 10.0,
        _ => 0.0,
    }).collect();
    if gains.iter().all(|g| *g == 0.0) {
        return Ok(json!({"sections_lifted": 0}));
    }
    // quietest 20 ms of the raw take within 0.6 s of a time
    let quiet_near = |t: f32| -> f32 {
        let w = (0.02 * SR) as usize;
        let (a, z) = (((t - 0.6).max(off) * SR) as usize, (((t + 0.6).min(dur)) * SR) as usize);
        let mut best = (t, f32::MAX);
        let mut i = a;
        while i + w < z.min(take_x.len()) {
            let e2: f32 = take_x[i..i + w].iter().map(|x| x * x).sum();
            if e2 < best.1 {
                best = ((i + w / 2) as f32 / SR, e2);
            }
            i += w / 2;
        }
        best.0
    };
    // segments of equal gain
    let mut segs: Vec<(f32, f32, f32)> = Vec::new();
    for i in 0..plan.sections.len() {
        let (t0, t1) = (starts[i].max(off), starts[i + 1].min(dur));
        if t1 <= t0 {
            continue;
        }
        match segs.last_mut() {
            Some(l) if (l.2 - gains[i]).abs() < 0.6 => l.1 = t1,
            _ => segs.push((t0, t1, gains[i])),
        }
    }
    for k in 1..segs.len() {
        let c = quiet_near(segs[k].0);
        segs[k - 1].1 = c;
        segs[k].0 = c;
    }
    e.project.audio_clips.retain(|c| c.track != "vox");
    for (t0, t1, g) in &segs {
        if t1 - t0 < 0.05 {
            continue;
        }
        call(e, "add_audio_clip", json!({"sample": "take", "track": "vox", "start_beat": (t0 - off + cs) / beat_s, "offset_s": t0, "length_s": t1 - t0, "gain_db": g, "fade_in_ms": 8, "fade_out_ms": 8}))?;
    }
    Ok(json!({"pieces": segs.len(), "gains_db": segs.iter().map(|s| json!({"from_s": r1(s.0), "to_s": r1(s.1), "gain_db": s.2})).collect::<Vec<_>>()}))
}

/// Level-match the parts (to the record's balance when there is one), then
/// set the beat under the voice: a bounded overall offset (+-6 dB) and, where a
/// section still needs room, a deeper narrow duck that acts only while he
/// sings. Measured from stems renders (a few seconds each), never static cuts.
pub fn mix(e: &mut Engine, plan: &Plan, rec: Option<&SongAnalysis>, rec_fe: Option<&Features>, target_presence: f32) -> Result<Value> {
    let st = render_stems(e, false)?;
    let lvl: Vec<(String, f32)> = st.roles.iter().map(|(n, l, r)| { let (lo, hi) = role_band(n); (n.clone(), energy_db(l, r, lo, hi)) }).collect();
    drop(st);
    let get = |n: &str| lvl.iter().find(|x| x.0 == n).map(|x| x.1);
    // targets relative to the drums: producer defaults, nudged toward the record (+-4 dB)
    let (mut t_bass, mut t_comp) = (-3.0f32, -5.0f32);
    let mut from_ref = Value::Null;
    if let Some(r) = rec {
        let full: Vec<&cover::RefSection> = r.sections.iter().filter(|s| s.drums.presence > 0.3).collect();
        if !full.is_empty() {
            let rb = full.iter().map(|s| s.bass_db - s.perc_db).sum::<f32>() / full.len() as f32;
            // the record's comp band also holds its singer: allow ~4 dB for that
            // (measured on Sweater Weather: 4 dB left the guitars only +5 dB under his voice)
            let rc = full.iter().map(|s| s.comp_db - s.perc_db).sum::<f32>() / full.len() as f32 - 6.0;
            from_ref = json!({"bass_vs_drums": r1(rb), "comp_vs_drums": r1(rc)});
            t_bass = (t_bass + (rb - t_bass).clamp(-4.0, 4.0)).clamp(-9.0, 3.0);
            t_comp = (t_comp + (rc - t_comp).clamp(-4.0, 4.0)).clamp(-10.0, 1.0);
        }
    }
    let mut gains: Vec<(String, f32)> = Vec::new();
    if let Some(d) = get("drums") {
        if let Some(b) = get("bass") {
            gains.push(("bass".into(), (d + t_bass - b).clamp(-30.0, 30.0)));
        }
        let comp_names: Vec<&str> = ["comp", "comp2"].into_iter().filter(|n| get(n).is_some()).collect();
        if !comp_names.is_empty() {
            let c = db(comp_names.iter().map(|n| 10f32.powf(get(n).unwrap() / 10.0)).sum::<f32>());
            let g = (d + t_comp - c).clamp(-30.0, 30.0);
            for n in comp_names {
                gains.push((n.into(), g));
            }
        }
    }
    for (n, g) in &gains {
        scale_instrument(e, n, *g)?;
    }
    // the beat under the voice, section by section: step volume automation on
    // every music track (+4..-6 dB per section), then a deeper narrow duck
    // (only while he sings) where a section still needs room
    // the voice's own returns never ride with the music
    let _music_tracks0: Vec<(String, f32)> = e.project.tracks.iter().filter(|t| !["vox", "plate", "vdelay"].contains(&t.name.as_str())).map(|t| (t.name.clone(), t.volume_db)).collect();
    // what masks him at 1-4 kHz moves most: the drums (hats, snare crack), then the comping
    // (stems of Sweater Weather's chorus: drums +10, guitars +14/+16 under him)
    let weight = |t: &str| -> f32 { match t { "drums" => 1.0, "bass" => 0.5, "comp" | "comp2" => 0.7, _ => 1.0 } };
    let nsec = plan.sections.len();
    let mut starts = Vec::new();
    let mut acc = 0.0f32;
    for s in &plan.sections {
        starts.push(acc);
        acc += (s.bars * 4) as f32;
    }
    // his level first: the take arrives at whatever level he recorded it (here
    // ~18 dB under a real kit at 1-4 kHz), so the voice fader comes up until the
    // typical section reads the target; section moves and the duck handle the rest
    let mut vocal_lift = Value::Null;
    {
        let st = render_stems(e, false)?;
        let pres = presence(&st, plan);
        drop(st);
        if !pres.is_empty() {
            let meds: Vec<f32> = pres.iter().map(|p| p.1).collect();
            let m = cover_median(&meds);
            let lift = (target_presence + 1.0 - m).clamp(0.0, 30.0);
            if lift > 0.5 {
                let vi = e.project.track_index("vox")?;
                let cur = e.project.tracks[vi].volume_db;
                let fader = (lift).min(12.0 - cur).max(0.0);
                e.project.tracks[vi].volume_db = cur + fader;
                let rest = lift - fader;
                if rest > 0.05 {
                    // past the fader's +12: every part comes down by the rest (same thing, more headroom)
                    // the guitars already sit ~6 dB under the kit (the record's balance)
                    // and go inaudible if they take this cut too: drums and bass take it
                    for t in ["drums", "bass"] {
                        if e.project.track_index(t).is_ok() {
                            scale_instrument(e, t, -rest)?;
                        }
                    }
                }
                vocal_lift = json!({"median_before_db": r1(m), "lift_db": r1(lift), "fader_db": r1(fader), "parts_down_db": r1(rest)});
            }
        }
    }
    let music_tracks: Vec<(String, f32)> = e.project.tracks.iter().filter(|t| !["vox", "plate", "vdelay"].contains(&t.name.as_str())).map(|t| (t.name.clone(), t.volume_db)).collect();
    let mut cum = vec![0.0f32; nsec];
    let mut duck = -6.0f32;
    let mut passes = Vec::new();
    for pass in 0..4 {
        let st = render_stems(e, false)?;
        let pres = presence(&st, plan);
        drop(st);
        if pres.is_empty() {
            break;
        }
        passes.push(json!({"pass": pass, "duck_db": r1(duck), "sections": pres.iter().map(|p| json!({p.0.clone(): p.1})).collect::<Vec<_>>()}));
        if pass == 3 {
            break;
        }
        let mut changed = false;
        let mut short_max = 0.0f32;
        for (name, med, _, _) in &pres {
            let Some(i) = plan.sections.iter().position(|s| &s.name == name) else { continue };
            if *med < target_presence - 1.5 || *med > target_presence + 5.0 {
                let d = (med - (target_presence + 1.0)).clamp(-10.0 - cum[i], 4.0 - cum[i]);
                if d.abs() > 0.3 {
                    cum[i] += d;
                    changed = true;
                }
                let after = med - d;
                if after < target_presence - 1.5 {
                    short_max = short_max.max(target_presence - 1.5 - after);
                }
            }
        }
        if short_max > 0.0 && duck > -12.0 {
            duck = (duck - short_max.max(1.5)).max(-12.0);
            set_duck(e, duck);
            changed = true;
        }
        if !changed {
            break;
        }
        // sections without singing follow the typical vocal section
        let sung: Vec<f32> = (0..nsec).filter(|i| pres.iter().any(|p| p.0 == plan.sections[*i].name)).map(|i| cum[i]).collect();
        let typical = if sung.is_empty() { 0.0 } else { cover_median(&sung) };
        let lvl: Vec<f32> = (0..nsec).map(|i| if pres.iter().any(|p| p.0 == plan.sections[i].name) { cum[i] } else { typical }).collect();
        for (t, base) in &music_tracks {
            let k = weight(t);
            let pts: Vec<Value> = (0..nsec).map(|i| json!([starts[i], r1(base + k * lvl[i])])).collect();
            call(e, "add_automation", json!({"track": t, "param": "volume", "points": pts, "curve": "step"}))?;
        }
    }
    let offset = cover_median(&cum);
    // tonal balance against the record (Naman: "the exact same beat as sweater
    // weather"): our choruses vs the record's mapped choruses, octave by octave,
    // corrected on the music tracks only (the lead keeps its own chain). The
    // 1.25-5 kHz octaves hold the singer on both sides, so they move half way.
    let mut tone_fix = Value::Null;
    if let (Some(ra), Some(rfe)) = (rec, rec_fe) {
        let ch = mapped_choruses(ra, plan);
        if !ch.is_empty() {
            let st = render_stems(e, false)?;
            let mono: Vec<f32> = st.mix.0.iter().zip(st.mix.1.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
            drop(st);
            let ofe = cover::features(&mono);
            drop(mono);
            let (mut rt, mut ot, mut w) = ([0.0f32; 9], [0.0f32; 9], 0.0f32);
            for (r0, re, s0) in &ch {
                let len = re - r0;
                let (a, b) = (cover::tone_db(rfe, *r0, *re), cover::tone_db(&ofe, *s0, s0 + len));
                for i in 0..9 {
                    rt[i] += a[i] * len;
                    ot[i] += b[i] * len;
                }
                w += len;
            }
            let w = w.max(1e-9);
            let mut diff = [0.0f32; 9];
            for i in 0..9 {
                rt[i] /= w;
                ot[i] /= w;
                diff[i] = rt[i] - ot[i];
            }
            // only the shape matters (loudness is mastered later): centre on 160-640 Hz
            let centre = (diff[2] + diff[3]) / 2.0;
            let centres = [57.0f32, 113.0, 226.0, 453.0, 894.0, 1768.0, 3536.0, 7071.0, 12649.0];
            let mut bands = Vec::new();
            let mut applied = Vec::new();
            for i in 0..8 {
                let mut g = diff[i] - centre;
                // 640 Hz-5 kHz is where the record's singer lives: our deficit there is
                // mostly his darker take, not the band, and lifting the band would bury
                // him (p8: presence fell 11 -> 4-7 dB). Half at 640-1.25k, none above.
                if i == 4 {
                    g *= 0.5;
                }
                if i == 5 || i == 6 {
                    g = g.min(0.0);
                }
                let g = g.clamp(-6.0, 6.0);
                applied.push(r1(g));
                if g.abs() < 0.75 {
                    continue;
                }
                if i == 0 {
                    bands.push(json!({"kind": "low_shelf", "freq": 90.0, "gain_db": r1(g)}));
                } else {
                    bands.push(json!({"kind": "bell", "freq": centres[i], "gain_db": r1(g), "q": 1.0}));
                }
            }
            if !bands.is_empty() {
                for t in ["drums", "bass", "comp", "comp2"] {
                    if e.project.track_index(t).is_ok() {
                        call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": bands.clone()}}))?;
                    }
                }
            }
            let mut fx = json!({"record_octaves_db": rt.iter().map(|v| r1(*v)).collect::<Vec<_>>(), "ours_before_db": ot.iter().map(|v| r1(*v)).collect::<Vec<_>>(), "eq_db_40_to_10k": applied});
            if !bands.is_empty() {
                // brighter music masks him more: the narrow duck takes up any shortfall
                let st = render_stems(e, false)?;
                let pres = presence(&st, plan);
                drop(st);
                let worst = pres.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                fx["presence_after"] = json!(pres.iter().map(|p| json!({p.0.clone(): p.1})).collect::<Vec<_>>());
                if worst < 8.5 {
                    // the band comes down around him (his fader is at its top already;
                    // mastering restores the loudness), never a deeper duck: a deep duck
                    // pumps the kit and eats the snare (p8: snare match 87 -> 84%)
                    let short = (8.5 - worst).min(6.0);
                    for t in ["drums", "bass", "comp", "comp2"] {
                        if e.project.track_index(t).is_ok() {
                            scale_instrument(e, t, -short)?;
                        }
                    }
                    fx["music_down_db"] = json!(r1(short));
                }
            }
            tone_fix = fx;
        }
    }
    // drums against the record: the percussive share of our choruses read the
    // same way as the record's (v4.2 sat ~2 dB under the record and was right);
    // the drum part alone moves, then the voice is checked once more
    let mut drums_fix = Value::Null;
    if let (Some(ra), Some(rfe)) = (rec, rec_fe) {
        let ch = mapped_choruses(ra, plan);
        if !ch.is_empty() && e.project.track_index("drums").is_ok() {
            let target_off = f_opt_v(plan, -1.0);
            let mut fix = json!({});
            let mut gains = Vec::new();
            // the share answers sub-linearly (louder drums also raise the total):
            // measure, move, re-measure, up to 3 times
            for it in 0..3 {
                let st = render_stems(e, false)?;
                let mono: Vec<f32> = st.mix.0.iter().zip(st.mix.1.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
                drop(st);
                let ofe = cover::features(&mono);
                drop(mono);
                let (mut rs, mut os, mut w) = (0.0f32, 0.0f32, 0.0f32);
                for (r0, re, s0) in &ch {
                    let len = re - r0;
                    rs += cover::perc_share_db(rfe, *r0, *re) * len;
                    os += cover::perc_share_db(&ofe, *s0, s0 + len) * len;
                    w += len;
                }
                let (pr, po) = (rs / w.max(1e-9), os / w.max(1e-9));
                let diff = po - (pr + target_off);
                if it == 0 {
                    fix = json!({"record_db": r1(pr), "ours_db": r1(po), "target_db": r1(pr + target_off)});
                } else {
                    fix["ours_after_db"] = json!(r1(po));
                }
                if diff.abs() <= 0.75 {
                    break;
                }
                let g = (-diff * 1.5).clamp(-9.0, 9.0);
                scale_instrument(e, "drums", g)?;
                gains.push(r1(g));
            }
            if !gains.is_empty() {
                fix["drums_gain_db"] = json!(gains);
                let st = render_stems(e, false)?;
                let pres = presence(&st, plan);
                drop(st);
                fix["presence_after"] = json!(pres.iter().map(|p| json!({p.0.clone(): p.1})).collect::<Vec<_>>());
                // a voice pushed under the gate by louder drums gets the deeper duck
                let worst = pres.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                if worst < 8.5 {
                    // the whole band comes down around him (keeps the drums share)
                    let short = (8.5 - worst).min(6.0);
                    for t in ["drums", "bass", "comp", "comp2"] {
                        if e.project.track_index(t).is_ok() {
                            scale_instrument(e, t, -short)?;
                        }
                    }
                    fix["music_down_db"] = json!(r1(short));
                }
            }
            drums_fix = fix;
        }
    }
    Ok(json!({
        "vocal_level": vocal_lift,
        "drums_vs_record": drums_fix,
        "tone_vs_record": tone_fix,
        "role_levels_db": lvl.iter().map(|(n, v)| json!({n.clone(): r1(*v)})).collect::<Vec<_>>(),
        "targets_vs_drums": {"bass": r1(t_bass), "comp": r1(t_comp)},
        "record_balance": from_ref,
        "role_gains_db": gains.iter().map(|(n, g)| json!({n.clone(): r1(*g)})).collect::<Vec<_>>(),
        "music_offset_db": r1(offset),
        "section_music_db": plan.sections.iter().zip(cum.iter()).map(|(s, c)| json!({s.name.clone(): r1(*c)})).collect::<Vec<_>>(),
        "duck_db": r1(duck),
        "passes": passes,
    }))
}

fn cover_median(v: &[f32]) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if s.is_empty() { 0.0 } else { s[s.len() / 2] }
}

/// Interior stretches quieter than -35 dBFS (short-term, 0.1 s hops) longer than 0.4 s.
fn silences(l: &[f32], r: &[f32]) -> Vec<(f32, f32)> {
    let hop = (SR * 0.1) as usize;
    let n = l.len().min(r.len());
    let lv: Vec<f32> = (0..n / hop).map(|i| {
        let s: f32 = (i * hop..(i + 1) * hop).map(|j| 0.5 * (l[j] * l[j] + r[j] * r[j])).sum();
        db(s / hop as f32)
    }).collect();
    let first = lv.iter().position(|v| *v > -35.0).unwrap_or(0);
    let last = lv.iter().rposition(|v| *v > -35.0).unwrap_or(0);
    let mut out = Vec::new();
    let mut run: Option<usize> = None;
    for i in first..=last.min(lv.len().saturating_sub(1)) {
        if lv[i] < -35.0 {
            run.get_or_insert(i);
        } else if let Some(s) = run.take() {
            if (i - s) as f32 * 0.1 > 0.4 {
                out.push(((s as f32 * 0.1 * 10.0).round() / 10.0, ((i - s) as f32 * 0.1 * 10.0).round() / 10.0));
            }
        }
    }
    out
}

/// cover_song air fix: the judge's air floor (5-11 kHz share >= -22 dB) is
/// aimed at this, a margin over the floor for the master's limiter.
pub const AIR_TARGET_DB: f32 = -21.5;
/// The parallel exciter bus.
pub const AIR_BUS: &str = "air";
/// Its corner: the bus hears and returns only what lies above this.
pub const AIR_CORNER_HZ: f32 = 5000.0;
/// Most gain the air bus fader gets, dB.
pub const AIR_MAX_SEND_DB: f32 = 6.0;

/// cover_song air fix. Measures the mix's air share (no master, as `mix`
/// left it); when it sits under `AIR_TARGET_DB`, adds a parallel exciter bus
/// fed by the vocal and the bright parts (comps, lead, drums for the hats):
/// a 48 dB/oct high-pass at 5 kHz, an exciter generating harmonics from that
/// band, a second 48 dB/oct high-pass so nothing under 5 kHz returns. The
/// parts' own chains are not touched, so presence (1-4 kHz) and the harsh
/// band keep what they had; an in-line exciter (v3's way) cancels part of
/// the vocal's 1-4 kHz and cost 0.9 dB of presence. The sends stay at 0 dB
/// and the bus fader is solved from one render (mix minus bus stem plus the
/// bus scaled: every band share is a quadratic in the gain) to land on the
/// target, capped at +6 dB. A mix that already has its air is left exactly
/// as it was.
fn air_fix(e: &mut Engine) -> Result<Value> {
    let before = e.project.clone();
    let parts: Vec<String> = ["vox", "comp", "comp2", "lead", "drums"]
        .iter()
        .filter(|n| e.project.track_index(n).is_ok())
        .map(|n| n.to_string())
        .collect();
    if parts.is_empty() || e.project.buses.iter().any(|b| b.name == AIR_BUS) {
        return Ok(json!({"applied": false, "note": "no vocal/bright parts, or an air bus already there"}));
    }
    call(e, "add_bus", json!({"name": AIR_BUS, "preset": "none"}))?;
    for (kind, params) in air_bus_chain() {
        call(e, "add_effect", json!({"track": AIR_BUS, "type": kind, "params": params}))?;
    }
    for p in &parts {
        call(e, "set_send", json!({"track": p, "bus": AIR_BUS, "db": 0.0}))?;
    }
    e.bank.sync(&e.project.samples);
    let mut p = e.project.clone();
    p.master_effects.clear();
    let m = render_run(&p, e, &RenderOptions { keep_stems: true, tail: 2.0, ..Default::default() })?;
    let Some(bus) = m.bus_stems.iter().find(|s| s.name == AIR_BUS) else {
        e.project = before;
        return Ok(json!({"applied": false, "note": "the air bus rendered nothing"}));
    };
    let n = m.left.len().min(bus.left.len());
    // the judge's shares are energies of the mono mix through linear band
    // filters, so with the bus at gain g every band's energy is the
    // quadratic a + 2gb + g^2c of the dry mix (mix minus bus) and the bus:
    // one pass per band, then the send is solved without rendering again
    let dry: Vec<f32> = (0..n).map(|i| 0.5 * ((m.left[i] - bus.left[i]) + (m.right[i] - bus.right[i]))).collect();
    let wet: Vec<f32> = (0..n).map(|i| 0.5 * (bus.left[i] + bus.right[i])).collect();
    let quad = |x: &[f32], y: &[f32]| -> [f64; 3] {
        let (mut a, mut b, mut c) = (0.0f64, 0.0f64, 0.0f64);
        for (u, v) in x.iter().zip(y) {
            a += (*u as f64) * (*u as f64);
            b += (*u as f64) * (*v as f64);
            c += (*v as f64) * (*v as f64);
        }
        [a, b, c]
    };
    let band = |lo: f32, hi: f32| -> [f64; 3] { quad(&band_pass_judge(&dry, lo, hi), &band_pass_judge(&wet, lo, hi)) };
    let (q_all, q_air, q_pres, q_harsh, q_mud) = (quad(&dry, &wet), band(5000.0, 11000.0), band(1000.0, 4000.0), band(2000.0, 5000.0), band(250.0, 500.0));
    let at = |q: &[f64; 3], g_db: f32| -> f64 {
        let g = crate::dsp::db_to_gain(g_db) as f64;
        q[0] + 2.0 * g * q[1] + g * g * q[2]
    };
    // share in dB at send g (None = the bus off)
    let share = |q: &[f64; 3], g_db: Option<f32>| -> f32 {
        let (e, t) = match g_db { Some(g) => (at(q, g), at(&q_all, g)), None => (q[0], q_all[0]) };
        (10.0 * (e.max(1e-30) / t.max(1e-30)).log10()) as f32
    };
    let shares = |g: Option<f32>| json!({"air_db": r1(share(&q_air, g)), "presence_db": r1(share(&q_pres, g)), "harsh_db": r1(share(&q_harsh, g)), "mud_db": r1(share(&q_mud, g))});
    let air0 = share(&q_air, None);
    if air0 >= AIR_TARGET_DB {
        e.project = before;
        return Ok(json!({"applied": false, "air_db": r1(air0), "target_db": AIR_TARGET_DB}));
    }
    // the send that lands the air share on the target (air rises with it)
    let send = if share(&q_air, Some(AIR_MAX_SEND_DB)) <= AIR_TARGET_DB {
        AIR_MAX_SEND_DB
    } else {
        let (mut lo, mut hi) = (-40.0f32, AIR_MAX_SEND_DB);
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if share(&q_air, Some(mid)) < AIR_TARGET_DB {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (hi * 10.0).ceil() / 10.0
    };
    let (before_sh, after_sh) = (shares(None), shares(Some(send)));
    // the sends stay at 0 dB (the exciter's drive is level-dependent); the
    // bus fader, after the exciter, is the linear gain the solve assumed
    if let Some(b) = e.project.buses.iter_mut().find(|b| b.name == AIR_BUS) {
        b.volume_db = send;
    }
    Ok(json!({
        "applied": true,
        "bus": AIR_BUS,
        "sources": parts,
        "bus_db": send,
        "target_db": AIR_TARGET_DB,
        "before": before_sh,
        "expected": after_sh,
    }))
}

/// The air bus's chain: a 48 dB/oct low cut at the corner, the exciter, the
/// same low cut again.
fn air_bus_chain() -> [(&'static str, Value); 3] {
    let hp = json!({"bands": [{"kind": "low_cut", "freq": AIR_CORNER_HZ, "q": 0.707, "stages": 4}]});
    [
        ("parametric_eq", hp.clone()),
        ("saturator", json!({"mode": "exciter", "drive_db": 12.0, "tone_hz": AIR_CORNER_HZ, "mix": 1.0, "oversample": true})),
        ("parametric_eq", hp),
    ]
}

/// The judge's band filter (mix_rules::band_energy_db): two 12 dB/oct low
/// cuts at `lo`, two high cuts at `hi`.
fn band_pass_judge(x: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    use crate::dsp::{Biquad, BiquadKind};
    let mut f = [
        Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
        Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
        Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
        Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
    ];
    x.iter()
        .map(|v| {
            let mut y = *v;
            for b in f.iter_mut() {
                y = b.process(y);
            }
            y
        })
        .collect()
}

/// Render, master to the target and write the file; then the QA report.
pub fn master_and_qa(e: &mut Engine, plan: &Plan, out: &Path, target_lufs: f32, take_text: Option<(&str, Option<&str>)>, asr: bool) -> Result<(Value, Stems)> {
    let st = render_stems(e, true)?;
    let ext = out.extension().and_then(|x| x.to_str()).unwrap_or("mp3").to_lowercase();
    let o = DeliverOpts { format: ext.clone(), target_lufs: Some(target_lufs), ceiling_dbtp: -1.0, limit: true, bitrate_kbps: 256, ..Default::default() };
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    let dv = deliver(&st.mix.0, &st.mix.1, out, &o)?;
    let (l, r) = samples::decode_stereo(out)?;
    let lu = analysis::loudness(&l, &r);
    let tp = lu.true_peak_dbtp;
    let sil = silences(&l, &r);
    let pres = presence(&st, plan);
    // each part audible: its band level against the rest of the mix in that band
    let mut aud = Vec::new();
    let mut roles: Vec<(String, Vec<f32>, Vec<f32>)> = Vec::new();
    for (n, rl, rr) in &st.roles {
        let key = if n == "comp2" { "comp" } else { n.as_str() };
        match roles.iter_mut().find(|x| x.0 == key) {
            Some(x) => {
                for i in 0..x.1.len().min(rl.len()) {
                    x.1[i] += rl[i];
                    x.2[i] += rr[i];
                }
            }
            None => roles.push((key.to_string(), rl.clone(), rr.clone())),
        }
    }
    for (n, rl, rr) in &roles {
        let (lo, hi) = role_band(n);
        let own = energy_db(rl, rr, lo, hi);
        let all = energy_db(&st.mix.0, &st.mix.1, lo, hi);
        aud.push(json!({"part": n, "share_of_its_band_db": r1(own - all), "audible": own - all > -12.0}));
    }
    let mut rep = json!({
        "file": out,
        "seconds": r1(l.len() as f32 / SR),
        "integrated_lufs": r1(lu.integrated_lufs),
        "true_peak_dbtp": r1(tp),
        "loudness_range_lu": r1(lu.loudness_range_lu),
        "interior_silences": sil.iter().map(|(t, d)| json!({"at_s": t, "len_s": d})).collect::<Vec<_>>(),
        "vocal_presence_1_4k": pres.iter().map(|p| json!({"section": p.0, "median_db": p.1, "p25_db": p.2, "pass": p.1 >= 8.0})).collect::<Vec<_>>(),
        "parts_audible": aud,
        "delivery": dv,
    });
    let pres_ok = !pres.is_empty() && pres.iter().all(|p| p.1 >= 8.0);
    let mut ok = pres_ok && (lu.integrated_lufs - target_lufs).abs() < 1.0 && tp <= -0.9;
    if asr {
        if let Some((txt, lang)) = take_text {
            // read the mix the way a record is read: no voice-activity filter (it is
            // trained on speech and drops whole sung lines under a band; on p5 it lost
            // the first line entirely), greedy decoding
            match vocal::transcribe_with(out, &e.workdir, "base", lang, None, true) {
                Ok(lm) => {
                    let rc = cover::word_recall(txt, &lm.text);
                    rep["asr_word_recall"] = json!((rc * 1000.0).round() / 1000.0);
                    rep["asr_mode"] = json!("base, no voice-activity filter, greedy");
                    rep["asr_gate_75"] = json!(rc >= 0.75);
                    rep["asr_mix_text_start"] = json!(lm.text.chars().take(160).collect::<String>());
                    if rc < 0.5 {
                        ok = false;
                    }
                }
                Err(err) => rep["asr_word_recall"] = json!(format!("not run: {err}")),
            }
        }
    }
    rep["pass"] = json!(ok);
    Ok((rep, st))
}


/// The record's chorus stretches the song plays: (record s0, record s1, song s0).
fn f_opt_v(_p: &Plan, d: f32) -> f32 {
    d
}

/// The record's chorus stretches the song plays: (record s0, record s1, song s0).
fn mapped_choruses(ra: &SongAnalysis, plan: &Plan) -> Vec<(f32, f32, f32)> {
    let bs = 4.0 * 60.0 / ra.bpm;
    let map: Vec<[f32; 3]> = if !plan.rec_map.is_empty() {
        plan.rec_map.clone()
    } else if let (Some(lo), true) = (plan.rec_bar0, plan.mode == "sync") {
        vec![[0.0, ra.bar_times.get(lo).copied().unwrap_or(0.0), 1e6]]
    } else {
        return vec![];
    };
    let mut out = Vec::new();
    for s in ra.sections.iter().filter(|s| s.kind == "chorus") {
        let r0 = ra.bar_times.get(s.bar0).copied().unwrap_or(0.0);
        let r1 = r0 + s.bars as f32 * bs;
        for m in &map {
            let (a, b) = (r0.max(m[1]), r1.min(m[1] + m[2]));
            if b - a >= 2.0 * bs {
                out.push((a, b, m[0] + (a - m[1])));
            }
        }
    }
    out
}

/// The beat gate against the record, read the same way on both: snare onsets
/// in the record's choruses matched (30 ms) by ours, and the drums-band level
/// (percussive share) of ours vs the record there.
fn beat_gate(ra: &SongAnalysis, rfe: &Features, plan: &Plan, mix: &(Vec<f32>, Vec<f32>), drums: Option<(&[f32], &[f32])>) -> Value {
    let ch = mapped_choruses(ra, plan);
    if ch.is_empty() {
        return json!({"checked": false, "why": "no chorus of the record is in the song"});
    }
    let mono: Vec<f32> = mix.0.iter().zip(mix.1.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
    let ofe = cover::features(&mono);
    drop(mono);
    // our snare hits come from our drum part (the mix detector also fires on his consonants)
    let dfe = drums.map(|(l, r)| cover::features(&l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect::<Vec<f32>>()));
    let sfe = dfe.as_ref().unwrap_or(&ofe);
    // the record's snare hits: strong snare-band onsets on its own 16th grid
    // (a programmed record's drums sit on the grid; sung consonants do not)
    let step = 60.0 / ra.bpm / 4.0;
    let on_grid = |t: f32| { let g = (t - ra.downbeat_s) / step; (g - g.round()).abs() * step <= 0.035 };
    let (mut want, mut got_n, mut hit) = (0usize, 0usize, 0.0f32);
    let (mut rs, mut os, mut w) = (0.0f32, 0.0f32, 0.0f32);
    let mut per = Vec::new();
    for (r0, re, s0) in &ch {
        // snare onsets of the record on its grid, at the steps its snare plays in
        // that section (sung consonants and guitar picks fire the snare band too),
        // plus any very strong on-grid hit anywhere (fills)
        let bs = 4.0 * 60.0 / ra.bpm;
        let snare_step = |t: f32| -> bool {
            let b = ((t - ra.downbeat_s) / bs).floor().max(0.0) as usize;
            let st = ((((t - ra.downbeat_s) / step).round() as i64).rem_euclid(16)) as usize;
            ra.sections.iter().find(|s| b >= s.bar0 && b < s.bar0 + s.bars).map(|s| s.drums.snare.get(st).copied().unwrap_or(0.0) > 0.0).unwrap_or(false)
        };
        let strong: Vec<f32> = cover::snare_onsets(rfe, *r0, *re, 1.0);
        let rec_on: Vec<f32> = cover::snare_onsets(rfe, *r0, *re, 0.7).into_iter().filter(|t| on_grid(*t) && (snare_step(*t) || strong.iter().any(|s| (s - t).abs() < 0.005))).collect();
        let all_on = cover::snare_onsets(rfe, *r0, *re, 0.7).len();
        let ours: Vec<f32> = cover::snare_onsets(sfe, *s0, s0 + (re - r0), 0.4).into_iter().map(|t| t - s0 + r0).collect();
        let rc = cover::onset_recall(&rec_on, &ours, 0.03);
        want += rec_on.len();
        got_n += ours.len();
        hit += rc * rec_on.len() as f32;
        let (pr, po) = (cover::perc_share_db(rfe, *r0, *re), cover::perc_share_db(&ofe, *s0, s0 + (re - r0)));
        let len = re - r0;
        rs += pr * len;
        os += po * len;
        w += len;
        per.push(json!({"record_s": [r1x(*r0), r1x(*re)], "song_s": r1x(*s0), "record_snares": rec_on.len(), "record_snare_band_onsets_all": all_on, "matched_pct": r1(rc * 100.0), "drums_share_record_db": r1(pr), "drums_share_ours_db": r1(po)}));
    }
    let recall = if want > 0 { hit / want as f32 } else { 0.0 };
    let (pr, po) = (rs / w.max(1e-9), os / w.max(1e-9));
    json!({
        "checked": true,
        "snare_matched_pct": r1(recall * 100.0),
        "record_snares": want,
        "our_snare_onsets": got_n,
        "snare_pass": recall >= 0.85,
        "drums_share_record_db": r1(pr),
        "drums_share_ours_db": r1(po),
        "drums_level_diff_db": r1(po - pr),
        "drums_level_pass": (po - pr).abs() <= 3.0,
        "choruses": per,
    })
}

fn r1x(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn lufs_match(l: &mut [f32], r: &mut [f32], target: f32) {
    let lu = analysis::loudness(l, r).integrated_lufs;
    let mut g = 10f32.powf((target - lu) / 20.0);
    let pk = l.iter().chain(r.iter()).fold(0.0f32, |m, x| m.max(x.abs())) * g;
    if pk > 0.95 {
        g *= 0.95 / pk;
    }
    l.iter_mut().chain(r.iter_mut()).for_each(|x| *x *= g);
}

/// A/B clip: the record's first chorus (15 s), a second of silence, the same
/// spot of ours; both at -14 LUFS. A listening aid, never part of the song.
fn ab_clip(rec_path: &Path, ra: &SongAnalysis, plan: &Plan, lo_bar: Option<usize>, ours: &(Vec<f32>, Vec<f32>), out: &Path) -> Result<()> {
    let x = samples::decode_file(rec_path)?;
    let sec = ra.sections.iter().find(|s| s.kind == "chorus").or_else(|| ra.sections.iter().max_by(|a, b| a.energy_db.partial_cmp(&b.energy_db).unwrap()));
    let mut rt = sec.map(|s| ra.bar_times.get(s.bar0).copied().unwrap_or(0.0)).unwrap_or(0.0);
    let beat_s = 60.0 / plan.bpm;
    let mapped = mapped_choruses(ra, plan);
    if let Some(m) = mapped.first() {
        rt = m.0;
    }
    let our_t = match (plan.mode.as_str(), lo_bar, sec) {
        _ if !mapped.is_empty() => mapped[0].2,
        ("sync", Some(lo), Some(s)) if s.bar0 >= lo => (s.bar0 - lo) as f32 * 4.0 * beat_s,
        _ => {
            let mut b = 0.0;
            let mut t = 0.0;
            for s in &plan.sections {
                if s.kind == "chorus" {
                    t = b * beat_s;
                    break;
                }
                b += (s.bars * 4) as f32;
            }
            t
        }
    };
    let n = (15.0 * SR) as usize;
    let a0 = ((rt * SR) as usize).min(x.len());
    let (mut al, mut ar): (Vec<f32>, Vec<f32>) = (x[a0..(a0 + n).min(x.len())].to_vec(), x[a0..(a0 + n).min(x.len())].to_vec());
    let b0 = ((our_t * SR) as usize).min(ours.0.len());
    let b1 = (b0 + n).min(ours.0.len());
    let (mut bl, mut br) = (ours.0[b0..b1].to_vec(), ours.1[b0..b1].to_vec());
    lufs_match(&mut al, &mut ar, -14.0);
    lufs_match(&mut bl, &mut br, -14.0);
    let gap = vec![0.0f32; SR as usize];
    let l = [al.as_slice(), &gap, bl.as_slice()].concat();
    let r = [ar.as_slice(), &gap, br.as_slice()].concat();
    let o = DeliverOpts { format: out.extension().and_then(|x| x.to_str()).unwrap_or("mp3").to_string(), target_lufs: None, ceiling_dbtp: -1.0, limit: true, bitrate_kbps: 256, ..Default::default() };
    deliver(&l, &r, out, &o)?;
    Ok(())
}

fn rss_mb() -> f32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f32>().ok()))
        .map(|kb| (kb / 1024.0).round())
        .unwrap_or(0.0)
}

/// cover_song's `sibilance_band` (opt-in, default off; Naman, Oct 14 2026):
/// one self-keyed dynamic EQ bell on "vox" at 7 kHz, Q2, ratio 4, at most
/// -4 dB, attack 1 ms, release 60 ms, threshold -27 dB (B52's IWBY v5 pick,
/// `sibilance_band_threshold_db` moves it), added LAST in vox's chain. It
/// cuts the 6-8 kHz "s" region only while it rises over the threshold and
/// leaves the 2-4.5 kHz consonant core and the 10 kHz+ air alone, which the
/// chain's de-esser (a 6.5 kHz high split, and a no-op today: see the
/// TECH DEBT note in raw_vocal_chain) cannot. Off = the chain as before.
pub const SIBILANCE_BAND_THRESHOLD_DB: f32 = -27.0;

/// The band's effect parameters (dynamic_eq; `source` empty = keyed by vox itself).
pub fn sibilance_band_params(threshold_db: f32) -> Value {
    json!({"freq": 7000.0, "q": 2.0, "threshold_db": threshold_db, "ratio": 4.0, "range_db": -4.0, "attack_ms": 1.0, "release_ms": 60.0})
}

/// The threshold when cover_song's args ask for the band, None when they do not.
fn sibilance_band_asked(a: &Value) -> Option<f32> {
    if !b_or(a, "sibilance_band", false) {
        return None;
    }
    Some(f_opt(a, "sibilance_band_threshold_db").unwrap_or(SIBILANCE_BAND_THRESHOLD_DB).clamp(-60.0, 0.0))
}

fn sibilance_band(e: &mut Engine, threshold_db: f32) -> Result<Value> {
    if e.project.track_index("vox").is_err() {
        return Ok(json!({"added": false, "note": "no vox track"}));
    }
    call(e, "add_effect", json!({"track": "vox", "type": "dynamic_eq", "params": sibilance_band_params(threshold_db)}))?;
    Ok(json!({"added": true, "track": "vox", "position": "last in vox's chain", "freq_hz": 7000.0, "q": 2.0, "ratio": 4.0, "max_cut_db": -4.0, "threshold_db": threshold_db, "attack_ms": 1.0, "release_ms": 60.0}))
}

/// The whole pipeline.
/// cover_song's `vocal_resonance` (opt-in): the resonance tamer
/// (`resonance::tame_track`: at most 4 self-keyed dynamic EQ cuts, Q5, at
/// most -4 dB, ratio 3, on the take's lasting rings at 700 Hz-8 kHz) at the
/// end of "vox"'s chain, measured on vox's pre-fader audio as its chain
/// leaves it. The bands are ordinary chain effects, so they are part of
/// vox's own render-cache key (exact canonical JSON; see
/// `render::TrackKey`): turning the tamer on, or any band setting moving,
/// re-renders vox alone. Nothing found = the chain is untouched. Only the
/// de-esser's band is kept clear: vox's EQ is cover_song's fixed recipe
/// (a broad Q0.8 +3 dB lift at 3 kHz), not a person's targeted move, and
/// keeping a third of an octave clear around it would hide the 2.4-3.8 kHz
/// rings the tamer is for (take_dn2's lasting 3.7 kHz one).
fn vocal_resonance(e: &mut Engine) -> Result<Value> {
    let Ok(vi) = e.project.track_index("vox") else {
        return Ok(json!({"bands": [], "note": "no vox track"}));
    };
    e.bank.sync(&e.project.samples);
    // pre-fader: the measuring copy has vox at 0 dB with no volume lane
    // (neither is in vox's cache key, so its cached buffer is reused)
    let mut p = e.project.clone();
    p.master_effects.clear();
    p.tracks[vi].volume_db = 0.0;
    p.tracks[vi].mute = false;
    let vname = p.tracks[vi].name.clone();
    p.automation.retain(|l| !(l.target.eq_ignore_ascii_case(&vname) && crate::automation::parse_target(&l.param) == Some(crate::automation::Target::Volume)));
    let m = render_run(&p, e, &RenderOptions { keep_stems: true, tail: 2.0, ..Default::default() })?;
    let Some(st) = m.stems.iter().find(|s| s.name == vname && !s.left.is_empty()) else {
        return Ok(json!({"bands": [], "note": "the vox stem is silent"}));
    };
    let x = crate::auto_eq::mono(&st.left, &st.right);
    drop(m);
    let mut log = Vec::new();
    let exclude = crate::resonance::deesser_bands(&e.project.tracks[vi].effects);
    let mut v = crate::resonance::tame_track(&mut e.project.tracks[vi], &x, &exclude, &mut log);
    v["log"] = json!(log);
    Ok(v)
}

fn cover_song(e: &mut Engine, a: &Value) -> Result<Value> {
    let mut clk = Clock::new();
    let _cache = StemCacheRun::start();
    let take = e.resolve(&s_opt(a, "vocal").or_else(|| s_opt(a, "path")).ok_or_else(|| anyhow!("give vocal (the sung take: wav/mp3)"))?);
    if !take.is_file() {
        bail!("no file at {}", take.display());
    }
    let style = s_opt(a, "style").unwrap_or_default();
    let language = s_opt(a, "language");
    let out = e.resolve(&s_opt(a, "out").unwrap_or_else(|| format!("renders/{}_song.mp3", samples::sample_name(&take.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "take".into())))));
    let take_x = samples::decode_file(&take)?;
    // 1. lyrics (also feed the hook finder and the ASR check)
    let lyr = if b_or(a, "lyrics", true) { lyrics_of(e, &take, s_opt(a, "model").as_deref().unwrap_or("base"), language.as_deref()) } else { None };
    clk.mark("transcribe");
    let words = lyr.as_ref().map(|l| l.words()).unwrap_or_default();
    // 2. the reference: given, or found from the lyrics
    let mut ident = Value::Null;
    let mut ref_src = s_opt(a, "reference");
    if ref_src.is_none() && b_or(a, "identify", true) {
        if let Some(l) = &lyr {
            ident = identify(l);
            if ident["identified"] == true {
                ref_src = ident["best"]["url"].as_str().map(|s| s.to_string());
            }
        }
        clk.mark("identify");
    }
    let mut fetched: Option<PathBuf> = None;
    let mut rec: Option<(SongAnalysis, Features, PathBuf)> = None;
    let mut ref_note = Value::Null;
    let mut rec_words: Vec<vocal::Word> = Vec::new();
    if let Some(src) = &ref_src {
        let title = ident["best"]["key"].as_str().map(|s| s.to_string());
        let dur = ident["best"]["duration_s"].as_f64().map(|d| d as f32);
        match fetch_reference_with(src, title.as_deref(), dur) {
            Ok((p, downloaded, from, failed)) => {
                if downloaded {
                    fetched = Some(p.clone());
                }
                ref_note = json!({"fetched_from": from, "failed_first": failed});
                clk.mark("fetch_reference");
                match analysis_of(&p) {
                    Ok((ra, fe)) => {
                        // the record's own sung lyrics with word times (cached next to it)
                        if !words.is_empty() && b_or(a, "lyric_sync", true) {
                            let wp = p.with_extension("words.mix.json");
                            rec_words = std::fs::read_to_string(&wp).ok().and_then(|s| serde_json::from_str::<Vec<vocal::Word>>(&s).ok()).unwrap_or_default();
                            if rec_words.is_empty() {
                                let lang = lyr.as_ref().map(|l| l.language.clone()).filter(|l| !l.is_empty()).or(language.clone());
                                let model = if lang.as_deref() == Some("en") { "base.en" } else { "base" };
                                if let Ok(rl) = vocal::transcribe_with(&p, &e.workdir, model, lang.as_deref(), None, true) {
                                    rec_words = rl.words();
                                    let _ = std::fs::write(&wp, serde_json::to_string(&rec_words).unwrap_or_default());
                                }
                            }
                            clk.mark("record_lyrics");
                        }
                        rec = Some((ra, fe, p))
                    }
                    Err(err) => ref_note = json!(format!("reference analysis failed: {err}")),
                }
                clk.mark("analyze_reference");
            }
            Err(err) => {
                ref_note = json!(format!("{err}"));
                clk.mark("fetch_reference_failed");
            }
        }
    }
    // 3. plan
    let take_fe = cover::features(&take_x);
    let (plan, pinfo) = make_plan(&take_x, &take_fe, &words, rec.as_ref().map(|(r, f, _)| (r, f)), &rec_words, &style, a)?;
    drop(take_fe);
    clk.mark("plan");
    // 4. build
    let name = s_opt(a, "name").unwrap_or_else(|| format!("{} (song)", take.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
    let genre = s_opt(a, "genre");
    let mut built = build(e, &plan, &take, &name, b_or(a, "allow_download", false), genre.as_deref())?;
    // the backing at the singer's tuning (make_plan decides; absent/0 = A440)
    if let Some(tc) = pinfo["take"]["tuning_applied_cents"].as_f64().filter(|t| *t != 0.0) {
        e.project.tuning_cents = tc as f32;
    }
    if b_or(a, "ride_vocal", true) && plan.vocal_clips.is_empty() {
        built["vocal_ride"] = ride_vocal(e, &plan, &take_x)?;
    }
    clk.mark("build");
    // 4b. the vocal resonance tamer (only when asked, vocal_resonance:true):
    // before the mix, so the mix sets his level with his rings tamed
    let resonance = if b_or(a, "vocal_resonance", false) {
        let v = vocal_resonance(e)?;
        clk.mark("vocal_resonance");
        Some(v)
    } else {
        None
    };
    // 4c. the 7 kHz sibilance band (only when asked, sibilance_band:true):
    // last in vox's chain, before the mix, so the mix sets his level with it in
    let sib_band = match sibilance_band_asked(a) {
        Some(thr) => {
            let v = sibilance_band(e, thr)?;
            clk.mark("sibilance_band");
            Some(v)
        }
        None => None,
    };
    // 5. mix
    let target_presence = f_opt(a, "presence_db").unwrap_or(12.0);
    let mut mixed = mix(e, &plan, rec.as_ref().map(|(r, _, _)| r), rec.as_ref().map(|(_, f, _)| f), target_presence)?;
    clk.mark("mix");
    if let Some(v) = resonance {
        mixed["vocal_resonance"] = v;
    }
    if let Some(v) = sib_band {
        mixed["sibilance_band"] = v;
    }
    // 5b. air: a parallel exciter bus when the mix is short of the judge's air
    // floor. On by default only on the free-mode default path (key heard by
    // yin_gated, no key/scale given); legacy and explicit-key runs take it
    // only when asked (air_fix:true)
    let air_default = plan.mode == "free"
        && s_opt(a, "key_detect").map_or(true, |k| k == "yin_gated")
        && s_opt(a, "key").is_none()
        && s_opt(a, "scale").is_none();
    if b_or(a, "air_fix", air_default) {
        mixed["air_fix"] = air_fix(e)?;
        clk.mark("air_fix");
    }
    // 6. master + QA
    let target = f_opt(a, "target_lufs").unwrap_or(-10.0);
    let take_text = lyr.as_ref().map(|l| l.text.clone());
    let (qa, st) = master_and_qa(e, &plan, &out, target, take_text.as_deref().map(|t| (t, language.as_deref())), b_or(a, "qa_asr", true))?;
    let gate = match &rec {
        Some((ra, rfe, _)) => {
            let d = st.roles.iter().find(|r| r.0 == "drums").map(|r| (r.1.as_slice(), r.2.as_slice()));
            beat_gate(ra, rfe, &plan, &st.mix, d)
        }
        None => Value::Null,
    };
    clk.mark("master_qa");
    // 7. A/B against the record
    let mut ab = Value::Null;
    if let Some((ra, _, rp)) = &rec {
        let abp = out.with_file_name(format!("{}_ab.mp3", out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
        let lo_bar = plan.rec_bar0;
        let mix = (st.mix.0.clone(), st.mix.1.clone());
        ab = match ab_clip(rp, ra, &plan, lo_bar, &mix, &abp) {
            Ok(()) => json!(abp),
            Err(err) => json!(format!("A/B clip failed: {err}")),
        };
        clk.mark("ab_clip");
    }
    drop(st);
    if let Some(p) = &fetched {
        if !b_or(a, "keep_reference", false) {
            let _ = std::fs::remove_file(p);
        }
    }
    let proj = out.with_extension("project.json");
    let _ = call(e, "save_project", json!({"path": proj.to_string_lossy()}));
    Ok(json!({
        "file": out,
        "project": proj,
        "ab_clip": ab,
        "mode": plan.mode,
        "bpm": plan.bpm,
        "key": format!("{} {}", plan.key_root, plan.scale),
        "plan_notes": plan.notes,
        "identify": ident,
        "reference": rec.as_ref().map(|(r, _, _)| json!({"source": r.source, "bpm": r.bpm, "key": format!("{} {}", r.key_root, r.scale), "sections": r.sections.iter().map(|s| format!("{} bar {} x{}", s.name, s.bar0, s.bars)).collect::<Vec<_>>(), "deleted_after": fetched.is_some() && !b_or(a, "keep_reference", false), "fetch": ref_note})).unwrap_or(ref_note),
        "plan_info": pinfo,
        "built": built,
        "mix": mixed,
        "qa": qa,
        "beat_gate": gate,
        "wall_time_s": clk.total(),
        "steps": clk.steps,
        "peak_rss_mb": rss_mb(),
    }))
}

fn cover_schema() -> Value {
    obj(json!({
        "vocal": {"type": "string", "description": "the sung take (wav/mp3/m4a), dry voice"},
        "reference": {"type": "string", "description": "optional: the original record as a file, URL or search words (analysis only: never placed in the song)"},
        "style": {"type": "string", "description": "optional notes: genre / feel / instruments (e.g. 'acoustic ballad, piano')"},
        "out": {"type": "string", "description": "output file (mp3/wav/flac), default renders/<take>_song.mp3"},
        "identify": {"type": "boolean", "description": "find the record from the lyrics when no reference is given (default true)"},
        "language": {"type": "string", "description": "lyrics language for the transcriber (e.g. hi, en); default auto"},
        "bpm": {"type": "number"}, "key": {"type": "string"}, "scale": {"type": "string"},
        "sibilance_band": {"type": "boolean", "description": "a 7 kHz sibilance band LAST on the vocal's chain: one self-keyed dynamic EQ bell (Q2, ratio 4, at most -4 dB, attack 1 ms, release 60 ms, threshold sibilance_band_threshold_db, default -27 dB) that cuts the 6-8 kHz 's' region only while it rises, leaving the 2-4.5 kHz consonants and the 10 kHz+ air alone. Default false (off = the song as before)"},
        "sibilance_band_threshold_db": {"type": "number", "description": "the sibilance band's threshold (dB, default -27); only with sibilance_band:true"},
        "vocal_resonance": {"type": "boolean", "description": "tame the take's lasting resonances (a room's or a phone's ring, a harsh formant): up to 4 narrow self-keyed dynamic EQ cuts on the vocal (Q5, at most -4 dB, ratio 3, attack 5 ms, release 80 ms) where its long-term spectrum (1/12-octave bands, 700 Hz-8 kHz) pokes 3 dB over its surroundings in at least 25% of the singing; each cuts only when its ring rises 2 dB over its usual level. Never near the de-esser or the chain's own EQ bands. Default false (off = the song as before)"},
        "air_fix": {"type": "boolean", "description": "when the mix's 5-11 kHz air share is under -21.5 dB (the judge's floor is -22), add a parallel exciter bus (48 dB/oct high-pass at 5 kHz, exciter, high-pass again) fed by the vocal, comps/lead and drums at 0 dB sends, its fader solved to land on -21.5 (max +6 dB); the parts' own chains are untouched. Default: on for free mode with the key heard by yin_gated (no key/scale given), off for legacy and explicit-key runs; false = the mix as it was"},
        "match_tuning": {"type": "boolean", "description": "with key_detect yin_gated and no key given: render every pitched backing instrument at the singer's measured tuning when it is 15+ cents off A440 (drums and the vocal untouched; default true). false = strict A440"},
        "pitch_detect": {"type": "string", "enum": ["yin_gated", "legacy"], "description": "free mode: the pitch track the sung notes are segmented from (melody, note-based key, tempo, phrases). yin_gated (default): only frames with YIN aperiodicity < 0.15 above -55 dBFS, in voiced runs of 120 ms or more, periods read without the CMNDF bias (the frames key_detect yin_gated trusts), the notes rounded on the gated key's tuning. legacy: the original YIN track with a level gate relative to the take; reproduces renders from before the default flip byte-for-byte"},
        "key_detect": {"type": "string", "enum": ["yin_gated", "legacy", "notes"], "description": "free mode: how the singer's key is heard when key/scale are not given. yin_gated (default): pitch frames gated on YIN aperiodicity < 0.15 and -55 dBFS, 120 ms smoothing, octave folding and tuning-offset removal (steadier on noisy, rubato or flat takes); an unknown read (margin < 0.05) falls back to the notes. legacy (= notes): the segmented sung notes only, as before; reproduces earlier renders byte-for-byte"},
        "intro_bars": {"type": "integer"}, "outro_bars": {"type": "integer"}, "section_bars": {"type": "integer"},
        "target_lufs": {"type": "number", "description": "default -10"},
        "presence_db": {"type": "number", "description": "voice over music at 1-4 kHz inside phrases, worst section (default 10; the QA gate is 8)"},
        "allow_download": {"type": "boolean", "description": "fetch catalog instruments that are not installed (default false: installed ones, then the GM SoundFont)"},
        "genre": {"type": "string", "description": "catalog genre tag to prefer when picking instruments"},
        "keep_reference": {"type": "boolean"}, "ride_vocal": {"type": "boolean", "description": "even out the take's sections with clip gain before the chain (default true)"}, "force_free": {"type": "boolean", "description": "fit the grid to the singer even if he sang along"},
        "qa_asr": {"type": "boolean", "description": "transcribe the finished song and report word recall (default true)"},
        "lyrics": {"type": "boolean"}, "model": {"type": "string"}, "name": {"type": "string"}
    }), &["vocal"])
}

const COVER_DESC: &str = "One call: a finished, mastered song around a sung vocal take, in minutes. Transcribes the take; with no reference it searches the lyric lines to find the record (or treats the take as an original); downloads the record for ANALYSIS ONLY (beat grid, key, chords, sections, drum/bass/comping patterns, part balance; deleted afterwards); checks whether he sang along to it (then the record's grid is used) or not (then the grid is fitted to HIM: his tempo, bar lines and key, the record's chords moved to his key; his take is never stretched or tuned); arranges real recorded instruments (installed catalog first), the raw lead chain, a narrow duck on the music only while he sings, levels matched in one stems render, master to -10 LUFS / -1 dBTP, QA report (loudness, silences, vocal presence per section, each part audible, speech-to-text word recall) and an A/B clip vs the record. Every step is its own tool too.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "cover_song", description: COVER_DESC, mutates: true, schema: cover_schema, run: cover_song },
        Tool { name: "make_song_from_reference", description: "Alias of cover_song (same arguments): vocal take + optional reference record -> finished song.", mutates: true, schema: cover_schema, run: cover_song },
        Tool { name: "make_song_from_vocal", description: "Alias of cover_song (same arguments): a sung take alone -> finished song (finds the record from the lyrics, or arranges an original).", mutates: true, schema: cover_schema, run: cover_song },
        Tool {
            name: "identify_song",
            description: "Which song is this take? Transcribes the vocal and searches its lyric lines (in parallel); answers a record only when several lines agree. Returns candidates with links (for fetch_reference).",
            mutates: false,
            schema: || obj(json!({"vocal": {"type": "string"}, "language": {"type": "string"}, "model": {"type": "string"}}), &["vocal"]),
            run: |e, a| {
                let p = e.resolve(&s_req(a, "vocal")?);
                let l = vocal::transcribe(&p, &e.workdir, s_opt(a, "model").as_deref().unwrap_or("base"), s_opt(a, "language").as_deref(), None)?;
                Ok(identify(&l))
            },
        },
        Tool {
            name: "fetch_reference",
            description: "Download a reference record (URL or search words) as a mono WAV for analysis only, into ~/.beatbox/ref_cache. Never place it in a song; delete it when done (delete:true removes a fetched file).",
            mutates: false,
            schema: || obj(json!({"source": {"type": "string"}, "delete": {"type": "string", "description": "path of a fetched reference to remove"}}), &[]),
            run: |_e, a| {
                if let Some(d) = s_opt(a, "delete") {
                    let p = PathBuf::from(&d);
                    if !p.starts_with(ref_cache_dir()) {
                        bail!("only files in the reference cache can be deleted here");
                    }
                    std::fs::remove_file(&p)?;
                    return Ok(json!({"deleted": d}));
                }
                let (p, dl) = fetch_reference(&s_req(a, "source")?)?;
                Ok(json!({"path": p, "downloaded": dl, "note": "analysis only: never sampled or committed"}))
            },
        },
        Tool {
            name: "analyze_song",
            description: "Analyse a record (file): tempo and beat grid, bar lines, key, two chords per bar, sections (intro/verse/chorus/break/outro), the drum pattern per section (kick/snare/hat strength per 16th, from the percussive part), bass and comping rhythm, part balance (percussive / bass / comping band levels). Cached next to the file.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}}), &["path"]),
            run: |e, a| {
                let p = e.resolve(&s_req(a, "path")?);
                let (an, _) = analysis_of(&p)?;
                Ok(serde_json::to_value(an)?)
            },
        },
        Tool {
            name: "align_take",
            description: "Did the singer sing along to the record? Cross-correlates the take against the record: the record time where the take starts (lag_s), how clearly (z), whether both halves agree, and the verdict synced.",
            mutates: false,
            schema: || obj(json!({"vocal": {"type": "string"}, "reference": {"type": "string"}}), &["vocal", "reference"]),
            run: |e, a| {
                let t = samples::decode_file(&e.resolve(&s_req(a, "vocal")?))?;
                let (_, rf) = analysis_of(&e.resolve(&s_req(a, "reference")?))?;
                Ok(serde_json::to_value(cover::align_take(&cover::features(&t), &rf))?)
            },
        },
        Tool {
            name: "align_phrases",
            description: "Line-by-line sync of a sung take to a record's vocal stem (from separate_stems): splits the take into phrases and finds where the record sings each one, keeping the lines in order. method words matches speech-recognised word timing inside a helper process that outputs times only (no lyrics are stored or returned); audio matches melody and syllable rhythm. Returns phrases [{from_s, to_s, at_s, score, followed}] (at_s on the reference's clock) and writes them to out (JSON) for arrange_transcription's vocal_phrases.",
            mutates: false,
            schema: || obj(json!({"vocal": {"type": "string", "description": "the sung take"}, "reference_vocal": {"type": "string", "description": "the record's vocal stem"}, "min_pause_s": {"type": "number", "description": "silence that ends a phrase (default 0.35)"}, "max_phrase_s": {"type": "number", "description": "longer phrases are split at their quietest point (default 6)"}, "method": {"type": "string", "enum": ["auto", "words", "audio"], "description": "words: speech-recognised word timing matched in memory (only times leave the helper; needs faster-whisper with a cached model); audio: melody + syllable rhythm; auto (default) tries words first"}, "model": {"type": "string", "description": "faster-whisper model name already in the local cache (default small)"}, "out": {"type": "string"}}), &["vocal", "reference_vocal"]),
            run: |e, a| {
                let method = s_opt(a, "method").unwrap_or_else(|| "auto".into());
                let vocal = e.resolve(&s_req(a, "vocal")?);
                let refv = e.resolve(&s_req(a, "reference_vocal")?);
                let mut note = Value::Null;
                let mut v = Value::Null;
                if method == "words" || method == "auto" {
                    match align_words(e, &vocal, &refv, a) {
                        Ok(w) => v = w,
                        Err(err) if method == "auto" => note = json!(format!("word timing unavailable ({err}); used the audio match")),
                        Err(err) => return Err(err),
                    }
                }
                if v.is_null() {
                    let t = samples::decode_file(&vocal)?;
                    let r = samples::decode_file(&refv)?;
                    let (ft, fr) = (cover::features(&t), cover::features(&r));
                    drop(t);
                    drop(r);
                    let ph = cover::align_phrases(&ft, &fr, f_opt(a, "min_pause_s").unwrap_or(0.35), f_opt(a, "max_phrase_s").unwrap_or(6.0));
                    if ph.is_empty() {
                        bail!("no phrases found in the take");
                    }
                    v = json!({"phrases": ph, "method": "audio (melody + syllable rhythm)"});
                }
                let ph = v["phrases"].as_array().cloned().unwrap_or_default();
                let mean_score = ph.iter().filter_map(|p| p["score"].as_f64()).sum::<f64>() / ph.len().max(1) as f64;
                let followed = ph.iter().filter(|p| p["followed"].as_bool() == Some(true)).count();
                v["count"] = json!(ph.len());
                v["mean_score"] = json!((mean_score * 1000.0).round() / 1000.0);
                v["followed"] = json!(followed);
                if !note.is_null() {
                    v["note"] = note;
                }
                if let Some(o) = s_opt(a, "out") {
                    let op = e.resolve(&o);
                    if let Some(d) = op.parent() { std::fs::create_dir_all(d)?; }
                    std::fs::write(&op, serde_json::to_string_pretty(&v)?)?;
                }
                Ok(v)
            },
        },
        Tool {
            name: "plan_cover",
            description: "The arrangement plan for a take (no project change): mode sync/free, tempo, key, sections with two chords per bar, drum/bass/comping patterns, where the take sits. Same arguments as cover_song (vocal, reference, style, bpm, key ...).",
            mutates: false,
            schema: cover_schema,
            run: |e, a| {
                let take = e.resolve(&s_opt(a, "vocal").ok_or_else(|| anyhow!("give vocal"))?);
                let x = samples::decode_file(&take)?;
                let lyr = if b_or(a, "lyrics", true) { lyrics_of(e, &take, "base", s_opt(a, "language").as_deref()) } else { None };
                let words = lyr.as_ref().map(|l| l.words()).unwrap_or_default();
                let rec = match s_opt(a, "reference") { Some(r) => Some(analysis_of(&fetch_reference(&r)?.0)?), None => None };
                let fe = cover::features(&x);
                let (plan, info) = make_plan(&x, &fe, &words, rec.as_ref().map(|(r, f)| (r, f)), &[], &s_opt(a, "style").unwrap_or_default(), a)?;
                Ok(json!({"plan": plan, "info": info}))
            },
        },
        Tool {
            name: "qa_song",
            description: "QA report of the current project: renders, masters to target_lufs into out, and reports loudness, true peak, interior silences, vocal presence (1-4 kHz inside phrases) per arrangement section, whether each part (drums, bass, comp) is audible, and speech-to-text word recall against the take's own lyrics (vocal = the take) when asked.",
            mutates: false,
            schema: || obj(json!({"out": {"type": "string"}, "target_lufs": {"type": "number"}, "vocal": {"type": "string", "description": "the take, for word recall"}, "language": {"type": "string"}}), &[]),
            run: |e, a| {
                let p = e.project.clone();
                let plan = Plan { mode: "project".into(), bpm: p.bpm, key_root: p.key_root.clone(), scale: p.scale.clone(), sections: p.arrangement.iter().filter_map(|s| p.patterns.iter().find(|q| q.name == s.pattern).map(|q| PlanSection { name: q.name.clone(), kind: String::new(), bars: (q.steps() / 16) as usize * s.repeats.max(1) as usize, chords: vec![], drums: cover::DrumPattern { kick: vec![], snare: vec![], hat: vec![], presence: 0.0 }, bass: vec![], comp: vec![], comp_arp: false, energy: 1.0 })).collect(), vocal_start_beat: 0.0, vocal_offset_s: 0.0, comp_role: String::new(), rec_bar0: None, vocal_clips: vec![], rec_map: vec![], notes: vec![] };
                let out = e.resolve(&s_opt(a, "out").unwrap_or_else(|| "renders/qa_song.mp3".into()));
                let lang = s_opt(a, "language");
                let txt = match s_opt(a, "vocal") { Some(v) => vocal::transcribe(&e.resolve(&v), &e.workdir, "base", lang.as_deref(), None).ok().map(|l| l.text), None => None };
                let (rep, _) = master_and_qa(e, &plan, &out, f_opt(a, "target_lufs").unwrap_or(-10.0), txt.as_deref().map(|t| (t, lang.as_deref())), txt.is_some())?;
                Ok(rep)
            },
        },
    ]
}

#[cfg(test)]
mod air_fix_tests {
    use super::*;

    /// A short beat whose first two tracks carry cover_song part names, so
    /// the air fix feeds its bus from them.
    fn beat(tag: &str) -> Engine {
        let d = std::env::temp_dir().join(format!("beatbox_air_fix_{tag}"));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("generate_beat", &json!({"style": "lofi", "seed": 7})).unwrap();
        e.project.tracks[0].name = "drums".into();
        e.project.tracks[1].name = "comp".into();
        e.bank.sync(&e.project.samples);
        e
    }

    /// The share of `lo..hi` in the mono mix, as the solve measures it.
    fn share_db(m: &render::Mix, lo: f32, hi: f32) -> f32 {
        let x: Vec<f32> = m.left.iter().zip(&m.right).map(|(l, r)| 0.5 * (l + r)).collect();
        let tot: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum();
        let b: f64 = band_pass_judge(&x, lo, hi).iter().map(|v| (*v as f64) * (*v as f64)).sum();
        (10.0 * (b / tot).log10()) as f32
    }

    fn dry_render(e: &Engine) -> render::Mix {
        let mut p = e.project.clone();
        p.master_effects.clear();
        render::render(&p, &e.bank, &RenderOptions { tail: 2.0, ..Default::default() }).unwrap()
    }

    /// The closed-form bus gain lands a real render where it said it would,
    /// and the bus is only an addition: the parts' own chains are untouched.
    #[test]
    fn air_fix_closed_form_matches_a_real_render() {
        let mut e = beat("solve");
        // darken every part so the mix starts under the target
        for t in 0..e.project.tracks.len() {
            let name = e.project.tracks[t].name.clone();
            call(&mut e, "add_effect", json!({"track": name, "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 5000.0, "gain_db": -6.0, "q": 0.7}]}})).unwrap();
        }
        let before = e.project.clone();
        let air0 = share_db(&dry_render(&e), 5000.0, 11000.0);
        assert!(air0 < AIR_TARGET_DB, "the test beat should be dark ({air0})");
        let r = air_fix(&mut e).unwrap();
        assert_eq!(r["applied"], json!(true), "{r}");
        assert!((r["before"]["air_db"].as_f64().unwrap() as f32 - air0).abs() <= 0.06, "{r} vs {air0}");
        let m = dry_render(&e);
        let air1 = share_db(&m, 5000.0, 11000.0);
        let want = r["expected"]["air_db"].as_f64().unwrap() as f32;
        assert!((air1 - want).abs() <= 0.1, "rendered {air1} vs solved {want}");
        let g = r["bus_db"].as_f64().unwrap() as f32;
        if g < AIR_MAX_SEND_DB {
            assert!(air1 >= AIR_TARGET_DB - 0.1, "{air1}");
        }
        assert_eq!(e.project.tracks.len(), before.tracks.len());
        for (a, b) in e.project.tracks.iter().zip(&before.tracks) {
            assert_eq!(serde_json::to_value(&a.effects).unwrap(), serde_json::to_value(&b.effects).unwrap());
            assert_eq!(a.volume_db, b.volume_db);
        }
        let bus = e.project.buses.iter().find(|b| b.name == AIR_BUS).unwrap();
        assert_eq!(bus.volume_db, g);
        // a second pass finds the bus and changes nothing
        let p1 = serde_json::to_string(&e.project).unwrap();
        assert_eq!(air_fix(&mut e).unwrap()["applied"], json!(false));
        assert_eq!(serde_json::to_string(&e.project).unwrap(), p1);
    }

    /// A mix that already has its air is left exactly as it was: no bus, no
    /// sends, the same project bytes (and so the same render).
    #[test]
    fn air_fix_leaves_a_bright_mix_alone() {
        let mut e = beat("bright");
        for t in 0..2 {
            let name = e.project.tracks[t].name.clone();
            call(&mut e, "add_effect", json!({"track": name, "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 4000.0, "gain_db": 24.0, "q": 0.7}]}})).unwrap();
        }
        let air0 = share_db(&dry_render(&e), 5000.0, 11000.0);
        assert!(air0 >= AIR_TARGET_DB, "the shelved beat should be bright ({air0})");
        let p0 = serde_json::to_string(&e.project).unwrap();
        let r = air_fix(&mut e).unwrap();
        assert_eq!(r["applied"], json!(false), "{r}");
        assert_eq!(serde_json::to_string(&e.project).unwrap(), p0);
    }

    /// No cover_song part names, no bus.
    /// sibilance_band is opt-in: off unless asked; asked, its threshold is
    /// -27 dB unless moved.
    #[test]
    fn sibilance_band_is_opt_in() {
        assert_eq!(sibilance_band_asked(&json!({})), None);
        assert_eq!(sibilance_band_asked(&json!({"sibilance_band": false, "sibilance_band_threshold_db": -20})), None);
        assert_eq!(sibilance_band_asked(&json!({"sibilance_band": true})), Some(-27.0));
        assert_eq!(sibilance_band_asked(&json!({"sibilance_band": true, "sibilance_band_threshold_db": -24})), Some(-24.0));
    }

    /// The band goes LAST on vox's chain with B52's settings, cuts a loud
    /// 7 kHz tone by up to 4 dB, and leaves a 1 kHz tone and a quiet 7 kHz
    /// tone (under the threshold) alone.
    #[test]
    fn sibilance_band_cuts_only_loud_7k() {
        let mut e = beat("sib_band");
        e.project.tracks[2].name = "vox".into();
        let n_before = e.project.tracks[2].effects.len();
        let v = sibilance_band(&mut e, SIBILANCE_BAND_THRESHOLD_DB).unwrap();
        assert_eq!(v["added"], true, "{v}");
        let fx = &e.project.tracks[2].effects;
        assert_eq!(fx.len(), n_before + 1);
        let last = serde_json::to_value(fx.last().unwrap()).unwrap();
        assert_eq!(last["type"], "dynamic_eq", "{last}");
        for (k, want) in [("freq", 7000.0), ("q", 2.0), ("threshold_db", -27.0), ("ratio", 4.0), ("range_db", -4.0), ("attack_ms", 1.0), ("release_ms", 60.0)] {
            assert!((last[k].as_f64().unwrap() - want).abs() < 1e-6, "{k}: {last}");
        }
        assert_eq!(last["source"], "", "self-keyed: {last}");
        // no vox track: nothing added
        let mut e2 = beat("sib_band_novox");
        assert_eq!(sibilance_band(&mut e2, -27.0).unwrap()["added"], false);
        // the DSP: tones in, level change out (steady part, after 0.5 s)
        let band = fx.last().unwrap().clone();
        let n = SR as usize * 2;
        let tone = |f: f32, db: f32| -> Vec<f32> { let a = crate::dsp::db_to_gain(db); (0..n).map(|i| a * (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin()).collect() };
        let level = |x: &[f32]| -> f32 { let s = &x[n / 4..]; 10.0 * (s.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / s.len() as f64).log10() as f32 };
        let run = |x: Vec<f32>| -> f32 {
            let (mut l, mut r) = (x.clone(), x.clone());
            let trig = std::collections::HashMap::new();
            let ctx = crate::fx::FxContext { step_secs: 0.125, triggers: &trig, offset: 0, levels: None };
            band.process(&mut l, &mut r, &ctx);
            level(&l) - level(&x)
        };
        let loud7 = run(tone(7000.0, -6.0));
        assert!(loud7 < -3.0 && loud7 >= -4.2, "loud 7 kHz cut {loud7:.2} dB");
        assert!(run(tone(7000.0, -45.0)).abs() < 0.1, "quiet 7 kHz untouched");
        assert!(run(tone(1000.0, -6.0)).abs() < 0.3, "1 kHz untouched");
        let air = run(tone(12000.0, -6.0));
        assert!(air > -1.5, "12 kHz air: {air:.2} dB (7 kHz: {loud7:.2})");
    }

    #[test]
    fn air_fix_without_parts_does_nothing() {
        let d = std::env::temp_dir().join("beatbox_air_fix_none");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("generate_beat", &json!({"style": "lofi", "seed": 7})).unwrap();
        for (i, t) in e.project.tracks.iter_mut().enumerate() {
            t.name = format!("t{i}");
        }
        let p0 = serde_json::to_string(&e.project).unwrap();
        assert_eq!(air_fix(&mut e).unwrap()["applied"], json!(false));
        assert_eq!(serde_json::to_string(&e.project).unwrap(), p0);
    }

    /// The bus chain as effects, the way the renderer builds them.
    fn chain_fx(oversample: bool) -> Vec<crate::fx::Effect> {
        air_bus_chain()
            .into_iter()
            .map(|(kind, params)| {
                let mut v = params;
                v["type"] = json!(kind);
                if kind == "saturator" {
                    v["oversample"] = json!(oversample);
                }
                serde_json::from_value(v).unwrap()
            })
            .collect()
    }

    fn run_chain(fx: &[crate::fx::Effect], x: &[f32]) -> Vec<f32> {
        let tr = std::collections::HashMap::new();
        let ctx = crate::fx::FxContext { step_secs: 0.125, triggers: &tr, offset: 0, levels: None };
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        for f in fx {
            f.process(&mut l, &mut r, &ctx);
        }
        l
    }

    /// Power spectrum of the last half second (Hann), bin width SR/n.
    fn spectrum(y: &[f32]) -> (Vec<f64>, f32) {
        let n = 16384usize;
        let s = &y[y.len() - n..];
        let w: Vec<f32> = s.iter().enumerate().map(|(i, v)| v * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())).collect();
        let fft = Fft::new(n);
        let (mut re, mut im) = (w, vec![0.0f32; n]);
        fft.forward(&mut re, &mut im);
        ((0..n / 2).map(|k| (re[k] as f64).powi(2) + (im[k] as f64).powi(2)).collect(), SR / n as f32)
    }

    /// Exciter aliasing on the air bus: a 7 kHz tone's harmonics sit at 14
    /// and 21 kHz; what folds back from above Nyquist (16.1, 9.1, 2.1 kHz)
    /// and every other inharmonic product is measured against them. And a
    /// 1 kHz tone (the vocal's band) barely comes back at all.
    #[test]
    fn air_bus_chain_aliasing_and_vocal_band() {
        let n = SR as usize;
        let tone = |f: f32, a: f32| -> Vec<f32> { (0..n).map(|i| a * (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin()).collect() };
        let mut ratios = Vec::new();
        for os in [true, false] {
            let y = run_chain(&chain_fx(os), &tone(7000.0, 0.3));
            let (p, bw) = spectrum(&y);
            let (mut harm, mut other) = (0.0f64, 0.0f64);
            for (k, e) in p.iter().enumerate() {
                let f = k as f32 * bw;
                if f < 1000.0 {
                    continue;
                }
                if [7000.0f32, 14000.0, 21000.0].iter().any(|h| (f - h).abs() <= 60.0) {
                    harm += e;
                } else {
                    other += e;
                }
            }
            let ratio = 10.0 * (other / harm).log10();
            println!("air bus 7 kHz tone, oversample {os}: inharmonic/aliased products {ratio:.1} dB under the harmonics");
            ratios.push(ratio);
        }
        assert!(ratios[0] < -30.0, "{ratios:?}");
        let x = tone(1000.0, 0.5);
        let y = run_chain(&chain_fx(true), &x);
        let rms = |v: &[f32]| (v[n / 2..].iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / (n / 2) as f64).sqrt();
        let back = 20.0 * (rms(&y) / rms(&x)).log10();
        println!("air bus 1 kHz tone at -6 dBFS: comes back {back:.1} dB under the input");
        assert!(back < -60.0, "{back}");
    }
}
