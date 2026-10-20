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
    db_to_gain, flush_denormals, gain_to_db, Biquad, BiquadKind, DcBlock, Fft, FilterMode,
    NoDenormals, Svf, SR,
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
    /// RT5: the master effect chain, streamed (the ChainStreamer
    /// subset: lookahead compressor, non-true-peak limiter, ...). None
    /// when the chain is empty or any link is one-shot-only (true-peak
    /// limiter): the master section then stays fader + DcBlock +
    /// zeroing, exactly as before.
    master_chain: Option<ChainStreamer>,
    coeff: f32,
    pos: usize,
    total: usize,
    /// RT4 PDC: max track-chain latency in samples plus the streamed
    /// master chain's latency (RT5). Every track's output is delayed
    /// (latency_max - own) so all paths align at the sum; the master
    /// chain then delays the sum itself.
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
        // RT5: stream the master chain when every link is in the
        // ChainStreamer subset; a one-shot-only link (true-peak
        // limiter) or master FX automation keeps the old fader +
        // DcBlock-only section. An empty chain keeps the old path too.
        let master_chain = if p.master_effects.is_empty() {
            None
        } else {
            ChainStreamer::build(
                &p.master_effects,
                &master_lanes,
                p.step_secs(),
                &triggers,
                total,
            )
        };
        let latency = latency
            + master_chain
                .as_ref()
                .map(|c| c.latency_samples())
                .unwrap_or(0);
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
            master_chain,
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
        let master_fx = self.master_chain.is_some();
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
            // Offline order (render::master_section): fader, DcBlock,
            // chain, THEN non-finite zeroing - so a streamed chain sees
            // the unclamped signal and zeroing runs after it.
            if master_fx {
                ml[i] = yl;
                mr[i] = yr;
            } else {
                ml[i] = if yl.is_finite() { yl } else { 0.0 };
                mr[i] = if yr.is_finite() { yr } else { 0.0 };
            }
        }
        if let Some(chain) = &mut self.master_chain {
            chain.process_block(ml, mr);
            for i in 0..n {
                if !ml[i].is_finite() {
                    ml[i] = 0.0;
                }
                if !mr[i].is_finite() {
                    mr[i] = 0.0;
                }
            }
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

/// The boxed state of [`StreamFx::CabIr`] (kept off the enum: the
/// partitioned-convolver buffers make it much larger than the other
/// arms).
pub struct CabIrArm {
    fft: Fft,
    k: usize,
    /// Per channel: the IR part spectra, the frequency-domain
    /// delay line, the current input block, the two-block output
    /// ring, and the overlap-add tail.
    parts_re: [Vec<Vec<f32>>; 2],
    parts_im: [Vec<Vec<f32>>; 2],
    fdl_re: [Vec<Vec<f32>>; 2],
    fdl_im: [Vec<Vec<f32>>; 2],
    inbuf: [Vec<f32>; 2],
    outring: [Vec<f32>; 2],
    overlap: [Vec<f32>; 2],
    head: usize,
    infill: usize,
    acc_re: Vec<f32>,
    acc_im: Vec<f32>,
    /// Dry delay lines for the wet/dry mix, B - 1 samples.
    dry: [Vec<f32>; 2],
    dw: usize,
    g: f32,
    mix: f32,
    in_pos: usize,
    total: usize,
}

/// One stereo uniformly partitioned FFT convolver for the freq_splitter
/// arm: the exact arithmetic of fx_extra::convolve (B = 2048), carried
/// across process blocks, emitting with the same B - 1 delay as CabIr.
/// All buffers are sized at build; nothing allocates on the audio path.
pub struct FirConvArm {
    fft: Fft,
    k: usize,
    parts_re: [Vec<Vec<f32>>; 2],
    parts_im: [Vec<Vec<f32>>; 2],
    fdl_re: [Vec<Vec<f32>>; 2],
    fdl_im: [Vec<Vec<f32>>; 2],
    inbuf: [Vec<f32>; 2],
    outring: [Vec<f32>; 2],
    overlap: [Vec<f32>; 2],
    head: usize,
    infill: usize,
    acc_re: Vec<f32>,
    acc_im: Vec<f32>,
}

impl FirConvArm {
    fn new(ir_l: &[f32], ir_r: &[f32]) -> Self {
        const B: usize = 2048;
        let fft = Fft::new(2 * B);
        let mk_parts = |ir: &[f32]| -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
            ir.chunks(B)
                .map(|c| {
                    let mut re = vec![0.0f32; 2 * B];
                    let mut im = vec![0.0f32; 2 * B];
                    re[..c.len()].copy_from_slice(c);
                    fft.forward(&mut re, &mut im);
                    (re, im)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .unzip()
        };
        let (pl_re, pl_im) = mk_parts(ir_l);
        let (pr_re, pr_im) = mk_parts(ir_r);
        let k = pl_re.len();
        FirConvArm {
            fft: Fft::new(2 * B),
            k,
            parts_re: [pl_re, pr_re],
            parts_im: [pl_im, pr_im],
            fdl_re: [vec![vec![0.0f32; 2 * B]; k], vec![vec![0.0f32; 2 * B]; k]],
            fdl_im: [vec![vec![0.0f32; 2 * B]; k], vec![vec![0.0f32; 2 * B]; k]],
            inbuf: [vec![0.0f32; B], vec![0.0f32; B]],
            outring: [vec![0.0f32; 2 * B], vec![0.0f32; 2 * B]],
            overlap: [vec![0.0f32; B], vec![0.0f32; B]],
            head: 0,
            infill: 0,
            acc_re: vec![0.0f32; 2 * B],
            acc_im: vec![0.0f32; 2 * B],
        }
    }

    /// Push one sample per channel; run the convolver block exactly as
    /// fx_extra::convolve does when a block fills (or at `total`).
    #[inline]
    fn push(&mut self, xl: f32, xr: f32, in_pos: usize, total: usize) {
        const B: usize = 2048;
        self.inbuf[0][self.infill] = xl;
        self.inbuf[1][self.infill] = xr;
        self.infill += 1;
        let last = in_pos == total;
        if self.infill == B || (last && self.infill > 0) {
            let fill = self.infill;
            let base = in_pos - fill;
            if base < total {
                for c in 0..2 {
                    {
                        let (re, im) = (
                            &mut self.fdl_re[c][self.head],
                            &mut self.fdl_im[c][self.head],
                        );
                        re.iter_mut().for_each(|v| *v = 0.0);
                        im.iter_mut().for_each(|v| *v = 0.0);
                        re[..fill].copy_from_slice(&self.inbuf[c][..fill]);
                        self.fft.forward(re, im);
                    }
                    self.acc_re.iter_mut().for_each(|v| *v = 0.0);
                    self.acc_im.iter_mut().for_each(|v| *v = 0.0);
                    for j in 0..self.k {
                        let slot = (self.head + self.k - j) % self.k;
                        let (xr, xi) = (&self.fdl_re[c][slot], &self.fdl_im[c][slot]);
                        let (hr, hi) = (&self.parts_re[c][j], &self.parts_im[c][j]);
                        for t in 0..2 * B {
                            self.acc_re[t] += xr[t] * hr[t] - xi[t] * hi[t];
                            self.acc_im[t] += xr[t] * hi[t] + xi[t] * hr[t];
                        }
                    }
                    self.fft.inverse(&mut self.acc_re, &mut self.acc_im);
                    for t in 0..B {
                        self.outring[c][(base + t) % (2 * B)] = self.acc_re[t] + self.overlap[c][t];
                    }
                    self.overlap[c].copy_from_slice(&self.acc_re[B..]);
                }
                self.head = (self.head + 1) % self.k;
            }
            self.infill = 0;
        }
    }

    /// The convolver output for absolute input position q (already
    /// delayed B - 1 by the caller's read index), 0 outside [0, total).
    #[inline]
    fn read(&self, c: usize, q: usize, total: usize) -> f32 {
        const B: usize = 2048;
        if q < total {
            self.outring[c][q % (2 * B)]
        } else {
            0.0
        }
    }
}

/// The boxed state of [`StreamFx::FreqSplit`] (kept off the enum: the
/// convolver buffers are large).
pub struct FreqSplitArm {
    /// h1 = lowpass(low_hz): the low band (None for band = high).
    conv1: Option<Box<FirConvArm>>,
    /// h2 = lowpass(high_hz): the mid/high split (None for band = low).
    conv2: Option<Box<FirConvArm>>,
    /// Delayed input for the high-band complement, (B - 1) + m samples.
    xd: [Vec<f32>; 2],
    xw: usize,
    /// Dry delay lines for the wet/dry mix, B - 1 samples.
    dry: [Vec<f32>; 2],
    dw: usize,
    band: crate::fx::FreqSplitBand,
    g: f32,
    mix: f32,
    in_pos: usize,
    total: usize,
}

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
    /// Brickwall limiter with true_peak = false (RT5): the offline
    /// limiter_pass chunked. The per-sample required gain feeds an
    /// 88-sample lookahead sliding minimum (monotonic deque) and the
    /// box-ramp f64 accumulator, both carried across blocks; the audio
    /// rides a `look`-sample delay line, so the limiter adds exactly
    /// `look` samples of latency. Silent input past the render end
    /// flushes the tail: need = 1.0 there never lowers the window min,
    /// which matches the offline clamped window bit for bit.
    Limiter {
        ceiling: f32,
        rel: f32,
        look: usize,
        dl: Vec<f32>,
        dr: Vec<f32>,
        dw: usize,
        need: Vec<f32>,
        /// Sliding-min candidate indices as a fixed-capacity ring
        /// (look + 2 slots, head + len): the window never holds more
        /// than look + 1 candidates, so process() never allocates.
        dq: Vec<usize>,
        dq_head: usize,
        dq_len: usize,
        held: Vec<f32>,
        acc: f64,
        g: f32,
        in_pos: usize,
    },
    /// Loudness maximizer (RT6): fx_sc::maximizer chunked. Drive into
    /// an la = 66-sample lookahead: required gain over the last `la`
    /// inputs (fixed ring, exact min), instant-attack / exp-release
    /// hold, box average over `la` with an f64 accumulator and the
    /// same resync on idx wrap, audio delayed la - 1 = 65 samples.
    /// Silence past the render end flushes the tail, matching the
    /// offline la - 1 silent frames.
    Maximizer {
        drive: f32,
        ceil: f32,
        rel: f32,
        la: usize,
        dl: Vec<f32>,
        dr: Vec<f32>,
        dw: usize,
        req: Vec<f32>,
        /// Fixed-capacity ring (la + 1 slots) of min candidates - the
        /// window never holds more than `la`, so process() never
        /// allocates.
        dq: Vec<usize>,
        dq_head: usize,
        dq_len: usize,
        held: f32,
        sum: f64,
        smooth: Vec<f32>,
        in_pos: usize,
    },
    /// Transient shaper (RT7): the Effect::Transient arm chunked.
    /// Dual fast/slow envelopes over the stereo abs-max detector are
    /// carried across blocks; constants and the gain curve are
    /// verbatim from fx.rs, zero lookahead, so latency is 0.
    Transient {
        fast: f32,
        slow: f32,
        fast_c: f32,
        slow_c: f32,
        attack: f32,
        sustain: f32,
    },
    /// Noise gate (RT8): fx_extra::gate chunked. Peak-hold detector,
    /// hold counter and attack/release smoothing carried across
    /// blocks; constants and state seeds (env 0, g = floor) verbatim
    /// from the offline pass, zero lookahead, so latency is 0.
    Gate {
        env: f32,
        g: f32,
        held: usize,
        thr: f32,
        floor: f32,
        att: f32,
        rel: f32,
        det: f32,
        hold: usize,
    },
    /// Soft clipper (RT9): fx_extra::soft_clipper chunked. Per-sample
    /// tanh shaper with the 2x-oversample midpoint filter carried
    /// (prev / prev_mid_out per channel), constants verbatim, zero
    /// lookahead, so latency is 0.
    SoftClipper {
        c: f32,
        t: f32,
        drive: f32,
        mix: f32,
        oversample: bool,
        prev_l: f32,
        prev_r: f32,
        pmo_l: f32,
        pmo_r: f32,
    },
    /// Phaser (RT11): fx_extra::phaser chunked. Allpass stage state
    /// (x1/y1), the feedback sample and the current coefficient are
    /// carried per channel; the LFO-driven coefficient updates on the
    /// song's 16-sample grid via `pos`, exactly as the offline's
    /// offset rule (the streamed chain always starts at sample 0, so
    /// the grid update at k % 16 == 0 covers the offline's i == 0
    /// case). Zero lookahead, so latency is 0.
    Phaser {
        x1: [Vec<f32>; 2],
        y1: [Vec<f32>; 2],
        last: [f32; 2],
        a: [f32; 2],
        rate: f32,
        stages: usize,
        mix: f32,
        fb: f32,
        center_hz: f32,
        depth: f32,
    },
    /// Flanger (RT12): fx_extra::flanger chunked. The modulated delay
    /// line is one ring per channel indexed by the absolute song
    /// position (w = k % max, read before write), so a persistent
    /// ring matches the offline's freshly zeroed buffer exactly:
    /// slot w holds the value written max samples ago, zeros before.
    /// Zero lookahead, so latency is 0.
    Flanger {
        buf: [Vec<f32>; 2],
        rate: f32,
        delay_ms: f32,
        depth_ms: f32,
        mix: f32,
        fb: f32,
    },
    /// Autopan (RT13): fx_extra::autopan chunked. Stateless
    /// per-sample gain math on the absolute song position
    /// (ph = ((i + pos) / period).fract()), so chunked and
    /// whole-song agree exactly. Zero latency.
    Autopan {
        period: f32,
        depth: f32,
        tremolo: bool,
    },
    /// Ring mod (RT14): fx_mod::ring_mod_at chunked. The carrier
    /// phase is carried per channel instead of replayed from sample
    /// 0 (the offline's offset replay loop); advance applies the
    /// same f32 ops in the same order, so the phase sequence is
    /// bit-identical. Zero latency.
    RingMod {
        ph: [f32; 2],
        shape: String,
        inc: f32,
        lfo_hz: f32,
        lfo_depth: f32,
        mix: f32,
    },
    /// Haas (RT15): fx_sat::haas chunked. The high-pass biquad state
    /// and the delayed hi signal ride a ring of d slots seeded with
    /// zeros, matching the offline's i < d guard. Only the delayed
    /// side is touched, as offline. Zero latency.
    Haas {
        hp: Biquad,
        ring: Vec<f32>,
        idx: usize,
        mix: f32,
        lvl: f32,
        right: bool,
    },
    /// Deesser (RT16): fx_extra::deesser chunked. The complementary
    /// high-split biquads and the shared detector envelope are
    /// carried; coef() and the gain law verbatim. Zero latency.
    Deesser {
        lp: [Biquad; 2],
        env: f32,
        att: f32,
        rel: f32,
        thr: f32,
        range: f32,
    },
    /// Multiband (RT17): fx_extra::multiband chunked. The split3
    /// cascades (two HighCut biquads at f_lo, two at f_hi on the
    /// residual, per channel) and the three band detector envelopes
    /// are carried; each detector sees its band's samples in song
    /// order, so the per-sample band interleave changes nothing.
    /// Zero latency.
    Multiband {
        split: [[Biquad; 4]; 2],
        env: [f32; 3],
        att: f32,
        rel: f32,
        thr: [f32; 3],
        ratio: [f32; 3],
        mk: [f32; 3],
        mix: f32,
    },
    /// Dynamic EQ (RT18): fx_extra::dynamic_eq chunked (the unkeyed
    /// path; a keyed band stays on the buffered pass). The two
    /// band-split biquads and the shared detector envelope are
    /// carried; coef() and the cut/boost law verbatim. Zero latency.
    DynamicEq {
        bp: [Biquad; 2],
        env: f32,
        att: f32,
        rel: f32,
        thr: f32,
        ratio: f32,
        range: f32,
    },
    /// Cabinet IR (RT10): fx_amp::cab_ir's convolve - the uniformly
    /// partitioned FFT convolver in fx_extra (B = 2048) - carried
    /// across blocks. IR loading (the global cache mutex) and the IR
    /// part FFTs happen at build, never on the audio path; every
    /// buffer is preallocated, so process_block never allocates. A
    /// block's convolver output is known only once its 2048 input
    /// samples have arrived, so the wet path and the dry leg of the
    /// wet/dry mix ride a constant B - 1 = 2047-sample delay:
    /// latency 2047 reported to PDC, bit-identical to the offline
    /// pass, and the tail past `total` is truncated exactly like the
    /// offline `out.len() == n`. Render-path only: a live monitoring
    /// chain must keep the buffered Effect::process path so
    /// monitoring never pays the 46 ms.
    CabIr(Box<CabIrArm>),
    /// cab_ir with a missing/unloadable IR: the offline pass returns
    /// early (a no-op), so the streamed arm passes audio through
    /// untouched and adds no latency.
    CabIrNop,
    /// freq_splitter (RT19): fx_extra::freq_splitter chunked - the two
    /// windowed-sinc FIR crossovers run as partitioned convolvers (the
    /// CabIr machinery, B = 2048), the complement delay line is
    /// (B - 1) + m. Latency 2047 reported to PDC, bit-identical to the
    /// offline pass.
    FreqSplit(Box<FreqSplitArm>),
    /// granular insert: the offline scheduler as a per-sample state
    /// machine (fx_extra::GranularCore), zero latency, every buffer
    /// preallocated at build; bit-identical to the offline pass at any
    /// block size.
    Granular(Box<crate::fx_extra::GranularCore>),
    /// pitch_shift: fx_extra::PitchShiftCore, the offline dual-tap delay
    /// as a per-sample state machine, zero latency, rings allocated at
    /// build; bit-identical to the offline pass at any block size.
    PitchShift(Box<crate::fx_extra::PitchShiftCore>),
    /// stutter (gate/stutter/half-time/tape-stop): fx_extra::StutterCore,
    /// the offline grid/ring math as a per-sample state machine; zero
    /// latency. Reverse stays one-shot (reads future samples).
    Stutter(Box<crate::fx_extra::StutterCore>),
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
    /// sc_dsp FDN reverb modes (room/plate/hall): the same Fdn the
    /// offline pass runs - Fdn::process is block-chunkable by design,
    /// so a chunked render is the full render bit for bit.
    ReverbFdn(Box<crate::sc_dsp::Fdn>),
    /// fx_vocoder: the same VocoderCore the offline pass runs (no
    /// lookahead, zero latency).
    Vocoder(Box<crate::fx_vocoder::VocoderCore>),
    /// fx_sat: the same SaturatorCore the offline pass runs (no
    /// lookahead, zero latency).
    Saturator(Box<crate::fx_sat::SaturatorCore>),
}

impl StreamFx {
    /// Algorithmic latency in samples (RT4): only the lookahead path
    /// delays the signal; every other arm is zero-latency.
    pub(crate) fn latency(&self) -> usize {
        match self {
            StreamFx::CompressorLa { la, .. } => *la,
            StreamFx::Limiter { look, .. } => *look,
            StreamFx::Maximizer { la, .. } => la - 1,
            // partitioned-conv block size 2048 minus 1 (see CabIr)
            StreamFx::CabIr(_) => 2047,
            StreamFx::FreqSplit(_) => 2047,
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
            StreamFx::Limiter {
                ceiling,
                rel,
                look,
                dl,
                dr,
                dw,
                need,
                dq,
                dq_head,
                dq_len,
                held,
                acc,
                g,
                in_pos,
            } => {
                // limiter_pass_impl (true_peak = false) per sample,
                // verbatim: need = ceiling / peak where over, 1.0 else;
                // held = exact sliding min over the next `look` samples;
                // box-average over the previous `look`; instant attack,
                // exponential release; clamp. The deque window is complete
                // once input j = o + look arrives, so output o is emitted
                // `look` samples late and the audio rides the delay line.
                let size = *look + 2;
                for i in 0..l.len() {
                    let j = *in_pos;
                    let x = l[i].abs().max(r[i].abs());
                    let nd = if x > *ceiling { *ceiling / x } else { 1.0 };
                    need[j % size] = nd;
                    while *dq_len > 0 && need[dq[(*dq_head + *dq_len - 1) % size] % size] >= nd {
                        *dq_len -= 1;
                    }
                    dq[(*dq_head + *dq_len) % size] = j;
                    *dq_len += 1;
                    debug_assert!(*dq_len <= *look + 1, "limiter min-window ring overflow");
                    let (y_l, y_r) = if *look == 0 {
                        (l[i], r[i])
                    } else {
                        let y = (dl[*dw], dr[*dw]);
                        dl[*dw] = l[i];
                        dr[*dw] = r[i];
                        *dw += 1;
                        if *dw == *look {
                            *dw = 0;
                        }
                        y
                    };
                    if j >= *look {
                        let o = j - *look;
                        while *dq_len > 0 && dq[*dq_head] < o {
                            *dq_head = (*dq_head + 1) % size;
                            *dq_len -= 1;
                        }
                        let h = need[dq[*dq_head] % size];
                        held[o % size] = h;
                        *acc += h as f64;
                        if o > *look {
                            *acc -= held[(o - *look - 1) % size] as f64;
                        }
                        let cnt = (o.min(*look) + 1) as f64;
                        let target = (*acc / cnt) as f32;
                        *g = if target < *g {
                            target
                        } else {
                            *rel * *g + (1.0 - *rel) * target
                        };
                        l[i] = (y_l * *g).clamp(-*ceiling, *ceiling);
                        r[i] = (y_r * *g).clamp(-*ceiling, *ceiling);
                    } else {
                        l[i] = 0.0;
                        r[i] = 0.0;
                    }
                    *in_pos += 1;
                }
            }
            StreamFx::Maximizer {
                drive,
                ceil,
                rel,
                la,
                dl,
                dr,
                dw,
                req,
                dq,
                dq_head,
                dq_len,
                held,
                sum,
                smooth,
                in_pos,
            } => {
                // fx_sc::maximizer per sample, verbatim: drive into
                // peak, req = ceiling / peak where over, exact min of
                // the last la req values, hold smoother (instant
                // attack, exp release), f64 box average with the
                // resync on idx wrap, g clamped to [0, 1], output
                // la - 1 samples late and clamped to the ceiling.
                let dly = *la - 1;
                let cap = *la + 1;
                for i in 0..l.len() {
                    let j = *in_pos;
                    let xl = (if l[i].is_finite() { l[i] } else { 0.0 }) * *drive;
                    let xr = (if r[i].is_finite() { r[i] } else { 0.0 }) * *drive;
                    let (yl, yr) = if dly == 0 {
                        (xl, xr)
                    } else {
                        let y = (dl[*dw], dr[*dw]);
                        dl[*dw] = xl;
                        dr[*dw] = xr;
                        *dw += 1;
                        if *dw == dly {
                            *dw = 0;
                        }
                        y
                    };
                    let peak = xl.abs().max(xr.abs());
                    let rq = if peak > *ceil { *ceil / peak } else { 1.0 };
                    req[j % *la] = rq;
                    while *dq_len > 0 && req[dq[(*dq_head + *dq_len - 1) % cap] % *la] >= rq {
                        *dq_len -= 1;
                    }
                    dq[(*dq_head + *dq_len) % cap] = j;
                    *dq_len += 1;
                    debug_assert!(*dq_len <= *la, "maximizer min-window ring overflow");
                    while *dq_len > 0 && dq[*dq_head] + *la <= j {
                        *dq_head = (*dq_head + 1) % cap;
                        *dq_len -= 1;
                    }
                    let hold = if *dq_len > 0 {
                        req[dq[*dq_head] % *la]
                    } else {
                        1.0
                    };
                    *held = if hold < *held {
                        hold
                    } else {
                        hold + (*held - hold) * *rel
                    };
                    let idx = j % *la;
                    *sum += f64::from(*held) - f64::from(smooth[idx]);
                    smooth[idx] = *held;
                    if idx + 1 == *la {
                        *sum = smooth.iter().map(|&v| f64::from(v)).sum();
                    }
                    let g = ((*sum / *la as f64) as f32).clamp(0.0, 1.0);
                    if j + 1 >= *la {
                        l[i] = (yl * g).clamp(-*ceil, *ceil);
                        r[i] = (yr * g).clamp(-*ceil, *ceil);
                    } else {
                        l[i] = 0.0;
                        r[i] = 0.0;
                    }
                    *in_pos += 1;
                }
            }
            StreamFx::Transient {
                fast,
                slow,
                fast_c,
                slow_c,
                attack,
                sustain,
            } => {
                for i in 0..l.len() {
                    let x = l[i].abs().max(r[i].abs());
                    *fast = *fast_c * *fast + (1.0 - *fast_c) * x;
                    *slow = *slow_c * *slow + (1.0 - *slow_c) * x;
                    let diff = (*fast - *slow) / (*slow + 1e-4);
                    let g = if diff > 0.0 {
                        1.0 + *attack * diff.min(2.0)
                    } else {
                        1.0 + *sustain * (-diff).min(1.0)
                    };
                    let g = g.clamp(0.1, 3.0);
                    l[i] *= g;
                    r[i] *= g;
                }
            }
            StreamFx::Gate {
                env,
                g,
                held,
                thr,
                floor,
                att,
                rel,
                det,
                hold,
            } => {
                for i in 0..l.len() {
                    let x = l[i].abs().max(r[i].abs());
                    *env = if x > *env {
                        x
                    } else {
                        *det * *env + (1.0 - *det) * x
                    };
                    let target = if *env >= *thr {
                        *held = *hold;
                        1.0
                    } else if *held > 0 {
                        *held -= 1;
                        1.0
                    } else {
                        *floor
                    };
                    let c = if target > *g { *att } else { *rel };
                    *g = c * *g + (1.0 - c) * target;
                    l[i] *= *g;
                    r[i] *= *g;
                }
            }
            StreamFx::SoftClipper {
                c,
                t,
                drive,
                mix,
                oversample,
                prev_l,
                prev_r,
                pmo_l,
                pmo_r,
            } => {
                // soft_clip_sample inlined verbatim from fx_extra.rs
                let clip = |x: f32, t: f32, c: f32| -> f32 {
                    let a = x.abs();
                    if a <= t {
                        x
                    } else {
                        let room = (c - t).max(1e-4);
                        x.signum() * (t + room * ((a - t) / room).tanh())
                    }
                };
                for (chi, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let (prev, prev_mid_out) = if chi == 0 {
                        (&mut *prev_l, &mut *pmo_l)
                    } else {
                        (&mut *prev_r, &mut *pmo_r)
                    };
                    for s in ch.iter_mut() {
                        let x = *s * *drive;
                        let wet = if *oversample {
                            // 2x: clip the midpoint and the sample, then
                            // average back down
                            let mid = 0.5 * (*prev + x);
                            let ym = clip(mid, *t, *c);
                            let y = clip(x, *t, *c);
                            let out = 0.25 * *prev_mid_out + 0.5 * ym + 0.25 * y;
                            *prev_mid_out = y;
                            out.clamp(-*c, *c)
                        } else {
                            clip(x, *t, *c)
                        };
                        *prev = x;
                        *s = *s * (1.0 - *mix) + wet * *mix;
                    }
                }
            }
            StreamFx::Phaser {
                x1,
                y1,
                last,
                a,
                rate,
                stages,
                mix,
                fb,
                center_hz,
                depth,
            } => {
                use std::f32::consts::PI;
                for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let phase0 = ci as f32 * 0.25;
                    for (i, s) in ch.iter_mut().enumerate() {
                        // coefficients update on the song's 16-sample
                        // grid; pos is the block's first sample in the
                        // song, as the offline's `offset`
                        let k = i + pos;
                        if k % 16 == 0 {
                            let k16 = k - k % 16;
                            let lfo =
                                0.5 + 0.5 * (2.0 * PI * (*rate * k16 as f32 / SR + phase0)).sin();
                            let f = (*center_hz * 2f32.powf((lfo - 0.5) * 4.0 * *depth))
                                .clamp(40.0, 16000.0);
                            let t = (PI * f / SR).tan();
                            a[ci] = (t - 1.0) / (t + 1.0);
                        }
                        let mut v = *s + last[ci] * *fb;
                        for k in 0..*stages {
                            let y = a[ci] * v + x1[ci][k] - a[ci] * y1[ci][k];
                            x1[ci][k] = v;
                            y1[ci][k] = y;
                            v = y;
                        }
                        last[ci] = v;
                        *s = *s * (1.0 - *mix * 0.5) + v * *mix * 0.5;
                        if !s.is_finite() {
                            *s = 0.0;
                        }
                    }
                }
            }
            StreamFx::Flanger {
                buf,
                rate,
                delay_ms,
                depth_ms,
                mix,
                fb,
            } => {
                use std::f32::consts::PI;
                for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    let phase0 = ci as f32 * 0.25;
                    let max = buf[ci].len();
                    for (i, s) in ch.iter_mut().enumerate() {
                        // k is the absolute song position, the
                        // offline's i + offset; the ring is read
                        // before it is written, exactly as offline
                        let k = i + pos;
                        let lfo = 0.5 + 0.5 * (2.0 * PI * (*rate * k as f32 / SR + phase0)).sin();
                        let d =
                            ((delay_ms.max(0.05) + depth_ms.max(0.0) * lfo) * 0.001 * SR).max(1.0);
                        let w = k % max;
                        let p0 = (w as f32 - d).rem_euclid(max as f32);
                        let i0 = p0 as usize % max;
                        let i1 = (i0 + 1) % max;
                        let f = p0 - p0.floor();
                        let delayed = buf[ci][i0] * (1.0 - f) + buf[ci][i1] * f;
                        buf[ci][w] = *s + delayed * *fb;
                        *s = *s * (1.0 - *mix * 0.5) + delayed * *mix * 0.7;
                    }
                }
            }
            StreamFx::Autopan {
                period,
                depth,
                tremolo,
            } => {
                use std::f32::consts::PI;
                for i in 0..l.len() {
                    // ph reads the absolute song position, as the
                    // offline's (i + offset)
                    let ph = ((i + pos) as f32 / *period).fract();
                    let lfo = (2.0 * PI * ph).sin();
                    if *tremolo {
                        let g = 1.0 - *depth * (0.5 - 0.5 * lfo);
                        l[i] *= g;
                        r[i] *= g;
                    } else {
                        let pan = lfo * *depth;
                        let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                        let (gl, gr) = (
                            angle.cos() * std::f32::consts::SQRT_2,
                            angle.sin() * std::f32::consts::SQRT_2,
                        );
                        let m = 0.5 * (l[i] + r[i]);
                        let s = 0.5 * (l[i] - r[i]);
                        l[i] = (m + s) * gl;
                        r[i] = (m - s) * gr;
                    }
                }
            }
            StreamFx::RingMod {
                ph,
                shape,
                inc,
                lfo_hz,
                lfo_depth,
                mix,
            } => {
                use std::f32::consts::PI;
                let step_at = |i: usize| {
                    let semis = if *lfo_hz > 0.0 {
                        *lfo_depth * (2.0 * PI * *lfo_hz * i as f32 / SR).sin()
                    } else {
                        0.0
                    };
                    *inc * 2f32.powf(semis / 12.0)
                };
                for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
                    for (i, x) in ch.iter_mut().enumerate() {
                        let step = step_at(i + pos);
                        let c = crate::fx_mod::carrier(shape, ph[ci]);
                        *x = *x * (1.0 - *mix) + *x * c * *mix;
                        ph[ci] += step;
                        if ph[ci] >= 1.0 {
                            ph[ci] -= ph[ci].floor();
                        }
                    }
                }
            }
            StreamFx::Haas {
                hp,
                ring,
                idx,
                mix,
                lvl,
                right,
            } => {
                let src: &mut [f32] = if *right { &mut *r } else { &mut *l };
                for x in src.iter_mut() {
                    // read before write: the ring slot still holds the
                    // hi value from d song samples ago (0.0 for the
                    // first d samples), as the offline's i < d guard
                    let dry = *x;
                    let hi = hp.process(dry);
                    let delayed = ring[*idx];
                    ring[*idx] = hi;
                    *idx = (*idx + 1) % ring.len();
                    let wet = (dry - hi) + delayed * *lvl;
                    *x = dry * (1.0 - *mix) + wet * *mix;
                }
            }
            StreamFx::Deesser {
                lp,
                env,
                att,
                rel,
                thr,
                range,
            } => {
                for i in 0..l.len() {
                    let hl = l[i] - lp[0].process(l[i]);
                    let hr = r[i] - lp[1].process(r[i]);
                    let x = hl.abs().max(hr.abs());
                    let c = if x > *env { *att } else { *rel };
                    *env = c * *env + (1.0 - c) * x;
                    let over = (gain_to_db(*env) - *thr).max(0.0);
                    let db = (-over * 0.75).max(*range);
                    let g = db_to_gain(db) - 1.0;
                    l[i] += hl * g;
                    r[i] += hr * g;
                }
            }
            StreamFx::Multiband {
                split,
                env,
                att,
                rel,
                thr,
                ratio,
                mk,
                mix,
            } => {
                for i in 0..l.len() {
                    let xl = l[i];
                    let xr = r[i];
                    // split3, per channel, verbatim
                    let [la1, la2, lb1, lb2] = &mut split[0];
                    let lo_l = la2.process(la1.process(xl));
                    let rest_l = xl - lo_l;
                    let mid_l = lb2.process(lb1.process(rest_l));
                    let hi_l = rest_l - mid_l;
                    let [ra1, ra2, rb1, rb2] = &mut split[1];
                    let lo_r = ra2.process(ra1.process(xr));
                    let rest_r = xr - lo_r;
                    let mid_r = rb2.process(rb1.process(rest_r));
                    let hi_r = rest_r - mid_r;
                    let bl = [lo_l, mid_l, hi_l];
                    let br = [lo_r, mid_r, hi_r];
                    let mut out_l = 0.0f32;
                    let mut out_r = 0.0f32;
                    for b in 0..3 {
                        // fx_extra::Comp::gain verbatim (soft 6 dB
                        // knee), then * mk as the offline band loop
                        let x = bl[b].abs().max(br[b].abs());
                        let c = if x > env[b] { *att } else { *rel };
                        env[b] = c * env[b] + (1.0 - c) * x;
                        let over = gain_to_db(env[b]) - thr[b];
                        let knee = 6.0;
                        let gr = if over <= -knee / 2.0 {
                            0.0
                        } else if over < knee / 2.0 {
                            (1.0 - 1.0 / ratio[b]) * (over + knee / 2.0).powi(2) / (2.0 * knee)
                        } else {
                            over * (1.0 - 1.0 / ratio[b])
                        };
                        let g = db_to_gain(-gr) * mk[b];
                        out_l += bl[b] * g;
                        out_r += br[b] * g;
                    }
                    l[i] = xl * (1.0 - *mix) + out_l * *mix;
                    r[i] = xr * (1.0 - *mix) + out_r * *mix;
                }
            }
            StreamFx::DynamicEq {
                bp,
                env,
                att,
                rel,
                thr,
                ratio,
                range,
            } => {
                for i in 0..l.len() {
                    let bl = bp[0].process(l[i]);
                    let br = bp[1].process(r[i]);
                    let x = bl.abs().max(br.abs());
                    let c = if x > *env { *att } else { *rel };
                    *env = c * *env + (1.0 - c) * x;
                    let over = (gain_to_db(*env) - *thr).max(0.0);
                    let amount = over * (1.0 - 1.0 / *ratio);
                    // cut (range < 0) or boost (range > 0) the band,
                    // at most |range| dB
                    let db = if *range < 0.0 {
                        -amount.min(-*range)
                    } else {
                        amount.min(*range)
                    };
                    let g = db_to_gain(db) - 1.0;
                    l[i] += bl * g;
                    r[i] += br * g;
                }
            }
            StreamFx::CabIr(arm) => {
                let CabIrArm {
                    fft,
                    k,
                    parts_re,
                    parts_im,
                    fdl_re,
                    fdl_im,
                    inbuf,
                    outring,
                    overlap,
                    head,
                    infill,
                    acc_re,
                    acc_im,
                    dry,
                    dw,
                    g,
                    mix,
                    in_pos,
                    total,
                } = &mut **arm;
                const B: usize = 2048;
                for i in 0..l.len() {
                    // push this sample into both block buffers; a full
                    // block (or the final partial one at `total`) runs
                    // the convolver exactly as fx_extra::convolve does
                    inbuf[0][*infill] = l[i];
                    inbuf[1][*infill] = r[i];
                    *infill += 1;
                    *in_pos += 1;
                    let last = *in_pos == *total;
                    if *infill == B || (last && *infill > 0) {
                        let fill = *infill;
                        let base = *in_pos - fill;
                        if base < *total {
                            for c in 0..2 {
                                {
                                    let (re, im) = (&mut fdl_re[c][*head], &mut fdl_im[c][*head]);
                                    re.iter_mut().for_each(|v| *v = 0.0);
                                    im.iter_mut().for_each(|v| *v = 0.0);
                                    re[..fill].copy_from_slice(&inbuf[c][..fill]);
                                    fft.forward(re, im);
                                }
                                acc_re.iter_mut().for_each(|v| *v = 0.0);
                                acc_im.iter_mut().for_each(|v| *v = 0.0);
                                for j in 0..*k {
                                    let slot = (*head + *k - j) % *k;
                                    let (xr, xi) = (&fdl_re[c][slot], &fdl_im[c][slot]);
                                    let (hr, hi) = (&parts_re[c][j], &parts_im[c][j]);
                                    for t in 0..2 * B {
                                        acc_re[t] += xr[t] * hr[t] - xi[t] * hi[t];
                                        acc_im[t] += xr[t] * hi[t] + xi[t] * hr[t];
                                    }
                                }
                                fft.inverse(acc_re, acc_im);
                                for t in 0..B {
                                    outring[c][(base + t) % (2 * B)] = acc_re[t] + overlap[c][t];
                                }
                                overlap[c].copy_from_slice(&acc_re[B..]);
                            }
                            *head = (*head + 1) % *k;
                        }
                        *infill = 0;
                    }
                    // emit: wet and dry both delayed B - 1, so the mix
                    // still pairs the same sample indices the offline
                    // pass pairs
                    let q = in_pos.wrapping_sub(B);
                    let (wl, wr) = if *in_pos >= B && q < *total {
                        (outring[0][q % (2 * B)], outring[1][q % (2 * B)])
                    } else {
                        (0.0, 0.0)
                    };
                    let dl = dry[0][*dw];
                    dry[0][*dw] = l[i];
                    let dr = dry[1][*dw];
                    dry[1][*dw] = r[i];
                    *dw = (*dw + 1) % (B - 1);
                    l[i] = (wl * *g) * *mix + dl * (1.0 - *mix);
                    r[i] = (wr * *g) * *mix + dr * (1.0 - *mix);
                }
            }
            StreamFx::Granular(core) => {
                if core.mix > 0.0 {
                    for i in 0..l.len() {
                        let (a, b) = core.step(l[i], r[i]);
                        l[i] = a;
                        r[i] = b;
                    }
                }
            }
            StreamFx::PitchShift(core) => {
                if core.mix > 0.0 {
                    for i in 0..l.len() {
                        let (a, b) = core.step(l[i], r[i]);
                        l[i] = a;
                        r[i] = b;
                    }
                }
            }
            StreamFx::Stutter(core) => {
                if core.mix > 0.0 {
                    for i in 0..l.len() {
                        let (a, b) = core.step(pos + i, l[i], r[i]);
                        l[i] = a;
                        r[i] = b;
                    }
                }
            }
            StreamFx::FreqSplit(arm) => {
                let FreqSplitArm {
                    conv1,
                    conv2,
                    xd,
                    xw,
                    dry,
                    dw,
                    band,
                    g,
                    mix,
                    in_pos,
                    total,
                } = &mut **arm;
                const B: usize = 2048;
                for i in 0..l.len() {
                    *in_pos += 1;
                    let ip = *in_pos;
                    if let Some(c1) = conv1 {
                        c1.push(l[i], r[i], ip, *total);
                    }
                    if let Some(c2) = conv2 {
                        c2.push(l[i], r[i], ip, *total);
                    }
                    // emit delayed B - 1, pairing the same sample
                    // indices the offline pass pairs
                    let q = ip.wrapping_sub(B);
                    let have = ip >= B && q < *total;
                    let (w1, w2) = if have {
                        (
                            conv1.as_ref().map(|c| c.read(0, q, *total)),
                            conv2.as_ref().map(|c| c.read(0, q, *total)),
                        )
                    } else {
                        (None, None)
                    };
                    let (w1r, w2r) = if have {
                        (
                            conv1.as_ref().map(|c| c.read(1, q, *total)),
                            conv2.as_ref().map(|c| c.read(1, q, *total)),
                        )
                    } else {
                        (None, None)
                    };
                    // delayed input for the high-band complement
                    let xl_d = xd[0][*xw];
                    let xr_d = xd[1][*xw];
                    xd[0][*xw] = l[i];
                    xd[1][*xw] = r[i];
                    *xw = (*xw + 1) % xd[0].len();
                    // priming (first B - 1 outputs) is silence, like
                    // cab_ir: the convolvers have not emitted yet and
                    // the offline pass has no negative indices
                    let wet_l = if !have {
                        0.0
                    } else {
                        match band {
                            crate::fx::FreqSplitBand::Low => w1.unwrap_or(0.0),
                            crate::fx::FreqSplitBand::Mid => w2.unwrap_or(0.0) - w1.unwrap_or(0.0),
                            crate::fx::FreqSplitBand::High => xl_d - w2.unwrap_or(0.0),
                        }
                    };
                    let wet_r = if !have {
                        0.0
                    } else {
                        match band {
                            crate::fx::FreqSplitBand::Low => w1r.unwrap_or(0.0),
                            crate::fx::FreqSplitBand::Mid => {
                                w2r.unwrap_or(0.0) - w1r.unwrap_or(0.0)
                            }
                            crate::fx::FreqSplitBand::High => xr_d - w2r.unwrap_or(0.0),
                        }
                    };
                    let dl = dry[0][*dw];
                    dry[0][*dw] = l[i];
                    let dr = dry[1][*dw];
                    dry[1][*dw] = r[i];
                    *dw = (*dw + 1) % (B - 1);
                    l[i] = (wet_l * *g) * *mix + dl * (1.0 - *mix);
                    r[i] = (wet_r * *g) * *mix + dr * (1.0 - *mix);
                }
            }
            StreamFx::Vocoder(core) => {
                // fx_vocoder::VocoderCore::step per sample: the same
                // state machine the offline pass runs, so a chunked
                // render is the full render bit for bit.
                for i in 0..l.len() {
                    let (a, b) = core.step(l[i], r[i]);
                    l[i] = a;
                    r[i] = b;
                }
            }
            StreamFx::CabIrNop => {}
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
            StreamFx::Saturator(core) => {
                for (ch, buf) in [l, r].into_iter().enumerate() {
                    for x in buf.iter_mut() {
                        *x = core.step(ch, *x);
                    }
                }
            }
            StreamFx::ReverbFdn(core) => {
                // sc_dsp::Fdn::process: block-chunkable by design (all
                // state in self), so chunked blocks equal the offline
                // whole-buffer pass bit for bit.
                core.process(l, r);
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
        Effect::Reverb(_) => Streamable,
        // OneShot, one-line reason each (latency in samples for RT4 PDC
        // comes from oneshot_latency_samples):
        Effect::Compressor(_) => Streamable,
        Effect::Limiter(p) if !p.true_peak => Streamable,
        Effect::Limiter(_) => OneShot("true-peak limiter: whole-buffer iterative passes"),
        Effect::Maximizer(_) => Streamable,
        Effect::Convolution(_) => OneShot("IR convolution: block processing"),
        Effect::CabIr(_) => Streamable,
        Effect::PitchShift(_) => Streamable,
        Effect::GrossBeat(_) => OneShot("gross beat: time-windowed"),
        Effect::Stutter(p) if p.mode != crate::fx::StutterMode::Reverse => Streamable,
        Effect::Stutter(_) => OneShot("reverse stutter: reads future samples in the cycle"),
        Effect::Vocoder(_) => Streamable,
        Effect::Multiband(_) => Streamable,
        Effect::DynamicEq(p) if p.source.is_empty() => Streamable,
        Effect::DynamicEq(_) => OneShot("dynamic EQ: keyed"),
        Effect::Deesser(_) => Streamable,
        Effect::SoftClipper(_) => Streamable,
        Effect::Gate(_) => Streamable,
        Effect::Transient(_) => Streamable,
        Effect::Phaser(_) => Streamable,
        Effect::Flanger(_) => Streamable,
        Effect::Autopan(_) => Streamable,
        Effect::Saturator(_) => Streamable,
        Effect::Haas(_) => Streamable,
        Effect::FreqShift(_) => OneShot("freq shift: unverified"),
        Effect::RingMod(_) => Streamable,
        Effect::Granular(_) => Streamable,
        Effect::FreqSplitter(_) => Streamable,
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
                Effect::Limiter(p) => {
                    // limiter_pass (true_peak = false), chunked. Constants
                    // verbatim from fx.rs: look = 0.002 * SR, ceiling
                    // clamped to <= 0 dB, release from max(1 ms).
                    let look = (0.002 * SR) as usize;
                    StreamFx::Limiter {
                        ceiling: db_to_gain(p.ceiling_db.min(0.0)),
                        rel: (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp(),
                        look,
                        dl: vec![0.0; look],
                        dr: vec![0.0; look],
                        dw: 0,
                        need: vec![0.0; look + 2],
                        dq: vec![0; look + 2],
                        dq_head: 0,
                        dq_len: 0,
                        held: vec![0.0; look + 2],
                        acc: 0.0,
                        g: 1.0,
                        in_pos: 0,
                    }
                }
                Effect::Maximizer(p) => {
                    // fx_sc::maximizer, chunked. Constants verbatim:
                    // lookahead 1.5 ms -> la = 66 at 44.1 kHz, drive
                    // from -threshold_db, ceiling clamped to <= 0 dB,
                    // release in samples.
                    let la = ((1.5f32 * 0.001 * SR).max(0.0).round() as usize).max(1);
                    let rn = (p.release_ms.clamp(1.0, 1000.0) * 0.001 * SR).max(0.0);
                    let rel = if rn < 1.0e-3 { 0.0 } else { (-1.0 / rn).exp() };
                    StreamFx::Maximizer {
                        drive: db_to_gain(-p.threshold_db.clamp(-30.0, 0.0)),
                        ceil: db_to_gain(p.ceiling_db.clamp(-30.0, 0.0)),
                        rel,
                        la,
                        dl: vec![0.0; la - 1],
                        dr: vec![0.0; la - 1],
                        dw: 0,
                        req: vec![1.0; la],
                        dq: vec![0; la + 1],
                        dq_head: 0,
                        dq_len: 0,
                        held: 1.0,
                        sum: la as f64,
                        smooth: vec![1.0; la],
                        in_pos: 0,
                    }
                }
                Effect::Transient(p) => {
                    // Effect::Transient (fx.rs), chunked: constants
                    // verbatim, envelopes start at 0.0 as offline.
                    StreamFx::Transient {
                        fast: 0.0,
                        slow: 0.0,
                        fast_c: (-1.0 / (0.001 * SR)).exp(),
                        slow_c: (-1.0 / (0.03 * SR)).exp(),
                        attack: p.attack.clamp(-1.0, 1.0),
                        sustain: p.sustain.clamp(-1.0, 1.0),
                    }
                }
                Effect::Gate(p) => {
                    // fx_extra::gate, chunked: thr/floor/coef() and the
                    // initial state (env 0.0, g = floor, held 0)
                    // verbatim from the offline pass.
                    let floor = db_to_gain(p.range_db.min(0.0));
                    StreamFx::Gate {
                        env: 0.0,
                        g: floor,
                        held: 0,
                        thr: db_to_gain(p.threshold_db),
                        floor,
                        att: (-1.0 / (p.attack_ms.max(0.05) * 0.001 * SR)).exp(),
                        rel: (-1.0 / (p.release_ms.max(0.05) * 0.001 * SR)).exp(),
                        det: (-1.0 / (1.0_f32.max(0.05) * 0.001 * SR)).exp(),
                        hold: (p.hold_ms.max(0.0) * 0.001 * SR) as usize,
                    }
                }
                Effect::SoftClipper(p) => {
                    // fx_extra::soft_clipper, chunked: c/t/drive/mix
                    // and the zeroed oversample filter state verbatim.
                    let c = db_to_gain(p.ceiling_db.min(0.0));
                    StreamFx::SoftClipper {
                        c,
                        t: db_to_gain(p.threshold_db.min(p.ceiling_db - 0.1)).min(c * 0.999),
                        drive: db_to_gain(p.drive_db.clamp(-12.0, 24.0)),
                        mix: p.mix.clamp(0.0, 1.0),
                        oversample: p.oversample,
                        prev_l: 0.0,
                        prev_r: 0.0,
                        pmo_l: 0.0,
                        pmo_r: 0.0,
                    }
                }
                Effect::CabIr(p) => {
                    // fx_amp::cab_ir, chunked (RT10). The IR load (a
                    // global cache mutex) and every part FFT run here
                    // at build; a missing IR is the offline no-op.
                    const B: usize = 2048;
                    match crate::fx_amp::load_ir(&p.path, p.max_ms / 1000.0) {
                        None => StreamFx::CabIrNop,
                        Some(ir) => {
                            let fft = Fft::new(2 * B);
                            let mk_parts = |ir: &[f32]| -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
                                ir.chunks(B)
                                    .map(|c| {
                                        let mut re = vec![0.0f32; 2 * B];
                                        let mut im = vec![0.0f32; 2 * B];
                                        re[..c.len()].copy_from_slice(c);
                                        fft.forward(&mut re, &mut im);
                                        (re, im)
                                    })
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .unzip()
                            };
                            let (pl_re, pl_im) = mk_parts(&ir.0);
                            let rr = if ir.1.is_empty() { &ir.0 } else { &ir.1 };
                            let (pr_re, pr_im) = mk_parts(rr);
                            let k = pl_re.len();
                            StreamFx::CabIr(Box::new(CabIrArm {
                                fft: Fft::new(2 * B),
                                k,
                                parts_re: [pl_re, pr_re],
                                parts_im: [pl_im, pr_im],
                                fdl_re: [
                                    vec![vec![0.0f32; 2 * B]; k],
                                    vec![vec![0.0f32; 2 * B]; k],
                                ],
                                fdl_im: [
                                    vec![vec![0.0f32; 2 * B]; k],
                                    vec![vec![0.0f32; 2 * B]; k],
                                ],
                                inbuf: [vec![0.0f32; B], vec![0.0f32; B]],
                                outring: [vec![0.0f32; 2 * B], vec![0.0f32; 2 * B]],
                                overlap: [vec![0.0f32; B], vec![0.0f32; B]],
                                head: 0,
                                infill: 0,
                                acc_re: vec![0.0f32; 2 * B],
                                acc_im: vec![0.0f32; 2 * B],
                                dry: [vec![0.0f32; B - 1], vec![0.0f32; B - 1]],
                                dw: 0,
                                g: db_to_gain(p.gain_db),
                                mix: p.mix.clamp(0.0, 1.0),
                                in_pos: 0,
                                total,
                            }))
                        }
                    }
                }
                Effect::Granular(p) => {
                    // fx_extra::granular: the same core, ring/grain pool
                    // allocated here and never on the audio path
                    StreamFx::Granular(Box::new(crate::fx_extra::GranularCore::new(p, total)))
                }
                Effect::PitchShift(p) => {
                    // fx_extra::pitch_shift: the same core, rings
                    // allocated here and never on the audio path
                    StreamFx::PitchShift(Box::new(crate::fx_extra::PitchShiftCore::new(p)))
                }
                Effect::Stutter(p) => StreamFx::Stutter(Box::new(
                    crate::fx_extra::StutterCore::new(p, step_secs, total),
                )),
                Effect::FreqSplitter(p) => {
                    // fx_extra::freq_splitter, chunked (RT19): the same
                    // FIR design, convolvers built here (part FFTs at
                    // build, never on the audio path), rings zeroed as
                    // the offline i < m guard.
                    const B: usize = 2048;
                    let taps = (p.taps as usize).max(3) | 1;
                    let m = (taps - 1) / 2;
                    let lo = p.low_hz.min(p.high_hz);
                    let hi = p.high_hz.max(p.low_hz);
                    let band = p.band;
                    let conv1 = if band == crate::fx::FreqSplitBand::High {
                        None
                    } else {
                        let h = crate::fx_extra::lowpass_fir(lo, taps);
                        Some(Box::new(FirConvArm::new(&h, &h)))
                    };
                    let conv2 = if band == crate::fx::FreqSplitBand::Low {
                        None
                    } else {
                        let h = crate::fx_extra::lowpass_fir(hi, taps);
                        Some(Box::new(FirConvArm::new(&h, &h)))
                    };
                    StreamFx::FreqSplit(Box::new(FreqSplitArm {
                        conv1,
                        conv2,
                        xd: [vec![0.0f32; (B - 1) + m], vec![0.0f32; (B - 1) + m]],
                        xw: 0,
                        dry: [vec![0.0f32; B - 1], vec![0.0f32; B - 1]],
                        dw: 0,
                        band,
                        g: db_to_gain(p.gain_db),
                        mix: p.mix.clamp(0.0, 1.0),
                        in_pos: 0,
                        total,
                    }))
                }
                Effect::Vocoder(p) => {
                    // fx_vocoder: the same VocoderCore the offline pass
                    // runs, from sample 0 (offset 0, no carrier
                    // pre-warm). Unparseable notes make the offline pass
                    // return early; a unity gain is that bypass, bit for
                    // bit (x * 1.0 is exact, even for -0.0 and NaN).
                    match crate::fx_vocoder::VocoderCore::new(p) {
                        Some(core) => StreamFx::Vocoder(Box::new(core)),
                        None => StreamFx::Gain { g: 1.0 },
                    }
                }
                Effect::Phaser(p) => {
                    // fx_extra::phaser, chunked: lfo_rate() and the
                    // clamps verbatim, stage state zeroed as offline.
                    let stages = p.stages.clamp(2, 12) as usize;
                    StreamFx::Phaser {
                        x1: [vec![0.0f32; stages], vec![0.0f32; stages]],
                        y1: [vec![0.0f32; stages], vec![0.0f32; stages]],
                        last: [0.0; 2],
                        a: [0.0; 2],
                        rate: if p.sync_steps > 0.0 {
                            1.0 / (p.sync_steps * step_secs).max(1e-3)
                        } else {
                            p.rate_hz.max(0.0)
                        },
                        stages,
                        mix: p.mix.clamp(0.0, 1.0),
                        fb: p.feedback.clamp(-0.95, 0.95),
                        center_hz: p.center_hz,
                        depth: p.depth.clamp(0.0, 1.0),
                    }
                }
                Effect::Flanger(p) => {
                    // fx_extra::flanger, chunked: lfo_rate() and the
                    // clamps verbatim, rings zeroed as offline.
                    let max = ((p.delay_ms + p.depth_ms).max(0.1) * 0.001 * SR) as usize + 8;
                    StreamFx::Flanger {
                        buf: [vec![0.0f32; max], vec![0.0f32; max]],
                        rate: if p.sync_steps > 0.0 {
                            1.0 / (p.sync_steps * step_secs).max(1e-3)
                        } else {
                            p.rate_hz.max(0.0)
                        },
                        delay_ms: p.delay_ms,
                        depth_ms: p.depth_ms,
                        mix: p.mix.clamp(0.0, 1.0),
                        fb: p.feedback.clamp(-0.95, 0.95),
                    }
                }
                Effect::Autopan(p) => {
                    // fx_extra::autopan, chunked: period and depth
                    // verbatim; no state to seed.
                    StreamFx::Autopan {
                        period: (p.steps.max(0.25) * step_secs * SR).max(1.0),
                        depth: p.depth.clamp(0.0, 1.0),
                        tremolo: p.tremolo,
                    }
                }
                Effect::RingMod(p) => {
                    // fx_mod::ring_mod_at, chunked: phase seeds and
                    // clamps verbatim (the offline replays advance()
                    // over 0..offset; the streamed chain starts at
                    // sample 0, so carrying the phase is the same
                    // sequence).
                    StreamFx::RingMod {
                        ph: [0.0, p.stereo_phase.rem_euclid(1.0)],
                        shape: p.shape.clone(),
                        inc: p.freq_hz / SR,
                        lfo_hz: p.lfo_hz,
                        lfo_depth: p.lfo_depth,
                        mix: p.mix.clamp(0.0, 1.0),
                    }
                }
                Effect::Haas(p) => {
                    // fx_sat::haas, chunked: clamps verbatim, biquad
                    // seeded fresh and the ring zeroed as offline.
                    let d = ((p.delay_ms.clamp(1.0, 40.0) * 0.001 * SR) as usize).max(1);
                    StreamFx::Haas {
                        hp: Biquad::new(
                            BiquadKind::LowCut,
                            p.low_cut_hz.clamp(20.0, 1000.0),
                            0.707,
                            0.0,
                        ),
                        ring: vec![0.0f32; d],
                        idx: 0,
                        mix: p.mix.clamp(0.0, 1.0),
                        lvl: db_to_gain(p.level_db.clamp(-12.0, 6.0)),
                        right: p.side >= 0,
                    }
                }
                Effect::Deesser(p) => {
                    // fx_extra::deesser, chunked: coef() and the
                    // clamps verbatim, biquads seeded fresh and
                    // env = 0.0 as offline.
                    let q = std::f32::consts::FRAC_1_SQRT_2;
                    StreamFx::Deesser {
                        lp: [
                            Biquad::new(BiquadKind::HighCut, p.freq, q, 0.0),
                            Biquad::new(BiquadKind::HighCut, p.freq, q, 0.0),
                        ],
                        env: 0.0,
                        att: (-1.0 / (0.5f32.max(0.05) * 0.001 * SR)).exp(),
                        rel: (-1.0 / (p.release_ms.max(0.05) * 0.001 * SR)).exp(),
                        thr: p.threshold_db,
                        range: p.range_db.clamp(-24.0, 0.0),
                    }
                }
                Effect::Multiband(p) => {
                    // fx_extra::multiband, chunked: crossover clamps,
                    // coef() and Comp::new's ratio floor verbatim;
                    // biquads seeded fresh and envs 0.0 as offline.
                    let q = std::f32::consts::FRAC_1_SQRT_2;
                    let f_lo = p.low_freq.clamp(40.0, 1000.0);
                    let f_hi = p.high_freq.clamp(f_lo * 1.5, 12000.0);
                    let mk_split = || {
                        [
                            Biquad::new(BiquadKind::HighCut, f_lo, q, 0.0),
                            Biquad::new(BiquadKind::HighCut, f_lo, q, 0.0),
                            Biquad::new(BiquadKind::HighCut, f_hi, q, 0.0),
                            Biquad::new(BiquadKind::HighCut, f_hi, q, 0.0),
                        ]
                    };
                    StreamFx::Multiband {
                        split: [mk_split(), mk_split()],
                        env: [0.0; 3],
                        att: (-1.0 / (p.attack_ms.max(0.05) * 0.001 * SR)).exp(),
                        rel: (-1.0 / (p.release_ms.max(0.05) * 0.001 * SR)).exp(),
                        thr: [p.low_threshold_db, p.mid_threshold_db, p.high_threshold_db],
                        ratio: [
                            p.low_ratio.max(1.0),
                            p.mid_ratio.max(1.0),
                            p.high_ratio.max(1.0),
                        ],
                        mk: [
                            db_to_gain(p.low_gain_db),
                            db_to_gain(p.mid_gain_db),
                            db_to_gain(p.high_gain_db),
                        ],
                        mix: p.mix.clamp(0.0, 1.0),
                    }
                }
                Effect::DynamicEq(p) => {
                    // fx_extra::dynamic_eq, chunked (p.source empty,
                    // enforced by classify): coef() and clamps
                    // verbatim, biquads seeded fresh, env = 0.0.
                    StreamFx::DynamicEq {
                        bp: [
                            Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
                            Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
                        ],
                        env: 0.0,
                        att: (-1.0 / (p.attack_ms.max(0.05) * 0.001 * SR)).exp(),
                        rel: (-1.0 / (p.release_ms.max(0.05) * 0.001 * SR)).exp(),
                        thr: p.threshold_db,
                        ratio: p.ratio.max(1.0),
                        range: p.range_db.clamp(-24.0, 12.0),
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
                Effect::Saturator(p) => {
                    StreamFx::Saturator(Box::new(crate::fx_sat::SaturatorCore::new(p)))
                }
                Effect::Reverb(p) if p.mode != crate::fx::ReverbMode::Freeverb => {
                    // FDN modes: the same sc_dsp::Fdn the offline pass
                    // runs, from sample 0 (offset 0).
                    let (cfg, fp) = crate::fx::fdn_params(p);
                    StreamFx::ReverbFdn(Box::new(crate::sc_dsp::Fdn::new(cfg, &fp, 0)))
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
                "autopan",
                "bitcrush",
                "cab_ir",
                "chorus",
                "compressor",
                "deesser",
                "delay",
                "distortion",
                "dynamic_eq",
                "eq",
                "filter",
                "flanger",
                "freq_splitter",
                "gain",
                "gate",
                "granular",
                "haas",
                "maximizer",
                "multiband",
                "parametric_eq",
                "phaser",
                "pitch_shift",
                "reverb",
                "ring_mod",
                "saturator",
                "sidechain",
                "soft_clipper",
                "stutter",
                "transient",
                "vocoder",
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

    /// RT5: the streamed brickwall limiter (true_peak = false) is
    /// bit-identical to the offline limiter pass once its lookahead
    /// latency is accounted for: streamed[look..n] == offline[0..n-look],
    /// and `look` extra silent input samples flush the delay line so the
    /// tail matches offline[n-look..n]. Same driving pattern as the
    /// lookahead-compressor identity test, at all 8 test-0 block sizes.
    #[test]
    fn stream_fx_limiter_shifted_identity() {
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
            "type":"limiter","ceiling_db":-1.0,"release_ms":80.0,"true_peak":false
        }))
        .unwrap();
        let look = (0.002 * SR) as usize;
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut ol = sig.clone();
            let mut or_ = sig.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("non-true-peak limiter streams");
            assert_eq!(cs.latency_samples(), look);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            // flush: look extra silent samples push the tail through
            let mut fl = vec![0.0f32; look];
            let mut fr = vec![0.0f32; look];
            cs.process_block(&mut fl, &mut fr);
            for i in look..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i - look].to_bits(),
                    "left block {block} i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i - look].to_bits(),
                    "right block {block} i {i}"
                );
            }
            for i in 0..look {
                assert_eq!(
                    fl[i].to_bits(),
                    ol[n - look + i].to_bits(),
                    "flush left block {block} i {i}"
                );
                assert_eq!(
                    fr[i].to_bits(),
                    or_[n - look + i].to_bits(),
                    "flush right block {block} i {i}"
                );
            }
        }
    }

    /// RT6: the streamed maximizer is bit-identical to the offline
    /// fx_sc::maximizer pass once its lookahead latency (la - 1 = 65)
    /// is accounted for: streamed[lat..n] == offline[0..n-lat], and
    /// `lat` extra silent input samples flush the delay line so the
    /// tail matches offline[n-lat..n] (the offline pass runs the same
    /// la - 1 silent frames). All 8 test-0 block sizes.
    #[test]
    fn stream_fx_maximizer_shifted_identity() {
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
            "type":"maximizer","threshold_db":-6.0,"ceiling_db":-0.3,"release_ms":50.0
        }))
        .unwrap();
        let la = ((1.5f32 * 0.001 * SR).max(0.0).round() as usize).max(1);
        let lat = la - 1;
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut ol = sig.clone();
            let mut or_ = sig.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("maximizer streams");
            assert_eq!(cs.latency_samples(), lat);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            // flush: lat extra silent samples push the tail through
            let mut fl = vec![0.0f32; lat];
            let mut fr = vec![0.0f32; lat];
            cs.process_block(&mut fl, &mut fr);
            for i in lat..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i - lat].to_bits(),
                    "left block {block} i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i - lat].to_bits(),
                    "right block {block} i {i}"
                );
            }
            for i in 0..lat {
                assert_eq!(
                    fl[i].to_bits(),
                    ol[n - lat + i].to_bits(),
                    "flush left block {block} i {i}"
                );
                assert_eq!(
                    fr[i].to_bits(),
                    or_[n - lat + i].to_bits(),
                    "flush right block {block} i {i}"
                );
            }
        }
    }

    /// RT7: the streamed transient shaper is bit-identical to the
    /// offline Effect::Transient arm at every block size (zero
    /// latency, so no shift and no flush).
    #[test]
    fn stream_fx_transient_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                // decaying clicks every 977 samples so the attack and
                // sustain branches both fire
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"transient","attack":0.8,"sustain":-0.6
        }))
        .unwrap();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut ol = sig.clone();
            let mut or_ = sig.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("transient streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(l[i].to_bits(), ol[i].to_bits(), "left block {block} i {i}");
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right block {block} i {i}"
                );
            }
        }
    }

    /// RT8: the streamed gate is bit-identical to fx_extra::gate at
    /// every block size (zero latency, so no shift and no flush).
    #[test]
    fn stream_fx_gate_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"gate","threshold_db":-30.0,"range_db":-40.0,
            "attack_ms":1.0,"hold_ms":20.0,"release_ms":50.0
        }))
        .unwrap();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut ol = sig.clone();
            let mut or_ = sig.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("gate streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(l[i].to_bits(), ol[i].to_bits(), "left block {block} i {i}");
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right block {block} i {i}"
                );
            }
        }
    }

    /// RT9: the streamed soft clipper is bit-identical to
    /// fx_extra::soft_clipper at every block size, with and without
    /// the 2x oversample filter (zero latency, so no shift/flush).
    #[test]
    fn stream_fx_soft_clipper_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for os in [true, false] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"soft_clipper","threshold_db":-12.0,"ceiling_db":-1.0,
                "drive_db":6.0,"mix":1.0,"oversample":os
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("soft clipper streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left os {os} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right os {os} block {block} i {i}"
                    );
                }
            }
        }
    }

    /// RT10: the streamed cab_ir (partitioned FFT convolver) is
    /// bit-identical to the offline Effect::CabIr pass at every block
    /// size, shifted by the constant 2047-sample latency plus a flush.
    #[test]
    fn stream_fx_cab_ir_shifted_identity() {
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
        let dir = std::env::temp_dir().join("beatbox_rt_cab_ir_test");
        std::fs::create_dir_all(&dir).unwrap();
        // (a) deterministic stereo IR spanning three convolver parts
        // (5000 > 2 * 2048, not a multiple of the 2048 partition), so
        // the delay line wraps and the last part is partial;
        // (b) a 1-sample IR (single part, minimum k)
        for (name, len) in [("ir5000", 5000usize), ("ir1", 1usize)] {
            let irl: Vec<f32> = (0..len)
                .map(|i| {
                    (((i * 2654435761) % 2000) as f32 / 1000.0 - 1.0)
                        * 0.5f32.powf(i as f32 / 1500.0)
                })
                .collect();
            let irr: Vec<f32> = (0..len)
                .map(|i| {
                    (((i * 2246822519 + 7) % 2000) as f32 / 1000.0 - 1.0)
                        * 0.5f32.powf(i as f32 / 1800.0)
                })
                .collect();
            let ir_path = dir.join(format!("{name}.wav"));
            crate::render::write_wav(&ir_path, &irl, &irr).unwrap();
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"cab_ir","path":ir_path.to_str().unwrap(),
                "max_ms":500.0,"mix":0.7,"gain_db":-2.0
            }))
            .unwrap();
            let lat = 2047usize;
            let triggers: std::collections::HashMap<String, Vec<usize>> =
                std::collections::HashMap::new();
            let step = 60.0 / 140.0 / 4.0;
            for block in [1usize, 7, 63, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("cab_ir streams");
                assert_eq!(cs.latency_samples(), lat);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                let mut fl = vec![0.0f32; lat];
                let mut fr = vec![0.0f32; lat];
                cs.process_block(&mut fl, &mut fr);
                for i in lat..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i - lat].to_bits(),
                        "left block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i - lat].to_bits(),
                        "right block {block} i {i}"
                    );
                }
                for i in 0..lat {
                    assert_eq!(
                        fl[i].to_bits(),
                        ol[n - lat + i].to_bits(),
                        "flush left block {block} i {i}"
                    );
                    assert_eq!(
                        fr[i].to_bits(),
                        or_[n - lat + i].to_bits(),
                        "flush right block {block} i {i}"
                    );
                }
            }
        }
    }

    /// RT10: a missing IR file no-ops exactly like the offline early
    /// return, and the streamed arm adds no latency.
    #[test]
    fn stream_fx_cab_ir_missing_ir_is_a_noop() {
        let n = 4096usize;
        let sig: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"cab_ir","path":"/nonexistent/definitely_missing_ir.wav",
            "mix":0.7,"gain_db":-2.0
        }))
        .unwrap();
        let mut ol = sig.clone();
        let mut or_ = sig.clone();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let ctx = crate::fx::FxContext {
            step_secs: 60.0 / 140.0 / 4.0,
            triggers: &triggers,
            offset: 0,
            levels: None,
        };
        fx.process(&mut ol, &mut or_, &ctx);
        assert!(ol.iter().zip(&sig).all(|(a, b)| a.to_bits() == b.to_bits()));
        let mut l = sig.clone();
        let mut r = sig.clone();
        let mut cs = ChainStreamer::build(
            std::slice::from_ref(&fx),
            &[],
            60.0 / 140.0 / 4.0,
            &triggers,
            n,
        )
        .expect("missing IR still builds the no-op arm");
        assert_eq!(cs.latency_samples(), 0);
        cs.process_block(&mut l, &mut r);
        assert!(l.iter().zip(&sig).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&sig).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    /// RT11: the streamed phaser is bit-identical to fx_extra::phaser
    /// at every block size, free-running and tempo-synced (zero
    /// latency, so no shift and no flush).
    #[test]
    fn stream_fx_phaser_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for sync_steps in [0.0f32, 2.0] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"phaser","rate_hz":0.7,"sync_steps":sync_steps,"depth":0.8,
                "center_hz":700.0,"feedback":0.5,"stages":6,"mix":0.6
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("phaser streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left sync {sync_steps} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right sync {sync_steps} block {block} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_flanger_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for sync_steps in [0.0f32, 2.0] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"flanger","rate_hz":0.7,"sync_steps":sync_steps,"delay_ms":1.5,
                "depth_ms":2.5,"feedback":0.5,"mix":0.6
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("flanger streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left sync {sync_steps} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right sync {sync_steps} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {sync_steps} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {sync_steps} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_autopan_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for tremolo in [false, true] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"autopan","steps":8.0,"depth":0.7,"tremolo":tremolo
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("autopan streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left tremolo {tremolo} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right tremolo {tremolo} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {tremolo} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {tremolo} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_ring_mod_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for shape in ["sine", "square"] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"ring_mod","freq_hz":130.0,"shape":shape,"stereo_phase":0.25,
                "lfo_hz":0.5,"lfo_depth":3.0,"mix":0.7
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("ring mod streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left shape {shape} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right shape {shape} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {shape} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {shape} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_haas_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for side in [-1i32, 1] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"haas","delay_ms":14.0,"side":side,"mix":0.8,
                "low_cut_hz":150.0,"level_db":-2.0
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("haas streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left side {side} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right side {side} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {side} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {side} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_deesser_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for freq in [4000.0f32, 8000.0] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"deesser","freq":freq,"threshold_db":-30.0,
                "range_db":-10.0,"release_ms":60.0
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("deesser streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left freq {freq} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right freq {freq} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {freq} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {freq} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_multiband_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for mix in [0.6f32, 1.0] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"multiband","low_freq":200.0,"high_freq":3000.0,
                "low_threshold_db":-20.0,"mid_threshold_db":-20.0,"high_threshold_db":-22.0,
                "low_ratio":3.0,"mid_ratio":2.0,"high_ratio":2.5,
                "attack_ms":15.0,"release_ms":150.0,
                "low_gain_db":1.5,"mid_gain_db":-1.0,"high_gain_db":2.0,"mix":mix
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("multiband streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left mix {mix} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right mix {mix} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {mix} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {mix} seq i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_dynamic_eq_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        for range_db in [-8.0f32, 4.0] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"dynamic_eq","freq":3000.0,"q":1.5,"threshold_db":-26.0,
                "ratio":3.0,"range_db":range_db,"attack_ms":3.0,"release_ms":90.0,
                "source":""
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("dynamic eq streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left range {range_db} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right range {range_db} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream (carried state must not
            // depend on block size) over distinct L/R inputs (a
            // shared per-channel buffer or index would show)
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            for k in &seq {
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    ol[i].to_bits(),
                    "left mixed {range_db} seq i {i}"
                );
                assert_eq!(
                    r[i].to_bits(),
                    or_[i].to_bits(),
                    "right mixed {range_db} seq i {i}"
                );
            }
        }
    }

    /// A keyed dynamic_eq (a `source` set) has no streamed arm: the
    /// chain stays on the buffered pass. Known debt (RT18 scope).
    #[test]
    fn keyed_dynamic_eq_stays_buffered() {
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"dynamic_eq","freq":3000.0,"q":1.5,"threshold_db":-26.0,
            "ratio":3.0,"range_db":-8.0,"attack_ms":3.0,"release_ms":90.0,
            "source":"lead"
        }))
        .unwrap();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        assert!(
            ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, 64).is_none(),
            "keyed dynamic_eq must route to the buffered pass"
        );
    }

    /// A dynamic_eq whose detector never engages (threshold above any
    /// reachable level) adds exactly zero: the streamed output matches
    /// the input bit for bit (to_bits, the golden-render standard).
    /// Note the only theoretical exception is an exact -0.0 sample
    /// (x + 0.0 normalises -0.0 to +0.0); the fixture below is always
    /// nonzero.
    #[test]
    fn stream_fx_dynamic_eq_unity_is_bit_exact() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                ((i as f32) * 0.031).sin() * 0.6
                    + ((i as f32) * 0.0013).cos() * 0.3
                    + ((i as f32) * 0.11).sin() * 0.1
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
            "type":"dynamic_eq","freq":3000.0,"q":1.5,"threshold_db":100.0,
            "ratio":3.0,"range_db":-8.0,"attack_ms":3.0,"release_ms":90.0,
            "source":""
        }))
        .unwrap();
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut l = sig.clone();
            let mut r: Vec<f32> = sig.iter().rev().copied().collect();
            let r_in = r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("dynamic eq streams");
            assert_eq!(cs.latency_samples(), 0);
            let mut pos = 0usize;
            while pos < n {
                let k = block.min(n - pos);
                cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                pos += k;
            }
            for i in 0..n {
                assert_eq!(
                    l[i].to_bits(),
                    sig[i].to_bits(),
                    "left must be bit-exact at unity (block {block} i {i})"
                );
                assert_eq!(
                    r[i].to_bits(),
                    r_in[i].to_bits(),
                    "right must be bit-exact at unity (block {block} i {i})"
                );
            }
        }
    }

    #[test]
    fn stream_fx_granular_bit_identical_to_offline() {
        let n = 30_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"granular","mix":0.5}),
            serde_json::json!({"type":"granular","mix":1.0,"pitch_semitones":7.0,
                "density_hz":80.0,"grain_ms":60.0,"seed":11}),
            serde_json::json!({"type":"granular","mix":1.0,"pitch_semitones":-12.0,
                "freeze_at_ms":200.0,"position_jitter_ms":150.0,"seed":5}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![256],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("granular streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_granular_mix_zero_is_bit_exact_bypass() {
        let n = 5_000usize;
        let l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.07).sin() * 0.4).collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"granular","mix":0.0})).unwrap();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, n)
            .expect("granular streams");
        let (mut l, mut r) = (l0.clone(), r0.clone());
        for (a, b) in l.chunks_mut(333).zip(r.chunks_mut(333)) {
            cs.process_block(a, b);
        }
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn stream_fx_pitch_shift_bit_identical_to_offline() {
        let n = 30_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"pitch_shift"}),
            serde_json::json!({"type":"pitch_shift","semitones":-7.0,"cents":33.0,
                "window_ms":90.0,"mix":0.65}),
            serde_json::json!({"type":"pitch_shift","semitones":24.0,
                "window_ms":200.0,"mix":1.0}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![256],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("pitch_shift streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_pitch_shift_mix_zero_is_bit_exact_bypass() {
        let n = 5_000usize;
        let l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.07).sin() * 0.4).collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"pitch_shift","mix":0.0})).unwrap();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, n)
            .expect("pitch_shift streams");
        let (mut l, mut r) = (l0.clone(), r0.clone());
        for (a, b) in l.chunks_mut(333).zip(r.chunks_mut(333)) {
            cs.process_block(a, b);
        }
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn stream_fx_stutter_bit_identical_to_offline() {
        let n = 50_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"stutter"}),
            serde_json::json!({"type":"stutter","pattern":"x...x.xx","smooth_ms":0.5,
                "mix":0.7}),
            serde_json::json!({"type":"stutter","mode":"stutter","slice_steps":0.5,
                "cycle_steps":8.0,"smooth_ms":5.0,"mix":0.8}),
            serde_json::json!({"type":"stutter","mode":"half_time","cycle_steps":2.0,
                "mix":1.0}),
            serde_json::json!({"type":"stutter","mode":"tape_stop","cycle_steps":4.0,
                "smooth_ms":2.0,"mix":0.9}),
            serde_json::json!({"type":"stutter","mode":"slice_repeat","slice_blocks":2.0,
                "repeat_count":3.0,"trigger_sample":1000.0,"wrap_fade_ms":3.0,"mix":0.9}),
            serde_json::json!({"type":"stutter","mode":"slice_repeat","block_samples":256.0,
                "slice_blocks":1.0,"repeat_count":40.0,"trigger_sample":999.0,"mix":1.0}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![256],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("stutter streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_stutter_mix_zero_is_bit_exact_bypass() {
        let n = 5_000usize;
        let l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.07).sin() * 0.4).collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"stutter","mix":0.0})).unwrap();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, n)
            .expect("stutter streams");
        let (mut l, mut r) = (l0.clone(), r0.clone());
        for (a, b) in l.chunks_mut(333).zip(r.chunks_mut(333)) {
            cs.process_block(a, b);
        }
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn stream_fx_stutter_reverse_stays_one_shot() {
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"stutter","mode":"reverse"})).unwrap();
        assert!(matches!(classify(&fx), FxClass::OneShot(_)));
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        assert!(
            ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, 1000).is_none()
        );
    }

    #[test]
    fn stream_fx_stutter_inf_in_history_no_nan_at_boundary() {
        // One ring-wrap before a cycle boundary, the streamed lerp's i + 1
        // slot holds old history where the offline pass reads the real future
        // sample; the frac == 0.0 guard in StutterCore::step skips that read,
        // so an inf/NaN there cannot poison the boundary sample.
        let n = 40_000usize;
        let mut l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.031).sin() * 0.6).collect();
        // same expressions as StutterCore::new for cycle_steps = 2.0
        let step = 60.0 / 140.0 / 4.0;
        let step_samples = (step * SR).max(1.0);
        let cycle = (2.0f32.max(0.25) * step_samples) as usize;
        let size = cycle + crate::dsp::tape_dc::preroll() + 2;
        let c0 = 3 * cycle;
        let p_inf = c0 + 1 - size; // the stale slot an unguarded lerp reads at c0
        l0[p_inf] = f32::INFINITY;
        let r0: Vec<f32> = l0.clone();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect = serde_json::from_value(
            serde_json::json!({"type":"stutter","mode":"half_time","cycle_steps":2.0,"mix":1.0}),
        )
        .unwrap();
        let mut ol = l0.clone();
        let mut or_ = r0.clone();
        let ctx = crate::fx::FxContext {
            step_secs: step,
            triggers: &triggers,
            offset: 0,
            levels: None,
        };
        fx.process(&mut ol, &mut or_, &ctx);
        let mut l = l0.clone();
        let mut r = r0.clone();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
            .expect("stutter streams");
        let mut pos = 0usize;
        while pos < n {
            let k = 777usize.min(n - pos);
            cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
            pos += k;
        }
        assert!(
            l[c0].is_finite() && ol[c0].is_finite(),
            "boundary sample poisoned"
        );
        for i in 0..n {
            assert_eq!(l[i].to_bits(), ol[i].to_bits(), "left i {i}");
            assert_eq!(r[i].to_bits(), or_[i].to_bits(), "right i {i}");
        }
    }

    #[test]
    fn stream_fx_stutter_neg_zero_boundary_matches_by_value() {
        // Known limit (R2): at an exact -0.0 input on a cycle boundary the
        // offline pass takes the zero sign from the future sample, which the
        // streamed pass cannot see; the values are equal, the bits may not
        // be. Asserted by value, not to_bits.
        let n = 40_000usize;
        let mut l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.031).sin() * 0.6).collect();
        let step = 60.0 / 140.0 / 4.0;
        let step_samples = (step * SR).max(1.0);
        let cycle = (2.0f32.max(0.25) * step_samples) as usize;
        let mut c0 = 0usize;
        while c0 < n {
            l0[c0] = -0.0;
            c0 += cycle;
        }
        let r0: Vec<f32> = l0.clone();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect = serde_json::from_value(
            serde_json::json!({"type":"stutter","mode":"half_time","cycle_steps":2.0,"mix":1.0}),
        )
        .unwrap();
        let mut ol = l0.clone();
        let mut or_ = r0.clone();
        let ctx = crate::fx::FxContext {
            step_secs: step,
            triggers: &triggers,
            offset: 0,
            levels: None,
        };
        fx.process(&mut ol, &mut or_, &ctx);
        let mut l = l0.clone();
        let mut r = r0.clone();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
            .expect("stutter streams");
        let mut pos = 0usize;
        while pos < n {
            let k = 777usize.min(n - pos);
            cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
            pos += k;
        }
        assert!(l[0] == 0.0 && l[cycle] == 0.0, "boundary sample by value");
        for i in 0..n {
            assert!(l[i] == ol[i], "left i {i}");
            assert!(r[i] == or_[i], "right i {i}");
        }
    }

    #[test]
    fn stream_fx_stutter_slice_repeat_streams() {
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"stutter","mode":"slice_repeat"}))
                .unwrap();
        assert!(matches!(classify(&fx), FxClass::Streamable));
    }

    #[test]
    fn stream_fx_freq_splitter_shifted_identity() {
        let n = 12000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                let base = if i < 6000 {
                    ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3
                } else {
                    0.0
                };
                let j = i % 977;
                let click = if i < 6000 && j < 8 {
                    0.8 * 0.5f32.powi(j as i32)
                } else {
                    0.0
                };
                base + click
            })
            .collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let lat = 2047usize;
        for band in ["low", "mid", "high"] {
            let fx: crate::fx::Effect = serde_json::from_value(serde_json::json!({
                "type":"freq_splitter","low_hz":250.0,"high_hz":2500.0,
                "taps":513,"band":band
            }))
            .unwrap();
            for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
                let mut ol = sig.clone();
                let mut or_ = sig.clone();
                let ctx = crate::fx::FxContext {
                    step_secs: step,
                    triggers: &triggers,
                    offset: 0,
                    levels: None,
                };
                fx.process(&mut ol, &mut or_, &ctx);
                let mut l = sig.clone();
                let mut r = sig.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("freq_splitter streams");
                assert_eq!(cs.latency_samples(), lat);
                let mut pos = 0usize;
                while pos < n {
                    let k = block.min(n - pos);
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                // flush: lat extra silent samples push the tail through
                let mut fl = vec![0.0f32; lat];
                let mut fr = vec![0.0f32; lat];
                cs.process_block(&mut fl, &mut fr);
                for i in lat..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i - lat].to_bits(),
                        "left band {band} block {block} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i - lat].to_bits(),
                        "right band {band} block {block} i {i}"
                    );
                }
                for i in 0..lat {
                    assert_eq!(
                        fl[i].to_bits(),
                        ol[n - lat + i].to_bits(),
                        "flush left band {band} block {block} i {i}"
                    );
                    assert_eq!(
                        fr[i].to_bits(),
                        or_[n - lat + i].to_bits(),
                        "flush right band {band} block {block} i {i}"
                    );
                }
            }
            // mixed block sizes mid-stream over distinct L/R inputs
            let mut seq: Vec<usize> = Vec::new();
            let mut t = 0usize;
            for b in [1usize, 999, 7, 256, 64].into_iter().cycle() {
                if t >= n {
                    break;
                }
                seq.push(b.min(n - t));
                t += b;
            }
            let mut sig_r = sig.clone();
            sig_r.reverse();
            let mut ol = sig.clone();
            let mut or_ = sig_r.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let mut l = sig.clone();
            let mut r = sig_r.clone();
            let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                .expect("freq_splitter streams");
            let mut pos = 0usize;
            for (si, &b) in seq.iter().enumerate() {
                cs.process_block(&mut l[pos..pos + b], &mut r[pos..pos + b]);
                pos += b;
                let _ = si;
            }
            let mut fl = vec![0.0f32; lat];
            let mut fr = vec![0.0f32; lat];
            cs.process_block(&mut fl, &mut fr);
            for i in lat..n {
                assert_eq!(l[i].to_bits(), ol[i - lat].to_bits(), "mixed left i {i}");
                assert_eq!(r[i].to_bits(), or_[i - lat].to_bits(), "mixed right i {i}");
            }
            for i in 0..lat {
                assert_eq!(
                    fl[i].to_bits(),
                    ol[n - lat + i].to_bits(),
                    "mixed flush i {i}"
                );
                assert_eq!(
                    fr[i].to_bits(),
                    or_[n - lat + i].to_bits(),
                    "mixed flush r i {i}"
                );
            }
        }
    }

    #[test]
    fn stream_fx_reverb_fdn_bit_identical_to_offline() {
        // FDN reverb modes (room/plate/hall): sc_dsp::Fdn::process is
        // block-chunkable by design, so chunked RT blocks must equal the
        // offline whole-buffer pass bit for bit.
        let n = 30_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"reverb","mode":"fdn_room"}),
            serde_json::json!({"type":"reverb","mode":"fdn_plate","size":0.85,
                "decay_s":2.8,"damping":0.35,"mix":0.45,"width":1.4,
                "predelay_ms":25.0,"diffusion":0.85,"low_cut_hz":120.0}),
            serde_json::json!({"type":"reverb","mode":"fdn_hall","decay_s":5.0,
                "mix":1.0,"width":2.0,"predelay_ms":0.0,"size":0.2}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            assert!(matches!(classify(&fx), FxClass::Streamable));
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("FDN reverb streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_vocoder_bit_identical_to_offline() {
        let n = 30_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"vocoder"}),
            serde_json::json!({"type":"vocoder","mix":0.6,"bands":24,
                "noise":0.3,"notes":"A2 C3 E3 G3","low_hz":200.0}),
            serde_json::json!({"type":"vocoder","mix":1.0,"bands":8,
                "sibilance":0.0,"attack_ms":1.0,"release_ms":80.0,
                "gain":1.5,"notes":"36,43,48,55","high_hz":12000.0}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            assert!(matches!(classify(&fx), FxClass::Streamable));
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![256],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("vocoder streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn stream_fx_vocoder_mix_zero_is_bit_exact_bypass() {
        let n = 5_000usize;
        let l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.07).sin() * 0.4).collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"vocoder","mix":0.0})).unwrap();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, n)
            .expect("vocoder streams");
        let (mut l, mut r) = (l0.clone(), r0.clone());
        for (a, b) in l.chunks_mut(333).zip(r.chunks_mut(333)) {
            cs.process_block(a, b);
        }
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn stream_fx_vocoder_bad_notes_is_bit_exact_bypass() {
        // Unparseable notes make the offline pass return early; the
        // streamer must bypass the same way, bit for bit.
        let n = 5_000usize;
        let l0: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.07).sin() * 0.4).collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let fx: crate::fx::Effect =
            serde_json::from_value(serde_json::json!({"type":"vocoder","notes":"H3"})).unwrap();
        let mut cs = ChainStreamer::build(std::slice::from_ref(&fx), &[], 0.1, &triggers, n)
            .expect("vocoder streams");
        let (mut l, mut r) = (l0.clone(), r0.clone());
        for (a, b) in l.chunks_mut(777).zip(r.chunks_mut(777)) {
            cs.process_block(a, b);
        }
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn stream_fx_saturator_bit_identical_to_offline() {
        // All six modes, oversample on and off: the core carries the
        // tone/output biquads and the 2x oversampler taps per channel,
        // so chunked RT blocks equal the offline whole-buffer pass bit
        // for bit.
        let n = 30_000usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.031).sin() * 0.6 + ((i as f32) * 0.0013).cos() * 0.3)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let triggers: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let step = 60.0 / 140.0 / 4.0;
        let variants = [
            serde_json::json!({"type":"saturator"}),
            serde_json::json!({"type":"saturator","mode":"tube","drive_db":24.0,
                "bias":0.3,"mix":0.5,"output_db":-6.0}),
            serde_json::json!({"type":"saturator","mode":"fold","drive_db":36.0,
                "oversample":false,"tone_hz":4000.0}),
            serde_json::json!({"type":"saturator","mode":"exciter","drive_db":18.0,
                "tone_hz":2500.0,"mix":0.8}),
            serde_json::json!({"type":"saturator","mode":"diode","drive_db":12.0,
                "bias":-0.4,"oversample":false,"mix":0.35}),
        ];
        for (vi, v) in variants.iter().enumerate() {
            let fx: crate::fx::Effect = serde_json::from_value(v.clone()).unwrap();
            assert!(matches!(classify(&fx), FxClass::Streamable));
            let mut ol = l0.clone();
            let mut or_ = r0.clone();
            let ctx = crate::fx::FxContext {
                step_secs: step,
                triggers: &triggers,
                offset: 0,
                levels: None,
            };
            fx.process(&mut ol, &mut or_, &ctx);
            let seqs: Vec<Vec<usize>> = vec![
                vec![1],
                vec![7],
                vec![64],
                vec![512],
                vec![999],
                vec![1000],
                vec![1 << 16],
                vec![1, 999, 7, 256, 64],
            ];
            for seq in seqs {
                let mut l = l0.clone();
                let mut r = r0.clone();
                let mut cs =
                    ChainStreamer::build(std::slice::from_ref(&fx), &[], step, &triggers, n)
                        .expect("saturator streams");
                assert_eq!(cs.latency_samples(), 0);
                let mut pos = 0usize;
                let mut si = 0usize;
                while pos < n {
                    let k = seq[si % seq.len()].min(n - pos);
                    si += 1;
                    cs.process_block(&mut l[pos..pos + k], &mut r[pos..pos + k]);
                    pos += k;
                }
                for i in 0..n {
                    assert_eq!(
                        l[i].to_bits(),
                        ol[i].to_bits(),
                        "left variant {vi} blocks {seq:?} i {i}"
                    );
                    assert_eq!(
                        r[i].to_bits(),
                        or_[i].to_bits(),
                        "right variant {vi} blocks {seq:?} i {i}"
                    );
                }
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

    /// RT5: a streamed master chain (2ms-lookahead compressor + non-TP
    /// limiter, latency 88 + 88) runs on MixNode's master bus: the
    /// engine output equals the one-shot oracle shifted by the reported
    /// latency, tail flushed by that many extra silent samples, at all
    /// 8 test-0 block sizes. A master chain with a one-shot-only link
    /// (true-peak limiter) keeps the old fader + DcBlock-only section.
    #[test]
    fn mix_node_master_chain_shifted_oracle() {
        let opts = RenderOptions::default();
        let mut e = pdc_project(false, None, "masterchain");
        e.project.master_effects = vec![
            serde_json::from_value(serde_json::json!({
                "type":"compressor","threshold_db":-14.0,"ratio":3.0,
                "knee_db":6.0,"lookahead_ms":2.0
            }))
            .unwrap(),
            serde_json::from_value(serde_json::json!({
                "type":"limiter","ceiling_db":-1.0,"release_ms":80.0,"true_peak":false
            }))
            .unwrap(),
        ];
        let lat_expect = 88 + 88;
        let one_shot = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let n = one_shot.left.len();
        for block in [1usize, 7, 64, 256, 512, 999, 1000, 1 << 16] {
            let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
            let lat = node.latency();
            assert_eq!(lat, lat_expect, "master chain latency (block {block})");
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
        // one-shot-only link (default true-peak limiter): no streaming,
        // no added latency - the section stays fader + DcBlock only.
        let mut ed = pdc_project(false, None, "masterdefault");
        ed.project.master_effects = vec![serde_json::from_value(serde_json::json!({
            "type": "limiter"
        }))
        .unwrap()];
        let node = MixNode::new(&ed.project, &ed.bank, &opts, 512);
        assert!(node.master_chain.is_none());
        assert_eq!(node.latency(), 0, "true-peak master chain does not stream");
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
