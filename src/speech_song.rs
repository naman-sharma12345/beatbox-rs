//! Words to a performance: spoken or synthesised words become a rap flow on
//! the grid or a sung melody (each syllable tuned onto a written note with
//! PSOLA). Shared by produce_song (a recording) and make_beat/sing_lyrics
//! (lyrics spoken by a local TTS guide voice).

use crate::analysis::fft;
use crate::dsp::SR;
use crate::theory::Chord;
use crate::vocal::{self, HOP_S};
use serde::Serialize;

/// One spoken word, cut from a recording or synthesised.
#[derive(Clone, Debug)]
pub struct Clip {
    pub text: String,
    pub audio: Vec<f32>,
    pub syl: usize,
}

/// A line of words (a lyric line or a spoken phrase) inside a section.
#[derive(Clone, Debug)]
pub struct Line {
    pub clips: Vec<Clip>,
    pub section: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlacedWord {
    pub text: String,
    pub beat: f32,
    pub beats: f32,
    /// (beat, beats, midi) per syllable in sing mode
    pub notes: Vec<(f32, f32, f32)>,
    pub stretch: f32,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Performance {
    pub mode: String,
    pub words: Vec<PlacedWord>,
    pub lines: usize,
    pub center_midi: f32,
    pub stretch_range: (f32, f32),
}

pub fn syllables(word: &str) -> usize {
    crate::prompt_beat::syllables(word).max(1)
}

/// Spectral-gate denoise: the noise floor per bin is read from the quietest
/// frames; bins near it are pulled down (smoothly, never below `floor_db`).
pub fn denoise(x: &[f32], floor_db: f32, strength: f32) -> (Vec<f32>, f32) {
    const N: usize = 1024;
    const H: usize = 256;
    if x.len() < N * 4 {
        return (x.to_vec(), 0.0);
    }
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos()).collect();
    let frames = (x.len() - N) / H + 1;
    let mut spec: Vec<(Vec<f32>, Vec<f32>)> = Vec::with_capacity(frames);
    let mut energy = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut re: Vec<f32> = (0..N).map(|i| x[f * H + i] * win[i]).collect();
        let mut im = vec![0.0f32; N];
        fft(&mut re, &mut im);
        energy.push(re.iter().zip(im.iter()).take(N / 2).map(|(a, b)| a * a + b * b).sum::<f32>());
        spec.push((re, im));
    }
    // noise profile: mean magnitude of the quietest 12% of frames
    let mut order: Vec<usize> = (0..frames).collect();
    order.sort_by(|a, b| energy[*a].partial_cmp(&energy[*b]).unwrap_or(std::cmp::Ordering::Equal));
    let q = (frames / 8).max(1);
    let mut noise = vec![0.0f32; N / 2 + 1];
    for &f in order.iter().take(q) {
        let (re, im) = &spec[f];
        for k in 0..=N / 2 {
            noise[k] += (re[k] * re[k] + im[k] * im[k]).sqrt() / q as f32;
        }
    }
    let floor = 10f32.powf(floor_db / 20.0);
    let mut prev = vec![1.0f32; N / 2 + 1];
    let mut out = vec![0.0f32; x.len()];
    let mut wsum = vec![0.0f32; x.len()];
    let mut removed = 0.0f64;
    let mut total = 0.0f64;
    for f in 0..frames {
        let (re, im) = &mut spec[f];
        let mut g = vec![1.0f32; N / 2 + 1];
        for k in 0..=N / 2 {
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt().max(1e-9);
            g[k] = (1.0 - strength * noise[k] / m).clamp(floor, 1.0);
        }
        // smooth across frequency and time (fast attack, slower release)
        let gs: Vec<f32> = (0..=N / 2).map(|k| {
            let a = g[k.saturating_sub(1)];
            let c = g[(k + 1).min(N / 2)];
            0.25 * a + 0.5 * g[k] + 0.25 * c
        }).collect();
        for k in 0..=N / 2 {
            let v = if gs[k] > prev[k] { gs[k] } else { 0.6 * prev[k] + 0.4 * gs[k] };
            prev[k] = v;
            let m2 = (re[k] * re[k] + im[k] * im[k]) as f64;
            total += m2;
            removed += m2 * (1.0 - (v * v) as f64);
            re[k] *= v;
            im[k] *= v;
            if k > 0 && k < N / 2 {
                re[N - k] *= v;
                im[N - k] *= v;
            }
        }
        // inverse via conjugate FFT
        let mut r2 = re.clone();
        let mut i2: Vec<f32> = im.iter().map(|v| -v).collect();
        fft(&mut r2, &mut i2);
        for i in 0..N {
            let y = r2[i] / N as f32;
            out[f * H + i] += y * win[i];
            wsum[f * H + i] += win[i] * win[i];
        }
    }
    for (o, w) in out.iter_mut().zip(wsum.iter()) {
        if *w > 1e-3 {
            *o /= *w;
        }
    }
    let red_db = if total > 0.0 { (10.0 * (1.0 - removed / total).max(1e-9).log10()) as f32 } else { 0.0 };
    (out, red_db)
}

