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
pub mod diff;
pub mod dsp;
pub mod engine;
pub mod fx;
pub mod fx_extra;
#[cfg(feature = "gui")]
pub mod gui;
pub mod instruments;
pub mod mcp;
pub mod midi_ops;
pub mod multisample;
pub mod project;
pub mod render;
pub mod samples;
pub mod server;
pub mod smf;
pub mod synth_extra;
pub mod theory;
pub mod tools;
pub mod tools_compose;
pub mod tools_midi;
pub mod tools_sound;
pub mod tools_studio;
pub mod validate;

pub use engine::Engine;
