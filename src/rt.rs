//! Real-time engine core: block-based audio graph.
//!
//! Milestone 1 scope: `Node` trait, `Graph` container, offline block driver.
//! Determinism contract: at the default 44.1 kHz rate everything stays f32,
//! and the one-pole smoothing coefficient is shared with the renderer via
//! [`crate::render::auto_smooth_coeff`], so engine and oracle agree bitwise.

/// Default block size in samples. This is a runtime parameter
/// ([`Graph::new`]); this constant is only the approved default.
pub const DEFAULT_BLOCK: usize = 512;

/// Per-block context handed to [`Node::process`].
#[derive(Debug, Clone, Copy)]
pub struct BlockCtx {
    /// Sample rate in Hz. f32 to match the existing DSP convention (dsp.rs).
    pub sample_rate: f32,
    /// Samples in this block. Equals the graph block size except for the
    /// final partial block of a render.
    pub block_len: usize,
    /// Absolute sample offset of the first sample in this block.
    pub frame_offset: u64,
}

/// Stable id assigned when a node joins the graph (RT2: state carry
/// across snapshot swaps matches on this id).
pub type NodeId = u32;

/// Timestamped parameter event (RT2): takes effect at absolute song
/// sample `sample`; the one-pole smoother runs from there.
#[derive(Debug, Clone, Copy)]
pub struct ParamEvent {
    pub sample: u64,
    pub value: f32,
}

/// Capacity of every RT2 audio-thread buffer and ring (approved: 64).
pub const EVENT_CAP: usize = 64;
/// Command-ring capacity (matches the event buffer).
pub const CMD_RING_CAP: usize = 64;
/// Trash-ring capacity: enough for several pending graph swaps.
pub const TRASH_RING_CAP: usize = 4;

/// Fixed-capacity ([`EVENT_CAP`]) audio-thread event buffer: sorted by
/// sample, drop-oldest on overflow (counted in `dropped`), never
/// allocates, never locks.
pub struct FixedEvents {
    buf: [ParamEvent; EVENT_CAP],
    len: usize,
    dropped: u64,
}

impl FixedEvents {
    pub fn new() -> Self {
        Self {
            buf: [ParamEvent {
                sample: 0,
                value: 0.0,
            }; 64],
            len: 0,
            dropped: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Events dropped over the buffer's lifetime (Test-visible stat).
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    /// Insert sorted by sample; on a full buffer the OLDEST event drops.
    pub fn push(&mut self, ev: ParamEvent) {
        if self.len == EVENT_CAP {
            self.buf.copy_within(1.., 0);
            self.len = EVENT_CAP - 1;
            self.dropped += 1;
        }
        let mut i = self.len;
        while i > 0 && self.buf[i - 1].sample > ev.sample {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[i] = ev;
        self.len += 1;
    }
    /// Earliest pending event, if any.
    pub fn peek(&self) -> Option<ParamEvent> {
        (self.len > 0).then(|| self.buf[0])
    }
    /// Drop the earliest pending event.
    pub fn pop(&mut self) {
        if self.len > 0 {
            self.buf.copy_within(1.., 0);
            self.len -= 1;
        }
    }
}

impl Default for FixedEvents {
    fn default() -> Self {
        Self::new()
    }
}

/// A node in the audio graph.
pub trait Node {
    /// Render one block. Every input and output slice has exactly
    /// `ctx.block_len` samples.
    fn process(&mut self, ctx: &BlockCtx, inputs: &[&[f32]], outputs: &mut [&mut [f32]]);

    /// Algorithmic latency introduced by this node, in samples.
    fn latency_samples(&self) -> u32 {
        0
    }

    /// Accept a timestamped parameter event (RT2). Default: ignore.
    /// Implementations must never allocate or lock here.
    fn push_event(&mut self, ev: ParamEvent) {
        let _ = ev;
    }

    /// Events this node has dropped (fixed buffer overflow). Default 0.
    /// RT3 metering reads this next to the xrun counter.
    fn event_drops(&self) -> u64 {
        0
    }
}

/// Serial audio graph rendered in fixed-size blocks.
///
/// Milestone 1 models the signal path as a single mono chain: node *i*
/// receives node *i-1*'s output buffer as its sole input. Scratch buffers
/// are allocated once at build (RT2: no allocation on the audio path).
pub struct Graph {
    block_size: usize,
    nodes: Vec<(NodeId, Box<dyn Node>)>,
    next_id: NodeId,
    front: Vec<f32>,
    back: Vec<f32>,
}

impl Graph {
    /// Create an empty graph rendering in blocks of `block_size` samples.
    pub fn new(block_size: usize) -> Self {
        assert!(block_size > 0, "block size must be non-zero");
        Self {
            block_size,
            nodes: Vec::new(),
            next_id: 0,
            front: vec![0.0; block_size],
            back: vec![0.0; block_size],
        }
    }

    /// Number of samples per block.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Append a node to the end of the chain; returns its stable id.
    pub fn add_node(&mut self, node: Box<dyn Node>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push((id, node));
        id
    }

    /// Number of nodes in the chain.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Access a node by id (RT2: param events target nodes by id).
    pub fn node_mut<'a>(&'a mut self, id: NodeId) -> Option<&'a mut dyn Node> {
        self.nodes.iter_mut().find(|(i, _)| *i == id).map(|(_, n)| {
            let r: &'a mut (dyn Node + 'static) = &mut **n;
            r
        })
    }

    /// Swap in a new node set (RT2 snapshot swap), returning the old nodes
    /// so the CONTROL thread can drop them: the audio thread never
    /// deallocates. Node ids are the swap's state-carry key.
    pub fn swap_nodes(
        &mut self,
        nodes: Vec<(NodeId, Box<dyn Node>)>,
    ) -> Vec<(NodeId, Box<dyn Node>)> {
        std::mem::replace(&mut self.nodes, nodes)
    }

    /// Read-only walk over (id, node) pairs (RT3 metering).
    pub fn iter_nodes(&self) -> impl Iterator<Item = (NodeId, &dyn Node)> {
        self.nodes.iter().map(|(i, n)| (*i, &**n as &dyn Node))
    }

    /// Total latency of the serial chain: the sum of node latencies.
    pub fn latency_samples(&self) -> u64 {
        self.nodes
            .iter()
            .map(|(_, n)| u64::from(n.latency_samples()))
            .sum()
    }

    /// Render one block of the serial chain into `out`. The block length
    /// is `out.len()`; the final partial block of a render is a shortened
    /// block (no padding).
    pub fn process_block(&mut self, frame_offset: u64, sample_rate: f32, out: &mut [f32]) {
        if self.nodes.is_empty() {
            return;
        }
        let block_len = out.len();
        let ctx = BlockCtx {
            sample_rate,
            block_len,
            frame_offset,
        };
        for slot in self.front.iter_mut().take(block_len) {
            *slot = 0.0;
        }
        let mut first = true;
        let mut parity = false; // false: current buffer is `front`
        for (_, node) in &mut self.nodes {
            let (src, dst): (&[f32], &mut [f32]) = if parity {
                (&self.front[..block_len], &mut self.back[..block_len])
            } else {
                (&self.back[..block_len], &mut self.front[..block_len])
            };
            let ins: &[&[f32]] = if first { &[] } else { &[src] };
            let mut outs = [dst];
            node.process(&ctx, ins, &mut outs);
            first = false;
            parity = !parity;
        }
        // The last node wrote into `front` on even turns and `back`
        // on odd turns; `parity` has flipped once more since then.
        let final_buf = if parity { &self.front } else { &self.back };
        out.copy_from_slice(&final_buf[..block_len]);
    }

    /// Drive the chain offline over `out` in blocks of `block_size`
    /// samples; the final partial block is rendered as a shortened block.
    /// With no nodes, `out` is left untouched.
    pub fn render_offline(&mut self, sample_rate: f32, out: &mut [f32]) {
        let mut written = 0usize;
        while written < out.len() {
            let n = self.block_size.min(out.len() - written);
            self.process_block(written as u64, sample_rate, &mut out[written..written + n]);
            written += n;
        }
    }
}

// ---- Engine offline mix render (Test 0 groundwork) ----
//
// MixNode renders a whole project the way the engine will: per-track
// buffers synthesized ONCE at build by render_track_synth (the same code
// mix() runs), then fader lanes, track-order summation and the master
// section (master fader, DcBlock, non-finite zeroing) streamed block by
// block with all state carried across block edges. Reference scope for
// milestone 1: no buses, no sends; track FX run inside render_track_synth.

use crate::automation::{parse_target, AutomationLane, Target};
use crate::dsp::{
    db_to_gain, flush_denormals, gain_to_db, Biquad, BiquadKind, DcBlock, FilterMode, NoDenormals,
    Svf, SR,
};
use crate::fx::Effect;
use crate::project::Project;
use crate::render::{
    auto_smooth_coeff, lane_curve, lane_raw_at, owner_lanes, plan_track_clips, plan_track_voices,
    render_timeline, render_track_synth, schedule, sidechain_inputs, PlannedClip, PlannedVoice,
    RenderOptions, Timeline, TrackSynth,
};
use crate::samples::SampleBank;
use std::f32::consts::PI;

/// One track's pre-fader audio: either the one-shot buffers
/// (render_track_synth - fallback for chains outside the streamable
/// subset) or the chunked path (VoiceStreamer + ChainStreamer) with
/// per-block scratch.
struct StreamedTrack<'a> {
    synth: VoiceStreamer<'a>,
    chain: ChainStreamer,
    bl: Vec<f32>,
    br: Vec<f32>,
}

enum TrackAudio<'a> {
    Buffered { l: Vec<f32>, r: Vec<f32> },
    Streamed(Box<StreamedTrack<'a>>),
}

impl<'a> TrackAudio<'a> {
    /// This block's stereo slices (local indexing from 0). A Buffered
    /// track CLAMPS at the end of its buffer: a caller driving past the
    /// render end (RT4 PDC flush) gets a short slice and treats the rest
    /// as silence. Streamed tracks always return n samples.
    fn block(&self, start: usize, n: usize) -> (&[f32], &[f32]) {
        match self {
            TrackAudio::Buffered { l, r } => {
                let end = (start + n).min(l.len());
                (&l[start..end], &r[start..end])
            }
            TrackAudio::Streamed(s) => (&s.bl[..n], &s.br[..n]),
        }
    }
}

/// One track's contribution: pre-fader stereo audio plus the fader
/// smoother state carried across blocks.
struct TrackStream<'a> {
    audio: TrackAudio<'a>,
    lane: Option<&'a AutomationLane>,
    volume_db: f32,
    y: f32,
    smoothed: bool,
    // RT4 PDC: compensation delay (max latency minus this track's own),
    // preallocated at prepare; comp == 0 -> passthrough.
    comp: usize,
    cdl: Vec<f32>,
    cdr: Vec<f32>,
    cdw: usize,
}

/// Offline engine render of a whole project. Output is bit-identical to
/// render() at every block size (Test 0): the per-sample op sequence
/// mirrors mix() exactly - track fader (lane smoothed, state carried),
/// tracks summed in declaration order, master fader, DcBlock, zeroing.
pub struct MixNode<'a> {
    block_size: usize,
    tl: Timeline,
    tracks: Vec<TrackStream<'a>>,
    master_lane: Option<&'a AutomationLane>,
    master_volume_db: f32,
    master_y: f32,
    master_smoothed: bool,
    dc_l: DcBlock,
    dc_r: DcBlock,
    coeff: f32,
    pos: usize,
    total: usize,
    /// RT4 PDC: max track-chain latency in samples. Every track's output
    /// is delayed (latency_max - own) so all paths align at the sum.
    latency: usize,
}

impl<'a> MixNode<'a> {
    /// Build: synthesize every audible track's pre-fader buffers once,
    /// using the same schedule/timeline/sidechain pre-computation as
    /// render_impl and the shared render_track_synth for the audio.
    pub fn new(
        p: &'a Project,
        bank: &'a SampleBank,
        opts: &RenderOptions,
        block_size: usize,
    ) -> Self {
        assert!(block_size > 0, "block size must be non-zero");
        let ftz = flush_denormals();
        let _no_denormals = ftz.then(NoDenormals::new);
        let (events, body_len) = schedule(p, opts);
        let total = body_len + (opts.tail.max(0.0) * SR) as usize;
        let (triggers, levels) = sidechain_inputs(p, bank, opts, &events, total);
        let (tl, offset) = render_timeline(p, opts);
        let tcurve = crate::tempo_curve::TempoCurve::new(p);
        let ctx = crate::fx::FxContext {
            step_secs: p.step_secs(),
            triggers: &triggers,
            offset,
            levels: Some(&levels),
        };
        let any_solo = p.tracks.iter().any(|t| t.solo);
        let mut tracks = Vec::new();
        for (ti, track) in p.tracks.iter().enumerate() {
            if track.mute || (any_solo && !track.solo) {
                continue;
            }
            let lanes = owner_lanes(p, &track.name);
            // RT3: chunked synth+chain when the whole chain is in the
            // verified subset; one-shot buffers otherwise (fallback list
            // in ChainStreamer::build).
            let audio =
                match ChainStreamer::build(&track.effects, &lanes, p.step_secs(), &triggers, total)
                {
                    Some(chain) => TrackAudio::Streamed(Box::new(StreamedTrack {
                        synth: VoiceStreamer::new(p, bank, opts, block_size, ti),
                        chain,
                        bl: vec![0.0; block_size],
                        br: vec![0.0; block_size],
                    })),
                    None => {
                        let (l, r, _fresh) = render_track_synth(
                            &TrackSynth {
                                p,
                                bank,
                                opts,
                                events: &events,
                                total,
                                tl: &tl,
                                tcurve: &tcurve,
                                ctx: &ctx,
                                index_base: None,
                            },
                            ti,
                            false,
                        );
                        TrackAudio::Buffered { l, r }
                    }
                };
            let lane = lanes
                .iter()
                .find(|x| parse_target(&x.param) == Some(Target::Volume))
                .copied();
            tracks.push(TrackStream {
                audio,
                lane,
                volume_db: track.volume_db,
                y: 0.0,
                smoothed: false,
                comp: 0,
                cdl: Vec::new(),
                cdr: Vec::new(),
                cdw: 0,
            });
        }
        // RT4 PDC: size every compensation line at prepare (never on the
        // audio thread). Latency sources in this engine: streamed chains
        // (lookahead compressor). OneShot/Buffered tracks are rendered
        // offline with no shift, so their latency is 0. This graph has a
        // mono serial node chain with MixNode as the only summing point -
        // no sends/returns or group buses exist to align; sidechain keys
        // are trigger envelopes, not audio paths. The dry leg inside a
        // lookahead effect rides the same delay line as the wet leg.
        let latency = tracks
            .iter()
            .map(|t| match &t.audio {
                TrackAudio::Streamed(s) => s.chain.latency_samples(),
                TrackAudio::Buffered { .. } => 0,
            })
            .max()
            .unwrap_or(0);
        for t in &mut tracks {
            let own = match &t.audio {
                TrackAudio::Streamed(s) => s.chain.latency_samples(),
                TrackAudio::Buffered { .. } => 0,
            };
            t.comp = latency - own;
            t.cdl = vec![0.0; t.comp];
            t.cdr = vec![0.0; t.comp];
        }
        let master_lanes = owner_lanes(p, "master");
        let master_lane = master_lanes
            .iter()
            .find(|x| parse_target(&x.param) == Some(Target::Volume))
            .copied();
        MixNode {
            block_size,
            tl,
            tracks,
            master_lane,
            master_volume_db: p.master_volume_db,
            master_y: 0.0,
            master_smoothed: false,
            dc_l: DcBlock::default(),
            dc_r: DcBlock::default(),
            latency,
            coeff: auto_smooth_coeff(SR),
            pos: 0,
            total,
        }
    }

