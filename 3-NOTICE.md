# Third-party notices

beatbox-rs bundles or depends on the following third-party components.
License rule (Naman's Ransack directive): permissive only (MIT/Apache/BSD),
no GPL/NC; new deps are flagged, pinned and listed here.

| Crate | Version | License | Used for |
|-------|---------|---------|----------|
| rtrb  | =0.4.0 (pinned) | MIT OR Apache-2.0 | RT engine: wait-free SPSC rings between control and audio threads (src/rt.rs `mod ring`, the only place it is touched) |

rtrb: https://github.com/mgeier/rtrb - license texts:
https://github.com/mgeier/rtrb/blob/main/LICENSE-MIT
https://github.com/mgeier/rtrb/blob/main/LICENSE-Apache-2.0
Licenses verified 2026-10-17 via crates.io and docs.rs.
