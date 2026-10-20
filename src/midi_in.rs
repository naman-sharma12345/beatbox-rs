//! MIDI controller input: play the chosen track from a keyboard, record what
//! is played into the pattern under the play head, and link knobs, faders and
//! pads to mixer levels, pattern launches and the transport. Hardware arrives
//! through the ALSA sequencer (studio builds on Linux); `midi_event` feeds the
//! same path from any MCP client, so every mapping is testable without a
//! device.

use crate::engine::Engine;
use crate::tools::{obj, s_opt, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A control on the controller: a CC number or a note, on one channel or any.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub cc: bool,
    pub number: u8,
    /// 1-16; None = any channel.
    pub channel: Option<u8>,
}

impl Control {
    pub fn parse(s: &str) -> Result<Control> {
        let w: Vec<String> = s.split_whitespace().map(|x| x.to_lowercase()).collect();
        let cc = match w.first().map(|x| x.as_str()) {
            Some("cc") | Some("control") => true,
            Some("note") | Some("pad") => false,
            _ => bail!("control must look like 'cc 7', 'note 36' or 'cc 7 ch 2' (got '{s}')"),
        };
        let number = w
            .get(1)
            .and_then(|x| x.parse::<u8>().ok())
            .filter(|n| *n < 128)
            .ok_or_else(|| anyhow!("control number must be 0-127 (got '{s}')"))?;
        let channel = match (w.get(2).map(|x| x.as_str()), w.get(3)) {
            (Some("ch") | Some("channel"), Some(c)) => Some(
                c.parse::<u8>()
                    .ok()
                    .filter(|c| (1..=16).contains(c))
                    .ok_or_else(|| anyhow!("channel must be 1-16"))?,
            ),
            (None, _) => None,
            _ => bail!("control must look like 'cc 7', 'note 36' or 'cc 7 ch 2' (got '{s}')"),
        };
        Ok(Control {
            cc,
            number,
            channel,
        })
    }

    fn matches(&self, cc: bool, number: u8, channel: u8) -> bool {
        self.cc == cc && self.number == number && self.channel.is_none_or(|c| c == channel)
    }

    pub fn label(&self) -> String {
        let mut s = format!("{} {}", if self.cc { "cc" } else { "note" }, self.number);
        if let Some(c) = self.channel {
            s.push_str(&format!(" ch {c}"));
        }
        s
    }
}

#[derive(Clone, Debug)]
pub struct Link {
    pub control: Control,
    /// volume:<track|master> | pan:<track> | launch:<pattern> | song | play | stop | toggle | record
    pub target: String,
}

impl Link {
    fn continuous(&self) -> bool {
        self.target.starts_with("volume:") || self.target.starts_with("pan:")
    }
}

/// A note held while recording: where it started.
#[derive(Clone, Debug)]
struct Held {
    pattern: usize,
    start: f32,
    vel: f32,
}

pub struct MidiState {
    pub links: Vec<Link>,
    /// Target waiting for the next control that moves.
    pub learn: Option<String>,
    /// Track the keyboard plays (empty = the first track).
    pub track: String,
    pub record: bool,
    /// Record grid in steps (16ths); 0 = no quantize.
    pub quantize: f32,
    /// How long a live note sounds, in seconds.
    pub gate_s: f32,
    held: HashMap<u8, Held>,
    pub recent: VecDeque<String>,
    /// Hardware: the connected port's name, its inbox and stop flag.
    pub port: Option<String>,
    pub inbox: Arc<Mutex<Vec<[u8; 3]>>>,
    stop: Option<Arc<AtomicBool>>,
}

impl Default for MidiState {
    fn default() -> Self {
        MidiState {
            links: Vec::new(),
            learn: None,
            track: String::new(),
            record: false,
            quantize: 1.0,
            gate_s: 0.6,
            held: HashMap::new(),
            recent: VecDeque::new(),
            port: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            stop: None,
        }
    }
}

impl Drop for MidiState {
    fn drop(&mut self) {
        if let Some(s) = &self.stop {
            s.store(true, Ordering::Relaxed);
        }
    }
}

/// Validate a link target against the project.
fn check_target(e: &Engine, t: &str) -> Result<String> {
    let t = t.trim();
    let low = t.to_lowercase();
    if matches!(low.as_str(), "song" | "play" | "stop" | "toggle" | "record") {
        return Ok(low);
    }
    if let Some((k, v)) = t.split_once(':') {
        let k = k.trim().to_lowercase();
        let v = v.trim();
        match k.as_str() {
            "volume" if v.eq_ignore_ascii_case("master") => return Ok("volume:master".into()),
            "volume" | "pan" => {
                let i = e.project.track_index(v)?;
                return Ok(format!("{k}:{}", e.project.tracks[i].name));
            }
            "launch" => {
                let i = e.project.pattern_index(v)?;
                return Ok(format!("launch:{}", e.project.patterns[i].name));
            }
            _ => {}
        }
    }
    bail!("unknown target '{t}': volume:<track|master>, pan:<track>, launch:<pattern>, song, play, stop, toggle or record")
}

