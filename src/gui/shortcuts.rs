// Adapted from SoundCraft `crates/ui-egui/src/shortcuts.rs` (commit eac0edd).
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! Keyboard shortcuts. A table of "Cmd+Shift+X" style strings, parsed once, maps keys
//! to studio actions; every action that edits the project goes through an `Engine`
//! tool call, so shortcuts, clicks and MCP clients share one undo history.

use eframe::egui::{self, Key, Modifiers};

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayStop,
    Undo,
    Redo,
    Save,
    Render,
    /// Switch the central view (0 sequencer, 1 mixer, 2 automation, 3 playlist).
    View(usize),
    PrevPattern,
    NextPattern,
    PrevTrack,
    NextTrack,
    MuteSelected,
    SoloSelected,
    SeekStart,
}

/// The shortcut table: (keys, action, help text).
pub const SHORTCUTS: &[(&str, Action, &str)] = &[
    ("Space", Action::PlayStop, "Play / stop"),
    ("Cmd+Z", Action::Undo, "Undo"),
    ("Cmd+Shift+Z", Action::Redo, "Redo"),
    ("Cmd+Y", Action::Redo, "Redo"),
    ("Cmd+S", Action::Save, "Save project"),
    ("Cmd+R", Action::Render, "Render WAV"),
    ("F5", Action::View(3), "Playlist"),
    ("F6", Action::View(0), "Sequencer"),
    ("F9", Action::View(1), "Mixer"),
    ("F7", Action::View(2), "Automation"),
    ("Cmd+1", Action::View(0), "Sequencer"),
    ("Cmd+2", Action::View(1), "Mixer"),
    ("Cmd+3", Action::View(2), "Automation"),
    ("Cmd+4", Action::View(3), "Playlist"),
    ("Cmd+7", Action::View(6), "Critique (critique_mix)"),
    ("F8", Action::View(7), "Browser"),
    ("[", Action::PrevPattern, "Previous pattern"),
    ("]", Action::NextPattern, "Next pattern"),
    ("Alt+ArrowUp", Action::PrevTrack, "Select track above"),
    ("Alt+ArrowDown", Action::NextTrack, "Select track below"),
    ("M", Action::MuteSelected, "Mute selected track"),
    ("S", Action::SoloSelected, "Solo selected track"),
    ("Home", Action::SeekStart, "Return to start"),
    ("Enter", Action::SeekStart, "Return to start"),
];

/// Parse "Cmd+Shift+X" style strings. Returns None for anything it can't map.
pub fn parse(s: &str) -> Option<(Modifiers, Key)> {
    let mut m = Modifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        match part.trim() {
            "Cmd" => m.command = true,
            "Shift" => m.shift = true,
            "Alt" => m.alt = true,
            "Ctrl" => m.ctrl = true,
            "" => {}
            k => {
                if key.is_some() {
                    return None;
                }
                key = Some(match k {
                    "Space" => Key::Space,
                    "Enter" => Key::Enter,
                    "Home" => Key::Home,
                    "End" => Key::End,
                    "Tab" => Key::Tab,
                    "[" => Key::OpenBracket,
                    "]" => Key::CloseBracket,
                    "," => Key::Comma,
                    "." => Key::Period,
                    "/" => Key::Slash,
                    "=" => Key::Equals,
                    other => Key::from_name(other)?,
                });
            }
        }
    }
    key.map(|k| (m, k))
}

/// Modifier match where Cmd means Ctrl off macOS (egui sets `command` for both).
fn mods_match(want: Modifiers, got: Modifiers) -> bool {
    want.command == got.command
        && want.shift == got.shift
        && want.alt == got.alt
        && (want.ctrl == got.ctrl || got.mac_cmd || (want.command && got.ctrl))
}

/// The actions triggered by this frame's key presses (none while a text field has focus).
pub fn pressed(ctx: &egui::Context) -> Vec<Action> {
    if ctx.wants_keyboard_input() {
        return Vec::new();
    }
    let events: Vec<(Key, Modifiers)> = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
            .collect()
    });
    let mut out = Vec::new();
    for (key, mods) in events {
        let hit = SHORTCUTS
            .iter()
            .find(|(s, _, _)| parse(s).is_some_and(|(m, k)| k == key && mods_match(m, mods)));
        if let Some((_, a, _)) = hit {
            out.push(*a);
        }
    }
    out
}

/// One-line help for tooltips: "Space play / stop · Cmd+Z undo · …".
pub fn help_line() -> String {
    let mut seen = Vec::new();
    let mut parts = Vec::new();
    for (k, a, h) in SHORTCUTS {
        if seen.contains(a) {
            continue;
        }
        seen.push(*a);
        parts.push(format!("{k}  {h}"));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_parses() {
        for (s, _, _) in SHORTCUTS {
            assert!(parse(s).is_some(), "{s}");
        }
        assert_eq!(
            parse("Cmd+Shift+Z"),
            Some((
                Modifiers {
                    command: true,
                    shift: true,
                    ..Modifiers::NONE
                },
                Key::Z
            ))
        );
        assert_eq!(parse("Cmd+A+B"), None);
        assert_eq!(parse("Cmd+Nope"), None);
    }

    #[test]
    fn modifiers_must_match() {
        let (m, _) = parse("Cmd+Z").unwrap_or((Modifiers::NONE, Key::A));
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert!(mods_match(m, ctrl));
        assert!(!mods_match(m, Modifiers::NONE));
        let shift = Modifiers {
            shift: true,
            ..ctrl
        };
        assert!(!mods_match(m, shift));
    }
}