/// Trim leading/trailing quiet (below `db` of the clip's peak).
pub fn tighten(x: &[f32], db: f32) -> Vec<f32> {
    let pk = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if pk <= 1e-6 {
        return Vec::new();
    }
    let th = pk * 10f32.powf(db / 20.0);
    let w = (SR * 0.005) as usize;
    let loud = |i: usize| x[i..(i + w).min(x.len())].iter().fold(0.0f32, |m, v| m.max(v.abs())) > th;
    let mut a = 0;
    while a + w < x.len() && !loud(a) {
        a += w;
    }
    let mut b = x.len();
    while b > a + w && !loud(b - w) {
        b -= w;
    }
    x[a.saturating_sub(w)..(b + w).min(x.len())].to_vec()
}

/// Fillers and false starts a producer would cut.
pub const FILLERS: &[&str] = &["um", "uh", "uhm", "umm", "uhh", "er", "erm", "ah", "hmm", "mm", "mhm"];

/// Plan where every word sits: lines start on bar lines inside their section,
/// words take whole grid units (16ths for rap, 8ths per syllable for singing),
/// and a line slows down to fill its share of the section when there is room.
/// Short words a singer passes through rather than holds.
pub fn is_function_word(w: &str) -> bool {
    let w: String = w.chars().filter(|c| c.is_alphabetic() || *c == '\'').collect::<String>().to_lowercase();
    matches!(w.as_str(), "a" | "an" | "the" | "to" | "of" | "and" | "or" | "but" | "in" | "on" | "at" | "for" | "is" | "it" | "i" | "my" | "me" | "we" | "you" | "your" | "be" | "as" | "so" | "that" | "this" | "with" | "from" | "by" | "do" | "if" | "are" | "was" | "not" | "i'm" | "don't" | "da" | "di" | "de" | "nu" | "ke" | "ki" | "ka" | "te" | "ve")
}

