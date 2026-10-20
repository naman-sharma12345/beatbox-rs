//! Headless waveform images, written by a PNG encoder that lives here.
//!
//! Nothing on this path uses an image or compression crate. It is written
//! from the published specifications:
//! - PNG (W3C PNG Specification, 2nd ed. / ISO/IEC 15948): signature, IHDR /
//!   IDAT / IEND chunks, CRC-32 over chunk type + data, filter types 0 and 2.
//! - zlib (RFC 1950): CMF/FLG header and the Adler-32 trailer.
//! - DEFLATE (RFC 1951): stored blocks, and a fixed-Huffman block fed by a
//!   small greedy LZ77 matcher (hash chains over a 32 KiB window).
//!
//! [`waveform_png`] draws min/max peaks per column (optionally an RMS band
//! inside them) for a mono fold or two stereo lanes, and encodes the image.
//! Same input, same bytes: integer pixel math from f64 sample statistics,
//! no clocks, no hashing seeds.

/// How the DEFLATE stream inside the PNG is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// Stored (uncompressed) blocks, RFC 1951 section 3.2.4.
    Stored,
    /// One fixed-Huffman block with LZ77 matches, RFC 1951 section 3.2.6.
    Fixed,
}

// ---------------------------------------------------------------- CRC-32

/// CRC-32 table (reflected polynomial 0xEDB88320), as in PNG Annex D.
const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
}

static CRC_TABLE: [u32; 256] = crc_table();

/// Continue a CRC-32: `crc32_update(crc32(a), b) == crc32(a ++ b)`.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// CRC-32 (ISO-HDLC, the one PNG and zip use).
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

// -------------------------------------------------------------- Adler-32

/// Adler-32 (RFC 1950 section 8.2).
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the largest run before b can overflow u32 (RFC 1950 / zlib NMAX)
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// --------------------------------------------------------------- DEFLATE

/// LSB-first bit writer (RFC 1951 section 3.1.1).
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new(cap: usize) -> Self {
        Bits {
            out: Vec::with_capacity(cap),
            acc: 0,
            n: 0,
        }
    }

    /// `len` low bits of `v`, least significant first (header fields, extra bits).
    fn put(&mut self, v: u32, len: u32) {
        let v = if len >= 32 {
            v as u64
        } else {
            (v & ((1u32 << len) - 1)) as u64
        };
        self.acc |= v << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code of `len` bits, most significant bit first.
    fn put_code(&mut self, code: u32, len: u32) {
        let mut r = 0u32;
        for i in 0..len {
            r |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(r, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Fixed literal/length code for symbol 0..=287 (RFC 1951 section 3.2.6).
fn put_fixed_symbol(b: &mut Bits, sym: u32) {
    match sym {
        0..=143 => b.put_code(0x30 + sym, 8),
        144..=255 => b.put_code(0x190 + (sym - 144), 9),
        256..=279 => b.put_code(sym - 256, 7),
        _ => b.put_code(0xC0 + (sym - 280), 8),
    }
}

/// Index of the last table entry `<= v`.
fn code_index(base: &[u16], v: usize) -> usize {
    let mut i = 0;
    while i + 1 < base.len() && base[i + 1] as usize <= v {
        i += 1;
    }
    i
}

fn put_match(b: &mut Bits, len: usize, dist: usize) {
    let li = code_index(&LEN_BASE, len);
    put_fixed_symbol(b, 257 + li as u32);
    b.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = code_index(&DIST_BASE, dist);
    b.put_code(di as u32, 5);
    b.put(
        (dist - DIST_BASE[di] as usize) as u32,
        DIST_EXTRA[di] as u32,
    );
}

/// DEFLATE as stored blocks of at most 65535 bytes.
pub fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 5 * (data.len() / 65_535 + 1));
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
        return out;
    }
    let mut chunks = data.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        // BFINAL in bit 0, BTYPE = 00, the rest of the byte is padding
        out.push(chunks.peek().is_none() as u8);
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out
}

/// DEFLATE as one fixed-Huffman block, greedy LZ77 over a 32 KiB window.
pub fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32_768;
    const HASH_BITS: u32 = 15;
    const MAX_CHAIN: usize = 48;
    const NONE: u32 = u32::MAX;
    let n = data.len();
    let mut b = Bits::new(n / 4 + 16);
    b.put(1, 1); // BFINAL
    b.put(1, 2); // BTYPE = 01, fixed Huffman
    let mut head = vec![NONE; 1 << HASH_BITS];
    let mut prev = vec![NONE; n];
    let hash = |i: usize| -> usize {
        let v = (data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let mut i = 0;
    while i < n {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + 3 <= n {
            let max_len = (n - i).min(258);
            let mut j = head[hash(i)];
            let mut chain = 0;
            while j != NONE && chain < MAX_CHAIN {
                let jj = j as usize;
                if i - jj > WINDOW {
                    break;
                }
                let mut l = 0;
                while l < max_len && data[jj + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - jj;
                    if l == max_len {
                        break;
                    }
                }
                j = prev[jj];
                chain += 1;
            }
        }
        let step = if best_len >= 3 {
            put_match(&mut b, best_len, best_dist);
            best_len
        } else {
            put_fixed_symbol(&mut b, data[i] as u32);
            1
        };
        for k in i..i + step {
            if k + 3 <= n {
                let h = hash(k);
                prev[k] = head[h];
                head[h] = k as u32;
            }
        }
        i += step;
    }
    put_fixed_symbol(&mut b, 256); // end of block
    b.finish()
}

/// A zlib stream (RFC 1950): CMF/FLG, DEFLATE data, Adler-32 (big-endian).
pub fn zlib(data: &[u8], c: Compression) -> Vec<u8> {
    let cmf: u8 = 0x78; // CM = 8 (deflate), CINFO = 7 (32 KiB window)
    let mut flg: u8 = match c {
        Compression::Stored => 0,
        Compression::Fixed => 1 << 6,
    };
    let rem = (cmf as u16 * 256 + flg as u16) % 31;
    if rem != 0 {
        flg += (31 - rem) as u8; // FCHECK
    }
    let body = match c {
        Compression::Stored => deflate_stored(data),
        Compression::Fixed => deflate_fixed(data),
    };
    let mut out = Vec::with_capacity(body.len() + 6);
    out.push(cmf);
    out.push(flg);
    out.extend_from_slice(&body);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

// ------------------------------------------------------------------- PNG

/// The 8-byte PNG signature.
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest IDAT payload per chunk (the stream is split across chunks).
pub const IDAT_CHUNK: usize = 1 << 18;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32_update(crc32(kind), data).to_be_bytes());
}

/// Encode 8-bit RGB pixels (row-major, `w * h * 3` bytes) as a PNG.
/// Row 0 uses filter 0 (None), every later row filter 2 (Up).
pub fn encode_rgb(w: u32, h: u32, rgb: &[u8], c: Compression) -> Vec<u8> {
    assert!(w > 0 && h > 0, "PNG needs a non-empty image");
    let stride = w as usize * 3;
    assert_eq!(
        rgb.len(),
        stride * h as usize,
        "RGB buffer is not {w}x{h}x3"
    );
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for y in 0..h as usize {
        let row = &rgb[y * stride..(y + 1) * stride];
        if y == 0 {
            raw.push(0);
            raw.extend_from_slice(row);
        } else {
            raw.push(2);
            let up = &rgb[(y - 1) * stride..y * stride];
            raw.extend(row.iter().zip(up).map(|(x, u)| x.wrapping_sub(*u)));
        }
    }
    let z = zlib(&raw, c);
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // depth 8, truecolour, deflate, filter 0, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    for part in z.chunks(IDAT_CHUNK) {
        chunk(&mut out, b"IDAT", part);
    }
    chunk(&mut out, b"IEND", &[]);
    out
}

