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
    /// Cumulative source samples played; never wraps or resets, so absolute
    /// musical time survives loop wraps and performance launches.
    abs: f64,
    /// Actual transition time on the cumulative player clock.
    last_landing: f64,
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
    /// Live update (an edit while playing): the new buffer, its loop length
    /// and the block the head was in when it arrived. It takes over when the
    /// head enters the next `SWAP_BLOCK`, crossfading over `XFADE` samples.
    pending: Option<(Arc<Mix>, usize, usize)>,
    /// Buffers being faded out, oldest first, each with how many crossfade
    /// samples of its fade into the next one (the last into `mix`) are
    /// done. A second update during a crossfade fades out of what is
    /// audible (the first fade, still running), not out of its target.
    fade: Vec<(Arc<Mix>, u32)>,
}

/// Live updates swap in on a boundary of this many source samples.
pub const SWAP_BLOCK: usize = 256;
/// Crossfade length of a live swap (10 ms at 44.1 kHz): no click, no restart.
pub const XFADE: u32 = 441;

impl Shared {
    /// New song buffer. `live` (a device is pulling frames) and playing:
    /// queue it for the next block boundary; otherwise swap at once.
    fn update(&mut self, mix: Arc<Mix>, len: usize, live: bool) {
        if live && self.playing && self.mix.is_some() {
            self.pending = Some((mix, len, self.pos as usize / SWAP_BLOCK));
            return;
        }
        self.pending = None;
        self.fade.clear();
        self.loop_len = len;
        if self.pos as usize >= self.loop_len {
            self.pos = 0.0;
        }
        self.mix = Some(mix);
    }