pub fn plan(lines: &[Line], sec_start_bar: &[u32], sec_bars: &[u32], bpm: f32, mode: &str) -> Performance {
    let beat_s = 60.0 / bpm;
    let sing = mode == "sing";
    let mut perf = Performance { mode: mode.into(), lines: lines.len(), stretch_range: (9.0, 0.0), ..Default::default() };
    let nsec = sec_start_bar.len();
    for s in 0..nsec {
        let ls: Vec<&Line> = lines.iter().filter(|l| l.section == s).collect();
        if ls.is_empty() {
            continue;
        }
        let share = (sec_bars[s] as f32 / ls.len() as f32).max(1.0);
        let mut bar = sec_start_bar[s] as f32;
        for l in ls {
            // natural length of each word in 16ths
            // sung function words ("to", "the", "and") keep their spoken length;
            // the melody's long notes go on the content words (critic: "to go to"
            // blurred when every word was stretched)
            let func: Vec<bool> = l.clips.iter().map(|c| sing && is_function_word(&c.text)).collect();
            let nat: Vec<f32> = l.clips.iter().zip(&func).map(|(c, f)| {
                let d = c.audio.len() as f32 / SR / (beat_s / 4.0);
                if *f { ((d / 2.0).ceil() * 2.0).max(2.0) } else if sing { (d.round()).max(2.0 * c.syl as f32) } else { (d.round()).max(c.syl as f32) }
            }).collect();
            let need: f32 = nat.iter().sum::<f32>() + if sing { 4.0 } else { 0.0 };
            let line_bars = (need / 16.0).ceil().max(share.floor()).max(1.0);
            // unit scale: slow the flow into the room (rap: 16ths -> 8ths at most 2x)
            let k = ((line_bars * 16.0) / need).floor().clamp(1.0, if sing { 2.0 } else { 2.0 });
            let mut pos = bar * 4.0;
            let nwords = l.clips.len();
            for (wi, c) in l.clips.iter().enumerate() {
                let last = wi + 1 == nwords;
                let mut units = if func[wi] && !last { nat[wi] } else { nat[wi] * k };
                if sing && last {
                    // the line's last syllable is held to the end of its room
                    let room = (bar + line_bars) * 16.0 - pos * 4.0;
                    units = units.max((room - 2.0).min(units + 6.0)).max(units);
                }
                let beats = units / 4.0;
                let src = c.audio.len() as f32 / SR;
                // a held vowel stops growing at about 0.6 s over the spoken word
                // (critic: long stretched vowels blurred the words); the rest of
                // the slot is a breath
                let max_sing = ((src + 0.6) / src.max(0.05)).min(2.6);
                let stretch = (beats * beat_s / src.max(0.05)).clamp(0.6, if sing { max_sing.max(1.0) } else { 1.35 });
                perf.stretch_range.0 = perf.stretch_range.0.min(stretch);
                perf.stretch_range.1 = perf.stretch_range.1.max(stretch);
                let mut notes = Vec::new();
                if sing {
                    let n = c.syl.max(1);
                    // the last syllable gets the long note
                    let first = if n > 1 { (beats * 0.5 / (n - 1) as f32).min(0.5) } else { beats };
                    let mut b0 = pos;
                    for si in 0..n {
                        let len = if si + 1 == n { pos + beats - b0 } else { first };
                        notes.push((b0, len, 0.0));
                        b0 += len;
                    }
                }
                perf.words.push(PlacedWord { text: c.text.clone(), beat: pos, beats, notes, stretch });
                pos += beats;
            }
            bar += line_bars;
        }
    }
    perf
}

