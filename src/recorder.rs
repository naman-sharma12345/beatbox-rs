//! Audio recording (FL's record button): capture a take from an audio input
//! into a WAV in the project's `recordings/` folder, register it as a
//! sample and, when asked, drop it on the timeline as an audio clip.
//!
//! `start_recording` / `stop_recording` run the capture on its own thread
//! (the studio's record button calls them); `record_audio` records a fixed
//! number of seconds in one call. The input runs at the device's rate and is
//! resampled to the engine's 44.1 kHz on stop.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{f_opt, obj, s_opt, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A finished take: interleaved samples at the device rate.
pub struct Take {
    pub data: Vec<f32>,
    pub channels: usize,
    pub rate: u32,
    pub device: String,
}

/// Peak (dBFS), RMS (dBFS) and the number of samples at or above full scale.
pub fn take_stats(x: &[f32]) -> (f32, f32, usize) {
    if x.is_empty() {
        return (-120.0, -120.0, 0);
    }
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let rms = (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32;
    let clipped = x.iter().filter(|v| v.abs() >= 0.999).count();
    let db = |g: f32| if g > 1e-6 { 20.0 * g.log10() } else { -120.0 };
    (db(peak), db(rms), clipped)
}

/// De-interleave to stereo (mono is copied to both sides; channels past two
/// are ignored) and resample to the engine rate.
pub fn take_to_stereo(t: &Take) -> (Vec<f32>, Vec<f32>) {
    let ch = t.channels.max(1);
    let frames = t.data.len() / ch;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for f in 0..frames {
        let a = t.data[f * ch];
        let b = if ch > 1 { t.data[f * ch + 1] } else { a };
        l.push(a);
        r.push(b);
    }
    let to = SR as u32;
    if t.rate != to && t.rate > 0 {
        (crate::resample::resample(&l, t.rate, to), crate::resample::resample(&r, t.rate, to))
    } else {
        (l, r)
    }
}

/// Next free `recordings/take_N.wav` (or `<name>.wav`) under the workdir.
fn take_path(workdir: &Path, name: Option<&str>) -> PathBuf {
    let dir = workdir.join("recordings");
    if let Some(n) = name {
        return dir.join(format!("{}.wav", crate::samples::sample_name(n)));
    }
    (1..10_000).map(|i| dir.join(format!("take_{i}.wav"))).find(|p| !p.exists()).unwrap_or_else(|| dir.join("take.wav"))
}

/// Write the take, register it as a sample and optionally place it.
pub fn finish_take(e: &mut Engine, t: Take, a: &Value) -> Result<Value> {
    let (l, r) = take_to_stereo(&t);
    if l.is_empty() {
        bail!("the take is empty: nothing came from '{}' (check the input level and that the right input is chosen; list_audio_inputs)", t.device);
    }
    // trim the input's latency (or a count-in) from the start
    let trim = (f_opt(a, "trim_start_ms").unwrap_or(0.0).max(0.0) / 1000.0 * SR) as usize;
    let (l, r) = if trim > 0 && trim < l.len() { (l[trim..].to_vec(), r[trim..].to_vec()) } else { (l, r) };
    let path = take_path(&e.workdir, s_opt(a, "name").as_deref());
    crate::render::write_wav(&path, &l, &r)?;
    let (peak, rms, clipped) = take_stats(&t.data);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("take").to_string();
    let reg = e.call("import_sample", &json!({"path": path.to_string_lossy(), "name": stem}))?;
    let sample = reg.get("sample").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or(stem);
    let mut out = json!({
        "path": path.to_string_lossy(), "sample": sample, "seconds": (l.len() as f32 / SR * 100.0).round() / 100.0,
        "device": t.device, "device_rate": t.rate, "channels": t.channels,
        "peak_dbfs": (peak * 10.0).round() / 10.0, "rms_dbfs": (rms * 10.0).round() / 10.0, "clipped_samples": clipped,
    });
    if clipped > 0 {
        out["warning"] = json!("the input clipped; lower the input gain and record again");
    } else if peak < -30.0 {
        out["warning"] = json!("very quiet take (peak under -30 dBFS); raise the input gain");
    }
    if let Some(track) = s_opt(a, "track") {
        if e.project.track_index(&track).is_err() {
            e.call("add_sample_track", &json!({"name": track, "sample": sample, "one_shot": true}))?;
        }
        let clip = e.call("add_audio_clip", &json!({"track": track, "sample": sample, "start_beat": f_opt(a, "start_beat").unwrap_or(0.0)}))?;
        out["clip"] = clip;
    }
    Ok(out)
}

#[cfg(feature = "gui")]
mod live {
    use super::Take;
    use anyhow::{anyhow, bail, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub struct Session {
        stop: Arc<AtomicBool>,
        buf: Arc<Mutex<Vec<f32>>>,
        info: Arc<Mutex<Option<(u32, usize, String)>>>,
        thread: Option<std::thread::JoinHandle<Result<()>>>,
        pub started: Instant,
        #[allow(dead_code)]
        pub max_seconds: f32,
    }

    static CURRENT: Mutex<Option<Session>> = Mutex::new(None);

    pub fn inputs() -> Result<Vec<(String, u32, u16, bool)>> {
        let host = cpal::default_host();
        let def = host.default_input_device().and_then(|d| d.name().ok());
        let mut out = Vec::new();
        for d in host.input_devices()? {
            let name = d.name().unwrap_or_else(|_| "input".into());
            if let Ok(c) = d.default_input_config() {
                out.push((name.clone(), c.sample_rate().0, c.channels(), def.as_deref() == Some(name.as_str())));
            }
        }
        Ok(out)
    }

    fn pick(host: &cpal::Host, want: Option<&str>) -> Result<cpal::Device> {
        match want {
            Some(w) => {
                let wl = w.to_lowercase();
                host.input_devices()?
                    .find(|d| d.name().map(|n| n.to_lowercase().contains(&wl)).unwrap_or(false))
                    .ok_or_else(|| anyhow!("no audio input matching '{w}' (list_audio_inputs)"))
            }
            None => host.default_input_device().ok_or_else(|| anyhow!("no audio input device on this machine (plug in a microphone or audio interface; list_audio_inputs)")),
        }
    }

    pub fn is_recording() -> Option<f32> {
        CURRENT.lock().unwrap().as_ref().map(|s| s.started.elapsed().as_secs_f32())
    }

    pub fn start(device: Option<String>, max_seconds: f32) -> Result<(String, u32, u16)> {
        let mut cur = CURRENT.lock().unwrap();
        if cur.is_some() {
            bail!("already recording (stop_recording first)");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
        let info = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(String, u32, u16)>>();
        let (s2, b2, i2) = (stop.clone(), buf.clone(), info.clone());
        // the stream lives on its own thread (cpal streams are not Send everywhere)
        let thread = std::thread::spawn(move || -> Result<()> {
            let run = || -> Result<(cpal::Stream, String, u32, u16)> {
                let host = cpal::default_host();
                let dev = pick(&host, device.as_deref())?;
                let name = dev.name().unwrap_or_else(|_| "input".into());
                let cfg = dev.default_input_config()?;
                let (rate, ch) = (cfg.sample_rate().0, cfg.channels());
                let limit = (max_seconds.max(1.0) * rate as f32) as usize * ch as usize;
                let err = |e: cpal::StreamError| eprintln!("recording stream error: {e}");
                let sc: cpal::StreamConfig = cfg.clone().into();
                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => {
                        let b = b2.clone();
                        dev.build_input_stream(&sc, move |d: &[f32], _: &_| push(&b, d.iter().copied(), limit), err, None)?
                    }
                    cpal::SampleFormat::I16 => {
                        let b = b2.clone();
                        dev.build_input_stream(&sc, move |d: &[i16], _: &_| push(&b, d.iter().map(|v| *v as f32 / 32768.0), limit), err, None)?
                    }
                    cpal::SampleFormat::U16 => {
                        let b = b2.clone();
                        dev.build_input_stream(&sc, move |d: &[u16], _: &_| push(&b, d.iter().map(|v| (*v as f32 - 32768.0) / 32768.0), limit), err, None)?
                    }
                    f => bail!("unsupported input sample format {f:?}"),
                };
                stream.play()?;
                Ok((stream, name, rate, ch))
            };
            match run() {
                Ok((stream, name, rate, ch)) => {
                    *i2.lock().unwrap() = Some((rate, ch as usize, name.clone()));
                    let _ = ready_tx.send(Ok((name, rate, ch)));
                    while !s2.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    drop(stream);
                    Ok(())
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = ready_tx.send(Err(e));
                    Err(anyhow!(msg))
                }
            }
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the audio input did not open within 10 s"))??;
        *cur = Some(Session { stop, buf, info, thread: Some(thread), started: Instant::now(), max_seconds });
        Ok(ready)
    }

    fn push(b: &Arc<Mutex<Vec<f32>>>, it: impl Iterator<Item = f32>, limit: usize) {
        if let Ok(mut v) = b.lock() {
            for x in it {
                if v.len() >= limit {
                    break;
                }
                v.push(x);
            }
        }
    }

    pub fn stop() -> Result<Take> {
        let mut s = CURRENT.lock().unwrap().take().ok_or_else(|| anyhow!("not recording (start_recording first)"))?;
        s.stop.store(true, Ordering::Relaxed);
        if let Some(t) = s.thread.take() {
            let _ = t.join();
        }
        let (rate, channels, device) = s.info.lock().unwrap().clone().unwrap_or((44100, 1, "input".into()));
        let data = std::mem::take(&mut *s.buf.lock().unwrap());
        Ok(Take { data, channels, rate, device })
    }
}

#[cfg(not(feature = "gui"))]
mod live {
    use super::Take;
    use anyhow::{bail, Result};
    const NO: &str = "this build has no audio input support (built without the gui feature)";
    pub fn inputs() -> Result<Vec<(String, u32, u16, bool)>> { bail!(NO) }
    pub fn is_recording() -> Option<f32> { None }
    pub fn start(_d: Option<String>, _m: f32) -> Result<(String, u32, u16)> { bail!(NO) }
    pub fn stop() -> Result<Take> { bail!(NO) }
}

/// Seconds since the current take started, when one is running (the studio
/// shows it on the record button).
pub fn recording_seconds() -> Option<f32> {
    live::is_recording()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_audio_inputs",
            description: "List the audio inputs (microphones, interfaces) you can record from, with their sample rate, channel count and which one is the default.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_, _| {
                let v = live::inputs()?;
                Ok(json!({"inputs": v.iter().map(|(n, r, c, d)| json!({"name": n, "rate": r, "channels": c, "default": d})).collect::<Vec<_>>(), "recording_s": live::is_recording()}))
            },
        },
        Tool {
            name: "start_recording",
            description: "Start recording from an audio input (default input unless device names one; list_audio_inputs). Recording runs until stop_recording or max_seconds (default 600). Play the song or a metronome click while it records to sing or play along.",
            mutates: false,
            schema: || obj(json!({"device": {"type": "string", "description": "part of an input's name"}, "max_seconds": {"type": "number"}}), &[]),
            run: |_, a| {
                let max = f_opt(a, "max_seconds").unwrap_or(600.0).clamp(1.0, 3600.0);
                let (name, rate, ch) = live::start(s_opt(a, "device"), max)?;
                Ok(json!({"recording": true, "device": name, "rate": rate, "channels": ch, "max_seconds": max, "next": "stop_recording {track, start_beat}"}))
            },
        },
        Tool {
            name: "stop_recording",
            description: "Stop the running recording and keep the take: writes recordings/take_N.wav (or name.wav), registers it as a sample and reports peak, RMS and clipping. With track, the take is placed on the timeline at start_beat (default 0) as an audio clip on that track (created when missing). trim_start_ms removes input latency or a count-in from the start.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "track": {"type": "string"}, "start_beat": {"type": "number"}, "trim_start_ms": {"type": "number"}}), &[]),
            run: |e, a| {
                let t = live::stop()?;
                finish_take(e, t, a)
            },
        },
        Tool {
            name: "record_audio",
            description: "Record a fixed number of seconds (1-600) from an audio input in one call, then keep the take like stop_recording: a WAV in recordings/, registered as a sample, optionally placed on track at start_beat. device picks an input (list_audio_inputs).",
            mutates: true,
            schema: || obj(json!({"seconds": {"type": "number"}, "device": {"type": "string"}, "name": {"type": "string"}, "track": {"type": "string"}, "start_beat": {"type": "number"}, "trim_start_ms": {"type": "number"}}), &["seconds"]),
            run: |e, a| {
                let secs = f_opt(a, "seconds").ok_or_else(|| anyhow!("seconds is required"))?.clamp(1.0, 600.0);
                live::start(s_opt(a, "device"), secs + 1.0)?;
                std::thread::sleep(std::time::Duration::from_secs_f32(secs));
                let t = live::stop()?;
                finish_take(e, t, a)
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_is_resampled_to_stereo_and_placed() {
        // a 48 kHz mono 440 Hz tone, 0.5 s
        let rate = 48_000u32;
        let data: Vec<f32> = (0..rate / 2).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin()).collect();
        let (l, r) = take_to_stereo(&Take { data: data.clone(), channels: 1, rate, device: "test".into() });
        assert!((l.len() as f32 - SR * 0.5).abs() < 50.0, "{}", l.len());
        assert_eq!(l.len(), r.len());
        let (peak, _, clipped) = take_stats(&data);
        assert!((peak + 6.0).abs() < 0.2 && clipped == 0, "{peak}");
        let dir = std::env::temp_dir().join("bb_recorder_test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut e = Engine::new(dir.clone());
        let out = finish_take(&mut e, Take { data, channels: 1, rate, device: "test".into() }, &json!({"track": "vox", "start_beat": 4})).unwrap();
        assert!(out["path"].as_str().unwrap().ends_with("recordings/take_1.wav"), "{out}");
        assert!(e.project.track_index("vox").is_ok());
        assert_eq!(e.project.audio_clips.len(), 1);
        assert_eq!(e.project.audio_clips[0].start_beat, 4.0);
        // stereo interleaved input keeps its sides
        let (l2, r2) = take_to_stereo(&Take { data: vec![0.1, -0.1, 0.2, -0.2], channels: 2, rate: SR as u32, device: "t".into() });
        assert_eq!((l2, r2), (vec![0.1, 0.2], vec![-0.1, -0.2]));
    }
}