// -------------------------------------------------------------- waveform

/// Which lanes the image has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lanes {
    /// One lane: every channel downmixed (their mean).
    Mono,
    /// Two lanes: left (channel 0) on top, right (channel 1) below; a mono
    /// source shows the same signal in both.
    Stereo,
    /// One lane per channel of the source, top to bottom.
    Channels,
}

impl Lanes {
    pub fn parse(s: &str) -> Option<Lanes> {
        match s {
            "mono" => Some(Lanes::Mono),
            "stereo" => Some(Lanes::Stereo),
            "channels" => Some(Lanes::Channels),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lanes::Mono => "mono",
            Lanes::Stereo => "stereo",
            Lanes::Channels => "channels",
        }
    }

    /// Lane count for a source with `channels` channels.
    pub fn count(self, channels: usize) -> usize {
        match self {
            Lanes::Mono => 1,
            Lanes::Stereo => 2,
            Lanes::Channels => channels.max(1),
        }
    }
}

/// How a waveform image looks. The default is a dark background with a
/// bright, high-contrast wave (easy to read in an MCP client).
#[derive(Clone, Debug, PartialEq)]
pub struct WaveStyle {
    pub width: usize,
    pub height: usize,
    pub lanes: Lanes,
    /// Draw each column's RMS as an inner band inside the peaks.
    pub rms: bool,
    pub background: [u8; 3],
    pub wave: [u8; 3],
    pub rms_color: [u8; 3],
    /// Zero line per lane, drawn under the wave (None = no line).
    pub center: Option<[u8; 3]>,
}

impl Default for WaveStyle {
    fn default() -> Self {
        WaveStyle {
            width: 1200,
            height: 300,
            lanes: Lanes::Mono,
            rms: false,
            background: [0x12, 0x14, 0x18],
            wave: [0x5A, 0xC8, 0xFA],
            rms_color: [0xD2, 0xF0, 0xFF],
            center: Some([0x2E, 0x34, 0x3C]),
        }
    }
}

/// `#rrggbb` or `rrggbb` to RGB.
pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Row of value `v` (clamped to -1..1, NaN as 0) in a lane of `lh` rows
/// starting at `y0`: +1 is the lane's top row, -1 its bottom row.
pub fn value_row(v: f64, y0: usize, lh: usize) -> usize {
    let v = if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) };
    y0 + ((1.0 - v) * 0.5 * (lh.saturating_sub(1)) as f64).round() as usize
}

/// Sample range `[a, b)` drawn by column `c` of `w` over `n` samples: the
/// columns tile the samples in order; with fewer samples than columns each
/// column still shows the one sample under it.
pub fn column_range(c: usize, w: usize, n: usize) -> (usize, usize) {
    if n == 0 || w == 0 {
        return (0, 0);
    }
    let at = |k: usize| ((k as u128 * n as u128) / w as u128) as usize;
    let a = at(c).min(n - 1);
    let b = at(c + 1).max(a + 1).min(n);
    (a, b)
}

/// One column's statistics. A column with no usable samples reads as silence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnStat {
    pub lo: f64,
    pub hi: f64,
    pub rms: f64,
}

#[derive(Clone, Copy)]
struct Acc {
    lo: f64,
    hi: f64,
    sq: f64,
    n: u64,
}

/// Streaming min / max / RMS per pixel column of one signal: samples are
/// pushed one at a time in order, memory is O(width) whatever the length.
/// NaN samples are skipped; everything else is clamped to -1..1 first
/// (so +-inf reads as full scale).
pub struct PeakScanner {
    width: usize,
    total: usize,
    seen: usize,
    first: usize,
    acc: Vec<Acc>,
}

impl PeakScanner {
    /// `total` is the number of samples that will be pushed (extra ones are ignored).
    pub fn new(width: usize, total: usize) -> Self {
        let width = width.max(1);
        PeakScanner {
            width,
            total,
            seen: 0,
            first: 0,
            acc: vec![
                Acc {
                    lo: f64::INFINITY,
                    hi: f64::NEG_INFINITY,
                    sq: 0.0,
                    n: 0
                };
                width
            ],
        }
    }

