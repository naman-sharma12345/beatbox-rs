//! Beatbox: an AI-native beat-making engine.
//!
//! Every feature is a tool in [`tools::registry`], callable from the CLI,
//! the desktop studio, or any MCP client.

#![allow(
    clippy::manual_is_multiple_of,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod analysis;
pub mod arrange_tx;
pub mod audio_edit;
pub mod auto_eq;
pub mod automation;
pub mod blind_ab;
pub mod carve;
pub mod console_law;
pub mod cover;
pub mod creative;
pub mod diff;
pub mod drill_engine;
pub mod dsp;
pub mod ears;
pub mod ears_pro;
pub mod engine;
pub mod export;
pub mod fx;
pub mod fx_amp;
pub mod fx_char;
pub mod fx_extra;
pub mod fx_mod;
pub mod fx_sat;
pub mod fx_sc;
pub mod fx_time;
pub mod fx_vocoder;
pub mod groove_extract;
pub mod groove_feel;
#[cfg(feature = "gui")]
pub mod gui;
pub mod gui_words;
pub mod history;
pub mod ids;
pub mod instruments;
pub mod interp;
pub mod judge;
pub mod listen;
pub mod live_splice;
pub mod mcp;
pub mod media;
pub mod midi_in;
pub mod midi_ops;
pub mod mix_rules;
pub mod multisample;
pub mod note_edit;
pub mod novelty;
pub mod palette;
pub mod producer;
pub mod project;
pub mod prompt_beat;
pub mod rack;
pub mod record_map;
pub mod recorder;
pub mod render;
pub mod resample;
pub mod resonance;
pub mod resynth;
pub mod rt;
pub mod sample_lib;
pub mod samples;
pub mod sc_dsp;
pub mod sc_strip;
pub mod server;
pub mod smf;
pub mod song;
pub mod song_critic;
pub mod sound_catalog;
pub mod soundfont;
pub mod speech_song;
pub mod stems;
pub mod synth_extra;
pub mod tempo_curve;
pub mod theory;
pub mod timbre_match;
pub mod timebase;
pub mod tools;
pub mod tools_ab_listen;
pub mod tools_bounce;
pub mod tools_categories;
pub mod tools_chain;
pub mod tools_click;
pub mod tools_compose;
pub mod tools_cover;
pub mod tools_creative;
pub mod tools_critic;
pub mod tools_delivery;
pub mod tools_discovery;
pub mod tools_drone;
pub mod tools_ears;
pub mod tools_ears_pro;
pub mod tools_fl;
pub mod tools_fx2;
pub mod tools_groove;
pub mod tools_ids;
pub mod tools_library;
pub mod tools_listen;
pub mod tools_markers;
pub mod tools_meta;
pub mod tools_midi;
pub mod tools_mix;
pub mod tools_mix_analysis;
pub mod tools_mix_primitives;
pub mod tools_multiband;
pub mod tools_palette;
pub mod tools_parity;
pub mod tools_producer;
pub mod tools_prompt;
pub mod tools_sound;
pub mod tools_studio;
pub mod tools_tempo;
pub mod tools_timeline;
pub mod tools_tp_limit;
pub mod tools_transcribe;
pub mod tools_vocal;
pub mod transcribe;
pub mod validate;
pub mod vocal;
pub mod vocal_clarity;
pub mod vocal_corpus;
pub mod vocal_edits;
pub mod vocal_flex;
pub mod vocal_repair;
pub mod vocal_sections;
pub mod vocal_style;
pub mod voice_ai;
pub mod voice_pro;
pub mod waveform_png;
pub mod web_import;

pub use engine::Engine;

#[cfg(test)]
mod test_fixtures;