/// Pattern and step under a play-head time: the pattern looping in
/// performance mode, else the song section at that time.
fn locate(e: &Engine, at_s: f32) -> Option<(usize, f32)> {
    let p = &e.project;
    let step = (at_s.max(0.0) / p.step_secs()) as f64;
    if let Some(name) = &e.transport.perf_playing {
        let i = p.pattern_index(name).ok()?;
        let n = p.patterns[i].steps().max(1) as f64;
        return Some((i, (step % n) as f32));
    }
    let mut acc = 0f64;
    for s in p.song_sections() {
        let i = p.pattern_index(&s.pattern).ok()?;
        let n = p.patterns[i].steps().max(1) as f64;
        let len = n * s.repeats.max(1) as f64;
        if step < acc + len {
            return Some((i, ((step - acc) % n) as f32));
        }
        acc += len;
    }
    None
}

fn snap(x: f32, q: f32) -> f32 {
    if q > 0.0 {
        (x / q).round() * q
    } else {
        x
    }
}

fn describe(m: &[u8; 3]) -> String {
    let ch = (m[0] & 0x0f) + 1;
    match m[0] & 0xf0 {
        0x90 if m[2] > 0 => format!("note on {} vel {} ch {ch}", m[1], m[2]),
        0x90 | 0x80 => format!("note off {} ch {ch}", m[1]),
        0xb0 => format!("cc {} = {} ch {ch}", m[1], m[2]),
        0xc0 => format!("program {} ch {ch}", m[1]),
        0xe0 => format!("pitch bend ch {ch}"),
        _ => format!("{:02X} {:02X} {:02X}", m[0], m[1], m[2]),
    }
}

/// Sound one note of the keyboard track on the studio's speakers.
fn sound_note(e: &mut Engine, pitch: u8, vel: f32) -> Option<String> {
    let ti = if e.midi.track.is_empty() {
        0
    } else {
        e.project.track_index(&e.midi.track).ok()?
    };
    let t = e.project.tracks.get(ti)?.clone();
    let samples = e.project.samples.clone();
    e.bank.sync(&samples);
    let tune = if e.project.tuning_cents != 0.0 && t.instrument.follows_tuning() {
        e.project.tuning_cents / 100.0
    } else {
        0.0
    };
    let mono = crate::instruments::render_note(
        &t.instrument,
        pitch as f32 + tune,
        vel,
        e.midi.gate_s.clamp(0.05, 8.0),
        &e.bank,
        pitch as u64,
    );
    let g = 10f32.powf(t.volume_db.clamp(-60.0, 12.0) / 20.0);
    let pan = t.pan.clamp(-1.0, 1.0);
    let (gl, gr) = (g * (1.0 - pan.max(0.0)), g * (1.0 + pan.min(0.0)));
    let l: Vec<f32> = mono.iter().map(|v| v * gl).collect();
    let r: Vec<f32> = mono.iter().map(|v| v * gr).collect();
    e.transport.pending_notes.push(Arc::new((l, r)));
    if e.transport.pending_notes.len() > 64 {
        e.transport.pending_notes.drain(..32);
    }
    Some(t.name)
}