    /// Total samples this render produces.
    pub fn total_samples(&self) -> usize {
        self.total
    }

    /// RT4 PDC: the mixer latency (max track-chain latency). 0 for
    /// no-lookahead projects: Test 0 stays bit-for-bit unshifted.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Drive the node block by block over the whole project. The final
    /// partial block is rendered as a shortened block (no padding).
    pub fn render(&mut self) -> (Vec<f32>, Vec<f32>) {
        let ftz = flush_denormals();
        let _no_denormals = ftz.then(NoDenormals::new);
        let mut l = vec![0.0f32; self.total];
        let mut r = vec![0.0f32; self.total];
        self.pos = 0;
        while self.pos < self.total {
            let n = self.block_size.min(self.total - self.pos);
            let ctx = BlockCtx {
                sample_rate: SR,
                block_len: n,
                frame_offset: self.pos as u64,
            };
            let mut outs: [&mut [f32]; 2] = [
                &mut l[self.pos..self.pos + n],
                &mut r[self.pos..self.pos + n],
            ];
            self.process(&ctx, &[], &mut outs);
            self.pos += n;
        }
        (l, r)
    }
}

impl<'a> Node for MixNode<'a> {
    fn latency_samples(&self) -> u32 {
        self.latency as u32
    }

    /// Writes one block: outputs[0] = left, outputs[1] = right.
    fn process(&mut self, ctx: &BlockCtx, _inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        assert!(outputs.len() == 2, "MixNode is a stereo terminal node");
        let n = ctx.block_len;
        let start = ctx.frame_offset as usize;
        let tl = &self.tl;
        let coeff = self.coeff;
        let master_lane = self.master_lane;
        let master_k = db_to_gain(self.master_volume_db);
        let mut master_y = self.master_y;
        let mut master_smoothed = self.master_smoothed;
        let (dl, dr) = (&mut self.dc_l, &mut self.dc_r);
        let (ml, mr) = outputs.split_at_mut(1);
        let ml = &mut ml[0];
        let mr = &mut mr[0];
        // chunked tracks: synthesize + chain this block (sequential block
        // order is the engine's contract; pos is carried in the streamer)
        for t in &mut self.tracks {
            if let TrackAudio::Streamed(s) = &mut t.audio {
                // RT4 PDC flush: driven past the render end, the synth is
                // silent but the chain must keep running (lookahead delay
                // lines drain, detector frozen past `total`).
                let sn = if start < self.total {
                    n.min(self.total - start)
                } else {
                    0
                };
                if sn > 0 {
                    s.synth.render_block(&mut s.bl[..sn], &mut s.br[..sn]);
                }
                for x in &mut s.bl[sn..n] {
                    *x = 0.0;
                }
                for x in &mut s.br[sn..n] {
                    *x = 0.0;
                }
                s.chain.process_block(&mut s.bl[..n], &mut s.br[..n]);
            }
        }
        for i in 0..n {
            let a = start + i;
            let mut al = 0.0f32;
            let mut ar = 0.0f32;
            for t in &mut self.tracks {
                let g = match t.lane {
                    Some(lane) => {
                        let raw = lane_raw_at(lane, tl, a, |db| db_to_gain(db.clamp(-120.0, 24.0)));
                        if !t.smoothed {
                            t.y = raw;
                            t.smoothed = true;
                        }
                        t.y += coeff * (raw - t.y);
                        t.y
                    }
                    None => db_to_gain(t.volume_db),
                };
                let (tl_buf, tr_buf) = t.audio.block(start, n);
                let (raw_l, raw_r) = if i < tl_buf.len() {
                    (tl_buf[i], tr_buf[i])
                } else {
                    (0.0, 0.0) // Buffered track clamped past render end
                };
                let (xl, xr) = if t.comp == 0 {
                    (raw_l, raw_r)
                } else {
                    let y = (t.cdl[t.cdw], t.cdr[t.cdw]);
                    t.cdl[t.cdw] = raw_l;
                    t.cdr[t.cdw] = raw_r;
                    t.cdw += 1;
                    if t.cdw == t.comp {
                        t.cdw = 0;
                    }
                    y
                };
                al += xl * g;
                ar += xr * g;
            }
            let gm = match master_lane {
                Some(lane) => {
                    let raw = lane_raw_at(lane, tl, a, |db| db_to_gain(db.clamp(-120.0, 24.0)));
                    if !master_smoothed {
                        master_y = raw;
                        master_smoothed = true;
                    }
                    master_y += coeff * (raw - master_y);
                    master_y
                }
                None => master_k,
            };
            al *= gm;
            ar *= gm;
            let yl = dl.process(al);
            let yr = dr.process(ar);
            ml[i] = if yl.is_finite() { yl } else { 0.0 };
            mr[i] = if yr.is_finite() { yr } else { 0.0 };
        }
        self.master_y = master_y;
        self.master_smoothed = master_smoothed;
    }
}

// ---------------------------------------------------------------------------
// RT3: chunked voice-level track synthesis (VoiceStreamer)
//
// The one-shot renderer builds a track's pre-chain audio by summing whole
// note buffers into a song-length mono pair (render_track_prechain). The
// streamer below produces the SAME audio block by block from the shared
// planners (plan_track_voices / plan_track_clips):
//   - voice state (play position, fade/choke parameters computed once at
//     activation with add_voice_faded's own arithmetic) carries across
//     block boundaries;
//   - activations land sample-accurately inside a block (voices/clips
//     activate when their start falls before the block's end);
//   - per-sample float addition order is the one-shot order: voices by
//     onset, then clips by (clip, loop) - so output is bit-identical to
//     render_track_prechain at every block size;
//   - render_block performs ZERO allocations: active lists are
//     pre-reserved to the full plan sizes, Arc clones and Vec pushes into
//     reserved capacity do not allocate, and retain() does not allocate.
// Pan curves are whole-song precomputed at build (lane_curve, same call
// as the one-shot), so no lane state crosses blocks.
// ---------------------------------------------------------------------------

/// Stereo pan gains (equal power), matching the one-shot `gains` closure.
#[derive(Clone, Copy)]
struct PanGains {
    gl: f32,
    gr: f32,
}

fn pan_gains(pan: f32) -> PanGains {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    PanGains {
        gl: angle.cos() * std::f32::consts::SQRT_2,
        gr: angle.sin() * std::f32::consts::SQRT_2,
    }
}

struct ActiveVoice {
    audio: std::sync::Arc<(Vec<f32>, Vec<f32>)>,
    start: usize,
    /// Effective length after track-end clipping and choke truncation
    /// (add_voice_faded's `n`).
    n: usize,
    choke_from: usize,
    end_fade: usize,
    start_fade: usize,
    xfade_in: bool,
}

impl ActiveVoice {
    /// add_voice_faded's activation arithmetic, verbatim.
    fn activate(v: &PlannedVoice, total: usize) -> Self {
        let fade = (crate::render::CHOKE_FADE_S * SR) as usize;
        let mut n = v.audio.0.len().min(total - v.start);
        let mut choke_from = usize::MAX;
        if let Some(c) = v.choke_after {
            if c + fade < n {
                n = c + fade;
                choke_from = c;
            }
        }
        let end_fade = crate::render::END_FADE.min(n / 8).max(1);
        let start_fade = v.start_fade.min(n / 8);
        ActiveVoice {
            audio: v.audio.clone(),
            start: v.start,
            n,
            choke_from,
            end_fade,
            start_fade,
            xfade_in: v.xfade_in,
        }
    }

    /// add_voice_faded's per-sample gain, verbatim.
    #[inline]
    fn gain(&self, j: usize) -> f32 {
        let fade = (crate::render::CHOKE_FADE_S * SR) as usize;
        let mut g = 1.0f32;
        if self.xfade_in {
            if j < fade {
                let x = j as f32 / fade as f32;
                g *= 0.5 - 0.5 * (std::f32::consts::PI * x).cos();
            }
        } else if j < self.start_fade {
            g *= (j as f32 + 0.5) / self.start_fade as f32;
        }
        let left = self.n - j;
        if left <= self.end_fade {
            g *= (left as f32 - 0.5).max(0.0) / self.end_fade as f32;
        }
        if j >= self.choke_from {
            let x = (j - self.choke_from) as f32 / fade as f32;
            g *= 0.5 + 0.5 * (std::f32::consts::PI * x.min(1.0)).cos();
        }
        g
    }
}

struct ActiveClip<'a> {
    start: i64,
    data: &'a [f32],
    off: usize,
    len: usize,
    gain: f32,
    fade_in_ms: Option<f32>,
    fade_out_ms: Option<f32>,
    /// The envelope fade constant (0.004 s), captured once.
    fade: usize,
}

/// Chunked, bit-identical re-issue of render_track_prechain: same voices,
/// clips, pan and spread, produced block by block. See the design comment
/// above for the invariants. #[doc(hidden)] pub only so the integration
/// no-alloc test can drive render_block directly.
#[doc(hidden)]
pub struct VoiceStreamer<'a> {
    voices: Vec<PlannedVoice>,
    clips: Vec<PlannedClip<'a>>,
    spread: f32,
    pan_curve: Option<Vec<f32>>,
    static_gains: PanGains,
    total: usize,
    block: usize,
    pos: usize,
    next_voice: usize,
    next_clip: usize,
    active_v: Vec<ActiveVoice>,
    active_c: Vec<ActiveClip<'a>>,
    mono: Vec<f32>,
    mono2: Vec<f32>,
}

