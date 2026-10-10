// Adapted from SoundCraft `crates/ui-egui/src/widgets.rs` (`meter_pos`, `pan_text`, `db_text`) and
// `crates/model/src/mixer.rs` (`fader_pos_to_db`, `fader_db_to_pos`) at commit eac0edd.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! Console laws shared by the studio's mixer widgets: the fader taper, the hardware
//! meter scale and the pan / dB readouts. Kept outside `gui` so they are unit-tested
//! in the headless (no-GUI) build.

/// Lowest dB a console fader reaches (shown as -inf).
pub const FADER_MIN_DB: f32 = -144.0;
/// Highest dB a console fader reaches.
pub const FADER_MAX_DB: f32 = 12.0;

fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        d
    }
}

/// Fader position 0..1 → dB. The top 75 % covers -24..+12 dB linearly; the bottom quarter
/// compresses down to -inf (-144 dB), like a hardware console.
pub fn fader_pos_to_db(pos: f32) -> f32 {
    let p = finite_or(pos, 0.0).clamp(0.0, 1.0);
    if p <= 0.0 {
        return FADER_MIN_DB;
    }
    if p >= 0.25 {
        -24.0 + (p - 0.25) / 0.75 * 36.0
    } else {
        -24.0 - (1.0 - p / 0.25).powf(1.5) * 120.0
    }
}

/// dB → fader position 0..1 (inverse of [`fader_pos_to_db`]).
pub fn fader_db_to_pos(db: f32) -> f32 {
    let db = finite_or(db, FADER_MIN_DB);
    if db <= FADER_MIN_DB {
        return 0.0;
    }
    if db >= -24.0 {
        (0.25 + (db.min(FADER_MAX_DB) + 24.0) / 36.0 * 0.75).min(1.0)
    } else {
        let x = ((-24.0 - db) / 120.0).clamp(0.0, 1.0).powf(1.0 / 1.5);
        ((1.0 - x) * 0.25).max(0.0)
    }
}

/// Format a pan value like a console: "<45", "0", "45>".
pub fn pan_text(v: f32) -> String {
    let n = (finite_or(v, 0.0).clamp(-1.0, 1.0) * 100.0).round() as i32;
    match n {
        0 => "0".into(),
        n if n < 0 => format!("<{}", -n),
        n => format!("{n}>"),
    }
}

/// dB → 0..1 meter position on a hardware-style scale:
/// -80..-40 dB fills the bottom 10 %, -40..-20 the next 20 %, -20..0 the top 70 %.
pub fn hw_meter_pos(db: f32) -> f32 {
    if !db.is_finite() || db <= -80.0 {
        return 0.0;
    }
    if db >= 0.0 {
        return 1.0;
    }
    if db < -40.0 {
        (db + 80.0) / 40.0 * 0.1
    } else if db < -20.0 {
        0.1 + (db + 40.0) / 20.0 * 0.2
    } else {
        0.3 + (db + 20.0) / 20.0 * 0.7
    }
}

/// dB readout: "-inf" at the bottom of the fader.
pub fn db_text(db: f32) -> String {
    if !db.is_finite() || db <= FADER_MIN_DB + 0.1 {
        "-inf".into()
    } else {
        format!("{db:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_law_round_trips() {
        for db in [-100.0f32, -60.0, -24.0, -12.0, -6.0, 0.0, 6.0, 12.0] {
            let back = fader_pos_to_db(fader_db_to_pos(db));
            assert!((back - db).abs() < 0.05, "{db} -> {back}");
        }
        assert_eq!(fader_db_to_pos(f32::NAN), 0.0);
        assert_eq!(fader_pos_to_db(f32::NAN), FADER_MIN_DB);
        assert_eq!(fader_db_to_pos(40.0), 1.0);
        assert!((fader_db_to_pos(0.0) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn meter_scale_is_monotonic() {
        let mut last = -1.0;
        for db in (-90..=3).map(|d| d as f32) {
            let p = hw_meter_pos(db);
            assert!(p >= last);
            last = p;
        }
        assert_eq!(hw_meter_pos(f32::NAN), 0.0);
        assert_eq!(hw_meter_pos(-20.0), 0.3);
    }

    #[test]
    fn pan_and_db_text() {
        assert_eq!(pan_text(0.0), "0");
        assert_eq!(pan_text(-0.45), "<45");
        assert_eq!(pan_text(1.0), "100>");
        assert_eq!(pan_text(f32::NAN), "0");
        assert_eq!(db_text(-144.0), "-inf");
        assert_eq!(db_text(-6.04), "-6.0");
    }
}