/// Write the melody: chord tones on strong beats, scale steps between, an arch
/// over each line, hooks sung higher, line ends resolving to a chord tone.
pub fn write_melody(perf: &mut Performance, chord_at: &dyn Fn(f32) -> Option<Chord>, key_pc: u8, scale: &[u8], center: f32, hook_beats: &[(f32, f32)]) {
    perf.center_midi = center;
    let in_scale = |m: i32| scale.contains(&((((m - key_pc as i32) % 12 + 12) % 12) as u8));
    let mut prev: Option<i32> = None;
    // group syllables into lines by gaps
    let mut sylls: Vec<(usize, usize)> = Vec::new();
    for (wi, w) in perf.words.iter().enumerate() {
        for si in 0..w.notes.len() {
            sylls.push((wi, si));
        }
    }
    let n = sylls.len();
    let mut line_start = 0;
    for i in 0..n {
        let (wi, si) = sylls[i];
        let b = perf.words[wi].notes[si].0;
        let next_gap = if i + 1 < n {
            let (w2, s2) = sylls[i + 1];
            let (b1, l1, _) = perf.words[wi].notes[si];
            perf.words[w2].notes[s2].0 - (b1 + l1)
        } else {
            9.0
        };
        let line_end = next_gap > 0.6 || i + 1 == n || perf.words[wi].notes[si].1 >= 1.5;
        if i == line_start {
            prev = None;
        }
        // position in the line (estimate the line length by scanning ahead)
        let mut j = i;
        while j + 1 < n {
            let (wa, sa) = sylls[j];
            let (wb, sb) = sylls[j + 1];
            let (ba, la, _) = perf.words[wa].notes[sa];
            if perf.words[wb].notes[sb].0 - (ba + la) > 0.6 || la >= 1.5 {
                break;
            }
            j += 1;
        }
        let len = (j + 1 - line_start).max(1) as f32;
        let p = (i - line_start) as f32 / (len - 1.0).max(1.0);
        let hook = hook_beats.iter().any(|(a, l)| b >= *a && b < a + l);
        let target = center + if hook { 3.0 } else { 0.0 } + 3.5 * (std::f32::consts::PI * p).sin() - if line_end { 1.0 } else { 0.0 };
        let chord = chord_at(b);
        let strong = (b % 1.0).abs() < 0.01 || line_end;
        let tones: Vec<i32> = chord.as_ref().map(|c| c.intervals.iter().map(|iv| ((c.root_pc + iv) % 12) as i32).collect()).unwrap_or_default();
        let mut best = target.round() as i32;
        let mut best_cost = f32::MAX;
        for m in (center as i32 - 9)..=(center as i32 + 12) {
            if !in_scale(m) {
                continue;
            }
            let is_tone = tones.contains(&(m.rem_euclid(12)));
            if strong && !tones.is_empty() && !is_tone {
                continue;
            }
            let mut cost = (m as f32 - target).abs();
            if let Some(pv) = prev {
                let leap = (m - pv).abs() as f32;
                cost += if leap > 4.0 { 0.6 * (leap - 4.0) } else { 0.0 } + if m == pv { 0.4 } else { 0.0 };
            }
            if line_end && !tones.is_empty() && chord.as_ref().map(|c| (c.root_pc % 12) as i32 == m.rem_euclid(12)).unwrap_or(false) {
                cost -= 0.8;
            }
            if cost < best_cost {
                best_cost = cost;
                best = m;
            }
        }
        perf.words[wi].notes[si].2 = best as f32;
        prev = Some(best);
        if line_end {
            line_start = i + 1;
        }
    }
}

/// Render the performance: every word stretched to its slot and laid at its
/// grid position (5 ms fade in, 20 ms out); in sing mode every syllable is
/// then tuned onto its note. Returns mono audio starting at beat 0.
pub fn render(perf: &Performance, clips: &[&Clip], bpm: f32, tune_hard: f32) -> Vec<f32> {
    let beat_s = 60.0 / bpm;
    let end_beat = perf.words.iter().map(|w| w.beat + w.beats).fold(0.0f32, f32::max);
    let mut out = vec![0.0f32; ((end_beat * beat_s + 1.0) * SR) as usize];
    for (w, c) in perf.words.iter().zip(clips.iter()) {
        let mut y = stretch_nucleus(&c.audio, w.stretch);
        // start on a zero crossing (within 3 ms), then equal-power fades:
        // 8 ms in, 25 ms out (critic: splice clicks at every word edge)
        let zc = (1..((0.003 * SR) as usize).min(y.len())).find(|&i| (y[i - 1] <= 0.0) != (y[i] <= 0.0)).unwrap_or(0);
        if zc > 0 {
            y.drain(..zc);
        }
        let fi = ((0.008 * SR) as usize).min(y.len() / 2);
        let fo = ((0.025 * SR) as usize).min(y.len() / 2);
        let n = y.len();
        for i in 0..fi {
            y[i] *= (std::f32::consts::FRAC_PI_2 * i as f32 / fi as f32).sin();
        }
        for i in 0..fo {
            y[n - 1 - i] *= (std::f32::consts::FRAC_PI_2 * i as f32 / fo as f32).sin();
        }
        let at = (w.beat * beat_s * SR) as usize;
        for (i, v) in y.iter().enumerate() {
            if at + i < out.len() {
                out[at + i] += v;
            }
        }
    }
    if perf.mode != "sing" {
        declick(&mut out);
        level(&mut out);
        return out;
    }
    // tune: the frame's pitch onto the written note (speech intonation flattened)
    let frames = vocal::pitch_track(&out, SR);
    let mut corr = vec![0.0f32; frames.len()];
    let mut targets: Vec<(f32, f32, f32)> = Vec::new();
    for w in &perf.words {
        for &(b, l, m) in &w.notes {
            targets.push((b * beat_s, (b + l) * beat_s, m));
        }
    }
    let mut ti = 0;
    for (i, fr) in frames.iter().enumerate() {
        if fr.midi <= 0.0 {
            continue;
        }
        let t = fr.t;
        while ti + 1 < targets.len() && targets[ti].1 <= t {
            ti += 1;
        }
        let Some(&(a, b, m)) = targets.get(ti) else { continue };
        if t < a - 0.05 || t > b + 0.05 {
            continue;
        }
        let m = m + expression(t - a, b - a, ti == 0 || targets[ti - 1].1 < a - 0.02);
        corr[i] = (tune_hard * (m - fr.midi)).clamp(-9.0, 9.0);
    }
    // smooth (30 ms) so note changes glide a little, like a singer
    let a = (-HOP_S / 0.03f32).exp();
    for i in 1..corr.len() {
        corr[i] = a * corr[i - 1] + (1.0 - a) * corr[i];
    }
    let mut y = vocal::psola(&out, SR, &frames, &corr, 0.55, 1.75);
    declick(&mut y);
    level(&mut y);
    y
}