impl<'a> VoiceStreamer<'a> {
    pub fn new(
        p: &'a Project,
        bank: &'a SampleBank,
        opts: &RenderOptions,
        block_size: usize,
        ti: usize,
    ) -> Self {
        assert!(block_size > 0, "block size must be non-zero");
        let (events, body_len) = schedule(p, opts);
        let total = body_len + (opts.tail.max(0.0) * SR) as usize;
        let (tl, _offset) = render_timeline(p, opts);
        let tcurve = crate::tempo_curve::TempoCurve::new(p);
        let voices = plan_track_voices(p, bank, &events, total, &tl, None, ti);
        let clips = plan_track_clips(p, bank, opts, &tcurve, ti);
        let track = &p.tracks[ti];
        let spread = track.instrument.stereo_spread().clamp(0.0, 1.0);
        let lanes = owner_lanes(p, &track.name);
        let pan_lane = lanes.iter().find(|x| {
            crate::automation::parse_target(&x.param) == Some(crate::automation::Target::Pan)
        });
        let pan_curve = pan_lane.map(|lane| lane_curve(lane, total, &tl, |v| v.clamp(-1.0, 1.0)));
        let nv = voices.len();
        let nc = clips.len();
        VoiceStreamer {
            voices,
            clips,
            spread,
            pan_curve,
            static_gains: pan_gains(track.pan),
            total,
            block: block_size,
            pos: 0,
            next_voice: 0,
            next_clip: 0,
            active_v: Vec::with_capacity(nv),
            active_c: Vec::with_capacity(nc),
            mono: vec![0.0; block_size],
            mono2: vec![0.0; block_size],
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn done(&self) -> bool {
        self.pos >= self.total
    }

    /// Render the next block into (l, r) (truncated at the song end).
    /// Allocates nothing.
    pub fn render_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.block.min(self.total - self.pos);
        let b0 = self.pos;
        let bend = b0 + n;
        for x in self.mono[..n].iter_mut() {
            *x = 0.0;
        }
        for x in self.mono2[..n].iter_mut() {
            *x = 0.0;
        }
        // activate everything starting before this block's end
        while self.next_voice < self.voices.len() && self.voices[self.next_voice].start < bend {
            self.active_v.push(ActiveVoice::activate(
                &self.voices[self.next_voice],
                self.total,
            ));
            self.next_voice += 1;
        }
        while self.next_clip < self.clips.len() && self.clips[self.next_clip].start < bend as i64 {
            let c = &self.clips[self.next_clip];
            self.active_c.push(ActiveClip {
                start: c.start,
                data: c.data,
                off: c.off,
                len: c.len,
                gain: c.gain,
                fade_in_ms: c.fade_in_ms,
                fade_out_ms: c.fade_out_ms,
                fade: (0.004 * SR) as usize,
            });
            self.next_clip += 1;
        }
        let spread = self.spread;
        let two = spread > 0.0;
        for i in 0..n {
            let s = b0 + i;
            let mut m = 0.0f32;
            let mut m2 = 0.0f32;
            for av in &self.active_v {
                if s >= av.start && s < av.start + av.n {
                    let j = s - av.start;
                    let g = av.gain(j);
                    m += av.audio.0[j] * g;
                    if two {
                        m2 += av.audio.1[j] * g;
                    }
                }
            }
            for ac in &self.active_c {
                let rel = s as i64 - ac.start;
                if rel >= 0 && (rel as usize) < ac.len {
                    let i = rel as usize;
                    let env = if ac.fade_in_ms.is_some() || ac.fade_out_ms.is_some() {
                        let fi = (ac.fade_in_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
                        let fo = (ac.fade_out_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
                        let a = (i as f32 / fi).min(1.0);
                        let b = ((ac.len - i) as f32 / fo).min(1.0);
                        (a * std::f32::consts::FRAC_PI_2).sin()
                            * (b * std::f32::consts::FRAC_PI_2).sin()
                    } else {
                        (i as f32 / ac.fade as f32)
                            .min((ac.len - i) as f32 / ac.fade as f32)
                            .min(1.0)
                    };
                    let v = ac.data[ac.off + i] * ac.gain * env;
                    m += v;
                    if two {
                        m2 += v;
                    }
                }
            }
            let g = match &self.pan_curve {
                Some(c) => pan_gains(c[s]),
                None => self.static_gains,
            };
            let xr = if two {
                m * (1.0 - spread) + m2 * spread
            } else {
                m
            };
            l[i] = m * g.gl;
            r[i] = xr * g.gr;
        }
        // retire finished sources (retain never allocates)
        self.active_v.retain(|av| av.start + av.n > bend);
        self.active_c
            .retain(|ac| ac.start + ac.len as i64 > bend as i64);
        self.pos = bend;
    }

    /// Convenience: render the whole track in `block`-sized chunks.
    pub fn render_all(&mut self) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; self.total];
        let mut r = vec![0.0; self.total];
        while !self.done() {
            let n = self.block.min(self.total - self.pos);
            let start = self.pos;
            self.render_block(&mut l[start..start + n], &mut r[start..start + n]);
        }
        (l, r)
    }
}

// ---------------------------------------------------------------------------
// RT3: streaming FX chain (verified sample-sequential subset)
//
// Every StreamFx variant carries exactly the locals of the corresponding
// Effect::process arm, moved verbatim, so processing a track block by
// block is bit-identical to the one-shot whole-buffer pass. ChainStreamer
// is built per track; any effect outside the subset (lookahead, multi-
// pass, window-relative, FDN reverb, the sc_dsp compressor arm), any
// FX-targeted automation lane, or any bypass handling other than "skip"
// makes build() return None and the track stays on the one-shot path
// (render_track_synth at build, as before). The per-effect bit-identity
// tests (blocks {1, 63, 512} vs the one-shot arm) gate every variant.
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub enum StreamFx {
    Filter {
        fl: Svf,
        fr: Svf,
        cutoff: f32,
        resonance: f32,
        mode: FilterMode,
    },
    Eq {
        lr: [(Svf, Svf); 2],
        low_freq: f32,
        high_freq: f32,
        gl: f32,
        gm: f32,
        gh: f32,
    },
    Distortion {
        d: f32,
        norm: f32,
        drive: f32,
        mix: f32,
    },
    Bitcrush {
        levels: f32,
        ds: usize,
        mix: f32,
        held_l: f32,
        held_r: f32,
    },
    Delay {
        bl: Vec<f32>,
        br: Vec<f32>,
        tl: Svf,
        tr: Svf,
        k: usize,
        feedback: f32,
        tone: f32,
        mix: f32,
        ping_pong: bool,
    },
    Chorus {
        bl: Vec<f32>,
        br: Vec<f32>,
        max: usize,
        depth_ms: f32,
        rate_hz: f32,
        mix: f32,
    },
    Width {
        hp: Svf,
        mono_lo: f32,
        amount: f32,
    },
    Gain {
        g: f32,
    },
    CompressorSimple {
        env: f32,
        att: f32,
        rel: f32,
        makeup: f32,
        threshold_db: f32,
        ratio: f32,
    },
    /// Soft-knee/lookahead compressor (RT4): the sc_dsp arm chunked.
    /// The detector reads the undelayed input; the wet path (including
    /// the dry leg of `mix`) is delayed `la` samples through preallocated
    /// lines, so the lookahead introduces exactly `la` samples of latency.
    CompressorLa {
        sc: Option<[Biquad; 2]>,
        att: f32,
        rel: f32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        makeup: f32,
        mix: f32,
        gr: f32,
        frozen: f32,
        dl: Vec<f32>,
        dr: Vec<f32>,
        dw: usize,
        la: usize,
        total: usize,
        in_pos: usize,
    },
    Sidechain {
        trig: std::sync::Arc<Vec<usize>>,
        ti: usize,
        last: Option<usize>,
        from: f32,
        prev: f32,
        rel_s: f32,
        att_n: usize,
        amount: f32,
    },
    ReverbFreeverb {
        cl: Vec<crate::fx::Comb>,
        cr: Vec<crate::fx::Comb>,
        al: Vec<crate::fx::Allpass>,
        ar: Vec<crate::fx::Allpass>,
        pbuf: Vec<f32>,
        k: usize,
        fb: f32,
        damp: f32,
        wet1: f32,
        wet2: f32,
        dry: f32,
    },
    ParametricEq {
        /// Per channel: the (band, stage) filters in the one-shot's
        /// application order.
        chans: [Vec<Biquad>; 2],
        out: f32,
    },
}

impl StreamFx {
    /// Algorithmic latency in samples (RT4): only the lookahead path
    /// delays the signal; every other arm is zero-latency.
    pub(crate) fn latency(&self) -> usize {
        match self {
            StreamFx::CompressorLa { la, .. } => *la,
            _ => 0,
        }
    }

