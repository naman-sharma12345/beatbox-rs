//! Audio output for the studio. Plays the rendered mix in a loop through the
//! default output device; falls back to a silent "virtual transport" so the
//! playhead still moves on machines without audio (CI, servers).

use crate::render::Mix;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Shared {
    mix: Option<Arc<Mix>>,
    /// Position in source (44.1 kHz) samples.
    pos: f64,
    /// Loop length in samples (song body without the tail).
    loop_len: usize,
    playing: bool,
    step: f64,
    /// One-shot preview sounded on top of the song (instrument previews,
    /// samples): buffer and its read position.
    one: Option<(Arc<(Vec<f32>, Vec<f32>)>, f64)>,
    /// Performance mode: the next loop (mix, loop length) and the grid it
    /// waits for, in source samples (0 = switch on the next frame).
    queued: Option<(Arc<Mix>, usize)>,
    quantum: usize,
}

impl Shared {
    /// The song's next stereo frame; advances the play head and swaps in a
    /// queued loop when the head crosses a quantum boundary or wraps.
    fn song_frame(&mut self) -> (f32, f32) {
        if !self.playing {
            return (0.0, 0.0);
        }
        let Some(m) = &self.mix else { return (0.0, 0.0) };
        let i = self.pos as usize;
        let v = (
            m.left.get(i).copied().unwrap_or(0.0),
            m.right.get(i).copied().unwrap_or(0.0),
        );
        let old = self.pos;
        self.pos += self.step;
        let wrapped = self.pos as usize >= self.loop_len;
        if wrapped {
            self.pos = 0.0;
        }
        if self.queued.is_some() {
            let q = self.quantum as f64;
            let crossed = q <= 0.0 || wrapped || (old / q).floor() != (self.pos / q).floor();
            if crossed {
                let (mix, len) = self.queued.take().unwrap();
                self.pos = if wrapped || q <= 0.0 { 0.0 } else { self.pos - (self.pos / q).floor() * q };
                self.mix = Some(mix);
                self.loop_len = len.max(1);
            }
        }
        v
    }
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    pub device_name: String,
    virtual_clock: Option<Instant>,
    virtual_start: f64,
    /// Silent transport: a queued loop and when it takes over.
    virtual_queued: Option<(Arc<Mix>, usize, Instant)>,
}

