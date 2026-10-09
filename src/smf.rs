//! Minimal Standard MIDI File (SMF) writer and reader: format 0/1,
//! tempo, time signature, track names, program changes and notes with
//! running status. Enough to round-trip a song with any DAW.

use anyhow::{anyhow, bail, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct SmfNote {
    pub tick: u32,
    pub len: u32,
    pub pitch: u8,
    pub vel: u8,
    pub channel: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmfTrack {
    pub name: String,
    pub program: Option<u8>,
    pub notes: Vec<SmfNote>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Smf {
    /// Ticks per quarter note.
    pub division: u16,
    pub bpm: f32,
    pub time_sig: (u8, u8),
    pub tracks: Vec<SmfTrack>,
}

fn vlq(mut v: u32, out: &mut Vec<u8>) {
    let mut buf = [0u8; 5];
    let mut i = 4;
    buf[i] = (v & 0x7F) as u8;
    v >>= 7;
    while v > 0 {
        i -= 1;
        buf[i] = (v & 0x7F) as u8 | 0x80;
        v >>= 7;
    }
    out.extend_from_slice(&buf[i..]);
}

fn chunk(id: &[u8; 4], body: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

/// Encode as a format-1 file: a conductor track (tempo, meter) + one track each.
pub fn write(smf: &Smf) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hdr = Vec::new();
    hdr.extend_from_slice(&1u16.to_be_bytes());
    hdr.extend_from_slice(&((smf.tracks.len() + 1) as u16).to_be_bytes());
    hdr.extend_from_slice(&smf.division.to_be_bytes());
    chunk(b"MThd", &hdr, &mut out);

    // conductor
    let mut c = Vec::new();
    let us = (60_000_000.0 / smf.bpm.clamp(20.0, 400.0)).round() as u32;
    c.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03]);
    c.extend_from_slice(&us.to_be_bytes()[1..]);
    let den_pow = (smf.time_sig.1.max(1) as f32).log2().round() as u8;
    c.extend_from_slice(&[0x00, 0xFF, 0x58, 0x04, smf.time_sig.0, den_pow, 24, 8]);
    c.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    chunk(b"MTrk", &c, &mut out);

    for t in &smf.tracks {
        let mut ev: Vec<(u32, u8, Vec<u8>)> = Vec::new(); // (tick, order, bytes); offs before ons
        let ch0 = t.notes.first().map(|n| n.channel & 0x0F).unwrap_or(0);
        if let Some(p) = t.program {
            ev.push((0, 0, vec![0xC0 | ch0, p & 0x7F]));
        }
        for n in &t.notes {
            let ch = n.channel & 0x0F;
            ev.push((
                n.tick,
                2,
                vec![0x90 | ch, n.pitch & 0x7F, n.vel.clamp(1, 127)],
            ));
            ev.push((
                n.tick + n.len.max(1),
                1,
                vec![0x80 | ch, n.pitch & 0x7F, 0x40],
            ));
        }
        ev.sort_by_key(|e| (e.0, e.1));
        let mut b = Vec::new();
        let name = t.name.as_bytes();
        b.extend_from_slice(&[0x00, 0xFF, 0x03]);
        vlq(name.len() as u32, &mut b);
        b.extend_from_slice(name);
        let mut last = 0u32;
        for (tick, _, bytes) in ev {
            vlq(tick - last, &mut b);
            b.extend_from_slice(&bytes);
            last = tick;
        }
        b.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        chunk(b"MTrk", &b, &mut out);
    }
    out
}

struct Reader<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8> {
        let v = *self
            .d
            .get(self.i)
            .ok_or_else(|| anyhow!("unexpected end of MIDI data"))?;
        self.i += 1;
        Ok(v)
    }
    fn vlq(&mut self) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let b = self.u8()?;
            v = (v << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("bad variable-length quantity")
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.i + n > self.d.len() {
            bail!("unexpected end of MIDI data");
        }
        let s = &self.d[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
}

/// Parse an SMF (format 0 or 1). Format-0 files are split by channel.
pub fn read(data: &[u8]) -> Result<Smf> {
    let mut r = Reader { d: data, i: 0 };
    if r.take(4)? != b"MThd" {
        bail!("not a Standard MIDI File (missing MThd)");
    }
    let hlen = u32::from_be_bytes(r.take(4)?.try_into()?) as usize;
    let h = r.take(hlen)?;
    if h.len() < 6 {
        bail!("short MIDI header");
    }
    let format = u16::from_be_bytes([h[0], h[1]]);
    let ntracks = u16::from_be_bytes([h[2], h[3]]);
    let division = u16::from_be_bytes([h[4], h[5]]);
    if division & 0x8000 != 0 {
        bail!("SMPTE time division is not supported");
    }
    if format > 1 {
        bail!("MIDI format {format} is not supported (use format 0 or 1)");
    }
    let mut smf = Smf {
        division: division.max(1),
        bpm: 120.0,
        time_sig: (4, 4),
        tracks: Vec::new(),
    };
    let mut tempo_set = false;
    for _ in 0..ntracks {
        if r.i + 8 > data.len() {
            break;
        }
        let id = r.take(4)?;
        let len = u32::from_be_bytes(r.take(4)?.try_into()?) as usize;
        let body = r.take(len.min(data.len() - r.i))?;
        if id != b"MTrk" {
            continue;
        }
        let mut t = Reader { d: body, i: 0 };
        let mut tick = 0u32;
        let mut status = 0u8;
        let mut name = String::new();
        let mut program = None;
        let mut open: Vec<(u8, u8, u32, u8)> = Vec::new(); // ch, pitch, tick, vel
        let mut notes: Vec<SmfNote> = Vec::new();
        while t.i < body.len() {
            tick += t.vlq()?;
            let mut b = t.u8()?;
            if b == 0xFF {
                let kind = t.u8()?;
                let l = t.vlq()? as usize;
                let payload = t.take(l)?;
                match kind {
                    0x03 if name.is_empty() => name = String::from_utf8_lossy(payload).to_string(),
                    0x51 if l == 3 && !tempo_set => {
                        let us = u32::from_be_bytes([0, payload[0], payload[1], payload[2]]);
                        smf.bpm = (60_000_000.0 / us.max(1) as f32 * 100.0).round() / 100.0;
                        tempo_set = true;
                    }
                    0x58 if l >= 2 => smf.time_sig = (payload[0], 1u8 << payload[1].min(6)),
                    0x2F => break,
                    _ => {}
                }
                continue;
            }
            if b == 0xF0 || b == 0xF7 {
                let l = t.vlq()? as usize;
                t.take(l)?;
                continue;
            }
            let first;
            if b & 0x80 != 0 {
                status = b;
                first = t.u8()?;
            } else {
                first = b;
                b = status;
            }
            let kind = b & 0xF0;
            let ch = b & 0x0F;
            let two = !matches!(kind, 0xC0 | 0xD0);
            let second = if two { t.u8()? } else { 0 };
            match kind {
                0x90 if second > 0 => open.push((ch, first, tick, second)),
                0x80 | 0x90 => {
                    if let Some(k) = open.iter().position(|o| o.0 == ch && o.1 == first) {
                        let (c, p, t0, v) = open.remove(k);
                        notes.push(SmfNote {
                            tick: t0,
                            len: (tick - t0).max(1),
                            pitch: p,
                            vel: v,
                            channel: c,
                        });
                    }
                }
                0xC0 => program = Some(first),
                _ => {}
            }
        }
        for (c, p, t0, v) in open {
            notes.push(SmfNote {
                tick: t0,
                len: (tick.saturating_sub(t0)).max(division as u32 / 4),
                pitch: p,
                vel: v,
                channel: c,
            });
        }
        notes.sort_by_key(|n| (n.tick, n.pitch));
        if notes.is_empty() {
            continue;
        }
        if format == 0 {
            let mut chans: Vec<u8> = notes.iter().map(|n| n.channel).collect();
            chans.sort_unstable();
            chans.dedup();
            for c in chans {
                smf.tracks.push(SmfTrack {
                    name: if c == 9 {
                        "drums".into()
                    } else {
                        format!("ch{}", c + 1)
                    },
                    program,
                    notes: notes.iter().filter(|n| n.channel == c).cloned().collect(),
                });
            }
        } else {
            smf.tracks.push(SmfTrack {
                name,
                program,
                notes,
            });
        }
    }
    Ok(smf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_encoding() {
        let mut v = Vec::new();
        vlq(0x0FFF_FFFF, &mut v);
        assert_eq!(v, vec![0xFF, 0xFF, 0xFF, 0x7F]);
        v.clear();
        vlq(0x80, &mut v);
        assert_eq!(v, vec![0x81, 0x00]);
    }

    #[test]
    fn roundtrip() {
        let smf = Smf {
            division: 480,
            bpm: 140.0,
            time_sig: (4, 4),
            tracks: vec![
                SmfTrack {
                    name: "keys".into(),
                    program: Some(4),
                    notes: vec![
                        SmfNote {
                            tick: 0,
                            len: 480,
                            pitch: 60,
                            vel: 100,
                            channel: 0,
                        },
                        SmfNote {
                            tick: 0,
                            len: 480,
                            pitch: 64,
                            vel: 90,
                            channel: 0,
                        },
                        SmfNote {
                            tick: 480,
                            len: 240,
                            pitch: 67,
                            vel: 80,
                            channel: 0,
                        },
                    ],
                },
                SmfTrack {
                    name: "drums".into(),
                    program: None,
                    notes: vec![SmfNote {
                        tick: 960,
                        len: 120,
                        pitch: 36,
                        vel: 127,
                        channel: 9,
                    }],
                },
            ],
        };
        let back = read(&write(&smf)).unwrap();
        assert_eq!(back, smf);
    }
}
