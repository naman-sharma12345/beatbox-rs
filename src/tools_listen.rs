//! AI ears v2 tools: ears_report, diff_renders, loudness_report,
//! masking_matrix, groove_analysis, hook_analysis, structure.

use crate::listen;
use crate::tools::{b_or, obj, s_opt, s_req, u_or, Tool};
use serde_json::json;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "ears_report",
            description: "ONE CALL TO LISTEN. A ranked verdict on the current render, bound to a render_id (hash of the project, so a stale verdict is impossible): loudness (I, LRA, PLR, per-section PSR, true peak), directional ERB-band masking between tracks in the busiest section, clicks, section-contrast flags, mix analyzer, hook analysis of the lead, groove and arrangement structure. Separate technical and musical scores, up to 7 findings each with a ready-to-run suggested_call, and the next_best_action. Pass render_ids to diff_renders after a change.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string", "description": "Melodic track for hook analysis (default lead)"}}), &[]),
            run: |e, a| listen::ears_report(e, s_opt(a, "track").as_deref()),
        },
        Tool {
            name: "diff_renders",
            description: "What changed between two renders: a and b are render_ids from ears_report (or snapshot names / 'current', analysed on demand). Deltas in loudness, true peak, LRA, PSR, stereo, per-section short-term loudness, band balance, masking and scores; improvements, regressions, fixed and new findings, and a verdict (better / worse / mixed / changed_neutral / no_audible_change). Use it to check that your last edit actually helped.",
            mutates: false,
            schema: || obj(json!({"a": {"type": "string"}, "b": {"type": "string", "description": "default 'current'"}}), &["a"]),
            run: |e, a| {
                let x = listen::summary_of(e, &s_req(a, "a")?)?;
                let y = listen::summary_of(e, &s_opt(a, "b").unwrap_or_else(|| "current".into()))?;
                Ok(listen::diff(&x, &y))
            },
        },
        Tool {
            name: "loudness_report",
            description: "Full loudness and dynamics: integrated LUFS, LRA, max short-term, true peak, sample peak, PLR, per-section PSR (punch) and LRA, a 1 s short-term series, platform playback gain (Spotify / Apple / YouTube) and, with codec:true, the true peak after an MP3-320 encode/decode round trip. Findings carry suggested calls.",
            mutates: false,
            schema: || obj(json!({"codec": {"type": "boolean", "description": "Simulate MP3 320 (needs ffmpeg; default false)"}}), &[]),
            run: |e, a| listen::loudness_report(e, b_or(a, "codec", false)),
        },
        Tool {
            name: "masking_matrix",
            description: "Which track hides which: directional masking (masker -> maskee) on 40 ERB bands, time-gated to where the maskee plays, in one section (default the busiest; at most 16 bars rendered with stems). Each pair: masking dB (masker-to-signal ratio over the maskee's important bands), active ratio, the worst band, and a fix as a tool call (sidechain for kick vs bass, else a parametric_eq cut on the lower-priority track).",
            mutates: false,
            schema: || obj(json!({"section": {"type": "string", "description": "Pattern name or arrangement index"}, "top_k": {"type": "integer", "description": "default 8"}}), &[]),
            run: |e, a| {
                let (pairs, sec) = listen::masking(e, s_opt(a, "section").as_deref(), u_or(a, "top_k", 8) as usize)?;
                Ok(json!({"section": sec, "pairs": pairs}))
            },
        },
        Tool {
            name: "groove_analysis",
            description: "Groove from the exact drum and bass data: per track timing offset mean/sd in ms (humanize + nudges), velocity profile per 16th, ghost-note ratio, accent entropy, hits per bar; flam risks (kick/bass and snare/perc hits 5-30 ms apart) and findings.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(listen::groove_analysis(&e.project)),
        },
        Tool {
            name: "hook_analysis",
            description: "Melody and hook features on the exact notes of a track across the song: range, scale fit, contour (step/leap ratio, interval entropy), rhythm (density, syncopation), repeating motifs (interval+gap n-grams) and the sections they recur in, hook candidates, singability and a 0-100 hook score with findings. Use it to pick the best of several melody variants.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string", "description": "default lead"}}), &[]),
            run: |e, a| listen::hook_analysis(&e.project, &s_opt(a, "track").unwrap_or_else(|| "lead".into())),
        },
        Tool {
            name: "structure",
            description: "Arrangement structure from the notes: section-by-section similarity matrix, A/B/A' labels, distinct ideas, density per section, and flags such as a section identical to the one before it (no development).",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(listen::structure(&e.project)),
        },
    ]
}
