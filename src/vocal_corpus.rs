//! Vocal regression corpus: CC0 recordings of real voices (different
//! singers, mics, rooms and noise) that every vocal-processing change is
//! checked against with DNSMOS (SIG = the voice itself, OVRL = overall).
//!
//! The clips are fetched by `scripts/fetch_vocal_corpus.py` (pinned by
//! SHA-1, cut to 12 s, never committed) into `$BEATBOX_VOCAL_CORPUS` or
//! `~/.beatbox/vocal_corpus`. Private takes never go in the corpus or git.
//!
//! The rule the tests hold the processors to: the delivered result never
//! scores below the raw take on SIG or OVRL. `guard_quality` is that rule as
//! code: it blends the processed take back toward the raw one (full, 60 %,
//! 35 %) and keeps the strongest blend that is not worse than raw on both,
//! or the raw take itself. DNSMOS is a speech-quality model, not a judge of
//! singing or of taste; it only catches damage (smearing, warbling, eaten
//! consonants), which is what a regression test is for.

use crate::voice_ai::{dnsmos, Mos};
use anyhow::Result;
use std::path::PathBuf;

/// Where the corpus lives.
pub fn corpus_dir() -> PathBuf {
    std::env::var("BEATBOX_VOCAL_CORPUS").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".beatbox").join("vocal_corpus")
    })
}

/// (name, file, what) for each clip the fetch script wrote.
pub fn corpus() -> Vec<(String, PathBuf, String)> {
    let dir = corpus_dir();
    let Ok(b) = std::fs::read(dir.join("corpus.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) else {
        return Vec::new();
    };
    v["clips"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let f = PathBuf::from(c["file"].as_str()?);
                    f.is_file().then(|| (c["name"].as_str().unwrap_or("").to_string(), f, c["what"].as_str().unwrap_or("").to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Blend levels tried, strongest first.
pub const GUARD_MIXES: [f32; 3] = [1.0, 0.6, 0.35];

/// The strongest blend of `processed` into `raw` that DNSMOS rates no worse
/// than the raw take on SIG and OVRL; the raw take when none is. Returns
/// (audio, mix used, raw score, chosen score).
pub fn guard_quality(raw: &[f32], processed: &[f32]) -> Result<(Vec<f32>, f32, Mos, Mos)> {
    let n = raw.len().min(processed.len());
    let blends: Vec<Vec<f32>> = GUARD_MIXES
        .iter()
        .map(|m| (0..n).map(|i| raw[i] * (1.0 - m) + processed[i] * m).collect())
        .collect();
    let mut bufs: Vec<&[f32]> = vec![raw];
    bufs.extend(blends.iter().map(|b| b.as_slice()));
    let s = dnsmos(&bufs)?;
    for (k, m) in GUARD_MIXES.iter().enumerate() {
        let c = s[k + 1];
        if c.sig >= s[0].sig && c.ovrl >= s[0].ovrl {
            return Ok((blends[k].clone(), *m, s[0], c));
        }
    }
    Ok((raw.to_vec(), 0.0, s[0], s[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_ai::{bands, clean_vocal_target, denoise, match_tone, DenoiseOpts, ToneOpts};

    /// Every processor, guarded, never scores below the raw take; the
    /// unguarded deltas are printed so a regression in the DSP itself shows.
    #[test]
    fn vocal_processing_never_lowers_dnsmos_on_the_corpus() {
        let clips = corpus();
        if clips.is_empty() {
            eprintln!("vocal corpus: skipped (run python3 scripts/fetch_vocal_corpus.py)");
            return;
        }
        if let Err(e) = dnsmos(&[&vec![0.0f32; 16000]]) {
            eprintln!("vocal corpus: skipped, DNSMOS unavailable: {e}");
            return;
        }
        let fc = bands();
        let target: Vec<f32> = fc.iter().map(|f| clean_vocal_target(*f)).collect();
        let mut bad = Vec::new();
        let mut table = Vec::new();
        for (name, file, what) in &clips {
            let x = crate::samples::decode_file(file).unwrap();
            let procs: Vec<(&str, Vec<f32>)> = vec![
                ("denoise_voice", denoise(&x, &DenoiseOpts { atten_lim_db: 15.0, mix: 1.0, backend: "auto".into() }).unwrap().0),
                ("match_vocal_tone", match_tone(&x, &target, &ToneOpts { amount: 0.7, max_db: 6.0, dyn_max_db: 4.0, dyn_threshold_db: 3.0 }).y),
            ];
            for (p, y) in procs {
                let (out, mix, raw, chosen) = guard_quality(&x, &y).unwrap();
                let full = dnsmos(&[&y[..x.len().min(y.len())]]).unwrap()[0];
                // re-score what is delivered, independently of the guard's own numbers
                let again = dnsmos(&[&out]).unwrap()[0];
                table.push(format!(
                    "{name:22} {p:17} raw SIG {:.2} OVRL {:.2} | full SIG {:+.2} OVRL {:+.2} | delivered mix {mix:.2} SIG {:+.2} OVRL {:+.2}  ({what})",
                    raw.sig, raw.ovrl, full.sig - raw.sig, full.ovrl - raw.ovrl, again.sig - raw.sig, again.ovrl - raw.ovrl
                ));
                if again.sig < raw.sig - 0.02 || again.ovrl < raw.ovrl - 0.02 || chosen.sig < raw.sig || chosen.ovrl < raw.ovrl {
                    bad.push(format!("{name}/{p}: SIG {:.2}->{:.2}, OVRL {:.2}->{:.2}", raw.sig, again.sig, raw.ovrl, again.ovrl));
                }
            }
        }
        eprintln!("vocal corpus ({} clips):\n{}", clips.len(), table.join("\n"));
        assert!(bad.is_empty(), "DNSMOS dropped below the raw take:\n{}", bad.join("\n"));
    }

    #[test]
    fn guard_returns_raw_when_processing_only_hurts() {
        // works without the corpus; needs DNSMOS
        let sr = crate::dsp::SR;
        let x: Vec<f32> = (0..(3.0 * sr) as usize)
            .map(|i| {
                let t = i as f32 / sr;
                let f0 = 180.0 + 20.0 * (t * 5.0).sin();
                0.3 * (t * f0 * std::f32::consts::TAU).sin() + 0.1 * (t * f0 * 2.0 * std::f32::consts::TAU).sin()
            })
            .collect();
        // "processing" that is pure damage: hard clipping plus noise
        let mut rng = crate::dsp::Rng::new(7);
        let y: Vec<f32> = x.iter().map(|v| (v * 8.0).clamp(-0.3, 0.3) + 0.1 * rng.bipolar()).collect();
        match guard_quality(&x, &y) {
            Ok((_, mix, raw, chosen)) => {
                assert!(chosen.sig >= raw.sig && chosen.ovrl >= raw.ovrl);
                assert!(mix < 1.0, "full-strength damage is never delivered");
            }
            Err(e) => eprintln!("skipped, DNSMOS unavailable: {e}"),
        }
    }
}