/// Run a batch of raw MIDI messages through links, learn, the keyboard and the
/// recorder. `at_s` overrides the play-head time (tests, offline feeds).
pub fn process(e: &mut Engine, msgs: &[[u8; 3]], at_s: Option<f32>) -> Vec<Value> {
    // a knob turned fast sends many values: only the last one per linked
    // continuous control in this batch is applied
    let mut last_of: HashMap<usize, usize> = HashMap::new();
    for (k, m) in msgs.iter().enumerate() {
        if m[0] & 0xf0 == 0xb0 {
            if let Some(li) =
                e.midi.links.iter().position(|l| {
                    l.continuous() && l.control.matches(true, m[1], (m[0] & 0x0f) + 1)
                })
            {
                last_of.insert(li, k);
            }
        }
    }
    let mut out = Vec::new();
    for (k, m) in msgs.iter().enumerate() {
        let kind = m[0] & 0xf0;
        let ch = (m[0] & 0x0f) + 1;
        let on = kind == 0x90 && m[2] > 0;
        let off = kind == 0x80 || (kind == 0x90 && m[2] == 0);
        let cc = kind == 0xb0;
        let mut did: String;
        if let (Some(t), true) = (e.midi.learn.clone(), cc || on) {
            let control = Control {
                cc,
                number: m[1],
                channel: None,
            };
            e.midi
                .links
                .retain(|l| l.control != control && l.target != t);
            e.midi.links.push(Link {
                control: control.clone(),
                target: t.clone(),
            });
            e.midi.learn = None;
            did = format!("learned {} -> {t}", control.label());
        } else if let Some(li) = e
            .midi
            .links
            .iter()
            .position(|l| l.control.matches(cc, m[1], ch) && (cc || on))
        {
            let link = e.midi.links[li].clone();
            let v = m[2] as f32 / 127.0;
            let press = on || (cc && m[2] >= 64);
            let r = if link.continuous() {
                if last_of.get(&li).is_none_or(|&last| last == k) {
                    let (what, name) = link.target.split_once(':').unwrap();
                    if what == "volume" {
                        let db = if m[2] == 0 {
                            -60.0
                        } else {
                            ((-48.0 + 54.0 * v) * 10.0).round() / 10.0
                        };
                        Some(
                            e.call_from(
                                "set_mixer",
                                &json!({"track": name, "volume_db": db}),
                                "midi",
                            )
                            .map(|_| format!("{name} volume {db} dB")),
                        )
                    } else {
                        let pan = ((v * 2.0 - 1.0) * 100.0).round() / 100.0;
                        Some(
                            e.call_from("set_mixer", &json!({"track": name, "pan": pan}), "midi")
                                .map(|_| format!("{name} pan {pan}")),
                        )
                    }
                } else {
                    None
                }
            } else if !press {
                None
            } else if let Some(pat) = link.target.strip_prefix("launch:") {
                Some(
                    e.call_from(
                        "perform",
                        &json!({"action": "launch", "pattern": pat}),
                        "midi",
                    )
                    .map(|_| format!("launch {pat}")),
                )
            } else {
                match link.target.as_str() {
                    "song" => Some(
                        e.call_from("perform", &json!({"action": "exit"}), "midi")
                            .map(|_| "back to the song".to_string()),
                    ),
                    "record" => {
                        e.midi.record = if cc { m[2] >= 64 } else { !e.midi.record };
                        Some(Ok(format!(
                            "record {}",
                            if e.midi.record { "on" } else { "off" }
                        )))
                    }
                    t => Some(
                        e.call_from("transport", &json!({"action": t}), "midi")
                            .map(|_| t.to_string()),
                    ),
                }
            };
            did = match r {
                Some(Ok(s)) => s,
                Some(Err(err)) => format!("error: {err:#}"),
                None => "coalesced".into(),
            };
        } else if on {
            let vel = m[2] as f32 / 127.0;
            let track = sound_note(e, m[1], vel);
            did = format!(
                "played on {}",
                track.clone().unwrap_or_else(|| "nothing (no track)".into())
            );
            let playing = e.transport.playing || at_s.is_some();
            if e.midi.record && playing && track.is_some() {
                let now = at_s.unwrap_or(e.transport.position_s);
                if let Some((pi, step)) = locate(e, now) {
                    let n = e.project.patterns[pi].steps() as f32;
                    let start = snap(step, e.midi.quantize) % n;
                    e.midi.held.insert(
                        m[1],
                        Held {
                            pattern: pi,
                            start,
                            vel,
                        },
                    );
                    did.push_str(&format!(
                        ", recording from step {start} in {}",
                        e.project.patterns[pi].name
                    ));
                }
            }
        } else if off {
            if let Some(h) = e.midi.held.remove(&m[1]) {
                let now = at_s.unwrap_or(e.transport.position_s);
                let pat = e.project.patterns[h.pattern].clone();
                let n = pat.steps() as f32;
                let end = locate(e, now)
                    .filter(|(pi, _)| *pi == h.pattern)
                    .map(|(_, s)| s)
                    .unwrap_or(h.start + 1.0);
                let raw = (end - h.start).rem_euclid(n);
                let q = if e.midi.quantize > 0.0 {
                    e.midi.quantize
                } else {
                    0.25
                };
                let len = snap(raw, q).max(q);
                let track = if e.midi.track.is_empty() {
                    e.project
                        .tracks
                        .first()
                        .map(|t| t.name.clone())
                        .unwrap_or_default()
                } else {
                    e.midi.track.clone()
                };
                let r = e.call_from("add_notes", &json!({"track": track, "pattern": pat.name, "notes": [{"start": h.start, "len": len, "pitch": m[1], "vel": (h.vel * 100.0).round() / 100.0}]}), "midi");
                did = match r {
                    Ok(_) => format!(
                        "recorded note {} at step {} len {len} in {} on {track}",
                        m[1], h.start, pat.name
                    ),
                    Err(err) => format!("error: {err:#}"),
                };
            } else {
                did = "released".into();
            }
        } else {
            did = "ignored".into();
        }
        let line = format!("{} : {did}", describe(m));
        e.midi.recent.push_back(line.clone());
        while e.midi.recent.len() > 12 {
            e.midi.recent.pop_front();
        }
        out.push(json!(line));
    }
    out
}

