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
    /// Open the piano roll (F7).
    Piano,
    /// Put every window back to the default arrangement (Ctrl+Shift+H).
    ResetLayout,
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
    ("F6", Action::View(0), "Channel rack"),
    ("F9", Action::View(1), "Mixer"),
    ("F7", Action::Piano, "Piano roll"),
    ("Cmd+Shift+H", Action::ResetLayout, "Reset window layout"),
    ("Cmd+1", Action::View(0), "Channel rack"),
    ("Cmd+2", Action::View(1), "Mixer"),
    ("Cmd+3", Action::View(2), "Automation"),
    ("Cmd+4", Action::View(3), "Playlist"),
    ("Alt+F8", Action::View(5), "Browser"),
    (
        "F8",
        Action::View(5),
        "Browser (FL Studio opens its plugin picker here)",
    ),
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

/// Group aliases by action, keeping every working binding visible.
/// `command_is_cmd` follows the platform rather than displaying Cmd on Windows/Linux.
pub fn help_rows(command_is_cmd: bool) -> Vec<(String, &'static str)> {
    let mut rows: Vec<(Action, Vec<String>, &'static str)> = Vec::new();
    for (key, action, description) in SHORTCUTS {
        let key = if command_is_cmd {
            key.to_string()
        } else {
            key.replace("Cmd", "Ctrl")
        };
        if let Some((_, keys, _)) = rows.iter_mut().find(|(a, _, _)| a == action) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        } else {
            rows.push((*action, vec![key], description));
        }
    }
    rows.into_iter()
        .map(|(_, keys, text)| (keys.join(" / "), text))
        .collect()
}

/// Tooltip help and the Help menu share one platform-aware list.
pub fn help_line() -> String {
    help_rows(cfg!(target_os = "macos"))
        .into_iter()
        .map(|(keys, text)| format!("{keys}  {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_keeps_aliases_and_uses_platform_modifier() {
        let ctrl = help_rows(false);
        let keys = |label| {
            ctrl.iter()
                .find(|(_, text)| *text == label)
                .unwrap()
                .0
                .clone()
        };
        assert_eq!(keys("Playlist"), "F5 / Ctrl+4");
        assert_eq!(keys("Channel rack"), "F6 / Ctrl+1");
        assert_eq!(keys("Redo"), "Ctrl+Shift+Z / Ctrl+Y");
        assert_eq!(keys("Return to start"), "Home / Enter");
        assert!(!ctrl.iter().any(|(key, _)| key.contains("Cmd")));
        assert!(help_rows(true).iter().any(|(key, _)| key == "Cmd+S"));
        for (key, action, _) in SHORTCUTS {
            let description = SHORTCUTS.iter().find(|(_, a, _)| a == action).unwrap().2;
            assert!(keys(description)
                .split(" / ")
                .any(|k| k == key.replace("Cmd", "Ctrl")));
        }
    }

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