    pub fn push(&mut self, x: f64) {
        let i = self.seen;
        if i >= self.total {
            return;
        }
        self.seen += 1;
        while self.first < self.width && column_range(self.first, self.width, self.total).1 <= i {
            self.first += 1;
        }
        if x.is_nan() {
            return;
        }
        let v = x.clamp(-1.0, 1.0);
        let mut c = self.first;
        while c < self.width {
            let (a, b) = column_range(c, self.width, self.total);
            if a > i {
                break;
            }
            if i < b {
                let s = &mut self.acc[c];
                s.lo = s.lo.min(v);
                s.hi = s.hi.max(v);
                s.sq += v * v;
                s.n += 1;
            }
            c += 1;
        }
    }

    /// Samples pushed so far.
    pub fn seen(&self) -> usize {
        self.seen
    }

    pub fn finish(&self) -> Vec<ColumnStat> {
        self.acc
            .iter()
            .map(|s| {
                if s.n == 0 {
                    ColumnStat {
                        lo: 0.0,
                        hi: 0.0,
                        rms: 0.0,
                    }
                } else {
                    ColumnStat {
                        lo: s.lo,
                        hi: s.hi,
                        rms: (s.sq / s.n as f64).sqrt(),
                    }
                }
            })
            .collect()
    }
}

/// Streaming scan of multi-channel frames into the lanes of a [`Lanes`] mode.
pub struct LaneScan {
    lanes: Lanes,
    channels: usize,
    scan: Vec<PeakScanner>,
}

impl LaneScan {
    pub fn new(width: usize, frames: usize, channels: usize, lanes: Lanes) -> Self {
        let channels = channels.max(1);
        let scan = (0..lanes.count(channels))
            .map(|_| PeakScanner::new(width, frames))
            .collect();
        LaneScan {
            lanes,
            channels,
            scan,
        }
    }

    /// One frame: `channels` samples (missing ones read as 0).
    pub fn push_frame(&mut self, frame: &[f64]) {
        let ch = |k: usize| frame.get(k).copied().unwrap_or(0.0);
        match self.lanes {
            Lanes::Mono => {
                let mut s = 0.0;
                for k in 0..self.channels {
                    s += ch(k);
                }
                self.scan[0].push(s / self.channels as f64);
            }
            Lanes::Stereo => {
                let r = if self.channels > 1 { 1 } else { 0 };
                self.scan[0].push(ch(0));
                self.scan[1].push(ch(r));
            }
            Lanes::Channels => {
                for (k, s) in self.scan.iter_mut().enumerate() {
                    s.push(ch(k));
                }
            }
        }
    }

    pub fn finish(&self) -> Vec<Vec<ColumnStat>> {
        self.scan.iter().map(|s| s.finish()).collect()
    }
}

/// Draw lanes of column statistics into RGB8 pixels (`width * height * 3`).
/// Lane `k` of `L` takes rows `k*h/L .. (k+1)*h/L`.
pub fn render_lanes(lanes: &[Vec<ColumnStat>], st: &WaveStyle) -> Vec<u8> {
    let (w, h) = (st.width.max(1), st.height.max(1));
    let mut px = Vec::with_capacity(w * h * 3);
    for _ in 0..w * h {
        px.extend_from_slice(&st.background);
    }
    let mut put = |c: usize, y: usize, col: [u8; 3]| {
        let o = (y * w + c) * 3;
        px[o..o + 3].copy_from_slice(&col);
    };
    let nl = lanes.len();
    for (k, cols) in lanes.iter().enumerate() {
        let (y0, y1) = (k * h / nl, (k + 1) * h / nl);
        let lh = y1 - y0;
        if lh == 0 {
            continue;
        }
        for c in 0..w {
            let s = cols.get(c).copied().unwrap_or(ColumnStat {
                lo: 0.0,
                hi: 0.0,
                rms: 0.0,
            });
            if let Some(cc) = st.center {
                put(c, value_row(0.0, y0, lh), cc);
            }
            let (top, bot) = (value_row(s.hi, y0, lh), value_row(s.lo, y0, lh));
            for y in top..=bot {
                put(c, y, st.wave);
            }
            if st.rms {
                let (rt, rb) = (
                    value_row(s.rms, y0, lh).max(top),
                    value_row(-s.rms, y0, lh).min(bot),
                );
                for y in rt..=rb {
                    put(c, y, st.rms_color);
                }
            }
        }
    }
    px
}

/// The waveform of a stereo buffer as RGB8 pixels, no encoding. One pass,
/// no copy of the samples.
pub fn render_rgb(l: &[f32], r: &[f32], st: &WaveStyle) -> Vec<u8> {
    let n = l.len().min(r.len());
    let mut scan = LaneScan::new(st.width.max(1), n, 2, st.lanes);
    for i in 0..n {
        scan.push_frame(&[l[i] as f64, r[i] as f64]);
    }
    render_lanes(&scan.finish(), st)
}

/// Draw the waveform of `l`/`r` and encode it as a PNG with the built-in encoder.
pub fn waveform_png(l: &[f32], r: &[f32], st: &WaveStyle, c: Compression) -> Vec<u8> {
    let (w, h) = (st.width.max(1), st.height.max(1));
    encode_rgb(w as u32, h as u32, &render_rgb(l, r, st), c)
}

// ------------------------------------------------------- streaming WAV

/// What [`wav_file_png`] read.
#[derive(Clone, Debug, PartialEq)]
pub struct WavScan {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u16,
    pub float: bool,
    /// Frames in the file and frames drawn (the window).
    pub frames: usize,
    pub drawn: usize,
    pub start_frame: usize,
}

/// Bytes per read from the file.
pub const WAV_READ_CHUNK: usize = 1 << 16;

fn read_exact_or_eof(f: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            k => got += k,
        }
    }
    Ok(got)
}

