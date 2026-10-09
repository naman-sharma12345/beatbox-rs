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
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    pub device_name: String,
    virtual_clock: Option<Instant>,
    virtual_start: f64,
}

impl Player {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            mix: None,
            pos: 0.0,
            loop_len: 0,
            playing: false,
            step: 1.0,
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
        let mut s = self.shared.lock().unwrap();
        s.playing = false;
        s.pos = 0.0;
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
                let (l, r) = match (&s.mix, s.playing) {
                    (Some(m), true) => {
                        let i = s.pos as usize;
                        let v = (
                            m.left.get(i).copied().unwrap_or(0.0),
                            m.right.get(i).copied().unwrap_or(0.0),
                        );
                        s.pos += s.step;
                        if s.pos as usize >= s.loop_len {
                            s.pos = 0.0;
                        }
                        v
                    }
                    _ => (0.0, 0.0),
                };
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