    /// The effect's per-sample math, verbatim from its Effect::process
    /// arm; `pos` is the song-sample index of l[0]/r[0].
    pub(crate) fn process_block(&mut self, l: &mut [f32], r: &mut [f32], pos: usize) {
        match self {
            StreamFx::Filter {
                fl,
                fr,
                cutoff,
                resonance,
                mode,
            } => {
                for i in 0..l.len() {
                    l[i] = fl.process(l[i], *cutoff, *resonance, *mode);
                    r[i] = fr.process(r[i], *cutoff, *resonance, *mode);
                }
            }
            StreamFx::Eq {
                lr,
                low_freq,
                high_freq,
                gl,
                gm,
                gh,
            } => {
                for (chi, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let (lo, hi) = &mut lr[chi];
                    for s in ch.iter_mut() {
                        let low = lo.process(*s, *low_freq, 0.0, FilterMode::Lowpass);
                        let high = hi.process(*s, *high_freq, 0.0, FilterMode::Highpass);
                        let mid = *s - low - high;
                        *s = low * *gl + mid * *gm + high * *gh;
                    }
                }
            }
            StreamFx::Distortion {
                d,
                norm,
                drive,
                mix,
            } => {
                for s in l.iter_mut().chain(r.iter_mut()) {
                    let wet = (*s * *d).tanh() * *norm * (1.0 / (1.0 + *drive * 2.0)).max(0.35);
                    *s = *s * (1.0 - *mix) + wet * *mix;
                }
            }
            StreamFx::Bitcrush {
                levels,
                ds,
                mix,
                held_l,
                held_r,
            } => {
                for (chi, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let held = if chi == 0 { &mut *held_l } else { &mut *held_r };
                    for (i, s) in ch.iter_mut().enumerate() {
                        // grabs on the song's grid, not the buffer's
                        if (i + pos) % *ds == 0 {
                            *held = (*s * *levels).round() / *levels;
                        }
                        *s = *s * (1.0 - *mix) + *held * *mix;
                    }
                }
            }
            StreamFx::Delay {
                bl,
                br,
                tl,
                tr,
                k,
                feedback,
                tone,
                mix,
                ping_pong,
            } => {
                let d = bl.len();
                let fb = *feedback;
                for i in 0..l.len() {
                    let kk = *k;
                    let (dl, dr) = (bl[kk], br[kk]);
                    let (inl, inr) = (l[i], r[i]);
                    if *ping_pong {
                        let mono = 0.5 * (inl + inr);
                        bl[kk] = tl.process(mono + dr * fb, *tone, 0.0, FilterMode::Lowpass);
                        br[kk] = tr.process(dl * fb, *tone, 0.0, FilterMode::Lowpass);
                    } else {
                        bl[kk] = tl.process(inl + dl * fb, *tone, 0.0, FilterMode::Lowpass);
                        br[kk] = tr.process(inr + dr * fb, *tone, 0.0, FilterMode::Lowpass);
                    }
                    l[i] = inl + dl * *mix;
                    r[i] = inr + dr * *mix;
                    *k = (kk + 1) % d;
                }
            }
            StreamFx::Chorus {
                bl,
                br,
                max,
                depth_ms,
                rate_hz,
                mix,
            } => {
                let max = *max;
                for i in 0..l.len() {
                    // song position: the phase and the ring index match a
                    // full render's at the same sample
                    let k = i + pos;
                    let w = k % max;
                    bl[w] = l[i];
                    br[w] = r[i];
                    let t = k as f32 / SR;
                    let base = 12.0;
                    let dl = (base + *depth_ms * (0.5 + 0.5 * (2.0 * PI * *rate_hz * t).sin()))
                        * 0.001
                        * SR;
                    let dr = (base
                        + *depth_ms * (0.5 + 0.5 * (2.0 * PI * *rate_hz * t + PI / 2.0).cos()))
                        * 0.001
                        * SR;
                    let rd = |buf: &[f32], d: f32| {
                        let pos = (w as f32 - d).rem_euclid(max as f32);
                        let i0 = pos as usize % max;
                        let i1 = (i0 + 1) % max;
                        let f = pos - pos.floor();
                        buf[i0] * (1.0 - f) + buf[i1] * f
                    };
                    let (wl, wr) = (rd(bl, dl), rd(br, dr));
                    l[i] = l[i] * (1.0 - *mix * 0.5) + wl * *mix;
                    r[i] = r[i] * (1.0 - *mix * 0.5) + wr * *mix;
                }
            }
            StreamFx::Width {
                hp,
                mono_lo,
                amount,
            } => {
                for i in 0..l.len() {
                    let m = 0.5 * (l[i] + r[i]);
                    let mut s = 0.5 * (l[i] - r[i]);
                    if *mono_lo > 0.0 {
                        s = hp.process(s, *mono_lo, 0.0, FilterMode::Highpass);
                    }
                    let s = s * *amount;
                    l[i] = m + s;
                    r[i] = m - s;
                }
            }
            StreamFx::Gain { g } => {
                for s in l.iter_mut().chain(r.iter_mut()) {
                    *s *= *g;
                }
            }
            StreamFx::CompressorSimple {
                env,
                att,
                rel,
                makeup,
                threshold_db,
                ratio,
            } => {
                for i in 0..l.len() {
                    let x = l[i].abs().max(r[i].abs());
                    let c = if x > *env { *att } else { *rel };
                    *env = c * *env + (1.0 - c) * x;
                    let db = gain_to_db(*env);
                    let over = db - *threshold_db;
                    let gr = if over > 0.0 {
                        over * (1.0 - 1.0 / *ratio)
                    } else {
                        0.0
                    };
                    let g = db_to_gain(-gr) * *makeup;
                    l[i] *= g;
                    r[i] *= g;
                }
            }
            StreamFx::CompressorLa {
                sc,
                att,
                rel,
                threshold_db,
                ratio,
                knee_db,
                makeup,
                mix,
                gr,
                frozen,
                dl,
                dr,
                dw,
                la,
                total,
                in_pos,
            } => {
                // sc_dsp::compress per sample: detector on the UNDELAYED
                // input (hpf, peak, soft-knee gr, attack/release
                // ballistics), gain applied to the sample that entered
                // the delay line `la` steps ago. Offline clamps the
                // lookahead read at the last sample; here the detector
                // freezes once the input passes `total`.
                for i in 0..l.len() {
                    if *in_pos < *total {
                        let (a, b) = match sc.as_mut() {
                            Some(f) => (f[0].process(l[i]), f[1].process(r[i])),
                            None => (l[i], r[i]),
                        };
                        let level = a.abs().max(b.abs());
                        let target = crate::sc_dsp::compress_gr(
                            gain_to_db(level),
                            *threshold_db,
                            *ratio,
                            *knee_db,
                        );
                        let c = if target > *gr { *att } else { *rel };
                        *gr = target + (*gr - target) * c;
                        if !(*gr).is_finite() {
                            *gr = 0.0;
                        }
                        *frozen = db_to_gain(-*gr);
                    }
                    let g = *frozen;
                    let (x_l, x_r) = if *la == 0 {
                        (l[i], r[i])
                    } else {
                        let y = (dl[*dw], dr[*dw]);
                        dl[*dw] = l[i];
                        dr[*dw] = r[i];
                        *dw += 1;
                        if *dw == *la {
                            *dw = 0;
                        }
                        y
                    };
                    let g = g * *makeup;
                    let k = (1.0 - *mix) + g * *mix;
                    l[i] = x_l * k;
                    r[i] = x_r * k;
                    *in_pos += 1;
                }
            }
            StreamFx::Sidechain {
                trig,
                ti,
                last,
                from,
                prev,
                rel_s,
                att_n,
                amount,
            } => {
                for i in 0..l.len() {
                    let i = pos + i;
                    while *ti < trig.len() && trig[*ti] <= i {
                        if *last != Some(trig[*ti]) {
                            *from = *prev;
                        }
                        *last = Some(trig[*ti]);
                        *ti += 1;
                    }
                    if let Some(t0) = *last {
                        let dt = i - t0;
                        let env = if dt < *att_n {
                            *from + (1.0 - *from) * dt as f32 / (*att_n).max(1) as f32
                        } else {
                            let x = (dt - *att_n) as f32 / SR / *rel_s;
                            if x >= 1.0 {
                                0.0
                            } else {
                                (1.0 - x).powf(1.6)
                            }
                        };
                        // env is "how ducked": rises quickly to 1, then recovers
                        let duck = env;
                        *prev = duck;
                        let g = 1.0 - *amount * duck;
                        l[i - pos] *= g;
                        r[i - pos] *= g;
                    }
                }
            }
            StreamFx::ReverbFreeverb {
                cl,
                cr,
                al,
                ar,
                pbuf,
                k,
                fb,
                damp,
                wet1,
                wet2,
                dry,
            } => {
                let pre = pbuf.len();
                for i in 0..l.len() {
                    let input = (l[i] + r[i]) * 0.015;
                    let kk = *k;
                    let x = pbuf[kk];
                    pbuf[kk] = input;
                    let mut ol = 0.0;
                    let mut orr = 0.0;
                    for c in cl.iter_mut() {
                        ol += c.process(x, *fb, *damp);
                    }
                    for c in cr.iter_mut() {
                        orr += c.process(x, *fb, *damp);
                    }
                    for a in al.iter_mut() {
                        ol = a.process(ol);
                    }
                    for a in ar.iter_mut() {
                        orr = a.process(orr);
                    }
                    let (nl, nr) = (
                        l[i] * *dry + ol * *wet1 + orr * *wet2,
                        r[i] * *dry + orr * *wet1 + ol * *wet2,
                    );
                    l[i] = nl;
                    r[i] = nr;
                    *k = (kk + 1) % pre;
                }
            }
            StreamFx::ParametricEq { chans, out } => {
                for (chi, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    for f in chans[chi].iter_mut() {
                        for s in ch.iter_mut() {
                            *s = f.process(*s);
                        }
                    }
                    if *out != 1.0 {
                        for s in ch.iter_mut() {
                            *s *= *out;
                        }
                    }
                }
            }
        }
    }
}

/// RT3 relay item 1: exhaustive classification of EVERY effect kind as
/// Streamable (block-chunked, bit-identical, verified) or OneShot (whole-
/// buffer at prepare time). No wildcard: adding an effect kind to the
/// enum breaks this match until it is classified. Reasons feed the final
/// report; latencies feed RT4 PDC.
#[doc(hidden)]
pub enum FxClass {
    Streamable,
    OneShot(&'static str),
}

#[doc(hidden)]
pub fn classify(fx: &Effect) -> FxClass {
    use FxClass::*;
    match fx {
        // verified sample-sequential, state carried (per-effect tests):
        Effect::Filter(_) => Streamable,
        Effect::Eq(_) => Streamable,
        Effect::Distortion(_) => Streamable,
        Effect::Bitcrush(_) => Streamable,
        Effect::Delay(_) => Streamable,
        Effect::Chorus(_) => Streamable,
        Effect::Width(_) => Streamable,
        Effect::Gain(_) => Streamable,
        Effect::Sidechain(_) => Streamable,
        Effect::ParametricEq(_) => Streamable,
        Effect::Compressor(p)
            if p.knee_db <= 0.0 && p.lookahead_ms <= 0.0 && p.sc_hpf_hz <= 0.0 && p.mix >= 1.0 =>
        {
            Streamable
        }
        Effect::Reverb(p) if p.mode == crate::fx::ReverbMode::Freeverb => Streamable,
        // OneShot, one-line reason each (latency in samples for RT4 PDC
        // comes from oneshot_latency_samples):
        Effect::Compressor(_) => Streamable,
        Effect::Reverb(_) => OneShot("FDN reverb modes unverified for chunking"),
        Effect::Limiter(_) => OneShot("brickwall limiter: multi-pass / true-peak re-reads"),
        Effect::Maximizer(_) => OneShot("sc_dsp maximizer: unverified, likely lookahead"),
        Effect::Convolution(_) => OneShot("IR convolution: block processing"),
        Effect::CabIr(_) => OneShot("cabinet IR: block processing"),
        Effect::PitchShift(_) => OneShot("pitch shift: time-windowed"),
        Effect::GrossBeat(_) => OneShot("gross beat: time-windowed"),
        Effect::Stutter(_) => OneShot("stutter: time-windowed"),
        Effect::Vocoder(_) => OneShot("vocoder: windowed analysis"),
        Effect::Multiband(_) => OneShot("multiband: unverified"),
        Effect::DynamicEq(_) => OneShot("dynamic EQ: unverified / cross-track keyed"),
        Effect::Deesser(_) => OneShot("deesser: unverified"),
        Effect::SoftClipper(_) => OneShot("soft clipper: unverified"),
        Effect::Gate(_) => OneShot("gate: unverified"),
        Effect::Transient(_) => OneShot("transient shaper: unverified"),
        Effect::Phaser(_) => OneShot("phaser: unverified"),
        Effect::Flanger(_) => OneShot("flanger: unverified"),
        Effect::Autopan(_) => OneShot("autopan: unverified"),
        Effect::Saturator(_) => OneShot("saturator: unverified (ADAA state carry pending)"),
        Effect::Haas(_) => OneShot("haas: unverified"),
        Effect::FreqShift(_) => OneShot("freq shift: unverified"),
        Effect::RingMod(_) => OneShot("ring mod: unverified"),
        Effect::StereoShaper(_) => OneShot("stereo shaper: unverified"),
        Effect::Vinyl(_) => OneShot("vinyl: unverified"),
        Effect::Waveshaper(_) => OneShot("waveshaper: unverified"),
        Effect::AmpSim(_) => OneShot("amp sim: unverified"),
        Effect::ModDelay(_) => OneShot("mod delay: unverified"),
        Effect::Lofi(_) => OneShot("lofi: unverified"),
        Effect::Rectifier(_) => OneShot("rectifier: unverified"),
    }
}

/// Latency introduced by a OneShot effect, in samples, when known from
/// its parameters, at the given sample rate. None = unmeasured (RT4
/// treats as unknown, not zero). Derived from `sr`, never hardcoded:
/// 10ms is 441 samples at 44.1k, 480 at 48k, 960 at 96k.
#[doc(hidden)]
pub fn oneshot_latency_samples(fx: &Effect, sr: f32) -> Option<usize> {
    match fx {
        Effect::Compressor(p) if p.lookahead_ms > 0.0 => {
            // exact match with sc_dsp::compress: clamp 0..=10ms, then round
            Some((p.lookahead_ms.clamp(0.0, 10.0) * 0.001 * sr).round() as usize)
        }
        _ => None,
    }
}

/// One track's FX chain as a block-streamed processor, or None when the
/// chain leaves the verified subset (see the design comment).
#[doc(hidden)]
pub struct ChainStreamer {
    fx: Vec<StreamFx>,
    pos: usize,
}

impl ChainStreamer {
    pub fn build(
        effects: &[Effect],
        lanes: &[&AutomationLane],
        step_secs: f32,
        triggers: &std::collections::HashMap<String, Vec<usize>>,
        total: usize,
    ) -> Option<ChainStreamer> {
        // FX automation takes the multi-combo one-shot path: not streamed.
        let automated = lanes.iter().any(|l| {
            matches!(
                crate::automation::parse_target(&l.param),
                Some(crate::automation::Target::Fx(_, _))
            )
        });
        if automated {
            return None;
        }
        let mut fx = Vec::new();
        for e in effects {
            if e.bypassed() {
                continue; // the one-shot returns early: a no-op either way
            }
            match classify(e) {
                FxClass::OneShot(_) => return None,
                FxClass::Streamable => {}
            }
            fx.push(match e {
                Effect::Filter(p) => StreamFx::Filter {
                    fl: Svf::default(),
                    fr: Svf::default(),
                    cutoff: p.cutoff,
                    resonance: p.resonance,
                    mode: p.mode,
                },
                Effect::Eq(p) => StreamFx::Eq {
                    lr: [(Svf::default(), Svf::default()); 2],
                    low_freq: p.low_freq,
                    high_freq: p.high_freq,
                    gl: db_to_gain(p.low_db),
                    gm: db_to_gain(p.mid_db),
                    gh: db_to_gain(p.high_db),
                },
                Effect::Distortion(p) => StreamFx::Distortion {
                    d: 1.0 + p.drive.clamp(0.0, 1.0) * 20.0,
                    norm: 1.0 / (1.0 + p.drive.clamp(0.0, 1.0) * 20.0).tanh(),
                    drive: p.drive,
                    mix: p.mix,
                },
                Effect::Bitcrush(p) => StreamFx::Bitcrush {
                    levels: 2f32.powf(p.bits.clamp(1.0, 16.0)),
                    ds: p.downsample.clamp(1, 64) as usize,
                    mix: p.mix,
                    held_l: 0.0,
                    held_r: 0.0,
                },
                Effect::Delay(p) => {
                    let d = ((p.steps * step_secs * SR) as usize).max(1);
                    StreamFx::Delay {
                        bl: vec![0.0f32; d],
                        br: vec![0.0f32; d],
                        tl: Svf::default(),
                        tr: Svf::default(),
                        k: 0,
                        feedback: p.feedback.clamp(0.0, 0.95),
                        tone: p.tone,
                        mix: p.mix,
                        ping_pong: p.ping_pong,
                    }
                }
                Effect::Chorus(p) => {
                    let max = ((p.depth_ms + 20.0) * 0.001 * SR) as usize + 4;
                    StreamFx::Chorus {
                        bl: vec![0.0f32; max],
                        br: vec![0.0f32; max],
                        max,
                        depth_ms: p.depth_ms,
                        rate_hz: p.rate_hz,
                        mix: p.mix,
                    }
                }
                Effect::Width(p) => StreamFx::Width {
                    hp: Svf::default(),
                    mono_lo: p.low_mono_hz.clamp(0.0, 500.0),
                    amount: p.amount.clamp(0.0, 3.0),
                },
                Effect::Gain(p) => StreamFx::Gain {
                    g: db_to_gain(p.db),
                },
                Effect::Compressor(p)
                    if p.knee_db <= 0.0
                        && p.lookahead_ms <= 0.0
                        && p.sc_hpf_hz <= 0.0
                        && p.mix >= 1.0 =>
                {
                    StreamFx::CompressorSimple {
                        env: 0.0,
                        att: (-1.0 / (p.attack_ms.max(0.1) * 0.001 * SR)).exp(),
                        rel: (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp(),
                        makeup: db_to_gain(p.makeup_db),
                        threshold_db: p.threshold_db,
                        ratio: p.ratio.max(1.0),
                    }
                }
                Effect::Compressor(p) => {
                    // sc_dsp::compress, chunked. Detector coefficients are
                    // time_coef(ms) verbatim: n = ms*0.001*SR, 0 when
                    // n < 1e-3, else exp(-1/n).
                    let tc = |ms_: f32| {
                        let n = ms_ * 0.001 * SR;
                        if n < 1.0e-3 {
                            0.0
                        } else {
                            (-1.0 / n).exp()
                        }
                    };
                    let la = (p.lookahead_ms.clamp(0.0, 10.0) * 0.001 * SR).round() as usize;
                    StreamFx::CompressorLa {
                        sc: if p.sc_hpf_hz > 0.0 {
                            Some([
                                Biquad::new(
                                    BiquadKind::LowCut,
                                    p.sc_hpf_hz,
                                    std::f32::consts::FRAC_1_SQRT_2,
                                    0.0,
                                ),
                                Biquad::new(
                                    BiquadKind::LowCut,
                                    p.sc_hpf_hz,
                                    std::f32::consts::FRAC_1_SQRT_2,
                                    0.0,
                                ),
                            ])
                        } else {
                            None
                        },
                        att: tc(p.attack_ms.max(0.01)),
                        rel: tc(p.release_ms.max(1.0)),
                        threshold_db: p.threshold_db,
                        ratio: p.ratio,
                        knee_db: p.knee_db.max(0.0),
                        makeup: db_to_gain(p.makeup_db),
                        mix: p.mix.clamp(0.0, 1.0),
                        gr: 0.0,
                        frozen: 1.0,
                        dl: vec![0.0; la],
                        dr: vec![0.0; la],
                        dw: 0,
                        la,
                        total,
                        in_pos: 0,
                    }
                }
                Effect::Sidechain(p) => {
                    let trig = triggers.get(&p.source.to_lowercase())?;
                    StreamFx::Sidechain {
                        trig: std::sync::Arc::new(trig.clone()),
                        ti: 0,
                        last: None,
                        from: 0.0,
                        prev: 0.0,
                        rel_s: p.release_ms.max(5.0) * 0.001,
                        att_n: (p.attack_ms.max(0.1) * 0.001 * SR) as usize,
                        amount: p.amount.clamp(0.0, 1.0),
                    }
                }
                Effect::Reverb(p) => {
                    const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
                    const APS: [usize; 4] = [556, 441, 341, 225];
                    const SPREAD: usize = 23;
                    StreamFx::ReverbFreeverb {
                        cl: COMBS.iter().map(|&n| crate::fx::Comb::new(n)).collect(),
                        cr: COMBS
                            .iter()
                            .map(|&n| crate::fx::Comb::new(n + SPREAD))
                            .collect(),
                        al: APS.iter().map(|&n| crate::fx::Allpass::new(n)).collect(),
                        ar: APS
                            .iter()
                            .map(|&n| crate::fx::Allpass::new(n + SPREAD))
                            .collect(),
                        pbuf: vec![0.0f32; ((p.predelay_ms.max(0.0) * 0.001 * SR) as usize).max(1)],
                        k: 0,
                        fb: 0.7 + p.size.clamp(0.0, 1.0) * 0.28,
                        damp: p.damping.clamp(0.0, 1.0) * 0.4,
                        wet1: p.mix * (p.width / 2.0 + 0.5),
                        wet2: p.mix * ((1.0 - p.width) / 2.0),
                        dry: 1.0 - p.mix * 0.5,
                    }
                }
                Effect::ParametricEq(p) => {
                    let build = || -> Vec<Biquad> {
                        let mut v = Vec::new();
                        for b in p.bands.iter().filter(|b| b.enabled) {
                            // cuts are 12 dB/oct per stage; `slope` stacks
                            let stages = if matches!(
                                b.kind,
                                crate::fx::EqBandKind::LowCut | crate::fx::EqBandKind::HighCut
                            ) {
                                b.stages.clamp(1, 4)
                            } else {
                                1
                            };
                            for _ in 0..stages {
                                v.push(crate::fx_extra::band_filter(b));
                            }
                        }
                        v
                    };
                    StreamFx::ParametricEq {
                        chans: [build(), build()],
                        out: db_to_gain(p.output_db),
                    }
                }
                _ => unreachable!("classify said Streamable but no construction arm"),
            });
        }
        Some(ChainStreamer { fx, pos: 0 })
    }

    /// Process one block; pos advances so song-grid effects (chorus,
    /// bitcrush, sidechain) stay aligned with a full render.
    pub fn process_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        let pos = self.pos;
        for fx in self.fx.iter_mut() {
            fx.process_block(l, r, pos);
        }
        self.pos = pos + l.len();
    }