/// Repair clicks: a 1 ms window whose energy above 6 kHz jumps 18 dB over
/// its neighbourhood and dies within a few ms is a splice or grain edge, not
/// a consonant; its high band is faded out over ~3 ms (the same measure the
/// critic counts clicks with). Returns how many were repaired.
pub fn declick(y: &mut [f32]) -> usize {
    let w = (0.001 * SR) as usize;
    let n = y.len() / w.max(1);
    if n < 100 {
        return 0;
    }
    let mut hp = crate::dsp::Biquad::new(crate::dsp::BiquadKind::LowCut, 6000.0, 0.7, 0.0);
    let mut hp2 = crate::dsp::Biquad::new(crate::dsp::BiquadKind::LowCut, 6000.0, 0.7, 0.0);
    let h: Vec<f32> = y.iter().map(|&v| hp2.process(hp.process(v))).collect();
    let e: Vec<f32> = (0..n).map(|i| h[i * w..(i + 1) * w].iter().map(|v| v * v).sum::<f32>() / w as f32 + 1e-12).collect();
    let mut sorted = e.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[n / 2];
    let mut fixed = 0;
    let mut i = 0;
    while i < n {
        let lo = i.saturating_sub(25);
        let hi = (i + 26).min(n);
        let mut nb: Vec<f32> = e[lo..hi].to_vec();
        nb.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = nb[nb.len() / 2];
        let after = if i + 8 < n { e[i + 3..i + 8].iter().copied().fold(0.0, f32::max) } else { e[i] };
        if e[i] > 30.0 * med && e[i] > 4.0 * floor && after < 0.3 * e[i] {
            // fade the high band out across the click (raised-cosine, 4 ms)
            let s0 = (i * w).saturating_sub(w);
            let s1 = ((i + 3) * w).min(y.len());
            let len = (s1 - s0).max(1) as f32;
            for k in s0..s1 {
                let g = 0.5 - 0.5 * (std::f32::consts::TAU * (k - s0) as f32 / len).cos();
                y[k] -= h[k] * g;
            }
            fixed += 1;
            i += 4;
            continue;
        }
        i += 1;
    }
    fixed
}