/// Apply whatever the connected controller sent since the last call.
pub fn pump(e: &mut Engine) -> usize {
    let msgs: Vec<[u8; 3]> = std::mem::take(&mut *e.midi.inbox.lock().unwrap());
    if !msgs.is_empty() {
        process(e, &msgs, None);
    }
    msgs.len()
}

/// The setup worth keeping with the project (None when it is all defaults).
pub fn snapshot(e: &Engine) -> Option<crate::project::MidiSetup> {
    let m = &e.midi;
    if m.links.is_empty() && m.track.is_empty() && m.quantize == 1.0 && m.gate_s == 0.6 {
        return None;
    }
    Some(crate::project::MidiSetup {
        links: m
            .links
            .iter()
            .map(|l| crate::project::MidiLinkSave {
                control: l.control.label(),
                target: l.target.clone(),
            })
            .collect(),
        track: m.track.clone(),
        quantize: m.quantize,
        gate_s: m.gate_s,
    })
}

/// Put a saved setup back (or the defaults for a project without one). The
/// connected port stays connected. Links whose control or target no longer
/// parse against this project are skipped and named.
pub fn restore(e: &mut Engine, s: Option<&crate::project::MidiSetup>) -> (usize, Vec<String>) {
    let d = crate::project::MidiSetup {
        quantize: 1.0,
        gate_s: 0.6,
        ..Default::default()
    };
    let s = s.cloned().unwrap_or(d);
    let mut links = Vec::new();
    let mut skipped = Vec::new();
    for l in &s.links {
        match Control::parse(&l.control).and_then(|c| check_target(e, &l.target).map(|t| (c, t))) {
            Ok((control, target)) => {
                links.retain(|x: &Link| x.control != control);
                links.push(Link { control, target });
            }
            Err(err) => skipped.push(format!("{} -> {}: {err}", l.control, l.target)),
        }
    }
    let track = if s.track.is_empty() || e.project.track_index(&s.track).is_ok() {
        s.track.clone()
    } else {
        skipped.push(format!("keys track '{}' is not in this project", s.track));
        String::new()
    };
    let n = links.len();
    let m = &mut e.midi;
    m.links = links;
    m.track = track;
    m.quantize = s.quantize.max(0.0);
    m.gate_s = if s.gate_s > 0.0 { s.gate_s } else { 0.6 };
    m.learn = None;
    m.held.clear();
    (n, skipped)
}

fn parse_msg(v: &Value) -> Result<[u8; 3]> {
    let bytes: Vec<u8> = if let Some(a) = v.as_array() {
        a.iter()
            .map(|x| {
                x.as_u64()
                    .filter(|b| *b < 256)
                    .map(|b| b as u8)
                    .ok_or_else(|| anyhow!("message bytes must be 0-255"))
            })
            .collect::<Result<_>>()?
    } else if let Some(s) = v.as_str() {
        s.split_whitespace()
            .map(|h| {
                u8::from_str_radix(h.trim_start_matches("0x"), 16)
                    .map_err(|_| anyhow!("'{h}' is not a hex byte"))
            })
            .collect::<Result<_>>()?
    } else {
        bail!("each message is [status, data1, data2] or a hex string like '90 3C 64'");
    };
    if bytes.is_empty() || bytes[0] < 0x80 {
        bail!("a message starts with a status byte (0x80-0xEF)");
    }
    Ok([
        bytes[0],
        *bytes.get(1).unwrap_or(&0) & 0x7f,
        *bytes.get(2).unwrap_or(&0) & 0x7f,
    ])
}

fn state(e: &Engine) -> Value {
    json!({
        "connected": e.midi.port, "track": if e.midi.track.is_empty() { e.project.tracks.first().map(|t| t.name.clone()).unwrap_or_default() } else { e.midi.track.clone() },
        "record": e.midi.record, "quantize": e.midi.quantize, "gate_s": e.midi.gate_s,
        "learning": e.midi.learn,
        "links": e.midi.links.iter().map(|l| json!({"control": l.control.label(), "target": l.target})).collect::<Vec<_>>(),
        "recent": e.midi.recent.iter().collect::<Vec<_>>(),
    })
}