impl Player {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            mix: None,
            pos: 0.0,
            loop_len: 0,
            playing: false,
            step: 1.0,
            one: None,
            queued: None,
            quantum: 0,
        }));
        let (stream, device_name) = match open_stream(shared.clone()) {
            Ok((s, n)) => (Some(s), n),
            Err(e) => (None, format!("no audio ({e})")),
        };
        Player {
            shared,
            _stream: stream,
            device_name,
            virtual_clock: None,
            virtual_start: 0.0,
            virtual_queued: None,
        }
    }

    /// Performance mode: loop `mix` (one pass is `loop_secs`). When nothing
    /// plays it starts at once; while playing it takes over on the next
    /// `quantum_secs` boundary of the current play head (or when the current
    /// loop wraps); 0 switches now.
    pub fn launch(&mut self, mix: Arc<Mix>, loop_secs: f32, quantum_secs: f32) {
        let len = ((loop_secs * crate::dsp::SR) as usize).max(1);
        if !self.is_playing() || quantum_secs <= 0.0 {
            let was = self.is_playing();
            {
                let mut s = self.shared.lock().unwrap();
                s.queued = None;
                s.mix = Some(mix);
                s.loop_len = len;
                s.pos = 0.0;
            }
            self.virtual_queued = None;
            self.virtual_start = 0.0;
            if was && !self.has_audio() {
                self.virtual_clock = Some(Instant::now());
            } else if !was {
                self.play();
            }
            return;
        }
        if self.has_audio() {
            let mut s = self.shared.lock().unwrap();
            s.queued = Some((mix, len));
            s.quantum = ((quantum_secs * crate::dsp::SR) as usize).max(1);
        } else {
            let now = self.position_secs();
            let loop_now = self.shared.lock().unwrap().loop_len as f32 / crate::dsp::SR;
            let next = (((now / quantum_secs).floor() + 1.0) * quantum_secs).min(loop_now.max(now));
            let wait = (next - now).max(0.0);
            self.virtual_queued = Some((mix, len, Instant::now() + std::time::Duration::from_secs_f32(wait)));
        }
    }

    /// True while a launched loop waits for its boundary.
    pub fn launch_pending(&self) -> bool {
        self.virtual_queued.is_some() || self.shared.lock().unwrap().queued.is_some()
    }

    /// Drop a launch that has not happened yet.
    pub fn cancel_launch(&mut self) {
        self.virtual_queued = None;
        self.shared.lock().unwrap().queued = None;
    }

    /// Silent transport: apply a queued loop whose time has come (the audio
    /// callback does this itself on a real device). Call once per frame.
    pub fn poll(&mut self) {
        let due = matches!(&self.virtual_queued, Some((_, _, t)) if Instant::now() >= *t);
        if due {
            let (mix, len, t) = self.virtual_queued.take().unwrap();
            let mut s = self.shared.lock().unwrap();
            s.mix = Some(mix);
            s.loop_len = len;
            s.pos = 0.0;
            self.virtual_start = 0.0;
            if self.virtual_clock.is_some() {
                self.virtual_clock = Some(t);
            }
        }
    }

    pub fn has_audio(&self) -> bool {
        self._stream.is_some()
    }

    pub fn set_mix(&self, mix: Arc<Mix>, loop_secs: f32) {
        let mut s = self.shared.lock().unwrap();
        s.loop_len = ((loop_secs * crate::dsp::SR) as usize).max(1);
        if s.pos as usize >= s.loop_len {
            s.pos = 0.0;
        }
        s.mix = Some(mix);
    }

    pub fn play(&mut self) {
        self.shared.lock().unwrap().playing = true;
        if !self.has_audio() {
            self.virtual_start = self.position_secs() as f64;
            self.virtual_clock = Some(Instant::now());
        }
    }

    pub fn stop(&mut self) {
        self.virtual_queued = None;
        let mut s = self.shared.lock().unwrap();
        s.playing = false;
        s.pos = 0.0;
        s.queued = None;
        self.virtual_clock = None;
        self.virtual_start = 0.0;
    }

    pub fn is_playing(&self) -> bool {
        self.shared.lock().unwrap().playing
    }

    pub fn seek(&mut self, secs: f32) {
        let mut s = self.shared.lock().unwrap();
        s.pos = (secs.max(0.0) * crate::dsp::SR) as f64;
        if self.virtual_clock.is_some() {
            self.virtual_start = secs as f64;
            self.virtual_clock = Some(Instant::now());
        }
    }

    /// Sound a buffer once on top of whatever is playing (replaces a preview
    /// that is still sounding).
    pub fn audition(&self, buf: Arc<(Vec<f32>, Vec<f32>)>) {
        self.shared.lock().unwrap().one = Some((buf, 0.0));
    }

    /// Layer a buffer on top of whatever preview is still sounding (live
    /// keyboard notes: a chord rings together instead of cutting off).
    pub fn layer(&self, buf: Arc<(Vec<f32>, Vec<f32>)>) {
        let mut s = self.shared.lock().unwrap();
        let next = match s.one.take() {
            Some((old, p)) => {
                let at = p as usize;
                let rest = old.0.len().saturating_sub(at);
                let n = rest.max(buf.0.len());
                let mut l = vec![0f32; n];
                let mut r = vec![0f32; n];
                for i in 0..n {
                    if i < rest {
                        l[i] += old.0[at + i];
                        r[i] += old.1.get(at + i).copied().unwrap_or(old.0[at + i]);
                    }
                    if i < buf.0.len() {
                        l[i] += buf.0[i];
                        r[i] += buf.1.get(i).copied().unwrap_or(buf.0[i]);
                    }
                }
                Arc::new((l, r))
            }
            None => buf,
        };
        s.one = Some((next, 0.0));
    }

    pub fn stop_audition(&self) {
        self.shared.lock().unwrap().one = None;
    }

    /// True while a preview is sounding on a real output device.
    pub fn auditioning(&self) -> bool {
        self.has_audio() && self.shared.lock().unwrap().one.is_some()
    }

    pub fn position_secs(&self) -> f32 {
        let s = self.shared.lock().unwrap();
        if let Some(t0) = self.virtual_clock {
            let len = s.loop_len.max(1) as f64 / crate::dsp::SR as f64;
            return ((self.virtual_start + t0.elapsed().as_secs_f64()) % len) as f32;
        }
        (s.pos / crate::dsp::SR as f64) as f32
    }
}