/// A singer's pitch life on top of a written note (semitones), `t` seconds
/// into a note `len` long: a scoop up into a note that starts a phrase, and
/// on notes over 0.4 s a 5.2 Hz vibrato of about +-25 cents that fades in
/// after 150 ms. (Critic: pitch moved 0.09 st frame to frame; a voice moves
/// 0.2-0.3 plus vibrato.)
pub fn expression(t: f32, len: f32, phrase_start: bool) -> f32 {
    let mut d = 0.0;
    if phrase_start && t < 0.09 {
        d -= 0.9 * (1.0 - (t / 0.09).max(0.0));
    }
    if len > 0.4 && t > 0.15 {
        let fade = ((t - 0.15) / 0.2).min(1.0);
        d += 0.25 * fade * (std::f32::consts::TAU * 5.2 * (t - 0.15)).sin();
    }
    d
}

/// Stretch a sung word without smearing its consonants: the first 60 ms
/// (the onset: "st" in "stay") and the last 40 ms stay at 1x and only the
/// vowel nucleus between them takes the stretch. Short words stretch whole.
pub fn stretch_nucleus(x: &[f32], factor: f32) -> Vec<f32> {
    let (on, off) = ((0.06 * SR) as usize, (0.04 * SR) as usize);
    if factor <= 1.05 || x.len() < on + off + (0.05 * SR) as usize {
        return crate::audio_edit::time_stretch(x, factor);
    }
    let target = (x.len() as f32 * factor) as usize;
    let mid = &x[on..x.len() - off];
    let mid_factor = (target - on - off) as f32 / mid.len() as f32;
    let m = crate::audio_edit::time_stretch(mid, mid_factor);
    // short crossfades at the joins
    let xf = (0.004 * SR) as usize;
    let mut y: Vec<f32> = x[..on].to_vec();
    for (i, v) in m.iter().enumerate() {
        if i < xf && !y.is_empty() {
            let k = y.len() - xf + i;
            if k < y.len() {
                let g = i as f32 / xf as f32;
                y[k] = y[k] * (1.0 - g) + v * g;
                continue;
            }
        }
        y.push(*v);
    }
    y.extend_from_slice(&x[x.len() - off..]);
    y
}

/// Bring a performance to a consistent level: the voiced parts at -12 dBFS
/// RMS (a quiet phone take and a loud TTS voice land the same), peaks
/// rounded off softly above -3 dBFS.
pub fn level(y: &mut [f32]) {
    let act: Vec<f32> = y.iter().copied().filter(|v| v.abs() > 1e-3).collect();
    if act.is_empty() {
        return;
    }
    let rms = (act.iter().map(|v| v * v).sum::<f32>() / act.len() as f32).sqrt();
    let g = 10f32.powf(-12.0 / 20.0) / rms.max(1e-6);
    for v in y.iter_mut() {
        let a = (*v * g).abs();
        let s = if a > 0.7 { 0.7 + 0.28 * ((a - 0.7) / 0.28).tanh() } else { a };
        *v = s.copysign(*v);
    }
}

