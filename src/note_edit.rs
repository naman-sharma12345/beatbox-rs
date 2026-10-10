// Adapted from SoundCraft `crates/ui-egui/src/midi_editor.rs` (commit eac0edd): the note
// move / resize math behind its drag gesture, reworked for Beatbox's step-based notes.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! Piano-roll edit math shared by the studio GUI: snapping, moving / resizing a selection,
//! and the `add_notes` JSON that writes the result back through the engine (one tool call,
//! one undo step). Lives outside `gui` so it is tested in the headless build.

use crate::project::Note;
use serde_json::{json, Value};

/// Round `x` to the nearest multiple of `g`.
pub fn snap_to(x: f32, g: f32) -> f32 {
    if g <= 0.0 || !x.is_finite() {
        return x;
    }
    (x / g).round() * g
}

/// A move (or, with `resize`, a length change of note `index`) in steps and semitones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteShift {
    pub index: usize,
    pub resize: bool,
    pub d_steps: f32,
    pub d_semi: i32,
}

/// A shift applied to the notes (moves every selected note when `index` is selected): moved/resized copies, clamped to the pattern and MIDI range.
pub fn apply_shift(
    notes: &[Note],
    selected: &[usize],
    d: &NoteShift,
    steps: f32,
    min_len: f32,
) -> Vec<Note> {
    let mut out = notes.to_vec();
    for (i, n) in out.iter_mut().enumerate() {
        if d.resize {
            if i == d.index {
                n.len = (n.len + d.d_steps).max(min_len.min(n.len).max(0.05));
            }
        } else if i == d.index || (selected.contains(&i) && selected.contains(&d.index)) {
            n.start = (n.start + d.d_steps).clamp(0.0, (steps - 0.05).max(0.0));
            n.pitch = (i32::from(n.pitch) + d.d_semi).clamp(0, 127) as u8;
        }
    }
    out
}

/// Identity of a note for re-finding it after an edit: (start, pitch, len).
pub type NoteKey = (f32, u8, f32);

/// Keys of the selected notes in `notes` (taken after a move, before the
/// engine re-sorts them).
pub fn keys_of(notes: &[Note], selected: &[usize]) -> Vec<NoteKey> {
    selected
        .iter()
        .filter_map(|i| notes.get(*i))
        .map(|n| (n.start, n.pitch, n.len))
        .collect()
}

/// Indices in `notes` of the notes matching `keys` (each key used once), so a
/// selection survives a drag or arrow-key move even when the engine re-sorts
/// the note list.
pub fn reselect(notes: &[Note], keys: &[NoteKey]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for k in keys {
        if let Some(i) = notes.iter().enumerate().position(|(i, n)| {
            !out.contains(&i)
                && (n.start - k.0).abs() < 1e-3
                && n.pitch == k.1
                && (n.len - k.2).abs() < 1e-3
        }) {
            out.push(i);
        }
    }
    out.sort_unstable();
    out
}

/// Notes as `add_notes` JSON (keeps probability, microtiming and slides).
pub fn notes_json(notes: &[Note]) -> Vec<Value> {
    notes
        .iter()
        .map(|n| {
            let mut v = json!({"start": n.start, "len": n.len, "pitch": n.pitch, "vel": n.vel,
                "prob": n.prob, "offset": n.offset});
            if let Some(s) = n.slide_to {
                v["slide_to"] = json!(s);
            }
            v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping() {
        assert_eq!(snap_to(2.6, 1.0), 3.0);
        assert_eq!(snap_to(2.6, 0.5), 2.5);
        assert!(snap_to(f32::NAN, 1.0).is_nan());
        assert_eq!(snap_to(3.3, 0.0), 3.3);
    }

    #[test]
    fn drag_moves_selection_and_resizes_one() {
        let notes = vec![
            Note::new(0.0, 1.0, 60, 0.8),
            Note::new(4.0, 2.0, 64, 0.8),
            Note::new(8.0, 1.0, 67, 0.8),
        ];
        let d = NoteShift {
            index: 0,
            resize: false,
            d_steps: 2.0,
            d_semi: 12,
        };
        let out = apply_shift(&notes, &[0, 2], &d, 16.0, 1.0);
        assert_eq!((out[0].start, out[0].pitch), (2.0, 72));
        assert_eq!((out[1].start, out[1].pitch), (4.0, 64));
        assert_eq!((out[2].start, out[2].pitch), (10.0, 79));
        let r = NoteShift {
            index: 1,
            resize: true,
            d_steps: -5.0,
            ..d
        };
        let out = apply_shift(&notes, &[0, 1], &r, 16.0, 1.0);
        assert_eq!(out[1].len, 1.0);
        assert_eq!(out[0].start, 0.0);
        // clamps to pattern and MIDI range
        let far = NoteShift {
            index: 2,
            resize: false,
            d_steps: 100.0,
            d_semi: 100,
        };
        let out = apply_shift(&notes, &[], &far, 16.0, 1.0);
        assert!(out[2].start < 16.0 && out[2].pitch == 127);
    }

    #[test]
    fn gui_edit_is_one_undo_step() {
        use crate::Engine;
        let mut e = Engine::new(std::env::temp_dir());
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "pitch": 60}, {"start": 4, "pitch": 64, "prob": 0.5}]})).unwrap();
        let before = e.project.patterns[0].notes("keys").to_vec();
        let d = NoteShift {
            index: 1,
            resize: false,
            d_steps: 2.0,
            d_semi: -2,
        };
        let moved = apply_shift(&before, &[1], &d, 64.0, 1.0);
        e.call(
            "add_notes",
            &json!({"track": "keys", "replace": true, "notes": notes_json(&moved)}),
        )
        .unwrap();
        let after = e.project.patterns[0].notes("keys").to_vec();
        assert_eq!(
            (after[1].start, after[1].pitch, after[1].prob),
            (6.0, 62, 0.5)
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.patterns[0].notes("keys"), &before[..]);
    }

    #[test]
    fn notes_json_keeps_feel() {
        let mut n = Note::new(1.0, 1.0, 36, 0.5);
        n.prob = 0.6;
        n.slide_to = Some(38);
        let v = notes_json(&[n]);
        assert_eq!(v[0]["prob"], json!(0.6f32));
        assert_eq!(v[0]["slide_to"], json!(38));
    }
    #[test]
    fn selection_survives_a_move_and_a_resort() {
        let n = |s: f32, p: u8| Note::new(s, 1.0, p, 0.8);
        let notes = vec![n(0.0, 60), n(4.0, 62), n(8.0, 64)];
        let d = NoteShift {
            index: 2,
            resize: false,
            d_steps: -6.0,
            d_semi: 1,
        };
        let moved = apply_shift(&notes, &[0, 2], &d, 16.0, 0.25);
        let keys = keys_of(&moved, &[0, 2]);
        // the engine stores notes sorted by start
        let mut stored = moved.clone();
        stored.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
        let sel = reselect(&stored, &keys);
        assert_eq!(sel.len(), 2);
        let got: Vec<(f32, u8)> = sel
            .iter()
            .map(|i| (stored[*i].start, stored[*i].pitch))
            .collect();
        assert!(
            got.contains(&(0.0, 61)) && got.contains(&(2.0, 65)),
            "{got:?}"
        );
    }
}
