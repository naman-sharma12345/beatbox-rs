//! A 5:00 song with ~80 vocal phrase clips renders (stems for the mix
//! rules, then the full mix) under 2.5 GB peak. The 2026-10-12 brutal test
//! (a 5:00 song, the vocal cut into its phrases) was killed out of memory
//! at ~3.7 GB on an 8 GB box during the stem render of the vocal balance.
//! Its own test binary: the peak (VmHWM) is this process alone.

use beatbox::engine::Engine;
use serde_json::json;

fn status_mb(key: &str) -> Option<f32> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with(key))?;
    l.split_whitespace().nth(1)?.parse::<f32>().ok().map(|k| k / 1024.0)
}

/// A free-time "take": `n` phrases of buzzy syllables with pauses between.
fn take(n: usize) -> Vec<f32> {
    let sr = 44100.0f32;
    let mut x = Vec::new();
    let mut k = 0u32;
    for p in 0..n {
        let syl = 8 + (p % 5);
        for s in 0..syl {
            let len = (0.12 * sr) as usize;
            for i in 0..len {
                let t = i as f32 / sr;
                let env = (i as f32 / (0.005 * sr)).min(1.0) * (1.0 - i as f32 / len as f32);
                let f = 180.0 + 20.0 * ((s + p) % 7) as f32;
                x.push(0.3 * env * ((t * f * std::f32::consts::TAU).sin() + 0.4 * (t * 3.0 * f * std::f32::consts::TAU).sin()));
            }
            let gap = (0.06 + 0.07 * ((k % 3) as f32)) * sr;
            x.extend(std::iter::repeat(0.0).take(gap as usize));
            k += 1;
        }
        x.extend(std::iter::repeat(0.0).take((0.9 * sr) as usize + (p % 4) * 3000));
    }
    x
}

#[test]
fn a_five_minute_song_with_80_vocal_clips_renders_under_2_5_gb() {
    let d = std::env::temp_dir().join("beatbox_long_song_memory");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut e = Engine::new(d.clone());
    e.call("generate_beat", &json!({"style": "trap", "seed": 9})).unwrap();
    // stretch it to 5:00
    let pat = e.project.patterns[0].name.clone();
    let bars = e.project.patterns[0].bars.max(1) as f32;
    let bar_s = 4.0 * 60.0 / e.project.bpm;
    let repeats = (300.0 / (bars * bar_s)).ceil() as u32;
    e.project.arrangement = vec![beatbox::project::Section { pattern: pat, repeats }];
    let x = take(80);
    let wav = d.join("take.wav");
    beatbox::render::write_wav(&wav, &x, &x).unwrap();
    e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "take"})).unwrap();
    e.call("add_track", &json!({"name": "vocal", "preset": "pad"})).unwrap();
    let r = e.call("lock_vocal_to_beat", &json!({"sample": "take", "track": "vocal", "start_beat": 4, "flex": 0.5})).unwrap();
    let clips = e.project.audio_clips.iter().filter(|c| c.track == "vocal").count();
    assert!(clips >= 70, "phrase clips placed: {clips} ({})", r["lock"]);
    e.project.audio_clips.retain(|c| c.track == "vocal");
    e.track_cache = Some(Default::default());
    let p = e.project.clone();
    let base = status_mb("VmRSS:").unwrap_or(0.0);
    // the peak from here on
    let reset = std::fs::write("/proc/self/clear_refs", "5").is_ok();
    // the vocal balance's stem renders (the second re-sums from the cache),
    // then the full mix the delivery measures
    let m1 = e.render_stems(&p).unwrap();
    assert!(m1.seconds > 295.0, "{}", m1.seconds);
    let v = m1.stems.iter().find(|s| s.name == "vocal").unwrap();
    assert!(!v.left.is_empty(), "the vocal stem keeps its audio");
    assert!(m1.stems.iter().filter(|s| s.name != "vocal").all(|s| s.left.is_empty() && s.bands.is_some()), "beat stems keep band levels only");
    drop(m1);
    let mut q = p.clone();
    for t in q.tracks.iter_mut() {
        t.volume_db -= 1.0;
    }
    let m2 = e.render_stems(&q).unwrap();
    drop(m2);
    let m3 = e.render_version(&q).unwrap();
    assert!(m3.left.iter().any(|v| v.abs() > 1e-3));
    drop(m3);
    let peak = status_mb("VmHWM:").unwrap_or(0.0);
    eprintln!("long song memory: base {base:.0} MB, peak {peak:.0} MB (peak reset: {reset})");
    if reset || base < 400.0 {
        assert!(peak < 2500.0, "peak {peak:.0} MB over 2.5 GB (base {base:.0} MB)");
    }
    let _ = std::fs::remove_dir_all(&d);
}