#[cfg(all(feature = "gui", target_os = "linux"))]
mod hw {
    use super::*;
    use alsa::seq;
    use std::ffi::CString;

    fn open(dir: alsa::Direction) -> Result<seq::Seq> {
        seq::Seq::open(None, Some(dir), true)
            .map_err(|e| anyhow!("no MIDI system on this machine ({e})"))
    }

    /// Readable ports: ("client:port", "client name: port name").
    pub fn ports() -> Result<Vec<(String, String)>> {
        let s = open(alsa::Direction::Capture)?;
        let me = s.client_id().unwrap_or(-1);
        let mut out = Vec::new();
        for c in seq::ClientIter::new(&s) {
            if c.get_client() == me || c.get_client() == 0 {
                continue;
            }
            let cname = c.get_name().unwrap_or("").to_string();
            for p in seq::PortIter::new(&s, c.get_client()) {
                let cap = p.get_capability();
                if cap.contains(seq::PortCap::READ | seq::PortCap::SUBS_READ) {
                    out.push((
                        format!("{}:{}", p.get_client(), p.get_port()),
                        format!("{cname}: {}", p.get_name().unwrap_or("")),
                    ));
                }
            }
        }
        Ok(out)
    }

    pub fn connect(
        want: &str,
        inbox: Arc<Mutex<Vec<[u8; 3]>>>,
        stop: Arc<AtomicBool>,
    ) -> Result<String> {
        let list = ports()?;
        let low = want.trim().to_lowercase();
        let (addr, name) = list
            .iter()
            .find(|(a, _)| *a == want.trim())
            .or_else(|| list.iter().find(|(_, n)| n.to_lowercase().contains(&low)))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "no MIDI port '{want}'. Ports: [{}]",
                    list.iter()
                        .map(|(a, n)| format!("{a} {n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let (c, p) = addr.split_once(':').unwrap();
        let src = seq::Addr {
            client: c.parse()?,
            port: p.parse()?,
        };
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("midi-in".into())
            .spawn(move || {
                let setup = || -> Result<seq::Seq> {
                    let s = open(alsa::Direction::Capture)?;
                    s.set_client_name(&CString::new("Beatbox Studio")?)?;
                    let mut pi = seq::PortInfo::empty()?;
                    pi.set_capability(seq::PortCap::WRITE | seq::PortCap::SUBS_WRITE);
                    pi.set_type(seq::PortType::MIDI_GENERIC | seq::PortType::APPLICATION);
                    pi.set_name(&CString::new("in")?);
                    s.create_port(&pi)?;
                    let sub = seq::PortSubscribe::empty()?;
                    sub.set_sender(src);
                    sub.set_dest(seq::Addr {
                        client: s.client_id()?,
                        port: pi.get_port(),
                    });
                    s.subscribe_port(&sub)?;
                    Ok(s)
                };
                let s = match setup() {
                    Ok(s) => {
                        let _ = tx.send(Ok(()));
                        s
                    }
                    Err(err) => {
                        let _ = tx.send(Err(format!("{err:#}")));
                        return;
                    }
                };
                let mut input = s.input();
                while !stop.load(Ordering::Relaxed) {
                    match input.event_input() {
                        Ok(ev) => {
                            let m = match ev.get_type() {
                                seq::EventType::Noteon => ev.get_data::<seq::EvNote>().map(|n| {
                                    [0x90 | (n.channel & 15), n.note & 127, n.velocity & 127]
                                }),
                                seq::EventType::Noteoff => ev.get_data::<seq::EvNote>().map(|n| {
                                    [0x80 | (n.channel & 15), n.note & 127, n.velocity & 127]
                                }),
                                seq::EventType::Controller => {
                                    ev.get_data::<seq::EvCtrl>().map(|c| {
                                        [
                                            0xb0 | (c.channel & 15),
                                            (c.param & 127) as u8,
                                            (c.value.clamp(0, 127)) as u8,
                                        ]
                                    })
                                }
                                seq::EventType::Pgmchange => {
                                    ev.get_data::<seq::EvCtrl>().map(|c| {
                                        [0xc0 | (c.channel & 15), (c.value.clamp(0, 127)) as u8, 0]
                                    })
                                }
                                _ => None,
                            };
                            if let Some(m) = m {
                                let mut q = inbox.lock().unwrap();
                                if q.len() < 4096 {
                                    q.push(m);
                                }
                            }
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
                    }
                }
            })?;
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(format!("{addr} {name}")),
            Ok(Err(err)) => bail!("could not connect to {name}: {err}"),
            Err(_) => bail!("MIDI connection timed out"),
        }
    }
}

#[cfg(not(all(feature = "gui", target_os = "linux")))]
mod hw {
    use super::*;
    pub fn ports() -> Result<Vec<(String, String)>> {
        bail!("this build has no MIDI device support (Linux studio builds have it)")
    }
    pub fn connect(_: &str, _: Arc<Mutex<Vec<[u8; 3]>>>, _: Arc<AtomicBool>) -> Result<String> {
        ports().map(|_| String::new())
    }
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "midi_input",
            description: "MIDI controller input. action status | ports (list controllers) | connect (port = 'client:port' or part of its name) | disconnect | set (track = which track the keyboard plays, record = write played notes into the pattern under the play head, quantize = record grid in steps, 0 = off, gate_s = live note length) | link (control 'cc 7' / 'note 36' / 'cc 7 ch 2' -> target volume:<track|master>, pan:<track>, launch:<pattern>, song, play, stop, toggle, record) | learn (target: the next knob or pad moved is linked to it) | unlink (control or target) | clear. Notes not linked play the track live; with record on and the transport running each note is written when released (one undo step per note).",
            mutates: false,
            schema: || obj(json!({
                "action": {"type": "string", "enum": ["status", "ports", "connect", "disconnect", "set", "link", "learn", "unlink", "clear"]},
                "port": {"type": "string"},
                "track": {"type": "string"},
                "record": {"type": "boolean"},
                "quantize": {"type": "number", "description": "record grid in steps (1 = 16th, 4 = beat, 0 = off)"},
                "gate_s": {"type": "number"},
                "control": {"type": "string", "description": "'cc 7', 'note 36', 'cc 7 ch 2'"},
                "target": {"type": "string"},
            }), &["action"]),
            run: |e, a| {
                pump(e);
                let act = s_opt(a, "action").unwrap_or_default();
                let mut extra = json!({});
                match act.as_str() {
                    "status" => {}
                    "ports" => {
                        let list = hw::ports()?;
                        extra = json!({"ports": list.iter().map(|(a, n)| json!({"port": a, "name": n})).collect::<Vec<_>>()});
                    }
                    "connect" => {
                        let want = s_opt(a, "port").ok_or_else(|| anyhow!("connect needs port (see action ports)"))?;
                        if let Some(s) = e.midi.stop.take() { s.store(true, Ordering::Relaxed); }
                        e.midi.port = None;
                        let stop = Arc::new(AtomicBool::new(false));
                        let name = hw::connect(&want, e.midi.inbox.clone(), stop.clone())?;
                        e.midi.stop = Some(stop);
                        e.midi.port = Some(name);
                    }
                    "disconnect" => {
                        if let Some(s) = e.midi.stop.take() { s.store(true, Ordering::Relaxed); }
                        e.midi.port = None;
                    }
                    "set" => {
                        if let Some(t) = s_opt(a, "track") {
                            let i = e.project.track_index(&t)?;
                            e.midi.track = e.project.tracks[i].name.clone();
                        }
                        if let Some(r) = a.get("record").and_then(|v| v.as_bool()) { e.midi.record = r; }
                        if let Some(q) = a.get("quantize").and_then(|v| v.as_f64()) {
                            if !(0.0..=64.0).contains(&q) { bail!("quantize must be 0-64 steps"); }
                            e.midi.quantize = q as f32;
                        }
                        if let Some(g) = a.get("gate_s").and_then(|v| v.as_f64()) { e.midi.gate_s = (g as f32).clamp(0.05, 8.0); }
                    }
                    "link" => {
                        let c = Control::parse(&s_opt(a, "control").ok_or_else(|| anyhow!("link needs control, e.g. 'cc 7'"))?)?;
                        let t = check_target(e, &s_opt(a, "target").ok_or_else(|| anyhow!("link needs target"))?)?;
                        e.midi.links.retain(|l| l.control != c);
                        e.midi.links.push(Link { control: c, target: t });
                    }
                    "learn" => {
                        let t = check_target(e, &s_opt(a, "target").ok_or_else(|| anyhow!("learn needs target"))?)?;
                        e.midi.learn = Some(t);
                    }
                    "unlink" => {
                        let n = e.midi.links.len();
                        if let Some(c) = s_opt(a, "control") {
                            let c = Control::parse(&c)?;
                            e.midi.links.retain(|l| l.control != c);
                        } else if let Some(t) = s_opt(a, "target") {
                            let t = t.trim().to_lowercase();
                            e.midi.links.retain(|l| l.target.to_lowercase() != t);
                        } else {
                            bail!("unlink needs control or target");
                        }
                        extra = json!({"removed": n - e.midi.links.len()});
                    }
                    "clear" => {
                        e.midi.links.clear();
                        e.midi.learn = None;
                    }
                    other => bail!("unknown action '{other}' (status, ports, connect, disconnect, set, link, learn, unlink, clear)"),
                }
                let mut v = state(e);
                if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
                    for (k, val) in x { o.insert(k.clone(), val.clone()); }
                }
                Ok(v)
            },
        },
        Tool {
            name: "midi_event",
            description: "Send MIDI messages into the studio as if a controller played them: messages = [[144, 60, 100], ...] or hex strings ['90 3C 64', '80 3C 00']. Goes through the same links, learn, live keyboard and recorder as a connected controller (midi_input). at_s sets the play-head time for recording without a running transport.",
            mutates: false,
            schema: || obj(json!({
                "messages": {"type": "array", "items": {}},
                "at_s": {"type": "number", "description": "play-head seconds to record at (default: the live transport position)"},
            }), &["messages"]),
            run: |e, a| {
                let arr = a.get("messages").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("messages must be an array"))?;
                let msgs = arr.iter().map(parse_msg).collect::<Result<Vec<_>>>()?;
                let at = a.get("at_s").and_then(|v| v.as_f64()).map(|v| v as f32);
                let done = process(e, &msgs, at);
                Ok(json!({"handled": done, "studio_attached": e.transport.attached, "record": e.midi.record}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng() -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_midi_tests"));
        e.call("generate_beat", &json!({"style": "house", "seed": 4}))
            .unwrap();
        e
    }

    #[test]
    fn controls_parse_and_bad_ones_are_refused() {
        assert_eq!(
            Control::parse("cc 7").unwrap(),
            Control {
                cc: true,
                number: 7,
                channel: None
            }
        );
        assert_eq!(
            Control::parse("Note 36 ch 10").unwrap(),
            Control {
                cc: false,
                number: 36,
                channel: Some(10)
            }
        );
        assert!(Control::parse("cc 200").is_err());
        assert!(Control::parse("knob 1").is_err());
        assert!(Control::parse("cc 7 ch 17").is_err());
        assert_eq!(parse_msg(&json!("90 3C 64")).unwrap(), [0x90, 60, 100]);
        assert!(parse_msg(&json!([60, 1, 2])).is_err());
    }

    #[test]
    fn knob_links_move_the_mixer_and_learn_binds_the_next_control() {
        let mut e = eng();
        let t = e.project.tracks[1].name.clone();
        e.call(
            "midi_input",
            &json!({"action": "link", "control": "cc 7", "target": format!("volume:{t}")}),
        )
        .unwrap();
        // a fast turn: only the last value lands
        let r = e
            .call(
                "midi_event",
                &json!({"messages": [[0xB0, 7, 10], [0xB0, 7, 50], [0xB0, 7, 127]]}),
            )
            .unwrap();
        assert!(r["handled"][0].as_str().unwrap().contains("coalesced"));
        assert!(
            (e.project.tracks[1].volume_db - 6.0).abs() < 0.01,
            "{}",
            e.project.tracks[1].volume_db
        );
        e.call(
            "midi_input",
            &json!({"action": "learn", "target": format!("pan:{t}")}),
        )
        .unwrap();
        e.call("midi_event", &json!({"messages": ["B0 0A 00"]}))
            .unwrap();
        let s = e.call("midi_input", &json!({"action": "status"})).unwrap();
        assert!(s["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["control"] == "cc 10" && l["target"] == format!("pan:{t}")));
        e.call("midi_event", &json!({"messages": ["B0 0A 7F"]}))
            .unwrap();
        assert!((e.project.tracks[1].pan - 1.0).abs() < 0.01);
        assert!(e
            .call(
                "midi_input",
                &json!({"action": "link", "control": "cc 1", "target": "volume:nope"})
            )
            .is_err());
        // a pad launches a pattern
        let pat = e.project.patterns[0].name.clone();
        e.call(
            "midi_input",
            &json!({"action": "link", "control": "note 36", "target": format!("launch:{pat}")}),
        )
        .unwrap();
        e.call("midi_event", &json!({"messages": [[0x99, 36, 90]]}))
            .unwrap();
        assert_eq!(
            e.transport
                .pending_launch
                .as_ref()
                .and_then(|l| l.pattern.clone()),
            Some(pat)
        );
    }

    #[test]
    fn links_and_keys_track_survive_save_and_load() {
        let mut e = eng();
        let t = e.project.tracks[1].name.clone();
        let pat = e.project.patterns[0].name.clone();
        e.call(
            "midi_input",
            &json!({"action": "link", "control": "cc 7 ch 2", "target": format!("volume:{t}")}),
        )
        .unwrap();
        e.call(
            "midi_input",
            &json!({"action": "link", "control": "note 36", "target": format!("launch:{pat}")}),
        )
        .unwrap();
        e.call(
            "midi_input",
            &json!({"action": "set", "track": t, "quantize": 4, "gate_s": 0.3}),
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("bb_midi_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("m.beatbox.json");
        let r = e
            .call("save_project", &json!({"path": f.to_string_lossy()}))
            .unwrap();
        assert_eq!(r["midi_links"], 2);
        // a fresh engine (a restart) gets the setup back from the file
        let mut e2 = eng();
        assert!(e2.midi.links.is_empty());
        let r = e2
            .call("load_project", &json!({"path": f.to_string_lossy()}))
            .unwrap();
        assert_eq!(r["midi"]["links"], 2, "{r}");
        assert_eq!(e2.midi.track, t);
        assert_eq!(e2.midi.quantize, 4.0);
        assert!((e2.midi.gate_s - 0.3).abs() < 1e-6);
        e2.call("midi_event", &json!({"messages": [[0xB1, 7, 127]]}))
            .unwrap();
        assert!((e2.project.tracks[1].volume_db - 6.0).abs() < 0.01);
        // a project with no setup clears the links; a stale link is skipped and named
        let mut p = e2.project.clone();
        p.midi = Some(crate::project::MidiSetup {
            links: vec![crate::project::MidiLinkSave {
                control: "cc 1".into(),
                target: "volume:gone".into(),
            }],
            track: "gone".into(),
            quantize: 1.0,
            gate_s: 0.6,
        });
        std::fs::write(&f, serde_json::to_string(&p).unwrap()).unwrap();
        let r = e2
            .call("load_project", &json!({"path": f.to_string_lossy()}))
            .unwrap();
        assert_eq!(r["midi"]["links"], 0);
        assert_eq!(r["midi"]["skipped"].as_array().unwrap().len(), 2);
        p.midi = None;
        std::fs::write(&f, serde_json::to_string(&p).unwrap()).unwrap();
        e2.call(
            "midi_input",
            &json!({"action": "link", "control": "cc 9", "target": "song"}),
        )
        .unwrap();
        e2.call("load_project", &json!({"path": f.to_string_lossy()}))
            .unwrap();
        assert!(e2.midi.links.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_play_the_track_and_record_into_the_pattern_under_the_play_head() {
        let mut e = eng();
        let t = e.project.tracks[0].name.clone();
        e.call(
            "midi_input",
            &json!({"action": "set", "track": t, "record": true, "quantize": 1}),
        )
        .unwrap();
        let pat = e.project.song_sections()[0].pattern.clone();
        let pi = e.project.pattern_index(&pat).unwrap();
        let ss = e.project.step_secs();
        let before = e.project.patterns[pi].notes(&t).len();
        // note on just after step 4 (snaps to 4), released at step 6.1
        let r = e
            .call(
                "midi_event",
                &json!({"messages": [[0x90, 64, 100]], "at_s": 4.2 * ss}),
            )
            .unwrap();
        assert!(
            r["handled"][0]
                .as_str()
                .unwrap()
                .contains("recording from step 4"),
            "{r}"
        );
        assert_eq!(e.transport.pending_notes.len(), 1);
        assert!(
            e.transport.pending_notes[0]
                .0
                .iter()
                .any(|v| v.abs() > 1e-4),
            "live note is silent"
        );
        e.call(
            "midi_event",
            &json!({"messages": ["80 40 00"], "at_s": 6.1 * ss}),
        )
        .unwrap();
        let notes = e.project.patterns[pi].notes(&t);
        assert_eq!(notes.len(), before + 1);
        assert!(notes
            .iter()
            .any(|n| n.pitch == 64 && (n.start - 4.0).abs() < 1e-3 && (n.len - 2.0).abs() < 1e-3));
        // record off: plays only
        e.call("midi_input", &json!({"action": "set", "record": false}))
            .unwrap();
        e.call(
            "midi_event",
            &json!({"messages": [[0x90, 65, 100], [0x80, 65, 0]], "at_s": 1.0}),
        )
        .unwrap();
        assert_eq!(e.project.patterns[pi].notes(&t).len(), before + 1);
        assert!(e
            .call("midi_input", &json!({"action": "set", "track": "nope"}))
            .is_err());
        let _ = e.call("midi_input", &json!({"action": "ports"})); // no MIDI system here: an error, not a panic
    }
}