/// The speaker's median pitch (MIDI), to set the melody's register.
pub fn median_pitch(x: &[f32]) -> f32 {
    let fr = vocal::pitch_track(x, SR);
    let mut v: Vec<f32> = fr.iter().filter(|f| f.midi > 0.0).map(|f| f.midi).collect();
    if v.is_empty() {
        return 57.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}


/// How a Hindi / Punjabi word should be spelled for an English TTS voice to
/// say it right ("tere" -> "theh-reh", not "tear"; critic heard "Tere bina"
/// as "Dear Venus"). Unknown words pass through unchanged.
pub fn respell(word: &str) -> String {
    let w: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase();
    let r = match w.as_str() {
        "tere" => "theh-reh", "teri" => "theh-ree", "tera" => "theh-raa", "tu" => "thoo", "tainu" => "thai-noo",
        "mere" => "meh-reh", "meri" => "meh-ree", "mera" => "meh-raa", "main" => "mainh", "mainu" => "mai-noo",
        "bina" => "bee-naa", "dil" => "dhill", "nahi" => "nuh-hee", "nahin" => "nuh-heen", "lagda" => "lug-dhaa",
        "lagdi" => "lug-dhee", "kuch" => "kooch", "chalda" => "chull-dhaa", "hai" => "hay", "hain" => "hain",
        "pyaar" => "pyaar", "pyar" => "pyaar", "yaad" => "yaadh", "yaar" => "yaar", "raat" => "raath", "raatan" => "raa-thaan",
        "jaan" => "jaan", "rab" => "rubb", "sajna" => "suj-naa", "sohneya" => "sohh-neh-yaa", "ve" => "vey",
        "kyun" => "kyoon", "kya" => "kyaa", "aaja" => "aa-jaa", "menu" => "mai-noo", "vich" => "vitch",
        "naal" => "naal", "ki" => "kee", "ke" => "keh", "da" => "dhaa", "di" => "dhee", "de" => "dheh", "nu" => "noo",
        "dard" => "dhurd", "ishq" => "ishk", "mohabbat" => "mo-hub-buth", "zindagi" => "zin-dhuh-gee", "dooriyan" => "dhoo-ree-yaan",
        "sapna" => "sup-naa", "sapne" => "sup-neh", "ankhiyan" => "ankh-ee-yaan", "akhiyan" => "akh-ee-yaan", "oh" => "oh",
        _ => return word.to_string(),
    };
    r.to_string()
}

/// The guide-voice helper, shipped inside the binary.
pub const TTS_PY: &str = include_str!("../scripts/tts_words.py");

/// Speak every word of `lines` with the local TTS voice; one clip per word,
/// trimmed tight. The voice lives in `<workdir>/tts` (or BEATBOX_TTS_DIR).
pub fn tts_words(workdir: &std::path::Path, lines: &[Vec<String>], tag: &str) -> anyhow::Result<Vec<Vec<Clip>>> {
    use anyhow::{anyhow, bail, Context};
    use std::io::Write;
    let script = workdir.join(".beatbox_tts_words.py");
    std::fs::write(&script, TTS_PY).context("writing the TTS helper")?;
    let vdir = std::env::var("BEATBOX_TTS_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| workdir.join("tts"));
    let out_dir = vdir.join(format!("words_{tag}"));
    let py = std::env::var("BEATBOX_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut child = std::process::Command::new(&py)
        .arg(&script)
        .arg(&out_dir)
        .arg("--voice-dir")
        .arg(&vdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("running {py} (set BEATBOX_PYTHON to a Python with piper-tts)"))?;
    // Hindi / Punjabi words go to the English voice respelled the way they sound
    let spoken: Vec<Vec<String>> = lines.iter().map(|l| l.iter().map(|w| respell(w)).collect()).collect();
    child.stdin.take().unwrap().write_all(serde_json::json!({"lines": spoken}).to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        anyhow!("TTS helper failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("no output"))
    })?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("{e}");
    }
    let mut res: Vec<Vec<Clip>> = vec![Vec::new(); lines.len()];
    for w in v["words"].as_array().cloned().unwrap_or_default() {
        let li = w["line"].as_u64().unwrap_or(0) as usize;
        let wi = w["index"].as_u64().map(|x| x as usize);
        // the written word, not its respelling, names the clip
        let text = wi.and_then(|i| lines.get(li).and_then(|l| l.get(i))).cloned().unwrap_or_else(|| w["word"].as_str().unwrap_or("").to_string());
        let path = std::path::PathBuf::from(w["path"].as_str().unwrap_or(""));
        let audio = tighten(&crate::samples::decode_file(&path)?, -38.0);
        if audio.len() < (SR * 0.04) as usize || li >= res.len() {
            continue;
        }
        let syl = syllables(&text);
        res[li].push(Clip { text, audio, syl });
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, secs: f32) -> Vec<f32> {
        (0..(secs * SR) as usize).map(|i| {
            let t = i as f32 / SR;
            0.4 * (std::f32::consts::TAU * hz * t).sin() + 0.2 * (std::f32::consts::TAU * 2.0 * hz * t).sin()
        }).collect()
    }

    #[test]
    fn denoise_pulls_down_steady_hiss() {
        let mut rng = crate::dsp::Rng::new(3);
        let mut x = vec![0.0f32; (SR * 3.0) as usize];
        let t = tone(220.0, 1.0);
        for (i, v) in x.iter_mut().enumerate() {
            *v = 0.03 * rng.bipolar();
            if i >= (SR as usize) && i < 2 * SR as usize {
                *v += t[i - SR as usize];
            }
        }
        let (y, red) = denoise(&x, -20.0, 1.5);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let q0 = rms(&x[..SR as usize / 2]);
        let q1 = rms(&y[..SR as usize / 2]);
        assert!(q1 < q0 * 0.3, "hiss {q0} -> {q1}");
        let s0 = rms(&x[SR as usize + 1000..2 * SR as usize - 1000]);
        let s1 = rms(&y[SR as usize + 1000..2 * SR as usize - 1000]);
        assert!(s1 > s0 * 0.8, "tone kept {s0} -> {s1}");
        assert!(red < 0.0);
    }

    #[test]
    fn nucleus_stretch_keeps_the_onset_and_expression_moves() {
        let x = tone(220.0, 0.4);
        let y = stretch_nucleus(&x, 2.0);
        assert!((y.len() as f32 / x.len() as f32 - 2.0).abs() < 0.1, "{} vs {}", y.len(), x.len());
        // the first 50 ms are untouched
        let k = (0.05 * SR) as usize;
        assert!(x[..k].iter().zip(&y[..k]).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(expression(0.0, 1.0, true) < -0.5);
        let vib: Vec<f32> = (0..100).map(|i| expression(0.4 + i as f32 * 0.01, 1.5, false)).collect();
        let span = vib.iter().cloned().fold(f32::MIN, f32::max) - vib.iter().cloned().fold(f32::MAX, f32::min);
        assert!(span > 0.4 && span < 0.6, "vibrato span {span}");
        assert_eq!(expression(0.2, 0.3, false), 0.0);
        assert_eq!(respell("Tere"), "theh-reh");
        assert_eq!(respell("tonight"), "tonight");
    }

    #[test]
    fn words_land_on_the_grid_and_get_sung_in_key() {
        let clip = |t: &str, hz: f32, d: f32| Clip { text: t.into(), audio: tone(hz, d), syl: syllables(t) };
        let lines = vec![
            Line { clips: vec![clip("stay", 200.0, 0.3), clip("with", 210.0, 0.2), clip("me", 190.0, 0.35)], section: 0 },
            Line { clips: vec![clip("tonight", 205.0, 0.5)], section: 0 },
        ];
        let mut perf = plan(&lines, &[4], &[4], 90.0, "sing");
        assert_eq!(perf.words.len(), 4);
        assert!((perf.words[0].beat - 16.0).abs() < 1e-3, "line 1 starts on bar 4");
        for w in &perf.words {
            assert!((w.beat * 2.0).fract().abs() < 1e-3, "8th grid: {}", w.beat);
        }
        let key_pc = 1; // C#
        let minor = [0u8, 2, 3, 5, 7, 8, 10];
        let ch = crate::theory::parse_progression("i", key_pc, "minor").unwrap();
        let c0 = ch[0].clone();
        write_melody(&mut perf, &|_b| Some(c0.clone()), key_pc, &minor, 55.0, &[]);
        for w in &perf.words {
            for n in &w.notes {
                assert!(minor.contains(&((((n.2 as i32 - 1) % 12 + 12) % 12) as u8)), "{}", n.2);
            }
        }
        let refs: Vec<&Clip> = lines.iter().flat_map(|l| l.clips.iter()).collect();
        let y = render(&perf, &refs, 90.0, 1.0);
        assert!(y.len() as f32 / SR > 16.0 * 60.0 / 90.0);
        // the first sung note is at its written pitch
        let w0 = &perf.words[0];
        let a = (w0.beat * 60.0 / 90.0 * SR) as usize;
        let seg = &y[a + 2000..a + 9000];
        let m = median_pitch(seg);
        assert!((m - w0.notes[0].2).abs() < 0.7, "sung {m} vs note {}", w0.notes[0].2);
    }
}