/// Scan a PCM (8/16/24/32-bit integer) or IEEE float (32/64-bit) WAV file in
/// fixed-size chunks, never holding the samples: memory is O(width) plus one
/// read buffer. `window_s` limits it to a seconds range of the file.
pub fn scan_wav(
    path: &std::path::Path,
    width: usize,
    lanes: Lanes,
    window_s: Option<(f64, f64)>,
) -> Result<(Vec<Vec<ColumnStat>>, WavScan), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let flen = f.metadata().map_err(|e| e.to_string())?.len();
    let mut hdr = [0u8; 12];
    if read_exact_or_eof(&mut f, &mut hdr).map_err(|e| e.to_string())? < 12
        || &hdr[..4] != b"RIFF"
        || &hdr[8..12] != b"WAVE"
    {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut fmt: Option<(u16, usize, u32, u16, usize)> = None; // (tag, channels, rate, bits, block_align)
    let (data_at, data_len) = loop {
        let mut ch = [0u8; 8];
        if read_exact_or_eof(&mut f, &mut ch).map_err(|e| e.to_string())? < 8 {
            return Err("no data chunk".into());
        }
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        let at = f.stream_position().map_err(|e| e.to_string())?;
        if &ch[..4] == b"fmt " {
            let mut b = vec![0u8; size.min(64) as usize];
            f.read_exact(&mut b)
                .map_err(|e| format!("fmt chunk: {e}"))?;
            if b.len() < 16 {
                return Err("short fmt chunk".into());
            }
            let mut tag = u16::from_le_bytes([b[0], b[1]]);
            if tag == 0xFFFE && b.len() >= 26 {
                tag = u16::from_le_bytes([b[24], b[25]]); // WAVE_FORMAT_EXTENSIBLE sub-format
            }
            let channels = u16::from_le_bytes([b[2], b[3]]) as usize;
            let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
            let align = u16::from_le_bytes([b[12], b[13]]) as usize;
            let bits = u16::from_le_bytes([b[14], b[15]]);
            fmt = Some((tag, channels, rate, bits, align));
        } else if &ch[..4] == b"data" {
            // a streamed writer may leave the size at 0 / 0xFFFFFFFF: read to the end
            let rest = flen.saturating_sub(at);
            let len = if size == 0 || size == 0xFFFF_FFFF || size > rest {
                rest
            } else {
                size
            };
            break (at, len);
        }
        f.seek(SeekFrom::Start(at + size + (size & 1)))
            .map_err(|e| e.to_string())?;
    };
    let (tag, channels, rate, bits, align) = fmt.ok_or("data before fmt chunk")?;
    let float = match (tag, bits) {
        (1, 8 | 16 | 24 | 32) => false,
        (3, 32 | 64) => true,
        _ => {
            return Err(format!(
                "unsupported WAV encoding (format {tag}, {bits} bits)"
            ))
        }
    };
    let bps = bits as usize / 8;
    if channels == 0 || rate == 0 || align != channels * bps {
        return Err("inconsistent fmt chunk".into());
    }
    let frames = (data_len / align as u64) as usize;
    let (s0, s1) = match window_s {
        Some((a, b)) => {
            let a = ((a.max(0.0) * rate as f64) as usize).min(frames);
            let b = ((b.max(0.0) * rate as f64) as usize).clamp(a, frames);
            (a, b)
        }
        None => (0, frames),
    };
    let drawn = s1 - s0;
    let mut scan = LaneScan::new(width, drawn, channels, lanes);
    f.seek(SeekFrom::Start(data_at + (s0 * align) as u64))
        .map_err(|e| e.to_string())?;
    let mut f = std::io::BufReader::with_capacity(WAV_READ_CHUNK, f);
    let per_read = (WAV_READ_CHUNK / align).max(1) * align;
    let mut buf = vec![0u8; per_read];
    let mut frame = vec![0f64; channels];
    let mut left = drawn;
    while left > 0 {
        let want = (left * align).min(per_read);
        let got = read_exact_or_eof(&mut f, &mut buf[..want]).map_err(|e| e.to_string())?;
        let whole = got / align;
        for fr in buf[..whole * align].chunks_exact(align) {
            for (k, s) in fr.chunks_exact(bps).enumerate() {
                frame[k] = match (float, bps) {
                    (false, 1) => (s[0] as f64 - 128.0) / 128.0,
                    (false, 2) => i16::from_le_bytes([s[0], s[1]]) as f64 / 32_768.0,
                    (false, 3) => {
                        (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f64 / 8_388_608.0
                    }
                    (false, _) => {
                        i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64 / 2_147_483_648.0
                    }
                    (true, 4) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64,
                    (true, _) => {
                        f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
                    }
                };
            }
            scan.push_frame(&frame);
        }
        left -= whole;
        if whole == 0 || got < want {
            break; // truncated file: the rest reads as silence
        }
    }
    Ok((
        scan.finish(),
        WavScan {
            sample_rate: rate,
            channels,
            bits,
            float,
            frames,
            drawn,
            start_frame: s0,
        },
    ))
}

/// Draw a WAV file's waveform straight from disk (see [`scan_wav`]) and encode it.
pub fn wav_file_png(
    path: &std::path::Path,
    st: &WaveStyle,
    c: Compression,
    window_s: Option<(f64, f64)>,
) -> Result<(Vec<u8>, WavScan), String> {
    let (w, h) = (st.width.max(1), st.height.max(1));
    let (lanes, info) = scan_wav(path, w, st.lanes, window_s)?;
    Ok((
        encode_rgb(w as u32, h as u32, &render_lanes(&lanes, st), c),
        info,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let dec = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut rd = dec.read_info().expect("png crate reads the header");
        let mut buf = vec![0u8; rd.output_buffer_size().expect("buffer size")];
        let info = rd
            .next_frame(&mut buf)
            .expect("png crate decodes the image");
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    fn inflate(z: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(z)
            .read_to_end(&mut out)
            .expect("valid zlib (adler checked)");
        out
    }

    /// Small deterministic generator for test data (xorshift32).
    fn noise(n: usize, mut s: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                s as u8
            })
            .collect()
    }

    fn chunks_of(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        let mut out = Vec::new();
        let mut p = 8;
        while p < png.len() {
            let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
            let kind = &png[p + 4..p + 8];
            let data = &png[p + 8..p + 8 + len];
            let crc = u32::from_be_bytes(png[p + 8 + len..p + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32_update(crc32(kind), data), "chunk CRC");
            out.push((String::from_utf8(kind.to_vec()).unwrap(), data.to_vec()));
            p += 12 + len;
        }
        assert_eq!(p, png.len(), "no trailing bytes");
        out
    }

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        // the IEND chunk's CRC every PNG ends with
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32_update(crc32(b"1234"), b"56789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_known_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"a"), 0x0062_0062);
        assert_eq!(adler32(b"abc"), 0x024D_0127);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b"123456789"), 0x091E_01DE);
        // past the 5552-byte reduction boundary: 100000 x 0xFF
        let ff = vec![0xFFu8; 100_000];
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &ff {
            a = (a + x as u64) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(adler32(&ff), ((b << 16) | a) as u32);
    }

    #[test]
    fn zlib_header_and_round_trip_both_modes() {
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"abcabcabcabcabcabcabc".to_vec(),
            vec![0u8; 70_000],
            noise(140_000, 7),
        ];
        // long runs, near-repeats and far matches (distance > 16 KiB)
        let mut mixed = noise(20_000, 3);
        mixed.extend(vec![9u8; 1000]);
        mixed.extend(noise(20_000, 3));
        mixed.extend((0..50_000u32).map(|i| (i % 251) as u8));
        cases.push(mixed);
        for d in &cases {
            for c in [Compression::Stored, Compression::Fixed] {
                let z = zlib(d, c);
                assert_eq!((z[0] as u16 * 256 + z[1] as u16) % 31, 0, "FCHECK");
                assert_eq!(z[0], 0x78);
                assert_eq!(z[1] & 0x20, 0, "no preset dictionary");
                assert_eq!(
                    u32::from_be_bytes(z[z.len() - 4..].try_into().unwrap()),
                    adler32(d)
                );
                assert_eq!(&inflate(&z), d, "{c:?} round trip, {} bytes", d.len());
            }
        }
        assert_eq!(&zlib(b"", Compression::Stored)[..2], &[0x78, 0x01]);
        assert_eq!(&zlib(b"", Compression::Fixed)[..2], &[0x78, 0x5E]);
        // stored blocks are capped at 65535 bytes
        let s = deflate_stored(&vec![0u8; 70_000]);
        assert_eq!(s.len(), 70_000 + 2 * 5);
        assert_eq!(&s[..5], &[0, 0xFF, 0xFF, 0x00, 0x00]);
        // the matcher actually compresses runs
        assert!(zlib(&vec![0u8; 70_000], Compression::Fixed).len() < 1000);
    }

    #[test]
    fn fixed_huffman_matches_known_stream() {
        // RFC 1951 fixed block for "a": BFINAL=1, BTYPE=01, 'a' (0x30+0x61, 8 bits), EOB (7 zero bits)
        assert_eq!(deflate_fixed(b"a"), vec![0x4B, 0x04, 0x00]);
        assert_eq!(deflate_fixed(b""), vec![0x03, 0x00]);
    }

    #[test]
    fn png_signature_and_chunk_structure() {
        let rgb: Vec<u8> = (0..7 * 5 * 3).map(|i| (i * 37 % 256) as u8).collect();
        for c in [Compression::Stored, Compression::Fixed] {
            let png = encode_rgb(7, 5, &rgb, c);
            let ch = chunks_of(&png);
            let kinds: Vec<&str> = ch.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
            let ihdr = &ch[0].1;
            assert_eq!(ihdr.len(), 13);
            assert_eq!(&ihdr[..8], &[0, 0, 0, 7, 0, 0, 0, 5]);
            assert_eq!(&ihdr[8..], &[8, 2, 0, 0, 0]);
            assert!(ch[2].1.is_empty());
            assert_eq!(
                &png[png.len() - 12..],
                &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
            );
            // the scanlines: filter 0 on row 0, Up on the rest, undone by hand
            let raw = inflate(&ch[1].1);
            assert_eq!(raw.len(), 5 * (1 + 21));
            let mut prev = vec![0u8; 21];
            for y in 0..5 {
                let line = &raw[y * 22..(y + 1) * 22];
                assert_eq!(line[0], if y == 0 { 0 } else { 2 });
                let row: Vec<u8> = line[1..]
                    .iter()
                    .zip(&prev)
                    .map(|(d, u)| if y == 0 { *d } else { d.wrapping_add(*u) })
                    .collect();
                assert_eq!(&row[..], &rgb[y * 21..(y + 1) * 21]);
                prev = row;
            }
            let (w, h, px) = decode(&png);
            assert_eq!((w, h), (7, 5));
            assert_eq!(px, rgb);
        }
        // a big stored image spans several IDAT chunks and still decodes
        let big = noise(400 * 300 * 3, 11);
        let png = encode_rgb(400, 300, &big, Compression::Stored);
        let ch = chunks_of(&png);
        assert!(ch.iter().filter(|(k, _)| k == "IDAT").count() >= 2);
        assert_eq!(decode(&png).2, big);
        assert_eq!(
            decode(&encode_rgb(400, 300, &big, Compression::Fixed)).2,
            big
        );
    }

    const BG: [u8; 3] = [1, 2, 3];
    const WAVE: [u8; 3] = [200, 100, 50];
    const RMS: [u8; 3] = [250, 250, 250];
    const MID: [u8; 3] = [9, 9, 9];

    fn style(w: usize, h: usize, stereo: bool, rms: bool) -> WaveStyle {
        WaveStyle {
            width: w,
            height: h,
            lanes: if stereo { Lanes::Stereo } else { Lanes::Mono },
            rms,
            background: BG,
            wave: WAVE,
            rms_color: RMS,
            center: Some(MID),
        }
    }

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    fn check(l: &[f32], r: &[f32], st: &WaveStyle, expect: &[u8]) {
        assert_eq!(render_rgb(l, r, st), expect, "rendered pixels");
        for c in [Compression::Stored, Compression::Fixed] {
            let (w, h, px) = decode(&waveform_png(l, r, st, c));
            assert_eq!((w as usize, h as usize), (st.width, st.height));
            assert_eq!(
                px, expect,
                "{c:?}: decoded pixels match the expected image exactly"
            );
        }
    }

    #[test]
    fn silence_draws_only_the_zero_line() {
        let z = vec![0.0f32; 64_000];
        // mono, 101 rows: the zero line is row 50 and the (flat) wave covers it
        check(
            &z,
            &z,
            &style(64, 101, false, false),
            &image(64, 101, |_, y| if y == 50 { WAVE } else { BG }),
        );
        // with the RMS overlay the flat band is RMS-coloured
        check(
            &z,
            &z,
            &style(64, 101, false, true),
            &image(64, 101, |_, y| if y == 50 { RMS } else { BG }),
        );
        // no samples at all is silence too
        check(
            &[],
            &[],
            &style(16, 21, false, false),
            &image(16, 21, |_, y| if y == 10 { WAVE } else { BG }),
        );
    }

    #[test]
    fn full_scale_square_fills_every_column() {
        // +1/-1 every 50 samples, 1000 samples per column: every column spans both rails
        let sq: Vec<f32> = (0..80_000)
            .map(|i| if (i / 50) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        check(
            &sq,
            &sq,
            &style(80, 60, false, false),
            &image(80, 60, |_, _| WAVE),
        );
        // RMS of a full-scale square is 1: the band covers the whole lane
        check(
            &sq,
            &sq,
            &style(80, 60, false, true),
            &image(80, 60, |_, _| RMS),
        );
        // left square, right inverted square: the mono fold cancels to the zero line
        let inv: Vec<f32> = sq.iter().map(|v| -v).collect();
        check(
            &sq,
            &inv,
            &style(80, 61, false, false),
            &image(80, 61, |_, y| if y == 30 { WAVE } else { BG }),
        );
    }

    fn sine(n: usize, period: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f64::consts::TAU * i as f64 / period as f64).sin() as f32)
            .collect()
    }

    #[test]
    fn sine_peaks_and_rms_band_are_pixel_exact() {
        // 100-sample period, 1000 samples per column: ten whole periods each
        let s = sine(100_000, 100, 1.0);
        let z = vec![0.0f32; s.len()];
        // mono fold of (sine, sine) is the sine: peaks fill rows 0..=100,
        // RMS 1/sqrt(2) puts the band at rows round(50*(1-0.7071)) = 15 ..= 85
        let mono = image(
            100,
            101,
            |_, y| if (15..=85).contains(&y) { RMS } else { WAVE },
        );
        check(&s, &s, &style(100, 101, false, true), &mono);
        // stereo: left lane rows 0..101 carries the sine, right lane (silence) shows its zero line at 101 + 50
        let st = image(100, 202, |_, y| match y {
            0..=100 if (15..=85).contains(&y) => RMS,
            0..=100 => WAVE,
            151 => RMS,
            _ => BG,
        });
        check(&s, &z, &style(100, 202, true, true), &st);
        // half amplitude, no RMS: rows round(50*(1-0.5)) = 25 ..= 75, zero line hidden under the wave
        let half = sine(100_000, 100, 0.5);
        check(
            &half,
            &half,
            &style(100, 101, false, false),
            &image(
                100,
                101,
                |_, y| if (25..=75).contains(&y) { WAVE } else { BG },
            ),
        );
    }

    #[test]
    fn waveform_png_is_deterministic() {
        let s = sine(48_000, 120, 0.8);
        let r: Vec<f32> = s
            .iter()
            .enumerate()
            .map(|(i, v)| v * (i as f32 / 48_000.0))
            .collect();
        let st = WaveStyle {
            lanes: Lanes::Stereo,
            rms: true,
            ..WaveStyle::default()
        };
        for c in [Compression::Stored, Compression::Fixed] {
            let a = waveform_png(&s, &r, &st, c);
            let b = waveform_png(&s, &r, &st, c);
            assert_eq!(a, b, "{c:?}: same input, same bytes");
            assert_eq!(decode(&a).2, render_rgb(&s, &r, &st));
        }
        // pinned fingerprint of the fixed-Huffman file (CRC-32 of the whole PNG)
        let png = waveform_png(&s, &r, &st, Compression::Fixed);
        assert_eq!(
            (png.len(), crc32(&png)),
            PINNED_DETERMINISM,
            "fixed-Huffman bytes changed"
        );
    }

    /// (byte length, CRC-32) of the determinism test's fixed-Huffman PNG.
    const PINNED_DETERMINISM: (usize, u32) = (9161, 0x13F18390);

    #[test]
    fn buffer_lengths_zero_one_few_odd_and_long() {
        let st = |w: usize, h: usize| style(w, h, false, false);
        // 0 samples: the flat line at the centre
        check(
            &[],
            &[],
            &st(16, 101),
            &image(16, 101, |_, y| if y == 50 { WAVE } else { BG }),
        );
        // 1 sample (0.5): every column shows it at row round(50 * 0.5) = 25, zero line at 50
        check(
            &[0.5],
            &[0.5],
            &st(16, 101),
            &image(16, 101, |_, y| match y {
                25 => WAVE,
                50 => MID,
                _ => BG,
            }),
        );
        // fewer samples than columns: 3 samples over 12 columns, 4 columns each
        let v = [1.0f32, 0.0, -1.0];
        check(
            &v,
            &v,
            &st(12, 101),
            &image(12, 101, |x, y| {
                let row = [0, 50, 100][x / 4];
                if y == row {
                    WAVE
                } else if y == 50 {
                    MID
                } else {
                    BG
                }
            }),
        );
        // non-divisible lengths: the columns tile the samples exactly, no gap or overlap
        for (n, w) in [
            (1001usize, 64usize),
            (64, 64),
            (65, 64),
            (127, 64),
            (99_999, 1200),
            (7, 7),
        ] {
            let mut next = 0;
            for c in 0..w {
                let (a, b) = column_range(c, w, n);
                assert_eq!(a, next, "n={n} w={w} c={c}");
                assert!(b > a);
                next = b;
            }
            assert_eq!(next, n);
        }
        // streaming statistics equal a brute-force pass, on odd and long buffers
        for n in [1001usize, 2_000_003] {
            let x: Vec<f32> = (0..n)
                .map(|i| ((i as f64 * 0.0137).sin() * (1.0 + (i % 97) as f64 / 50.0)) as f32)
                .collect();
            let w = 333;
            let mut sc = PeakScanner::new(w, n);
            for &v in &x {
                sc.push(v as f64);
            }
            assert_eq!(sc.acc.len(), w, "memory is one accumulator per column");
            assert_eq!(sc.seen(), n);
            let got = sc.finish();
            for c in 0..w {
                let (a, b) = column_range(c, w, n);
                let col: Vec<f64> = x[a..b]
                    .iter()
                    .map(|v| (*v as f64).clamp(-1.0, 1.0))
                    .collect();
                let lo = col.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = col.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let rms = (col.iter().map(|v| v * v).sum::<f64>() / col.len() as f64).sqrt();
                assert_eq!((got[c].lo, got[c].hi), (lo, hi), "n={n} c={c}");
                assert!((got[c].rms - rms).abs() < 1e-12, "n={n} c={c}");
            }
            let png = waveform_png(
                &x,
                &x,
                &WaveStyle {
                    width: w,
                    height: 90,
                    rms: true,
                    ..WaveStyle::default()
                },
                Compression::Fixed,
            );
            assert_eq!(decode(&png).0, w as u32);
        }
    }

    #[test]
    fn silence_is_a_flat_centre_line_without_nan() {
        let z = vec![0.0f32; 12_345];
        let mut sc = LaneScan::new(50, z.len(), 2, Lanes::Stereo);
        for &v in &z {
            sc.push_frame(&[v as f64, v as f64]);
        }
        for lane in sc.finish() {
            assert!(lane.iter().all(|c| *c
                == ColumnStat {
                    lo: 0.0,
                    hi: 0.0,
                    rms: 0.0
                }));
        }
        // stereo, 2 x 51 rows: one flat row per lane, RMS band (0) on it, nothing else
        check(
            &z,
            &z,
            &style(50, 102, true, true),
            &image(
                50,
                102,
                |_, y| if y == 25 || y == 51 + 25 { RMS } else { BG },
            ),
        );
    }

    #[test]
    fn extreme_peaks_clamp_and_nan_inf_do_not_panic() {
        let full = image(40, 61, |_, _| RMS);
        let sq = |amp: f32| -> Vec<f32> {
            (0..40_000)
                .map(|i| if (i / 25) % 2 == 0 { amp } else { -amp })
                .collect()
        };
        // +-1.0 exactly, and +-4.0 clamped to it: the same image
        check(&sq(1.0), &sq(1.0), &style(40, 61, false, true), &full);
        check(&sq(4.0), &sq(4.0), &style(40, 61, false, true), &full);
        // +-inf reads as full scale
        let inf: Vec<f32> = sq(1.0).iter().map(|v| v * f32::INFINITY).collect();
        check(&inf, &inf, &style(40, 61, false, true), &full);
        // NaN samples are skipped: all-NaN is silence, NaN sprinkled into the square changes nothing
        let nan = vec![f32::NAN; 40_000];
        check(
            &nan,
            &nan,
            &style(40, 61, false, true),
            &image(40, 61, |_, y| if y == 30 { RMS } else { BG }),
        );
        let mut holes = sq(1.0);
        for i in (3..holes.len()).step_by(7) {
            holes[i] = f32::NAN;
        }
        check(&holes, &holes, &style(40, 61, false, true), &full);
        // a NaN on one side of a stereo pair only touches the mono fold of that frame
        let mut mixed = sq(0.5);
        mixed[0] = f32::NAN;
        let png = waveform_png(
            &mixed,
            &sq(0.5),
            &style(40, 61, false, true),
            Compression::Fixed,
        );
        assert_eq!(decode(&png).0, 40);
        // stats stay finite
        let mut sc = PeakScanner::new(8, 4);
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 2.0] {
            sc.push(v);
        }
        assert!(sc
            .finish()
            .iter()
            .all(|c| c.lo.is_finite() && c.hi.is_finite() && c.rms.is_finite()));
    }

    /// A WAV file: `fmt` tag, bits, channels, rate, interleaved frames already encoded.
    fn wav_file(
        name: &str,
        tag: u16,
        bits: u16,
        channels: u16,
        rate: u32,
        data: &[u8],
        data_size: Option<u32>,
    ) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("beatbox_waveform_png_wav");
        std::fs::create_dir_all(&dir).unwrap();
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((4 + 8 + 16 + 8 + 10 + 8 + data.len()) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        let align = channels * bits / 8;
        v.extend_from_slice(&(rate * align as u32).to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        // an unrelated chunk before the data (odd size, padded)
        v.extend_from_slice(b"LIST");
        v.extend_from_slice(&9u32.to_le_bytes());
        v.extend_from_slice(b"INFOabcde\0");
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_size.unwrap_or(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        let p = dir.join(name);
        std::fs::write(&p, v).unwrap();
        p
    }

    #[test]
    fn wav_files_stream_in_chunks_and_match_the_buffer_path() {
        // 16-bit stereo, 120000 frames (480 KB, several read chunks)
        let n = 120_000;
        let li: Vec<i16> = (0..n)
            .map(|i| ((i as f64 * 0.003).sin() * 30_000.0 * (i as f64 / n as f64)) as i16)
            .collect();
        let ri: Vec<i16> = (0..n)
            .map(|i| if (i / 300) % 2 == 0 { 20_000 } else { -32_768 })
            .collect();
        let mut data = Vec::new();
        for i in 0..n {
            data.extend_from_slice(&li[i].to_le_bytes());
            data.extend_from_slice(&ri[i].to_le_bytes());
        }
        assert!(data.len() > 4 * WAV_READ_CHUNK);
        let p = wav_file("s16.wav", 1, 16, 2, 44_100, &data, None);
        let l: Vec<f32> = li.iter().map(|v| *v as f32 / 32_768.0).collect();
        let r: Vec<f32> = ri.iter().map(|v| *v as f32 / 32_768.0).collect();
        for lanes in [Lanes::Mono, Lanes::Stereo, Lanes::Channels] {
            let st = WaveStyle {
                width: 300,
                height: 120,
                lanes,
                rms: true,
                ..WaveStyle::default()
            };
            let (png, info) = wav_file_png(&p, &st, Compression::Fixed, None).unwrap();
            assert_eq!(
                (info.frames, info.drawn, info.channels, info.sample_rate),
                (n, n, 2, 44_100)
            );
            assert_eq!(
                decode(&png).2,
                render_rgb(&l, &r, &st),
                "{lanes:?}: disk scan == buffer path"
            );
        }
        // a seconds window: the second half only
        let st = WaveStyle {
            width: 100,
            height: 60,
            ..WaveStyle::default()
        };
        let half = n / 2;
        let (png, info) = wav_file_png(
            &p,
            &st,
            Compression::Stored,
            Some(((half as f64 + 0.5) / 44_100.0, 1e9)),
        )
        .unwrap();
        assert_eq!((info.start_frame, info.drawn), (half, n - half));
        assert_eq!(decode(&png).2, render_rgb(&l[half..], &r[half..], &st));
        // a writer that left the data size open (0xFFFFFFFF) reads to the end of the file
        let p2 = wav_file("open.wav", 1, 16, 2, 44_100, &data, Some(u32::MAX));
        let (png2, info2) = wav_file_png(&p2, &st, Compression::Fixed, None).unwrap();
        assert_eq!(info2.frames, n);
        assert_eq!(decode(&png2).2, render_rgb(&l, &r, &st));
    }

    #[test]
    fn wav_encodings_and_multichannel_lanes() {
        // 4-channel 32-bit float: square, silence, +0.5, -1.0 (with a value past full scale)
        let n = 2000;
        let mut data = Vec::new();
        for i in 0..n {
            let sq = if (i / 10) % 2 == 0 { 1.0f32 } else { -1.5 };
            for v in [sq, 0.0, 0.5, -1.0] {
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        let p = wav_file("f32x4.wav", 3, 32, 4, 48_000, &data, None);
        let st = WaveStyle {
            width: 20,
            height: 100,
            lanes: Lanes::Channels,
            rms: false,
            background: BG,
            wave: WAVE,
            rms_color: RMS,
            center: Some(MID),
        };
        let (png, info) = wav_file_png(&p, &st, Compression::Fixed, None).unwrap();
        assert_eq!((info.channels, info.float, info.bits), (4, true, 32));
        // lanes of 25 rows: row = lane_top + round(12 * (1 - v))
        let expect = image(20, 100, |_, y| match y {
            0..=24 => WAVE,
            37 => WAVE,
            56 => WAVE,
            99 => WAVE,
            62 | 87 => MID,
            _ => BG,
        });
        assert_eq!(decode(&png).2, expect);
        // the same file downmixed to mono (mean before clamping): (1 + 0 + 0.5 - 1) / 4 = 0.125, (-1.5 + 0 + 0.5 - 1) / 4 = -0.5
        let mono = WaveStyle {
            lanes: Lanes::Mono,
            height: 101,
            ..st.clone()
        };
        let (png, _) = wav_file_png(&p, &mono, Compression::Fixed, None).unwrap();
        let (top, bot) = (value_row(0.125, 0, 101), value_row(-0.5, 0, 101));
        assert_eq!((top, bot), (44, 75));
        assert_eq!(
            decode(&png).2,
            image(20, 101, |_, y| if (top..=bot).contains(&y) {
                WAVE
            } else {
                BG
            })
        );
        // 24-bit and 8-bit mono: +0.5 then -0.5, a window picks the second half
        let mut d24 = Vec::new();
        let mut d8 = Vec::new();
        for i in 0..1000 {
            let pos = i < 500;
            let v24: i32 = if pos { 4_194_304 } else { -4_194_304 };
            d24.extend_from_slice(&v24.to_le_bytes()[..3]);
            d8.push(if pos { 192u8 } else { 64u8 });
        }
        let plain = WaveStyle {
            width: 10,
            height: 101,
            lanes: Lanes::Stereo,
            center: None,
            ..st.clone()
        };
        for (name, bits, d) in [("s24.wav", 24u16, d24), ("u8.wav", 8, d8)] {
            let p = wav_file(name, 1, bits, 1, 1000, &d, None);
            let (png, info) =
                wav_file_png(&p, &plain, Compression::Fixed, Some((0.5, 1.0))).unwrap();
            assert_eq!((info.start_frame, info.drawn, info.channels), (500, 500, 1));
            // two lanes of 50 / 51 rows, both showing the mono channel at -0.5
            let (r0, r1) = (value_row(-0.5, 0, 50), value_row(-0.5, 50, 51));
            assert_eq!(
                decode(&png).2,
                image(10, 101, |_, y| if y == r0 || y == r1 { WAVE } else { BG }),
                "{name}"
            );
        }
        // what it cannot read is an error, not a panic
        assert!(wav_file_png(
            &wav_file("adpcm.wav", 2, 4, 1, 8000, &[0u8; 64], None),
            &plain,
            Compression::Fixed,
            None
        )
        .is_err());
        let junk = std::env::temp_dir()
            .join("beatbox_waveform_png_wav")
            .join("junk.wav");
        std::fs::write(&junk, b"RIFF\0\0\0\0WAVEnope").unwrap();
        assert!(wav_file_png(&junk, &plain, Compression::Fixed, None).is_err());
        assert!(wav_file_png(
            std::path::Path::new("/nonexistent/x.wav"),
            &plain,
            Compression::Fixed,
            None
        )
        .is_err());
    }

    #[test]
    fn parses_colours_and_maps_rows() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_color("0A0b0C"), Some([10, 11, 12]));
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(parse_color("zzzzzz"), None);
        assert_eq!(value_row(1.0, 0, 101), 0);
        assert_eq!(value_row(-1.0, 0, 101), 100);
        assert_eq!(value_row(7.0, 10, 101), 10);
        assert_eq!(value_row(f64::NAN, 0, 101), 50);
        assert_eq!(value_row(f64::NEG_INFINITY, 0, 101), 100);
        assert_eq!(column_range(0, 10, 3), (0, 1));
        assert_eq!(column_range(9, 10, 3), (2, 3));
        assert_eq!(column_range(3, 4, 100), (75, 100));
    }
}