    /// Chain latency: the sum of effect latencies (RT4). Chained
    /// lookahead effects add.
    pub fn latency_samples(&self) -> usize {
        self.fx.iter().map(StreamFx::latency).sum()
    }
}

// ---- RT2: engine pair (control/audio split over SPSC rings) ----

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Command from the control thread to the audio thread. Carrying a whole
/// node set through the ring is pointer moves only; all allocation and
/// deallocation happens control-side.
pub enum EngineCmd {
    /// Timestamped parameter event for one node.
    SetParam { node: NodeId, ev: ParamEvent },
    /// Replace the graph's node set at a block boundary. The old nodes
    /// return through the trash ring for control-side drop.
    SwapGraph(Vec<(NodeId, Box<dyn Node>)>),
}

/// Old graph nodes handed back to the control thread for deallocation.
pub struct Trash(pub Vec<(NodeId, Box<dyn Node>)>);

/// The ONE place rtrb is touched (RT2 license boundary: rtrb 0.4.0,
/// MIT OR Apache-2.0, pinned in Cargo.toml). Everything RT-threaded talks
/// to these wrappers, so a future swap of the transport touches this
/// module only. All operations are wait-free, non-allocating moves.
mod ring {
    pub struct Tx<T>(rtrb::Producer<T>);
    pub struct Rx<T>(rtrb::Consumer<T>);

    pub fn pair<T>(cap: usize) -> (Tx<T>, Rx<T>) {
        let (tx, rx) = rtrb::RingBuffer::new(cap);
        (Tx(tx), Rx(rx))
    }

    /// Push; false when the ring is full (caller keeps the value back).
    pub fn push<T>(tx: &mut Tx<T>, v: T) -> Result<(), T> {
        tx.0.push(v).map_err(|e| match e {
            rtrb::PushError::Full(v) => v,
        })
    }

    /// Pop the oldest value, if any.
    pub fn pop<T>(rx: &mut Rx<T>) -> Option<T> {
        rx.0.pop().ok()
    }
}

/// The audio-thread half: graph, command input, xrun accounting. Never
/// allocates, locks, or deallocates on the block path.
pub struct Engine {
    graph: Graph,
    budget_ns: u64,
    clock: fn() -> u64,
    cmd_rx: ring::Rx<EngineCmd>,
    trash_tx: ring::Tx<Trash>,
    pending_trash: Option<Vec<(NodeId, Box<dyn Node>)>>,
    pending_swap: Option<Vec<(NodeId, Box<dyn Node>)>>,
    xruns: Arc<AtomicU64>,
    event_drops: Arc<AtomicU64>,
    peak_bits: Arc<AtomicU32>,
    ms_bits: Arc<AtomicU32>,
    blocks: Arc<AtomicU64>,
    latency: Arc<AtomicU64>,
    pos: u64,
}

/// The control-thread half: sends commands, reads the xrun counter,
/// collects trash (deallocation is its job).
/// A single read of every engine meter (RT3). One call, relaxed
/// atomics only.
#[derive(Clone, Copy, Debug)]
pub struct MeterSnapshot {
    pub xruns: u64,
    pub event_drops: u64,
    pub blocks: u64,
    pub peak: f32,
    pub rms: f32,
}

pub struct EngineHandle {
    cmd_tx: ring::Tx<EngineCmd>,
    trash_rx: ring::Rx<Trash>,
    xruns: Arc<AtomicU64>,
    event_drops: Arc<AtomicU64>,
    peak_bits: Arc<AtomicU32>,
    ms_bits: Arc<AtomicU32>,
    blocks: Arc<AtomicU64>,
    latency: Arc<AtomicU64>,
}

/// Build the (audio, control) pair over two rings: 64 command slots
/// (matches the event-buffer cap), 4 trash slots. `clock` is a
/// nanosecond source (the live driver passes a real clock; tests pass a
/// fake one a node can advance - Test 2).
pub fn engine_pair(
    block_size: usize,
    budget_ns: u64,
    clock: fn() -> u64,
) -> (Engine, EngineHandle) {
    let (cmd_tx, cmd_rx) = ring::pair(CMD_RING_CAP);
    let (trash_tx, trash_rx) = ring::pair(TRASH_RING_CAP);
    let xruns = Arc::new(AtomicU64::new(0));
    let event_drops = Arc::new(AtomicU64::new(0));
    let peak_bits = Arc::new(AtomicU32::new(0));
    let ms_bits = Arc::new(AtomicU32::new(0));
    let blocks = Arc::new(AtomicU64::new(0));
    let latency = Arc::new(AtomicU64::new(0));
    (
        Engine {
            graph: Graph::new(block_size),
            budget_ns,
            clock,
            cmd_rx,
            trash_tx,
            pending_trash: None,
            pending_swap: None,
            xruns: xruns.clone(),
            event_drops: event_drops.clone(),
            peak_bits: peak_bits.clone(),
            ms_bits: ms_bits.clone(),
            blocks: blocks.clone(),
            latency: latency.clone(),
            pos: 0,
        },
        EngineHandle {
            cmd_tx,
            trash_rx,
            xruns,
            event_drops,
            peak_bits,
            ms_bits,
            blocks,
            latency,
        },
    )
}

impl Engine {
    /// Graph access for offline construction (before streaming starts).
    pub fn graph(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Absolute sample position of the next block.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Render one block. Returns true when the block OVERRAN its budget:
    /// the output is then fully silence and the xrun counter advanced
    /// (Test 2: no allocation or lock on that path, only fill + fetch_add).
    ///
    /// Commands apply at block boundaries only. Under trash-ring pressure
    /// a swap defers to a later block (pending_swap), and commands behind
    /// it wait with it - FIFO, documented.
    pub fn render_block(&mut self, out: &mut [f32]) -> bool {
        if let Some(old) = self.pending_trash.take() {
            if let Err(old) = ring::push(&mut self.trash_tx, Trash(old)) {
                self.pending_trash = Some(old.0);
            }
        }
        if self.pending_trash.is_none() {
            if let Some(nodes) = self.pending_swap.take() {
                self.pending_trash = Some(self.graph.swap_nodes(nodes));
            }
        }
        while self.pending_trash.is_none() && self.pending_swap.is_none() {
            match ring::pop(&mut self.cmd_rx) {
                Some(EngineCmd::SetParam { node, ev }) => {
                    if let Some(n) = self.graph.node_mut(node) {
                        n.push_event(ev);
                    }
                }
                Some(EngineCmd::SwapGraph(nodes)) => {
                    self.pending_trash = Some(self.graph.swap_nodes(nodes));
                }
                None => break,
            }
        }
        let t0 = (self.clock)();
        self.graph.process_block(self.pos, crate::dsp::SR, out);
        let overrun = (self.clock)().saturating_sub(t0) > self.budget_ns;
        if overrun {
            out.fill(0.0);
            self.xruns.fetch_add(1, Ordering::Relaxed);
        }
        self.pos += out.len() as u64;
        // RT3 metering: mirror the nodes' event-drop total control-side
        // (one atomic store per block, no alloc/lock).
        let drops: u64 = self.graph.iter_nodes().map(|(_, n)| n.event_drops()).sum();
        self.event_drops.store(drops, Ordering::Relaxed);
        // RT4 PDC: mirror the graph latency control-side (constant per
        // graph; changes on the first block after a swap that changes it).
        self.latency
            .store(self.graph.latency_samples(), Ordering::Relaxed);
        // RT3 metering: per-block peak hold (atomic max over f32 bits -
        // the bit pattern is monotonic for non-negative floats) and a
        // mean-square store, plus the block counter. Reads use these
        // relaxed atomics only; no locks, no allocation.
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        for &s in out.iter() {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            sum_sq += (s as f64) * (s as f64);
        }
        self.peak_bits.fetch_max(peak.to_bits(), Ordering::Relaxed);
        let ms = (sum_sq / out.len() as f64) as f32;
        self.ms_bits.store(ms.to_bits(), Ordering::Relaxed);
        self.blocks.fetch_add(1, Ordering::Relaxed);
        overrun
    }
}

impl EngineHandle {
    /// Queue a timestamped parameter event for a node. Err(cmd) back when
    /// the ring is full; the CALLER (control thread) decides to retry or
    /// drop - the audio thread is never blocked.
    pub fn set_param(&mut self, node: NodeId, sample: u64, value: f32) -> Result<(), EngineCmd> {
        ring::push(
            &mut self.cmd_tx,
            EngineCmd::SetParam {
                node,
                ev: ParamEvent { sample, value },
            },
        )
    }

    /// Queue a graph swap for the next available block boundary. Err(cmd)
    /// back when the ring is full (same rule as set_param).
    pub fn swap_graph(&mut self, nodes: Vec<(NodeId, Box<dyn Node>)>) -> Result<(), EngineCmd> {
        ring::push(&mut self.cmd_tx, EngineCmd::SwapGraph(nodes))
    }

    /// Blocks that overran their budget (the atomic type is the lock-free
    /// assertion, Test 2.1).
    pub fn xruns(&self) -> u64 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// Param events dropped engine-wide (fixed buffers full), mirrored
    /// from the audio side at each block - one place for RT3 metering,
    /// next to the xrun counter.
    pub fn event_drops(&self) -> u64 {
        self.event_drops.load(Ordering::Relaxed)
    }

    /// RT4 PDC: total graph latency in samples (for a MixNode graph, the
    /// max track-chain latency; every path is delayed to align at the
    /// sum). Constant per graph. On a graph swap that changes it, the
    /// reported value updates on the first block the new graph processes;
    /// compensation delay lines belong to the new graph and start empty
    /// (its old-graph tail is dropped with the old graph control-side),
    /// so a mid-run swap to a different latency is a hard cut, not a
    /// crossfade.
    pub fn latency_samples(&self) -> u64 {
        self.latency.load(Ordering::Relaxed)
    }

    /// All meters in one call (relaxed atomics, no locks). `peak` is a
    /// hold value since the last `reset_peak`; `rms` is the last block's
    /// root-mean-square. Ballistics and smoothing live reader-side.
    pub fn meter(&self) -> MeterSnapshot {
        MeterSnapshot {
            xruns: self.xruns.load(Ordering::Relaxed),
            event_drops: self.event_drops.load(Ordering::Relaxed),
            blocks: self.blocks.load(Ordering::Relaxed),
            peak: f32::from_bits(self.peak_bits.load(Ordering::Relaxed)),
            rms: f32::from_bits(self.ms_bits.load(Ordering::Relaxed)).sqrt(),
        }
    }

    /// Reset the peak-hold meter (control-side; decay/ballistics are
    /// the reader's job, e.g. call this after each UI frame read).
    pub fn reset_peak(&self) {
        self.peak_bits.store(0, Ordering::Relaxed);
    }

    /// Drop every returned old graph (deallocation lives control-side).
    /// Returns how many were collected.
    pub fn collect_trash(&mut self) -> usize {
        let mut n = 0;
        while ring::pop(&mut self.trash_rx).is_some() {
            n += 1;
        }
        n
    }
}

/// A gain node whose parameter is timestamped-event driven and smoothed
/// per sample with the shared coefficient (Test 3). First in a chain it
/// acts as a source writing the gain ramp; after a node it scales its
/// input per channel. The event buffer is fixed-cap ([`EVENT_CAP`]),
/// drop-oldest with a counter - never blocks or allocates.
pub struct GainNode {
    target: f32,
    y: f32,
    init: bool,
    coeff: f32,
    events: FixedEvents,
}

impl GainNode {
    pub fn new(initial: f32, sample_rate: f32) -> Self {
        Self {
            target: initial,
            y: 0.0,
            init: false,
            coeff: auto_smooth_coeff(sample_rate),
            events: FixedEvents::new(),
        }
    }

    /// Events dropped because the buffer was full (Test-visible).
    pub fn dropped_events(&self) -> u64 {
        self.events.dropped()
    }
}

impl Node for GainNode {
    fn process(&mut self, ctx: &BlockCtx, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        for i in 0..ctx.block_len {
            let a = ctx.frame_offset + i as u64;
            while let Some(ev) = self.events.peek() {
                if ev.sample <= a {
                    self.target = ev.value;
                    self.events.pop();
                } else {
                    break;
                }
            }
            if !self.init {
                self.y = self.target;
                self.init = true;
            }
            self.y += self.coeff * (self.target - self.y);
            for (ch, out) in outputs.iter_mut().enumerate() {
                out[i] = self.y * inputs.get(ch).map_or(1.0, |src| src[i]);
            }
        }
    }

