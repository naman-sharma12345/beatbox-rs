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
    if g <= 0.0 || !g.is_finite() || !x.is_finite() {
        return x;
    }
    let snapped = (x / g).round() * g;
    if snapped.is_finite() {
        snapped
    } else {
        x
    }
}

/// A move (or, with `resize`, a length change of note `index`) in steps and semitones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteShift {
    pub index: usize,
    pub resize: bool,
    pub d_steps: f32,
    pub d_semi: i32,
}

/// One horizontal delta for a group; never repair pre-existing out-of-range starts.
pub fn group_move_delta(
    notes: &[Note],
    selected: &[usize],
    index: usize,
    requested: f32,
    steps: f32,
) -> f32 {
    if !requested.is_finite() || !steps.is_finite() || steps < 0.0 {
        return 0.0;
    }
    if !selected.contains(&index) {
        return requested;
    }
    let selected: std::collections::HashSet<_> = selected
        .iter()
        .copied()
        .filter(|i| *i < notes.len())
        .collect();
    if selected.len() < 2 {
        return requested;
    }
    let max = (steps - 0.05).max(0.0);
    let mut low = max;
    let mut high = max;
    for i in selected {
        let start = notes[i].start;
        if !start.is_finite() || !(0.0..=max).contains(&start) {
            return 0.0;
        }
        low = low.min(start);
        high = high.min(max - start);
    }
    requested.clamp(-low, high)
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
    if d.index >= notes.len()
        || !d.d_steps.is_finite()
        || !steps.is_finite()
        || steps < 0.0
        || !min_len.is_finite()
        || min_len < 0.0
    {
        return out;
    }
    let move_selection = !d.resize && selected.contains(&d.index);
    let horizontal = if move_selection {
        group_move_delta(notes, selected, d.index, d.d_steps, steps)
    } else {
        d.d_steps
    };
    let selected: std::collections::HashSet<usize> = if move_selection {
        selected.iter().copied().collect()
    } else {
        std::collections::HashSet::new()
    };
    for (i, n) in out.iter_mut().enumerate() {
        if d.resize {
            if i == d.index {
                let length = n.len + d.d_steps;
                if length.is_finite() {
                    n.len = length.max(min_len.max(0.05).min(n.len));
                }
            }
        } else if i == d.index || (move_selection && selected.contains(&i)) {
            if horizontal != 0.0 {
                n.start = (n.start + horizontal).clamp(0.0, (steps - 0.05).max(0.0));
            }
            if d.d_semi != 0 {
                n.pitch = i32::from(n.pitch).saturating_add(d.d_semi).clamp(0, 127) as u8;
            }
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
        let n = &moved[1];
        e.call("update_note_geometry", &json!({"track":"keys","updates":[{"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}]})).unwrap();
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
            got.contains(&(0.0, 61)) && got.contains(&(8.0, 65)),
            "{got:?}"
        );
    }
}

#[cfg(test)]
mod independent_axis_tests {
    use super::*;
    #[test]
    fn pitch_only_preserves_off_pattern_starts_expression_ids_and_order() {
        let mut notes = vec![Note::new(80., 0.02, 72, 0.), Note::new(4., 2., 64, 0.6)];
        notes[0].id = "beyond".into();
        notes[0].prob = 0.5;
        notes[0].offset = 0.2;
        notes[0].slide_to = Some(74);
        let mut expected = notes.clone();
        expected[0].pitch = 84;
        expected[1].pitch = 76;
        let shift = NoteShift {
            index: 0,
            resize: false,
            d_steps: 0.,
            d_semi: 12,
        };
        assert_eq!(apply_shift(&notes, &[0, 1], &shift, 64., 1.), expected);
        let noop = NoteShift { d_semi: 0, ..shift };
        assert_eq!(apply_shift(&notes, &[0, 1], &noop, 64., 1.), notes);
    }
}

#[cfg(test)]
mod short_resize_tests {
    use super::*;
    #[test]
    fn shortening_a_tiny_hit_never_lengthens_it() {
        let mut n = Note::new(4., 0.02, 64, 0.6);
        n.id = "tiny".into();
        n.prob = 0.5;
        n.offset = 0.2;
        n.slide_to = Some(67);
        for delta in [-8., -1., 0.] {
            let shift = NoteShift {
                index: 0,
                resize: true,
                d_steps: delta,
                d_semi: 12,
            };
            assert_eq!(
                apply_shift(&[n.clone()], &[0], &shift, 64., 1.),
                vec![n.clone()]
            );
        }
        let shift = NoteShift {
            index: 0,
            resize: true,
            d_steps: 1.,
            d_semi: 12,
        };
        let mut expected = n.clone();
        expected.len = 1.02;
        assert_eq!(apply_shift(&[n], &[0], &shift, 64., 1.), vec![expected]);
    }
    #[test]
    fn resize_preserves_every_other_field_and_unlisted_note_across_short_lengths() {
        for i in 1..128 {
            let mut note = Note::new(i as f32, i as f32 / 1000., 48 + (i % 60) as u8, 0.7);
            note.id = format!("tiny{i}");
            note.prob = 0.2;
            note.offset = -0.2;
            note.slide_to = Some(72);
            let notes = vec![Note::new(80., 0.02, 72, 0.), note];
            for delta in [-2., 0., 0.5] {
                let shift = NoteShift {
                    index: 1,
                    resize: true,
                    d_steps: delta,
                    d_semi: 12,
                };
                let got = apply_shift(&notes, &[0, 1], &shift, 64., 1.);
                assert_eq!(got[0], notes[0]);
                let mut restored = got.clone();
                restored[1].len = notes[1].len;
                assert_eq!(restored, notes);
                if delta <= 0. {
                    assert_eq!(got, notes);
                }
            }
        }
    }
}

#[cfg(test)]
mod geometry_preservation_audit_tests {
    use super::*;
    fn clip(seed: usize) -> Vec<Note> {
        let mut notes: Vec<Note> = (0..64usize)
            .map(|i| {
                let mut n = Note::new(
                    ((i * 13 + seed) % 103) as f32 * 0.75,
                    (i % 9 + 1) as f32 * 0.01,
                    (i * 11 % 128) as u8,
                    (i % 5) as f32 * 0.25,
                );
                n.id = format!("n{i}");
                n.prob = (i % 4) as f32 / 3.;
                n.offset = (i % 5) as f32 * 0.1 - 0.2;
                if i % 3 == 0 {
                    n.slide_to = Some((i % 128) as u8);
                }
                n
            })
            .collect();
        if seed % 2 == 0 {
            notes.reverse();
        }
        notes
    }
    #[test]
    fn pitch_only_matches_independent_reference_over_mixed_clips() {
        for seed in 0..64 {
            let notes = clip(seed);
            let selected: Vec<_> = (0..notes.len()).filter(|i| (i + seed) % 3 == 0).collect();
            let index = seed % notes.len();
            for delta in [-12, -1, 0, 1, 12] {
                let shift = NoteShift {
                    index,
                    resize: false,
                    d_steps: 0.,
                    d_semi: delta,
                };
                let mut expected = notes.clone();
                for (i, n) in expected.iter_mut().enumerate() {
                    if i == index || (selected.contains(&index) && selected.contains(&i)) {
                        n.pitch = (i32::from(n.pitch) + delta).clamp(0, 127) as u8;
                    }
                }
                assert_eq!(
                    apply_shift(&notes, &selected, &shift, 64., 1.),
                    expected,
                    "seed{seed} delta{delta}"
                );
            }
        }
    }
    #[test]
    fn resize_changes_only_target_length_over_mixed_clips() {
        for seed in 0..64 {
            let notes = clip(seed);
            let selected: Vec<_> = (0..notes.len()).collect();
            let index = seed % notes.len();
            for delta in [-4., -0.01, 0., 0.5, 4.] {
                let shift = NoteShift {
                    index,
                    resize: true,
                    d_steps: delta,
                    d_semi: 12,
                };
                let mut expected = notes.clone();
                // These clips have lengths below the snap floor: shrinking is
                // a no-op, growing adds the requested distance to only one hit.
                if delta > 0. {
                    expected[index].len += delta;
                }
                assert_eq!(
                    apply_shift(&notes, &selected, &shift, 64., 1.),
                    expected,
                    "seed{seed} delta{delta}"
                );
            }
        }
    }
}

#[cfg(test)]
mod invalid_geometry_tests {
    use super::*;
    #[test]
    fn missing_target_does_not_move_even_a_selection_containing_stale_index() {
        let notes = vec![Note::new(4., 1., 64, 0.6), Note::new(8., 2., 67, 0.8)];
        for resize in [false, true] {
            let shift = NoteShift {
                index: 9,
                resize,
                d_steps: 2.,
                d_semi: 12,
            };
            assert_eq!(apply_shift(&notes, &[0, 1, 9], &shift, 64., 1.), notes);
            assert!(apply_shift(&[], &[0, 9], &shift, 64., 1.).is_empty());
        }
    }
    #[test]
    fn invalid_numeric_geometry_is_a_noop() {
        let notes = vec![Note::new(80., 0.02, 72, 0.), Note::new(4., 1., 64, 0.6)];
        for resize in [false, true] {
            for (delta, steps, minimum) in [
                (f32::NAN, 64., 1.),
                (f32::INFINITY, 64., 1.),
                (f32::NEG_INFINITY, 64., 1.),
                (1., f32::NAN, 1.),
                (1., f32::INFINITY, 1.),
                (1., -1., 1.),
                (1., 64., f32::NAN),
                (1., 64., f32::INFINITY),
                (1., 64., -1.),
            ] {
                let shift = NoteShift {
                    index: 1,
                    resize,
                    d_steps: delta,
                    d_semi: 12,
                };
                assert_eq!(apply_shift(&notes, &[0, 1], &shift, steps, minimum), notes);
            }
        }
    }
}

#[cfg(test)]
mod geometry_roundtrip_tests {
    use super::*;
    #[test]
    fn pitch_and_resize_roundtrips_preserve_complete_project_and_one_step_history() {
        for (resize, delta, semi) in [(false, 0., 12), (true, 2., 0)] {
            let mut e = crate::Engine::new(std::env::temp_dir());
            e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
                .unwrap();
            for (i, start) in [80., 4., 0.].iter().enumerate() {
                let mut n = Note::new(*start, 0.02, 60 + i as u8, 0.);
                n.id = format!("n{i}");
                n.prob = 0.7;
                n.offset = 0.1;
                n.slide_to = Some(72);
                e.project.patterns[0].notes_mut("keys").push(n);
            }
            let before = serde_json::to_value(&e.project).unwrap();
            let notes = e.project.patterns[0].notes("keys").to_vec();
            let shift = NoteShift {
                index: 1,
                resize,
                d_steps: delta,
                d_semi: semi,
            };
            let edited = apply_shift(&notes, &[0, 1, 2], &shift, 64., 1.);
            let updates: Vec<_> = edited
                .iter()
                .zip(&notes)
                .filter(|(a, b)| (a.start, a.len, a.pitch) != (b.start, b.len, b.pitch))
                .map(|(n, _)| json!({"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}))
                .collect();
            let history_before = e.history().0.len();
            e.call(
                "update_note_geometry",
                &json!({"track":"keys","updates":updates}),
            )
            .unwrap();
            assert_eq!(e.history().0.len(), history_before + 1);
            let mut expected = before.clone();
            expected["patterns"][0]["clips"]["keys"] = serde_json::to_value(&edited).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
            e.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            e.call("redo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        }
    }
}

#[cfg(test)]
mod geometry_numeric_limits_tests {
    use super::*;
    #[test]
    fn extreme_semitone_deltas_clamp_without_integer_overflow() {
        let notes = vec![Note::new(80., 0.02, 127, 0.6), Note::new(4., 1., 0, 0.8)];
        for delta in [i32::MIN, i32::MAX] {
            let shift = NoteShift {
                index: 0,
                resize: false,
                d_steps: 0.,
                d_semi: delta,
            };
            let mut expected = notes.clone();
            for n in &mut expected {
                n.pitch = if delta > 0 { 127 } else { 0 };
            }
            assert_eq!(apply_shift(&notes, &[0, 1], &shift, 64., 1.), expected);
        }
    }
    #[test]
    fn overflowing_resize_preserves_original_length_and_other_notes() {
        let notes = vec![
            Note::new(4., f32::MAX, 64, 0.6),
            Note::new(80., 0.02, 72, 0.),
        ];
        let shift = NoteShift {
            index: 0,
            resize: true,
            d_steps: f32::MAX,
            d_semi: 0,
        };
        assert_eq!(apply_shift(&notes, &[0, 1], &shift, 64., 1.), notes);
    }
}

#[cfg(test)]
mod move_selection_lookup_tests {
    use super::*;
    #[test]
    fn move_lookup_matches_linear_reference_for_mixed_selection_membership() {
        for seed in 0..64usize {
            let notes: Vec<_> = (0..256usize)
                .map(|i| {
                    let mut n = Note::new(
                        ((i * 17 + seed) % 97) as f32,
                        (i % 7 + 1) as f32 * 0.01,
                        (i % 128) as u8,
                        0.5,
                    );
                    n.id = format!("n{i}");
                    n.prob = 0.3;
                    n.offset = 0.1;
                    n.slide_to = Some(72);
                    n
                })
                .collect();
            let index = seed * 3 % notes.len();
            let mut selected: Vec<_> = (seed % 5..notes.len())
                .step_by(seed % 9 + 1)
                .rev()
                .collect();
            selected.extend([9999, seed, seed]);
            for delta in [-4., 0., 4.] {
                let shift = NoteShift {
                    index,
                    resize: false,
                    d_steps: delta,
                    d_semi: 2,
                };
                let mut expected = notes.clone();
                let targets: Vec<_> = (0..notes.len())
                    .filter(|i| *i == index || (selected.contains(&index) && selected.contains(i)))
                    .collect();
                let actual_delta = if targets.len() > 1 { 0.0 } else { delta }; // generated groups contain out-of-pattern starts
                for (i, n) in expected.iter_mut().enumerate() {
                    if targets.contains(&i) {
                        if actual_delta != 0. {
                            n.start = (n.start + actual_delta).clamp(0., 63.95);
                        }
                        n.pitch = (u16::from(n.pitch) + 2).min(127) as u8;
                    }
                }
                assert_eq!(apply_shift(&notes, &selected, &shift, 64., 1.), expected);
            }
        }
    }
    #[test]
    fn dense_move_handles_large_reversed_selection_without_field_changes() {
        let notes: Vec<_> = (0..16384usize)
            .map(|i| Note::new((i % 32) as f32, 0.02, 60, 0.5))
            .collect();
        let selected: Vec<_> = (0..notes.len()).rev().collect();
        let shift = NoteShift {
            index: 8192,
            resize: false,
            d_steps: 1.,
            d_semi: 0,
        };
        let mut expected = notes.clone();
        for n in &mut expected {
            n.start += 1.;
        }
        assert_eq!(apply_shift(&notes, &selected, &shift, 64., 1.), expected);
    }
}

#[cfg(test)]
mod snap_numeric_safety_tests {
    use super::*;
    #[test]
    fn invalid_or_overflowing_snap_preserves_finite_position() {
        for x in [-4., 0., 3.3, f32::MAX] {
            for grid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0., -1.] {
                assert_eq!(snap_to(x, grid), x);
            }
        }
        assert_eq!(snap_to(f32::MAX, f32::MIN_POSITIVE), f32::MAX);
        assert_eq!(snap_to(f32::MAX, f32::MAX * 0.6), f32::MAX);
        assert_eq!(snap_to(f32::INFINITY, 1.), f32::INFINITY);
        assert!(snap_to(f32::NAN, 1.).is_nan());
    }
    #[test]
    fn ordinary_snap_grids_match_rounding_reference() {
        for grid in [0.25, 0.5, 1., 2., 4., 8.] {
            for i in -4096..4096 {
                let x = i as f32 * 0.0625;
                let expected = (x / grid).round() * grid;
                assert_eq!(snap_to(x, grid), expected);
                assert_eq!(snap_to(snap_to(x, grid), grid), expected);
            }
        }
    }
}

#[cfg(test)]
mod dense_engine_geometry_tests {
    use super::*;
    #[test]
    fn dense_selected_transpose_keeps_fields_and_exact_history_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("beatbox-dense-geometry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = crate::Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let notes: Vec<_> = (0..16384usize)
            .map(|i| {
                let mut n = Note::new(
                    (i / 128) as f32 * 0.5,
                    0.25,
                    48 + (i % 24) as u8,
                    0.4 + (i % 7) as f32 * 0.1,
                );
                n.id = format!("dense{i}");
                n.prob = 0.75;
                n.offset = 0.1;
                n.slide_to = Some(72);
                n
            })
            .collect();
        e.project.patterns[0]
            .clips
            .insert("keys".into(), notes.clone());
        let before = serde_json::to_value(&e.project).unwrap();
        let selected: Vec<_> = (0..notes.len()).rev().collect();
        let edited = apply_shift(
            &notes,
            &selected,
            &NoteShift {
                index: 8192,
                resize: false,
                d_steps: 0.,
                d_semi: 1,
            },
            64.,
            1.,
        );
        let updates: Vec<_> = edited
            .iter()
            .map(|n| json!({"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}))
            .collect();
        let history = e.history().0.len();
        e.call(
            "update_note_geometry",
            &json!({"track":"keys","updates":updates}),
        )
        .unwrap();
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"] = serde_json::to_value(&edited).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        assert_eq!(e.history().0.len(), history + 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod grouped_time_preservation_tests {
    use super::*;
    #[test]
    fn boundaries_use_one_delta_and_off_pattern_group_blocks_time_only() {
        for (starts, requested, expected) in [
            ([0., 8.], -6., [0., 8.]),
            ([4., 62.], 4., [5.95, 63.95]),
            ([4., 80.], 2., [4., 80.]),
        ] {
            let notes: Vec<_> = starts.iter().map(|s| Note::new(*s, 1., 60, 0.5)).collect();
            let edited = apply_shift(
                &notes,
                &[0, 1, 1, 999],
                &NoteShift {
                    index: 0,
                    resize: false,
                    d_steps: requested,
                    d_semi: 1,
                },
                64.,
                1.,
            );
            for i in 0..2 {
                assert!((edited[i].start - expected[i]).abs() < 0.00001);
                assert_eq!(edited[i].pitch, 61);
            }
            assert!(
                ((edited[1].start - edited[0].start) - (notes[1].start - notes[0].start)).abs()
                    < 0.00001
            );
        }
    }
}

#[cfg(test)]
mod grouped_move_history_tests {
    use super::*;

    #[test]
    fn group_bounds_preserve_expression_unselected_notes_and_history() {
        for (case, starts, delta, semi) in [
            ("left", [2., 10.], -6., 0),
            ("right", [4., 62.], 4., 0),
            ("blocked", [4., 80.], 2., 0),
            ("blocked_transpose", [4., 80.], 2., 1),
        ] {
            let dir =
                std::env::temp_dir().join(format!("beatbox-group-{case}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut e = crate::Engine::new(dir.clone());
            e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
                .unwrap();
            let notes: Vec<_> = [starts[0], starts[1], 12.]
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut n = Note::new(*s, 0.02 + i as f32, 60 + i as u8, i as f32 * 0.3);
                    n.id = format!("group{i}");
                    n.prob = 0.25 + i as f32 * 0.25;
                    n.offset = -0.125 + i as f32 * 0.1;
                    n.slide_to = Some(72);
                    n
                })
                .collect();
            e.project.patterns[0]
                .clips
                .insert("keys".into(), notes.clone());
            let before = serde_json::to_value(&e.project).unwrap();
            let edited = apply_shift(
                &notes,
                &[1, 0, 1, 999],
                &NoteShift {
                    index: 0,
                    resize: false,
                    d_steps: delta,
                    d_semi: semi,
                },
                64.,
                1.,
            );
            assert_eq!(edited[2], notes[2]);
            for i in 0..2 {
                let mut expected = notes[i].clone();
                expected.start = edited[i].start;
                expected.pitch = edited[i].pitch;
                assert_eq!(edited[i], expected);
            }
            assert!(
                ((edited[1].start - edited[0].start) - (starts[1] - starts[0])).abs() < 0.00001
            );
            let updates: Vec<_> = edited
                .iter()
                .zip(&notes)
                .filter(|(a, b)| (a.start, a.len, a.pitch) != (b.start, b.len, b.pitch))
                .map(|(n, _)| json!({"id":n.id,"start":n.start,"len":n.len,"pitch":n.pitch}))
                .collect();
            let history = e.history().0.len();
            e.call(
                "update_note_geometry",
                &json!({"track":"keys","updates":updates}),
            )
            .unwrap();
            let mut expected = before.clone();
            expected["patterns"][0]["clips"]["keys"] = serde_json::to_value(&edited).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
            if updates.is_empty() {
                assert_eq!(e.history().0.len(), history);
                assert_eq!(expected, before);
            } else {
                assert_eq!(e.history().0.len(), history + 1);
                e.call("undo", &json!({})).unwrap();
                assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
                e.call("redo", &json!({})).unwrap();
                assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
            }
            e.call("save_project", &json!({"path":"group.json"}))
                .unwrap();
            e.call("load_project", &json!({"path":"group.json"}))
                .unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn invalid_group_starts_block_time_without_touching_other_fields() {
        for start in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.01, 64.] {
            let notes = vec![Note::new(4., 1., 60, 0.5), Note::new(start, 1., 64, 0.7)];
            assert_eq!(group_move_delta(&notes, &[0, 1], 0, 2., 64.), 0.);
            let out = apply_shift(
                &notes,
                &[0, 1],
                &NoteShift {
                    index: 0,
                    resize: false,
                    d_steps: 2.,
                    d_semi: 1,
                },
                64.,
                1.,
            );
            for i in 0..2 {
                assert_eq!(out[i].start.to_bits(), notes[i].start.to_bits());
                assert_eq!(out[i].pitch, notes[i].pitch + 1);
                assert_eq!((out[i].len, out[i].vel), (notes[i].len, notes[i].vel));
            }
        }
    }
}