fn open_stream(shared: Arc<Mutex<Shared>>) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let name = device.name().unwrap_or_else(|_| "audio".into());
    let config = device.default_output_config()?;
    let rate = config.sample_rate().0 as f64;
    shared.lock().unwrap().step = crate::dsp::SR as f64 / rate;
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config.into(), shared)?,
        cpal::SampleFormat::I16 => build::<i16>(&device, &config.into(), shared)?,
        cpal::SampleFormat::U16 => build::<u16>(&device, &config.into(), shared)?,
        f => anyhow::bail!("unsupported sample format {f:?}"),
    };
    stream.play()?;
    Ok((stream, name))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Mutex<Shared>>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let Ok(mut s) = shared.try_lock() else {
                for x in data.iter_mut() {
                    *x = T::from_sample(0.0);
                }
                return;
            };
            for frame in data.chunks_mut(channels) {
                let (l, r) = s.song_frame();
                let step = s.step;
                let (mut l, mut r) = (l, r);
                let mut done = false;
                if let Some((b, p)) = s.one.as_mut() {
                    let i = *p as usize;
                    if i < b.0.len() {
                        l += b.0[i];
                        r += b.1.get(i).copied().unwrap_or(b.0[i]);
                        *p += step;
                    } else {
                        done = true;
                    }
                }
                if done {
                    s.one = None;
                }
                for (c, out) in frame.iter_mut().enumerate() {
                    let v = match c {
                        0 => l,
                        1 => r,
                        _ => 0.5 * (l + r),
                    };
                    *out = T::from_sample(v);
                }
            }
        },
        |e| eprintln!("audio stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(v: f32, n: usize) -> Arc<Mix> {
        Arc::new(Mix { left: vec![v; n], right: vec![v; n], stems: vec![], track_info: vec![], bus_stems: vec![], seconds: n as f32 / crate::dsp::SR })
    }

    fn shared(m: Arc<Mix>, len: usize) -> Shared {
        Shared { mix: Some(m), pos: 0.0, loop_len: len, playing: true, step: 1.0, one: None, queued: None, quantum: 0 }
    }

    #[test]
    fn launch_waits_for_the_next_bar_then_loops_from_the_top() {
        let mut s = shared(mix(1.0, 1000), 1000);
        for _ in 0..130 { s.song_frame(); }
        s.queued = Some((mix(2.0, 300), 300));
        s.quantum = 100;
        let mut first_new = None;
        for n in 0..200 {
            let (l, _) = s.song_frame();
            if l == 2.0 && first_new.is_none() { first_new = Some(n); }
        }
        // 130 frames in: the old loop sounds through frame 199, the new one from 200
        assert_eq!(first_new, Some(70));
        assert_eq!(s.loop_len, 300);
        assert!(s.queued.is_none());
    }

    #[test]
    fn launch_also_happens_when_the_old_loop_wraps() {
        let mut s = shared(mix(1.0, 150), 150);
        s.queued = Some((mix(2.0, 300), 300));
        s.quantum = 1000; // longer than the playing loop
        let mut first_new = None;
        for n in 0..400 {
            let (l, _) = s.song_frame();
            if l == 2.0 && first_new.is_none() { first_new = Some(n); }
        }
        assert_eq!(first_new, Some(150));
    }
}