    fn push_event(&mut self, ev: ParamEvent) {
        self.events.push(ev);
    }

    fn event_drops(&self) -> u64 {
        self.events.dropped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{auto_smooth_coeff, RenderOptions, AUTO_SMOOTH_MS};

    /// First node in a chain writes a ramp `frame_offset + sample_index`;
    /// later nodes double their input. The ramp makes block boundaries and
    /// offsets visible in the audio itself.
    struct Probe;

    impl Node for Probe {
        fn process(&mut self, ctx: &BlockCtx, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
            for out in outputs.iter_mut() {
                if let Some(src) = inputs.first() {
                    for (o, i) in out.iter_mut().zip(src.iter()) {
                        *o = 2.0 * *i;
                    }
                } else {
                    for (n, o) in out.iter_mut().enumerate() {
                        *o = ctx.frame_offset as f32 + n as f32;
                    }
                }
            }
        }
    }

    struct Latency(u32);

    impl Node for Latency {
        fn process(&mut self, _ctx: &BlockCtx, _inputs: &[&[f32]], _outputs: &mut [&mut [f32]]) {}
        fn latency_samples(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn block_driver_handles_partial_tail_and_offsets() {
        let mut g = Graph::new(DEFAULT_BLOCK);
        g.add_node(Box::new(Probe));
        g.add_node(Box::new(Probe));
        let mut out = vec![0.0f32; 1000]; // 512 + 488: exercises the partial tail
        g.render_offline(44_100.0, &mut out);
        // ramp then doubled: sample i must hold 2*i (exact in f32 below 2^24)
        for (i, &s) in out.iter().enumerate() {
            assert_eq!(s.to_bits(), (2.0 * i as f32).to_bits(), "sample {i}");
        }
    }

    #[test]
    fn block_driver_handles_block_larger_than_output() {
        let mut g = Graph::new(1 << 16);
        g.add_node(Box::new(Probe));
        let mut out = vec![0.0f32; 37]; // shorter than one block
        g.render_offline(44_100.0, &mut out);
        for (i, &s) in out.iter().enumerate() {
            assert_eq!(s.to_bits(), (i as f32).to_bits(), "sample {i}");
        }
    }

    #[test]
    fn latency_is_sum_of_serial_chain() {
        let mut g = Graph::new(64);
        g.add_node(Box::new(Latency(64)));
        g.add_node(Box::new(Latency(0)));
        g.add_node(Box::new(Latency(3)));
        assert_eq!(g.latency_samples(), 67);
    }

    #[test]
    fn empty_graph_leaves_output_untouched() {
        let mut g = Graph::new(DEFAULT_BLOCK);
        let mut out = vec![0.25f32; 37];
        let before = out.clone();
        g.render_offline(44_100.0, &mut out);
        assert_eq!(out, before);
    }

    /// Test 4 (spec v6, Naman 9:36pm): the shared coefficient function must
    /// be bit-for-bit equal to the render.rs:513 expression at every rate.
    #[test]
    fn smoother_coeff_matches_render_expression_bitwise() {
        for sr in [44_100.0f32, 48_000.0, 96_000.0] {
            let inline = 1.0 - (-1.0 / (AUTO_SMOOTH_MS * 0.001 * sr)).exp();
            assert_eq!(auto_smooth_coeff(sr).to_bits(), inline.to_bits(), "sr={sr}");
        }
    }

    // ---- Tests 0-3 (spec v6): scaffolds, pending later RT units ----

    /// Fixed reference project (trap beat, seed 9) WITH automation lanes:
    /// a volume lane with a step and ramps on the first track, a master
    /// volume ramp. Any pre-existing volume lanes on those targets are
    /// replaced so the added lanes are the ones that bite.
    fn reference_project() -> (crate::engine::Engine, RenderOptions) {
        use crate::automation::{AutoPoint, AutomationLane};
        let d = std::env::temp_dir().join("beatbox_rt_test0");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        // Test 0 covers track/mixdown identity: MixNode's master section
        // is fader + DcBlock only, so the fixture deliberately drops the
        // default master compressor+limiter until master-chain streaming.
        e.project.master_effects.clear();
        let pt = |beat: f32, value: f32| AutoPoint {
            beat,
            value,
            curve: Default::default(),
        };
        let name = e.project.tracks[0].name.clone();
        e.project
            .automation
            .retain(|l| !l.is_target(&name, "volume"));
        let mut lane = AutomationLane::new(&name, "volume");
        lane.points = vec![
            pt(0.0, -6.0),
            pt(4.0, -6.0),
            pt(4.5, 0.0),
            pt(12.0, -12.0),
            pt(16.0, -3.0),
        ];
        lane.sort();
        e.project.automation.push(lane);
        e.project
            .automation
            .retain(|l| !l.is_target("master", "volume"));
        let mut master = AutomationLane::new("master", "volume");
        master.points = vec![pt(0.0, 0.0), pt(8.0, 0.0), pt(16.0, -2.0)];
        master.sort();
        e.project.automation.push(master);
        (
            e,
            RenderOptions {
                tail: 0.25,
                ..Default::default()
            },
        )
    }

    /// Spec v6 mismatch printout: block size, first differing sample, both
    /// values, FTZ/DAZ state. Bit-for-bit, never epsilon: a diff means
    /// carried state advanced per block - fix the engine, not the test.
    fn assert_bit_identical(label: &str, block: usize, want: &[f32], got: &[f32]) {
        assert_eq!(
            want.len(),
            got.len(),
            "{label} block {block}: length drift (partial tail must match, no zero-pad)"
        );
        for (i, (w, g)) in want.iter().zip(got.iter()).enumerate() {
            assert!(
                w.to_bits() == g.to_bits(),
                "{label} block {block}: first diff at sample {i}: mix={w} engine={g} (ftz={})",
                crate::dsp::flush_denormals()
            );
        }
    }

    /// Test 0 (spec v6 + Naman's relay): engine render of the reference
    /// project is BIT-IDENTICAL to one-shot mix() at every block size,
    /// including non-divisors and a block larger than the project.
    #[test]
    fn test0_engine_render_is_bit_identical_at_every_block_size() {
        let (e, opts) = reference_project();
        let one_shot = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
            let (l, r) = node.render();
            assert_bit_identical("left", block, &one_shot.left, &l);
            assert_bit_identical("right", block, &one_shot.right, &r);
        }
    }

    /// RT3: VoiceStreamer == render_track_prechain, bit for bit, at every
    /// block size, on the short non-silent fixture plus an automated pan
    /// lane (covers the pan-curve path). This is the chunked-synth gate:
    /// voice state carries across blocks, activations split
    /// sample-accurately, addition order matches the one-shot.
    #[test]
    fn test0_voice_streamer_bit_identical() {
        use crate::automation::AutoPoint;
        use crate::dsp::SR;
        let d = std::env::temp_dir().join("beatbox_rt_streamer");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
        )
        .unwrap();
        e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
            .unwrap();
        e.bank.sync(&e.project.samples);
        e.project.arrangement = vec![crate::project::Section {
            pattern: "main".into(),
            repeats: 1,
        }];
        let pt = |beat: f32, value: f32| AutoPoint {
            beat,
            value,
            curve: Default::default(),
        };
        let mut pan = crate::automation::AutomationLane::new("kick", "pan");
        pan.points = vec![pt(0.0, -0.8), pt(2.0, 0.9), pt(4.0, -0.2)];
        e.project.automation.push(pan);
        let opts = RenderOptions {
            tail: 0.03,
            ..Default::default()
        };
        // oracle inputs, built exactly as MixNode::new builds them
        let (events, body_len) = crate::render::schedule(&e.project, &opts);
        let total = body_len + (opts.tail.max(0.0) * SR) as usize;
        let (triggers, levels) =
            crate::render::sidechain_inputs(&e.project, &e.bank, &opts, &events, total);
        let (tl, offset) = crate::render::render_timeline(&e.project, &opts);
        let tcurve = crate::tempo_curve::TempoCurve::new(&e.project);
        let ctx = crate::fx::FxContext {
            step_secs: e.project.step_secs(),
            triggers: &triggers,
            offset,
            levels: Some(&levels),
        };
        let args = TrackSynth {
            p: &e.project,
            bank: &e.bank,
            opts: &opts,
            events: &events,
            total,
            tl: &tl,
            tcurve: &tcurve,
            ctx: &ctx,
            index_base: None,
        };
        for ti in 0..e.project.tracks.len() {
            let (ol, or_) = crate::render::render_track_prechain(&args, ti);
            for block in [1usize, 63, 256, DEFAULT_BLOCK, 4096] {
                let mut st = VoiceStreamer::new(&e.project, &e.bank, &opts, block, ti);
                let (l, r) = st.render_all();
                assert_bit_identical("streamer left", block, &ol, &l);
                assert_bit_identical("streamer right", block, &or_, &r);
            }
        }
    }

    /// RT3: every StreamFx variant is bit-identical to its one-shot
    /// Effect::process arm at block sizes {1, 63, 512, whole}.
    #[test]
    fn stream_fx_bit_identical() {
        let mk_triggers = || {
            let mut m = std::collections::HashMap::new();
            m.insert("kick".to_string(), vec![0usize, 1000, 5000, 9000]);
            m
        };
        let step = 60.0 / 140.0 / 4.0;
        let effects: Vec<crate::fx::Effect> = vec![
            serde_json::from_value(
                serde_json::json!({"type":"filter","cutoff":800.0,"resonance":0.5}),
            )
            .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"eq","low_db":-6.0,"mid_db":2.0,"high_db":-1.5}),
            )
            .unwrap(),
            serde_json::from_value(serde_json::json!({"type":"distortion","drive":0.4,"mix":0.7}))
                .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"bitcrush","bits":12.0,"downsample":2,"mix":0.35}),
            )
            .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"delay","steps":3.0,"mix":0.22,"feedback":0.35}),
            )
            .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"delay","steps":1.5,"mix":0.4,"feedback":0.5,"ping_pong":true}),
            )
            .unwrap(),
            serde_json::from_value(serde_json::json!({"type":"chorus","mix":0.35})).unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"width","amount":1.6,"low_mono_hz":120.0}),
            )
            .unwrap(),
            serde_json::from_value(serde_json::json!({"type":"gain","db":-3.5})).unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"compressor","threshold_db":-12.0,"ratio":4.0,"knee_db":0.0,"lookahead_ms":0.0,"sc_hpf_hz":0.0,"mix":1.0}),
            )
            .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"sidechain","source":"kick","amount":0.5}),
            )
            .unwrap(),
            serde_json::from_value(serde_json::json!({"type":"reverb","size":0.8,"mix":0.3}))
                .unwrap(),
            serde_json::from_value(
                serde_json::json!({"type":"parametric_eq","bands":[{"kind":"low_cut","freq":100},{"kind":"bell","freq":3200,"gain_db":3.0}],"output_db":-1.0}),
            )
            .unwrap(),
        ];
        // deterministic pseudo-audio for the first half, then SILENCE:
        // tails (reverb rings, delay feedback, smoother release) cross
        // block boundaries and are compared past the end of the input;
        // 6000 is a multiple of no tested block size.
        let n = 12_000usize;
        let signal = 6_000usize;
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let l0: Vec<f32> = (0..n)
            .map(|i| if i < signal { rnd() } else { 0.0 })
            .collect();
        let r0: Vec<f32> = (0..n)
            .map(|i| if i < signal { rnd() } else { 0.0 })
            .collect();
        for fx in &effects {
            let triggers = mk_triggers();
            let (mut ol, mut or_) = (l0.clone(), r0.clone());
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            for block in [1usize, 63, 256, 512, 4096] {
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(fx), &[], step, &triggers, n)
                        .unwrap_or_else(|| panic!("{} must be streamable", fx.type_name()));
                let (mut l, mut r) = (l0.clone(), r0.clone());
                let mut pos = 0;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                assert_bit_identical("fx left", block, &ol, &l);
                assert_bit_identical("fx right", block, &or_, &r);
            }
        }
        // the fallback list stays honest: these must NOT build
        let triggers = mk_triggers();
        for bad in [
            serde_json::json!({"type":"limiter"}),
            serde_json::json!({"type":"convolution","sample":"x"}),
            serde_json::json!({"type":"reverb","mode":"fdn_hall"}),
        ] {
            let fx: crate::fx::Effect = serde_json::from_value(bad).unwrap();
            assert!(
                ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, 4096)
                    .is_none(),
                "{} must fall back",
                fx.type_name()
            );
        }
    }

    /// RT3 relay item 1: the classification table is exhaustive and
    /// consistent with ChainStreamer::build across every effect kind.
    #[test]
    fn fx_classification_covers_all_kinds() {
        let mut triggers = std::collections::HashMap::new();
        triggers.insert("kick".to_string(), vec![0usize, 1000]);
        let step = 60.0 / 140.0 / 4.0;
        let mut streamable = Vec::new();
        for &(name, _desc) in crate::fx::EFFECT_TYPES {
            let fx = crate::fx::Effect::from_type(name)
                .unwrap_or_else(|| panic!("{name} must build from its type name"));
            let cls = classify(&fx);
            let built = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, 4096);
            match cls {
                FxClass::Streamable => {
                    assert!(built.is_some(), "{name}: Streamable but build() refused");
                    streamable.push(name);
                }
                FxClass::OneShot(reason) => {
                    assert!(
                        built.is_none(),
                        "{name}: OneShot({reason}) but build() accepted"
                    );
                    assert!(!reason.is_empty(), "{name}: OneShot needs a reason");
                }
            }
        }
        // the verified set, pinned so a regression in the table fails loudly
        streamable.sort();
        assert_eq!(
            streamable,
            vec![
                "bitcrush",
                "chorus",
                "compressor",
                "delay",
                "distortion",
                "eq",
                "filter",
                "gain",
                "parametric_eq",
                "reverb",
                "sidechain",
                "width",
            ]
        );
    }

    /// Test 0 (spec v6): a project shorter than one block - NON-SILENT
    /// (trap seed 9, bars=1, bpm=200: one bar ~= 1.2 s + 30 ms tail <
    /// 1<<16 samples at 44.1 kHz), so the short-block path is proven on
    /// real audio, not silence. Two extra kick notes (relay item 1):
    /// one onset EXACTLY on a block boundary (sample 6656 = 512*13 =
    /// 256*26) and one straddling every block size tested (6693).
    /// Placement goes through microtiming offsets verified against
    /// schedule() itself, nudged by f32 ulps.
    #[test]
    fn test0_project_shorter_than_one_block() {
        use crate::dsp::SR;
        let d = std::env::temp_dir().join("beatbox_rt_test0_short");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
        )
        .unwrap();
        e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
            .unwrap();
        e.bank.sync(&e.project.samples);
        // Test 0 covers track/mixdown identity: no master chain in the
        // engine path yet, so the default master chain is cleared here.
        e.project.master_effects.clear();
        e.project.arrangement = vec![crate::project::Section {
            pattern: "main".into(),
            repeats: 1,
        }];
        let opts = RenderOptions {
            tail: 0.03,
            ..Default::default()
        };
        // place a kick onset exactly at `target` samples via microtiming
        // around step 2 (even: untouched by swing), replicating the exact
        // f32 arithmetic of schedule() and nudging by ulps
        let step = e.project.step_secs();
        let place = |e: &mut crate::engine::Engine, target: usize| {
            let mut o = (target as f64 / (step as f64 * SR as f64) - 2.0) as f32;
            assert!((-0.5..=0.5).contains(&o), "target unreachable from step 2");
            for _ in 0..64 {
                let s = ((2.0f32 + o) * step * SR) as usize;
                if s == target {
                    break;
                }
                o = if s < target {
                    f32::from_bits(o.to_bits() + 1)
                } else {
                    f32::from_bits(o.to_bits() - 1)
                };
            }
            assert_eq!(
                ((2.0f32 + o) * step * SR) as usize,
                target,
                "onset placement failed"
            );
            e.project.patterns[0]
                .notes_mut("kick")
                .push(crate::project::Note {
                    start: 2.0,
                    len: 0.5,
                    pitch: 60,
                    vel: 1.0,
                    offset: o,
                    ..Default::default()
                });
        };
        place(&mut e, 512 * 13);
        place(&mut e, 512 * 13 + 37);
        // the placement really lands where intended, through schedule()
        let (events, _) = crate::render::schedule(&e.project, &opts);
        let ki = e
            .project
            .tracks
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case("kick"))
            .expect("trap has a kick track");
        assert!(events[ki].iter().any(|x| x.start == 512 * 13));
        assert!(events[ki].iter().any(|x| x.start == 512 * 13 + 37));
        let one_shot = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert!(one_shot.left.len() < 1 << 16, "fixture must be short");
        assert!(
            one_shot.left.iter().any(|&s| s.to_bits() != 0),
            "fixture must be non-silent"
        );
        for block in [DEFAULT_BLOCK, 256, 63, 1 << 16] {
            let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
            let (l, r) = node.render();
            assert_bit_identical("left", block, &one_shot.left, &l);
            assert_bit_identical("right", block, &one_shot.right, &r);
        }
    }

    /// RT4: the streamed soft-knee/lookahead compressor is bit-identical
    /// to offline sc_dsp::compress once its latency is accounted for:
    /// streamed[la..n] == offline[0..n-la], and `la` extra silent input
    /// samples flush the delay line so the tail matches offline[n-la..n].
    /// n=12000 is not a multiple of any tested block size; the first 6000
    /// samples carry signal, the rest silence (tails cross blocks).
    #[test]
    fn stream_fx_compressor_lookahead_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                }
            })
            .collect();
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"compressor","threshold_db":-14.0,"ratio":3.0,
            "knee_db":6.0,"lookahead_ms":2.0,"sc_hpf_hz":120.0,
            "attack_ms":5.0,"release_ms":80.0,"makeup_db":2.0,"mix":0.85
        }))
        .unwrap();
        let la = oneshot_latency_samples(&fx, SR).expect("lookahead latency");
        assert_eq!(la, 88);
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for block in [1usize, 63, 256, 512, 4096] {
            let mut ol = sig.clone();
            let mut or_ = sig.clone();
            crate::sc_dsp::compress(
                &crate::sc_dsp::CompParams {
                    threshold_db: -14.0,
                    ratio: 3.0,
                    attack_ms: 5.0,
                    release_ms: 80.0,
                    knee_db: 6.0,
                    makeup_db: 2.0,
                    sc_hpf_hz: 120.0,
                    lookahead_ms: 2.0,
                    mix: 0.85,
                },
                &mut ol,
                &mut or_,
            );
            let mut l = sig.clone();
            let mut r = sig.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("lookahead compressor streams");
            assert_eq!(cs.latency_samples(), la);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            // flush: la extra silent samples push the tail through
            let mut fl = vec![0.0f32; la];
            let mut fr = vec![0.0f32; la];
            cs.process_block(&mut fl, &mut fr);
            for i in la..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i - la].to_bits(),
                    "left block {block} i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i - la].to_bits(),
                    "right block {block} i {i}"
                );
            }
            for i in 0..la {
                assert_eq!(
                    fl[i].to_bits(),
                    ol[n - la + i].to_bits(),
                    "flush left block {block} i {i}"
                );
                assert_eq!(
                    fr[i].to_bits(),
                    or_[n - la + i].to_bits(),
                    "flush right block {block} i {i}"
                );
            }
        }
    }

    /// RT4 PDC test fixture: trap seed 9 with a 10ms-lookahead soft-knee
    /// compressor appended to the lead chain (la = 441 samples).
    fn pdc_project(with_fx: bool, solo: Option<&str>, tag: &str) -> crate::engine::Engine {
        let d = std::env::temp_dir().join(format!("beatbox_rt_pdc_{tag}"));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        if with_fx {
            e.call(
                "add_effect",
                &serde_json::json!({
                    "track": "lead", "type": "compressor",
                    "params": {
                        "threshold_db": -14.0, "ratio": 3.0,
                        "knee_db": 6.0, "lookahead_ms": 10.0
                    }
                }),
            )
            .unwrap();
        }
        e.bank.sync(&e.project.samples);
        // Engine-vs-oracle identity: MixNode's master section is fader +
        // DcBlock only, so the fixture deliberately drops the default
        // master compressor+limiter until master-chain streaming lands.
        e.project.master_effects.clear();
        if let Some(name) = solo {
            for t in &mut e.project.tracks {
                t.solo = t.name == name;
            }
        }
        e
    }

    fn first_nonzero(b: &[f32]) -> Option<usize> {
        b.iter().position(|&x| x != 0.0)
    }

    /// RT4 relay: the impulse lands at the exact same index on the
    /// lookahead path and the compensated dry path; renders are
    /// bit-identical across block sizes 512/256/63 (441 wraps blocks).
    #[test]
    fn rt4_pdc_impulse_alignment() {
        let opts = RenderOptions::default();
        let mut ref_l: Option<Vec<f32>> = None;
        let mut ref_r: Option<Vec<f32>> = None;
        for block in [512usize, 256, 63] {
            let e = pdc_project(true, None, &format!("mix{block}"));
            let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
            assert_eq!(node.latency(), 441, "lookahead_ms 10 -> 441 samples");
            let (l, r) = node.render();
            // the whole mix shifts by exactly L vs the no-lookahead mix
            let e0 = pdc_project(false, None, &format!("mix0_{block}"));
            let mut node0 = MixNode::new(&e0.project, &e0.bank, &opts, block);
            assert_eq!(node0.latency(), 0, "no lookahead -> zero latency");
            let (l0, _r0) = node0.render();
            assert_eq!(
                first_nonzero(&l),
                first_nonzero(&l0).map(|i| i + 441),
                "mix marker must shift by L (block {block})"
            );
            // dry path with the lookahead track solo-muted: muting the lead
            // removes its 441-sample latency from the PDC graph, so the
            // bass renders at latency 0 and is BIT-IDENTICAL to the no-FX
            // render - no shift, no compensation line.
            let eb = pdc_project(true, Some("bass"), &format!("bass{block}"));
            let mut nb = MixNode::new(&eb.project, &eb.bank, &opts, block);
            assert_eq!(
                nb.latency(),
                0,
                "solo-muted lookahead track must leave PDC latency 0 (block {block})"
            );
            let (bl, br) = nb.render();
            let eb0 = pdc_project(false, Some("bass"), &format!("bass0_{block}"));
            let mut nb0 = MixNode::new(&eb0.project, &eb0.bank, &opts, block);
            let (bl0, br0) = nb0.render();
            assert!(
                bl0.iter().any(|&s| s != 0.0),
                "bass solo must be non-silent (block {block})"
            );
            let n = bl0.len();
            assert_eq!(bl.len(), n, "dry length {block}");
            for i in 0..n {
                assert_eq!(bl[i].to_bits(), bl0[i].to_bits(), "dry left {block} {i}");
                assert_eq!(br[i].to_bits(), br0[i].to_bits(), "dry right {block} {i}");
            }
            // lookahead path: lead marker shifts by exactly L
            let el = pdc_project(true, Some("lead"), &format!("lead{block}"));
            let mut nl = MixNode::new(&el.project, &el.bank, &opts, block);
            let (ll, _) = nl.render();
            let el0 = pdc_project(false, Some("lead"), &format!("lead0_{block}"));
            let mut nl0 = MixNode::new(&el0.project, &el0.bank, &opts, block);
            let (ll0, _) = nl0.render();
            assert_eq!(
                first_nonzero(&ll),
                first_nonzero(&ll0).map(|i| i + 441),
                "lookahead-path marker must shift by L (block {block})"
            );
            // nothing differs at block boundaries
            if let (Some(rl), Some(rr)) = (&ref_l, &ref_r) {
                assert_bit_identical("pdc left vs 512", block, rl, &l);
                assert_bit_identical("pdc right vs 512", block, rr, &r);
            } else {
                ref_l = Some(l);
                ref_r = Some(r);
            }
        }
    }

    /// RT4 relay decision 1: with lookahead present, the engine output
    /// equals the one-shot oracle SHIFTED by the reported latency, and
    /// running L extra silent samples flushes the tail:
    /// engine_out[L..L+n] == oracle[0..n] bitwise, tail included.
    #[test]
    fn rt4_test0_shifted_oracle() {
        let opts = RenderOptions::default();
        let e = pdc_project(true, None, "oracle");
        let one_shot = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let n = one_shot.left.len();
        for block in [512usize, 256, 63] {
            let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
            let lat = node.latency();
            assert_eq!(lat, 441);
            let ftz = crate::dsp::flush_denormals();
            let _nd = ftz.then(crate::dsp::NoDenormals::new);
            let mut el = vec![0.0f32; n + lat];
            let mut er = vec![0.0f32; n + lat];
            let mut pos = 0usize;
            while pos < n + lat {
                let k = block.min(n + lat - pos);
                let ctx = BlockCtx {
                    sample_rate: SR,
                    block_len: k,
                    frame_offset: pos as u64,
                };
                let mut outs: [&mut [f32]; 2] = [&mut el[pos..pos + k], &mut er[pos..pos + k]];
                node.process(&ctx, &[], &mut outs);
                pos += k;
            }
            for i in lat..n {
                assert_eq!(
                    el[i].to_bits(),
                    one_shot.left[i - lat].to_bits(),
                    "shifted left block {block} i {i}"
                );
                assert_eq!(
                    er[i].to_bits(),
                    one_shot.right[i - lat].to_bits(),
                    "shifted right block {block} i {i}"
                );
            }
            for i in 0..lat {
                assert_eq!(
                    el[n + i].to_bits(),
                    one_shot.left[n - lat + i].to_bits(),
                    "flush left block {block} i {i}"
                );
                assert_eq!(
                    er[n + i].to_bits(),
                    one_shot.right[n - lat + i].to_bits(),
                    "flush right block {block} i {i}"
                );
            }
        }
    }

    /// RT4 relay: chained lookahead effects add - two 10ms lookahead
    /// compressors in series give latency 882 = 2 x 441.
    #[test]
    fn rt4_chained_lookahead_latencies_add() {
        let mk = || {
            serde_json::from_value::<crate::fx::Effect>(serde_json::json!({
                "type": "compressor", "lookahead_ms": 10.0, "knee_db": 6.0
            }))
            .unwrap()
        };
        let fx = [mk(), mk()];
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let cs = ChainStreamer::build(&fx, &[], 0.1, &triggers, 4096)
            .expect("two lookahead compressors stream");
        assert_eq!(cs.latency_samples(), 2 * 441);
        // and each still reports through oneshot_latency_samples
        assert_eq!(oneshot_latency_samples(&fx[0], SR), Some(441));
    }

    /// RT4 relay: a mid-run graph swap that changes latency updates the
    /// reported value on the first block the new graph processes. The new
    /// graph's compensation lines start empty; the old graph's tail is
    /// dropped with it control-side (hard cut, no crossfade).
    #[test]
    fn rt4_latency_swap_reporting() {
        struct Lat(u32);
        impl Node for Lat {
            fn process(&mut self, ctx: &BlockCtx, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
                for (ch, out) in outputs.iter_mut().enumerate() {
                    for i in 0..ctx.block_len {
                        out[i] = inputs.get(ch).map_or(0.0, |s| s[i]);
                    }
                }
            }
            fn latency_samples(&self) -> u32 {
                self.0
            }
        }
        fn still_clock() -> u64 {
            0
        }
        let (mut engine, mut handle) = engine_pair(64, u64::MAX, still_clock);
        let g = engine.graph().add_node(Box::new(Lat(0)));
        let mut out = vec![0.0f32; 64];
        engine.render_block(&mut out);
        assert_eq!(handle.latency_samples(), 0);
        handle
            .swap_graph(vec![(g, Box::new(Lat(441)))])
            .map_err(|_| ())
            .unwrap();
        engine.render_block(&mut out);
        assert_eq!(
            handle.latency_samples(),
            441,
            "reported latency updates on the first block after the swap"
        );
        handle
            .swap_graph(vec![(g, Box::new(Lat(0)))])
            .map_err(|_| ())
            .unwrap();
        engine.render_block(&mut out);
        assert_eq!(handle.latency_samples(), 0);
        let _ = handle.collect_trash();
    }

    /// RT4: the lookahead latency const matches sc_dsp::compress exactly
    /// (clamp 0..=10ms, round to nearest sample).
    #[test]
    fn lookahead_latency_const_matches_sc_dsp() {
        let mk = |ms: f32| {
            serde_json::from_value::<crate::fx::Effect>(serde_json::json!({
                "type": "compressor", "lookahead_ms": ms
            }))
            .unwrap()
        };
        // 2ms -> 88.2 -> 88; 0.1ms -> 4.41 -> 4; 20ms clamps to 10ms -> 441
        assert_eq!(oneshot_latency_samples(&mk(2.0), SR), Some(88));
        assert_eq!(oneshot_latency_samples(&mk(0.1), SR), Some(4));
        assert_eq!(oneshot_latency_samples(&mk(20.0), SR), Some(441));
        // no lookahead -> no latency
        assert_eq!(oneshot_latency_samples(&mk(0.0), SR), None);
        // derived from the sample rate, not hardcoded: 10ms at 48k/96k
        assert_eq!(oneshot_latency_samples(&mk(10.0), 48_000.0), Some(480));
        assert_eq!(oneshot_latency_samples(&mk(10.0), 96_000.0), Some(960));
        assert_eq!(oneshot_latency_samples(&mk(2.0), 48_000.0), Some(96));
        assert_eq!(oneshot_latency_samples(&mk(2.0), 96_000.0), Some(192));
        // clamp still pinned at other rates: 20ms -> 10ms
        assert_eq!(oneshot_latency_samples(&mk(20.0), 48_000.0), Some(480));
    }

    /// RT3 metering: one snapshot call reads xruns, drops, blocks and
    /// the peak/rms atomics; peak is a hold with control-side reset.
    #[test]
    fn meter_snapshot_reads_atomics() {
        fn still_clock() -> u64 {
            0
        }
        let (mut engine, handle) = engine_pair(64, u64::MAX, still_clock);
        engine
            .graph()
            .add_node(Box::new(GainNode::new(1.0, crate::dsp::SR)));
        let mut out = vec![0.0f32; 64];
        for _ in 0..4 {
            engine.render_block(&mut out);
        }
        let m = handle.meter();
        assert_eq!(m.blocks, 4);
        assert_eq!(m.xruns, 0);
        assert_eq!(m.event_drops, 0);
        // GainNode as chain head writes a constant 1.0, so peak == rms == 1.
        assert_eq!(m.peak.to_bits(), 1.0f32.to_bits());
        assert_eq!(m.rms.to_bits(), 1.0f32.to_bits());
        handle.reset_peak();
        assert_eq!(handle.meter().peak.to_bits(), 0.0f32.to_bits());
    }

    /// Test 1 (RT4): two tracks sharing the same source notes - one with
    /// a 10ms-lookahead compressor (chain latency 441), one dry (PDC
    /// compensation delay 441) - land their impulse on the SAME output
    /// sample offset. The zero-latency leg is bitwise a pure 441-sample
    /// shift of its uncompensated control render. A silent anchor track
    /// carries the lookahead compressor so L stays 441 in every render.
    #[test]
    fn test1_pdc_mixed_latency_alignment() {
        let opts = RenderOptions::default();
        let comp = || {
            serde_json::from_value::<crate::fx::Effect>(serde_json::json!({
                "type": "compressor", "threshold_db": -14.0, "ratio": 3.0,
                "knee_db": 6.0, "lookahead_ms": 10.0
            }))
            .unwrap()
        };
        let build = |tag: &str| {
            let d = std::env::temp_dir().join(format!("beatbox_rt_test1_{tag}"));
            std::fs::create_dir_all(&d).unwrap();
            let mut e = crate::engine::Engine::new(d);
            e.call(
                "generate_beat",
                &serde_json::json!({"style": "trap", "seed": 9}),
            )
            .unwrap();
            e.bank.sync(&e.project.samples);
            let li = e
                .project
                .tracks
                .iter()
                .position(|t| t.name == "lead")
                .expect("lead track");
            let mut dry = e.project.tracks[li].clone();
            dry.name = "lead_dry".into();
            dry.effects.clear();
            let mut anchor = e.project.tracks[li].clone();
            anchor.name = "anchor".into();
            anchor.effects = vec![comp()];
            e.project.tracks[li].effects.push(comp());
            e.project.tracks.push(dry);
            e.project.tracks.push(anchor);
            // notes live in patterns keyed by lowercase track name: the
            // dry clone gets lead's notes, the anchor stays silent
            for pat in &mut e.project.patterns {
                if let Some(notes) = pat.clips.get("lead").cloned() {
                    pat.clips.insert("lead_dry".into(), notes);
                }
            }
            e
        };
        let render = |e: &crate::engine::Engine,
                      keep: &[&str],
                      block: usize|
         -> (usize, (Vec<f32>, Vec<f32>)) {
            let mut p = e.project.clone();
            for t in &mut p.tracks {
                t.mute = !keep.contains(&t.name.as_str());
            }
            let mut node = MixNode::new(&p, &e.bank, &opts, block);
            (node.latency(), node.render())
        };
        for block in [512usize, 256, 63] {
            let e = build(&format!("b{block}"));
            let (lat_a, (al, _ar)) = render(&e, &["lead", "anchor"], block);
            assert_eq!(lat_a, 441, "lookahead path present");
            let (lat_b, (bl, _br)) = render(&e, &["lead_dry", "anchor"], block);
            assert_eq!(lat_b, 441, "anchor holds L with lead muted");
            let (lat_c, (cl, _cr)) = render(&e, &["lead_dry"], block);
            assert_eq!(lat_c, 0, "no lookahead in the graph -> L == 0");
            let ia = first_nonzero(&al).expect("lead audio");
            let ib = first_nonzero(&bl).expect("dry audio");
            let ic = first_nonzero(&cl).expect("control audio");
            assert_eq!(
                ia, ib,
                "both paths land on the same output offset (block {block})"
            );
            assert_eq!(
                ia,
                ic + 441,
                "offset is the source marker + L (block {block})"
            );
            // the zero-latency leg is bitwise a pure 441-sample shift
            let n = cl.len();
            for i in 0..441 {
                assert_eq!(
                    bl[i].to_bits(),
                    0.0f32.to_bits(),
                    "dry pre-roll must be exact zeros (block {block} i {i})"
                );
            }
            for i in 441..n {
                assert_eq!(
                    bl[i].to_bits(),
                    cl[i - 441].to_bits(),
                    "dry leg bitwise shift (block {block} i {i})"
                );
            }
        }
    }

    // Fake nanosecond clock for Test 2 (thread-local, no locks).
    thread_local! {
        static FAKE_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    fn fake_ns() -> u64 {
        FAKE_NS.with(|c| c.get())
    }

    /// Deterministic overrun node: advances the fake clock past the block
    /// budget inside process() (no busy-spin, spec v6), writes 1.0.
    struct SlowNode {
        advance_ns: u64,
    }

    impl Node for SlowNode {
        fn process(&mut self, _ctx: &BlockCtx, _inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
            FAKE_NS.with(|c| c.set(c.get() + self.advance_ns));
            for out in outputs.iter_mut() {
                out.fill(1.0);
            }
        }
    }

    struct FastNode;

    impl Node for FastNode {
        fn process(&mut self, _ctx: &BlockCtx, _inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
            for out in outputs.iter_mut() {
                out.fill(2.0);
            }
        }
    }

    /// Test 2 (spec v6): xrun counter - atomic, silent, correct. Exactly M
    /// deliberate overruns count M; overrun blocks are fully silence;
    /// recovery via a graph swap over the command ring; the counter
    /// survives the swap; old nodes come back as trash for control-side
    /// drop. (2.3, no alloc/lock on the xrun path, is structural - fill +
    /// fetch_add only - and proven by tests/no_alloc_rt_engine.rs.)
    #[test]
    fn test2_xrun_counter_silence_and_recovery() {
        const BLOCK: usize = 512;
        const M: u64 = 3;
        let (mut engine, mut handle) = engine_pair(BLOCK, 1_000, fake_ns);
        engine
            .graph()
            .add_node(Box::new(SlowNode { advance_ns: 2_000 }));
        let mut out = vec![0.0f32; BLOCK];

        for _ in 0..M {
            assert!(engine.render_block(&mut out), "must flag the overrun");
            assert!(
                out.iter().all(|&s| s.to_bits() == 0),
                "overrun block must be fully silence"
            );
        }
        assert_eq!(handle.xruns(), M, "counter reads exactly M");

        // Recovery: swap the slow node out over the ring. The swap lands
        // at the next block boundary; the old graph returns as trash.
        handle
            .swap_graph(vec![(0, Box::new(FastNode))])
            .map_err(|_| ())
            .unwrap();
        assert!(
            !engine.render_block(&mut out),
            "fast block must not overrun"
        );
        assert!(
            out.iter().all(|&s| s.to_bits() == 2.0f32.to_bits()),
            "normal output resumes"
        );
        engine.render_block(&mut out); // flushes the trash ring
        assert_eq!(handle.collect_trash(), 1, "old graph freed control-side");
        assert_eq!(handle.xruns(), M, "counter stops after recovery");
        assert_eq!(handle.xruns(), M, "counter survives graph swaps");

        // Partial-tail overrun: a shortened block that overruns is
        // silence of exactly the tail length, and still counts.
        handle
            .swap_graph(vec![(0, Box::new(SlowNode { advance_ns: 2_000 }))])
            .map_err(|_| ())
            .unwrap();
        let mut tail = vec![7.0f32; 300]; // 300 < BLOCK: partial tail
        assert!(engine.render_block(&mut tail), "tail overrun must flag");
        assert!(
            tail.iter().all(|&s| s.to_bits() == 0),
            "overrun partial tail is fully silence, no drift"
        );
        assert_eq!(tail.len(), 300);
        assert_eq!(handle.xruns(), M + 1, "tail overrun counts");
    }

    /// Independently-written scalar f32 oracle of the gain smoother (Test
    /// 3.2): same recurrence, same op order, NO shared code with GainNode
    /// beyond the one coefficient function (the contract's single source).
    fn oracle_gain(events: &[(u64, f32)], n: usize, initial: f32, sr: f32) -> Vec<f32> {
        let a = auto_smooth_coeff(sr);
        let mut y = initial;
        let mut target = initial;
        let mut next = 0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            while next < events.len() && events[next].0 <= i as u64 {
                target = events[next].1;
                next += 1;
            }
            y += a * (target - y);
            out.push(y);
        }
        out
    }

    fn render_gain(block: usize, total: usize, events: &[(u64, f32)]) -> Vec<f32> {
        fn fake_clock() -> u64 {
            0 // never advances: no xruns in this test
        }
        let (mut engine, mut handle) = engine_pair(block, u64::MAX, fake_clock);
        let id = engine
            .graph()
            .add_node(Box::new(GainNode::new(1.0, crate::dsp::SR)));
        for &(sample, value) in events {
            handle.set_param(id, sample, value).map_err(|_| ()).unwrap();
        }
        let mut out = vec![0.0f32; total];
        let mut written = 0;
        while written < total {
            let n = block.min(total - written);
            assert!(!engine.render_block(&mut out[written..written + n]));
            written += n;
        }
        out
    }

    /// Test 3 (spec v6 + Naman's relay): timestamped param events, per-sample
    /// smoothed. Gain begins changing exactly at the event sample (not a
    /// block boundary); oracle bit-identical in BOTH FTZ/DAZ modes; same
    /// events at block sizes 1 and 512 bit-identical.
    #[test]
    fn test3_parameter_events_timestamped_smoothed_bit_identical() {
        let total = 3 * DEFAULT_BLOCK + 100; // partial tail included
        let t0 = DEFAULT_BLOCK + 37; // mid-block of block 1 at the default size
        let events = [(t0 as u64, 0.5), ((2 * DEFAULT_BLOCK + 5) as u64, 1.25)];

        // 3.1: the gain starts moving exactly at t0
        let out = render_gain(DEFAULT_BLOCK, total, &events);
        assert_eq!(
            out[t0 - 1].to_bits(),
            1.0f32.to_bits(),
            "gain must not move before t0"
        );
        assert!(out[t0] < 1.0, "gain must start moving AT t0");

        // 3.3: block-size independence (1 vs 512), bit-for-bit
        let out1 = render_gain(1, total, &events);
        for (i, (a, b)) in out.iter().zip(out1.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "block {DEFAULT_BLOCK} vs 1 differ at sample {i}"
            );
        }

        // 3.2: oracle, both FTZ/DAZ modes, engine at BOTH approved block
        // sizes (512 default, 256 runtime option), bit-identical per mode
        for ftz in [true, false] {
            crate::dsp::set_flush_denormals(ftz);
            let _guard = ftz.then(crate::dsp::NoDenormals::new);
            let oracle: Vec<f32> = oracle_gain(&events, total, 1.0, crate::dsp::SR);
            for block in [DEFAULT_BLOCK, 256] {
                let engine_out = render_gain(block, total, &events);
                for (i, (e, o)) in engine_out.iter().zip(oracle.iter()).enumerate() {
                    assert_eq!(
                        e.to_bits(),
                        o.to_bits(),
                        "ftz={ftz} block={block}: engine vs oracle differ at sample {i}"
                    );
                }
            }
        }
        crate::dsp::set_flush_denormals(true);
    }
}