    /// The song's next stereo frame; advances the play head and swaps in a
    /// queued loop when the head crosses a quantum boundary or wraps.
    fn song_frame(&mut self) -> (f32, f32) {
        if !self.playing {
            return (0.0, 0.0);
        }
        let i = self.pos as usize;
        // a live update takes over once the head is in a new block
        if matches!(&self.pending, Some((_, _, b)) if i / SWAP_BLOCK != *b) {
            let (mix, len, _) = self.pending.take().unwrap();
            if let Some(old) = self.mix.take() {
                self.fade.push((old, 0));
            }
            self.mix = Some(mix);
            self.loop_len = len.max(1);
        }
        let Some(m) = &self.mix else {
            return (0.0, 0.0);
        };
        let at = |m: &Mix| {
            (
                m.left.get(i).copied().unwrap_or(0.0),
                m.right.get(i).copied().unwrap_or(0.0),
            )
        };
        let mut v = at(m);
        if !self.fade.is_empty() {
            // what was audible before each swap, fading into the next buffer
            let mut o = at(&self.fade[0].0);
            for k in 0..self.fade.len() {
                let next = if k + 1 < self.fade.len() {
                    at(&self.fade[k + 1].0)
                } else {
                    v
                };
                let g = (self.fade[k].1 + 1) as f32 / (XFADE + 1) as f32;
                o = (o.0 + (next.0 - o.0) * g, o.1 + (next.1 - o.1) * g);
                self.fade[k].1 += 1;
            }
            v = o;
            // fades end oldest first (all are XFADE long)
            while self.fade.first().is_some_and(|f| f.1 >= XFADE) {
                self.fade.remove(0);
            }
        }
        let old = self.pos;
        self.pos += self.step;
        self.abs += self.step;
        let wrapped = self.pos as usize >= self.loop_len;
        if wrapped {
            self.pos = 0.0;
        }
        if self.queued.is_some() {
            let q = self.quantum as f64;
            let crossed = q <= 0.0 || wrapped || (old / q).floor() != (self.pos / q).floor();
            if crossed {
                let (mix, len) = self.queued.take().unwrap();
                self.last_landing = self.abs / crate::dsp::SR as f64;
                self.pos = if wrapped || q <= 0.0 {
                    0.0
                } else {
                    self.pos - (self.pos / q).floor() * q
                };
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
    /// Absolute-time base for the virtual clock: rebased by the musical
    /// position at each launch landing so abs_position_secs stays continuous.
    abs_offset: f64,
    pub seek_revision: u64,
    virtual_clock: Option<Instant>,
    virtual_start: f64,
    /// Silent transport: a queued loop and when it takes over.
    virtual_queued: Option<(Arc<Mix>, usize, Instant, f64)>,
}

impl Player {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            abs: 0.0,
            last_landing: 0.0,
            mix: None,
            pos: 0.0,
            loop_len: 0,
            playing: false,
            step: 1.0,
            one: None,
            queued: None,
            quantum: 0,
            pending: None,
            fade: Vec::new(),
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
            abs_offset: 0.0,
            seek_revision: 0,
        }
    }

    /// Performance mode: loop `mix` (one pass is `loop_secs`). When nothing
    /// plays it starts at once; while playing it takes over on the next
    /// `quantum_secs` boundary of the current play head (or when the current
    /// loop wraps); 0 switches now. Returns the scheduled absolute landing
    /// time. last_landing_secs() reports the actual transition afterwards.
    pub fn launch(&mut self, mix: Arc<Mix>, loop_secs: f32, quantum_secs: f32) -> f64 {
        let len = ((loop_secs * crate::dsp::SR) as usize).max(1);
        if !self.is_playing() || quantum_secs <= 0.0 {
            let was = self.is_playing();
            let land_abs = self.abs_position_secs();
            self.abs_offset = land_abs;
            {
                let mut s = self.shared.lock().unwrap();
                s.last_landing = land_abs;
                s.queued = None;
                s.pending = None;
                s.fade.clear();
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
            return land_abs;
        }
        let now = self.position_secs();
        let loop_now = self.shared.lock().unwrap().loop_len as f32 / crate::dsp::SR;
        let next = (((now / quantum_secs).floor() + 1.0) * quantum_secs).min(loop_now.max(now));
        let wait = (next - now).max(0.0);
        let land_abs = self.abs_position_secs() + wait as f64;
        if self.has_audio() {
            let mut s = self.shared.lock().unwrap();
            s.pending = None;
            s.queued = Some((mix, len));
            s.quantum = ((quantum_secs * crate::dsp::SR) as usize).max(1);
        } else {
            self.virtual_queued = Some((
                mix,
                len,
                Instant::now() + std::time::Duration::from_secs_f32(wait),
                land_abs,
            ));
        }
        land_abs
    }

    /// True while a launched loop waits for its boundary.
    pub fn launch_pending(&self) -> bool {
        self.virtual_queued.is_some() || self.shared.lock().unwrap().queued.is_some()
    }

    /// Drop a launch that has not happened yet.
    #[allow(dead_code)]
    pub fn cancel_launch(&mut self) {
        self.virtual_queued = None;
        self.shared.lock().unwrap().queued = None;
    }

    /// Silent transport: apply a queued loop whose time has come (the audio
    /// callback does this itself on a real device). Call once per frame.
    pub fn poll(&mut self) {
        let due = matches!(&self.virtual_queued, Some((_, _, t, _)) if Instant::now() >= *t);
        if due {
            let (mix, len, t, land) = self.virtual_queued.take().unwrap();
            self.abs_offset = land;
            let mut s = self.shared.lock().unwrap();
            s.last_landing = land;
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

    /// Replace the song buffer. While it plays on a real device the swap
    /// waits for the next block boundary and crossfades (`Shared::update`),
    /// so an edit during playback never clicks or restarts the song.
    pub fn set_mix(&self, mix: Arc<Mix>, loop_secs: f32) {
        let len = ((loop_secs * crate::dsp::SR) as usize).max(1);
        let live = self.has_audio();
        self.shared.lock().unwrap().update(mix, len, live);
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
        if !self.has_audio() {
            self.abs_offset = self.abs_position_secs();
        }
        let mut s = self.shared.lock().unwrap();
        s.playing = false;
        s.pos = 0.0;
        s.queued = None;
        s.fade.clear();
        if let Some((mix, len, _)) = s.pending.take() {
            s.mix = Some(mix);
            s.loop_len = len.max(1);
        }
        self.virtual_clock = None;
        self.virtual_start = 0.0;
    }

    pub fn is_playing(&self) -> bool {
        self.shared.lock().unwrap().playing
    }

    pub fn seek(&mut self, secs: f32) {
        self.seek_revision = self.seek_revision.wrapping_add(1);
        if !self.has_audio() {
            self.abs_offset = self.abs_position_secs() - secs.max(0.0) as f64;
        }
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

    /// Cumulative musical seconds played: never wraps at loop boundaries and
    /// stays continuous across performance launches. This is the player clock
    /// itself, so it neither drifts against wall time nor can be confused by
    /// a stalled observer.
    /// Timestamp stored when the queued loop actually took over.
    pub fn last_landing_secs(&self) -> f64 {
        self.shared.lock().unwrap().last_landing
    }

    /// Read both clocks from one locked snapshot when arming a take.
    pub fn recording_anchor(&self) -> (f64, f32) {
        let s = self.shared.lock().unwrap();
        if self.has_audio() {
            (
                s.abs / crate::dsp::SR as f64,
                (s.pos / crate::dsp::SR as f64) as f32,
            )
        } else {
            let elapsed = match self.virtual_clock {
                Some(t) => self.virtual_start + t.elapsed().as_secs_f64(),
                None => s.pos / crate::dsp::SR as f64,
            };
            let len = s.loop_len.max(1) as f64 / crate::dsp::SR as f64;
            (self.abs_offset + elapsed, (elapsed % len) as f32)
        }
    }

    pub fn abs_position_secs(&self) -> f64 {
        let s = self.shared.lock().unwrap();
        if self.has_audio() {
            s.abs / crate::dsp::SR as f64
        } else {
            self.abs_offset
                + match self.virtual_clock {
                    Some(t0) => self.virtual_start + t0.elapsed().as_secs_f64(),
                    None => s.pos / crate::dsp::SR as f64,
                }
        }
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
        Arc::new(Mix {
            left: vec![v; n],
            right: vec![v; n],
            stems: vec![],
            track_info: vec![],
            bus_stems: vec![],
            seconds: n as f32 / crate::dsp::SR,
        })
    }

    fn shared(m: Arc<Mix>, len: usize) -> Shared {
        Shared {
            abs: 0.0,
            last_landing: 0.0,
            mix: Some(m),
            pos: 0.0,
            loop_len: len,
            playing: true,
            step: 1.0,
            one: None,
            queued: None,
            quantum: 0,
            pending: None,
            fade: Vec::new(),
        }
    }

    #[test]
    fn cumulative_audio_clock_keeps_actual_landing_after_wraps() {
        let mut s = shared(mix(1.0, 100), 100);
        for _ in 0..1130 {
            s.song_frame();
        }
        s.queued = Some((mix(2.0, 80), 80));
        s.quantum = 50;
        for _ in 0..25 {
            s.song_frame();
        }
        assert_eq!(s.abs, 1155.0);
        assert!((s.last_landing - 1150.0 / crate::dsp::SR as f64).abs() < 1e-9);
        assert!(s.queued.is_none());
    }

    #[test]
    fn virtual_launch_keeps_absolute_time_after_wraps_and_stalled_poll() {
        let mut p = Player {
            shared: Arc::new(Mutex::new(shared(mix(1.0, 44100), 44100))),
            _stream: None,
            device_name: "test silent".into(),
            abs_offset: 0.0,
            seek_revision: 0,
            virtual_clock: Some(Instant::now() - std::time::Duration::from_secs(10)),
            virtual_start: 0.0,
            virtual_queued: None,
        };
        let scheduled = p.launch(mix(2.0, 44100), 1.0, 0.25);
        assert!(scheduled >= 10.0);
        let (_, _, due, abs) = p.virtual_queued.as_mut().unwrap();
        *due = Instant::now() - std::time::Duration::from_secs(3);
        let actual = *abs;
        p.poll();
        assert_eq!(p.last_landing_secs(), actual);
        assert!((p.abs_position_secs() - actual - 3.0).abs() < 0.05);
        let before = p.abs_position_secs();
        p.stop();
        assert!((p.abs_position_secs() - before).abs() < 0.05);
        let frozen = p.abs_position_secs();
        assert!((p.abs_position_secs() - frozen).abs() < 0.001);
    }

    #[test]
    fn launch_waits_for_the_next_bar_then_loops_from_the_top() {
        let mut s = shared(mix(1.0, 1000), 1000);
        for _ in 0..130 {
            s.song_frame();
        }
        s.queued = Some((mix(2.0, 300), 300));
        s.quantum = 100;
        let mut first_new = None;
        for n in 0..200 {
            let (l, _) = s.song_frame();
            if l == 2.0 && first_new.is_none() {
                first_new = Some(n);
            }
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
            if l == 2.0 && first_new.is_none() {
                first_new = Some(n);
            }
        }
        assert_eq!(first_new, Some(150));
    }
    #[test]
    fn a_live_update_waits_for_the_next_block_then_crossfades() {
        // old buffer +0.5, new -0.5: a hard swap would step by 1.0
        let mut s = shared(mix(0.5, 10_000), 10_000);
        for _ in 0..1000 {
            s.song_frame();
        }
        s.update(mix(-0.5, 10_000), 10_000, true);
        let out: Vec<f32> = (0..2000).map(|_| s.song_frame().0).collect();
        // the head was at 1000 (block 3): untouched until 1024
        assert!(
            out[..24].iter().all(|v| *v == 0.5),
            "old buffer plays to the block boundary"
        );
        assert!(out[24] < 0.5, "the swap starts at the boundary");
        assert!(
            out[24 + XFADE as usize..].iter().all(|v| *v == -0.5),
            "the new buffer after the crossfade"
        );
        let steps = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0f32, f32::max);
        assert!(
            steps <= 1.0 / XFADE as f32 + 1e-4,
            "no click: max step {steps}"
        );
        // no restart: the head kept going
        assert_eq!(s.pos as usize, 3000);
        assert!(s.pending.is_none() && s.fade.is_empty());
    }

    /// A ramp buffer (sample i = i * k) shows exactly which source sample
    /// each output frame read.
    fn ramp(k: f32, n: usize) -> Arc<Mix> {
        let v: Vec<f32> = (0..n).map(|i| i as f32 * k).collect();
        Arc::new(Mix {
            left: v.clone(),
            right: v,
            stems: vec![],
            track_info: vec![],
            bus_stems: vec![],
            seconds: n as f32 / crate::dsp::SR,
        })
    }

    /// The expected frame while a crossfade is `done` samples in.
    fn blend(old: f32, new: f32, done: u32) -> f32 {
        let g = (done + 1) as f32 / (XFADE + 1) as f32;
        old + (new - old) * g
    }

    /// An update that arrives on the last sample of a block, and one on the
    /// first sample of a block: either way the old buffer plays to the end
    /// of the block the head is in, the swap starts on the next SWAP_BLOCK
    /// boundary, and the head never moves.
    #[test]
    fn a_live_update_on_either_side_of_a_block_boundary_swaps_on_the_next_one() {
        for head in [3 * SWAP_BLOCK - 1, 3 * SWAP_BLOCK] {
            let mut s = shared(ramp(1e-4, 10_000), 10_000);
            for _ in 0..head {
                s.song_frame();
            }
            s.update(ramp(-1e-4, 10_000), 10_000, true);
            let swap_at = (head / SWAP_BLOCK + 1) * SWAP_BLOCK;
            for i in head..swap_at + XFADE as usize + 10 {
                let (l, _) = s.song_frame();
                let (o, n) = (i as f32 * 1e-4, i as f32 * -1e-4);
                let want = if i < swap_at {
                    o
                } else if i < swap_at + XFADE as usize {
                    blend(o, n, (i - swap_at) as u32)
                } else {
                    n
                };
                assert_eq!(l, want, "head {head}: frame at source sample {i}");
            }
        }
    }

    /// An update queued in the last block before the loop wraps: the head
    /// wraps to 0 (a new block), the swap starts there, and the crossfade
    /// reads both buffers at the wrapped position.
    #[test]
    fn a_live_update_just_before_the_loop_wraps_swaps_at_the_top() {
        let len = 5 * SWAP_BLOCK + 100; // the loop ends 100 samples into block 5
        let mut s = shared(ramp(1e-4, len + 2000), len);
        for _ in 0..len - 20 {
            s.song_frame();
        }
        s.update(ramp(-1e-4, len + 2000), len, true);
        for k in 0..20 {
            let (l, _) = s.song_frame();
            assert_eq!(
                l,
                (len - 20 + k) as f32 * 1e-4,
                "old buffer to the end of the loop"
            );
        }
        for i in 0..XFADE as usize + 10 {
            let (l, _) = s.song_frame();
            let (o, n) = (i as f32 * 1e-4, i as f32 * -1e-4);
            let want = if i < XFADE as usize {
                blend(o, n, i as u32)
            } else {
                n
            };
            assert_eq!(l, want, "after the wrap, source sample {i}");
        }
        assert!(s.pending.is_none() && s.fade.is_empty());
    }

    /// A crossfade that starts in the last samples of the loop carries on
    /// across the wrap, reading both buffers at the wrapped position.
    #[test]
    fn a_crossfade_running_across_the_loop_wrap_continues_at_the_top() {
        let len = 4 * SWAP_BLOCK;
        let mut s = shared(ramp(1e-4, len + 2000), len);
        for _ in 0..len - SWAP_BLOCK - 1 {
            s.song_frame();
        } // head in block 2, last sample
        s.update(ramp(-1e-4, len + 2000), len, true);
        s.song_frame(); // last sample of block 2
                        // the swap starts at block 3 = the loop's last 256 samples, so the fade wraps
        for d in 0..XFADE as usize + 10 {
            let i = (len - SWAP_BLOCK + d) % len;
            let (l, _) = s.song_frame();
            let (o, n) = (i as f32 * 1e-4, i as f32 * -1e-4);
            let want = if d < XFADE as usize {
                blend(o, n, d as u32)
            } else {
                n
            };
            assert_eq!(l, want, "fade sample {d} (source {i})");
        }
    }

    /// A second live update that lands while the first one's crossfade is
    /// still running. Constant buffers +0.5 -> -0.5 -> +0.25, the second
    /// update queued 100 samples into the first fade: the largest
    /// frame-to-frame step stays within 1.5x a single fade's own step
    /// (1/XFADE of the jump).
    /// Before B47's fix the second swap started a new fade from the current
    /// buffer and dropped the half-done blend: a one-frame jump of 0.419 (a
    /// single fade steps 0.0023), a click when two updates land within one
    /// crossfade + block (~16 ms). Now it fades out of what is audible.
    #[test]
    fn a_second_live_update_during_a_crossfade() {
        let mut s = shared(mix(0.5, 10_000), 10_000);
        for _ in 0..1000 {
            s.song_frame();
        }
        s.update(mix(-0.5, 10_000), 10_000, true);
        let mut out: Vec<f32> = (0..124).map(|_| s.song_frame().0).collect(); // to 1124: 100 into the fade
        s.update(mix(0.25, 10_000), 10_000, true);
        out.extend((0..2000).map(|_| s.song_frame().0));
        let steps: Vec<f32> = out.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let (at, max) =
            steps
                .iter()
                .enumerate()
                .fold((0, 0f32), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
        let single = 1.0 / XFADE as f32;
        eprintln!("second update during a fade: max step {max:.5} at frame {} (head {}), a single fade's step {single:.5}", at + 1, 1000 + at + 1);
        assert!(
            out.last().copied() == Some(0.25),
            "ends on the newest buffer"
        );
        assert!(max <= single * 1.5, "a step of {max} where a single fade steps {single}: a click when two edits land within one crossfade");
    }

    #[test]
    fn without_a_device_or_when_stopped_the_update_is_immediate() {
        let mut s = shared(mix(0.5, 1000), 1000);
        s.update(mix(0.25, 1000), 1000, false);
        assert_eq!(s.song_frame().0, 0.25);
        s.playing = false;
        s.update(mix(0.75, 1000), 1000, true);
        s.playing = true;
        assert_eq!(s.song_frame().0, 0.75);
    }

    #[test]
    fn a_step_edited_while_playing_rerenders_its_bars_and_swaps_without_a_click() {
        use crate::engine::Engine;
        use crate::live_splice::{self, Dirty};
        use crate::render::{render, RenderOptions};
        use serde_json::json;
        let d = std::env::temp_dir().join(format!("live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "verse", "bars": 16}))
            .unwrap();
        for st in (0..16).map(|b| b * 16) {
            e.call(
                "toggle_step",
                &json!({"track": "kick", "step": st, "pattern": "verse"}),
            )
            .unwrap();
        }
        let chords: Vec<serde_json::Value> = (0..16)
            .map(|b| json!({"start": b * 16, "len": 12, "pitch": 60, "vel": 0.6}))
            .collect();
        e.call(
            "add_notes",
            &json!({"track": "keys", "pattern": "verse", "notes": chords}),
        )
        .unwrap();
        // the song plays the pattern from the playlist (as the rack does)
        e.call(
            "place_pattern",
            &json!({"pattern": "verse", "start_bar": 0, "lane": 0}),
        )
        .unwrap();
        let opts = RenderOptions::default();
        let p0 = e.project.clone();
        assert_eq!(live_splice::dirty_steps(&p0, &p0), Dirty::Nothing);
        let m0 = render(&p0, &e.bank, &opts).unwrap();
        // the edit: one kick step in bar 3 (step 36), "during playback"
        e.call(
            "toggle_step",
            &json!({"track": "kick", "step": 36, "pattern": "verse"}),
        )
        .unwrap();
        let p1 = e.project.clone();
        let Dirty::Steps(a, b) = live_splice::dirty_steps(&p0, &p1) else {
            panic!(
                "a step edit is a partial update: {:?}",
                live_splice::dirty_steps(&p0, &p1)
            )
        };
        assert!(
            (32..=36).contains(&a) && b <= 48,
            "only bar 3 is dirty: {a}..{b}"
        );
        let t = std::time::Instant::now();
        let (m1, changed) = live_splice::splice(&p0, &p1, &e.bank, &m0, a, b).unwrap();
        let t_splice = t.elapsed();
        let t = std::time::Instant::now();
        let full = render(&p1, &e.bank, &opts).unwrap();
        let t_full = t.elapsed();
        let sps = p0.step_secs() * crate::dsp::SR;
        let (w0, w1) = live_splice::window(&p0, &p1, a, b);
        assert!(live_splice::worth_splicing(&p0, &p1, a, b));
        assert!(
            w0 > 0 && w1 < p0.song_steps(),
            "the window is part of the song: {w0}..{w1}"
        );
        // outside the window the playing mix is kept bit for bit
        let ws = (w0 as f32 * sps) as usize;
        assert!(changed.0 >= ws, "nothing before the window changed");
        assert_eq!(&m1.left[..ws], &m0.left[..ws]);
        let we = ((w1 as f32 * sps) as usize + (opts.tail * crate::dsp::SR) as usize + 1)
            .min(m0.left.len());
        assert_eq!(&m1.left[we..], &m0.left[we..]);
        // inside, it is what a full render of the edit sounds like
        let (h0, h1) = ((36.0 * sps) as usize, (40.0 * sps) as usize);
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        let hit = rms(&full.left[h0..h1]);
        let before = rms(&m0.left[h0..h1]);
        let err: Vec<f32> = (h0..h1).map(|i| m1.left[i] - full.left[i]).collect();
        let err = rms(&err);
        eprintln!("live splice: dirty {a}..{b}, window {w0}..{w1}, splice {t_splice:?} vs full {t_full:?}; hit rms {hit}, before {before}, splice-vs-full err {err}");
        assert!(hit > before * 2.0 + 1e-4, "the new kick is audible");
        assert!(
            err < hit * 0.1,
            "the splice matches a full render: err {err} vs {hit}"
        );
        // playback: the head is mid-bar just before the hit, off any boundary
        let start = h0 - 300;
        let mut s = shared(Arc::new(m0), full.left.len());
        s.pos = start as f64 + 0.0;
        let old = s.mix.clone().unwrap();
        s.update(Arc::new(m1), full.left.len(), true);
        let new = s.pending.as_ref().unwrap().0.clone();
        let n = 4000;
        let out: Vec<(f32, f32)> = (0..n).map(|_| s.song_frame()).collect();
        let boundary = (start / SWAP_BLOCK + 1) * SWAP_BLOCK;
        for k in 0..(boundary - start) {
            assert_eq!(
                out[k].0,
                old.left[start + k],
                "old buffer until the boundary"
            );
        }
        let fade_end = boundary + XFADE as usize;
        for k in (fade_end - start)..n {
            assert_eq!(
                out[k].0,
                new.left[start + k],
                "new buffer after the crossfade"
            );
        }
        let l: Vec<f32> = out.iter().map(|v| v.0).collect();
        let r: Vec<f32> = out.iter().map(|v| v.1).collect();
        let played = live_splice::max_step(&l, &r, 0, n);
        let stat = live_splice::max_step(&old.left, &old.right, start, start + n).max(
            live_splice::max_step(&new.left, &new.right, start, start + n),
        );
        eprintln!("live swap: max step {played} vs static {stat}");
        assert!(
            played <= stat * 2.5 + 1e-6,
            "no discontinuity at the swap: {played} vs static {stat}"
        );
        assert_eq!(s.pos as usize, start + n, "no restart");
        // a mixer move is global: the whole song re-renders
        e.call("set_mixer", &json!({"track": "kick", "volume_db": -12.0}))
            .unwrap();
        assert_eq!(live_splice::dirty_steps(&p1, &e.project), Dirty::Everything);
    }
}
