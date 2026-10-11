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
pub mod audio_edit;
pub mod automation;
pub mod blind_ab;
pub mod carve;
pub mod console_law;
pub mod creative;
pub mod diff;
pub mod dsp;
pub mod ears;
pub mod ears_pro;
pub mod engine;
pub mod export;
pub mod fx;
pub mod fx_char;
pub mod fx_extra;
pub mod fx_mod;
pub mod fx_sat;
pub mod fx_time;
pub mod fx_vocoder;
pub mod groove_extract;
#[cfg(feature = "gui")]
pub mod gui;
pub mod instruments;
pub mod listen;
pub mod mcp;
pub mod media;
pub mod midi_ops;
pub mod multisample;
pub mod note_edit;
pub mod novelty;
pub mod palette;
pub mod producer;
pub mod project;
pub mod prompt_beat;
pub mod speech_song;
pub mod render;
pub mod resample;
pub mod sample_lib;
pub mod samples;
pub mod sc_dsp;
pub mod server;
pub mod smf;
pub mod synth_extra;
pub mod theory;
pub mod timebase;
pub mod tools;
pub mod tools_compose;
pub mod tools_creative;
pub mod tools_delivery;
pub mod tools_ears;
pub mod tools_ears_pro;
pub mod tools_fl;
pub mod tools_fx2;
pub mod tools_groove;
pub mod tools_listen;
pub mod tools_midi;
pub mod tools_mix;
pub mod tools_palette;
pub mod tools_parity;
pub mod tools_producer;
pub mod tools_prompt;
pub mod tools_meta;
pub mod tools_bounce;
pub mod tools_chain;
pub mod tools_click;
pub mod tools_markers;
pub mod tools_tempo;
pub mod tempo_curve;
pub mod stems;
pub mod resynth;
pub mod tools_critic;
pub mod tools_drone;
pub mod tools_sound;
pub mod tools_studio;
pub mod tools_vocal;
pub mod validate;
pub mod vocal;
pub mod voice_pro;

pub use engine::Engine;
