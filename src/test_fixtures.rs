//! Recorded regression setup shared by tests, never production builds.
use std::path::PathBuf;
use std::sync::Once;

pub fn path(name: &str) -> PathBuf {
    static PREPARED: Once = Once::new();
    PREPARED.call_once(|| {
        let status = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/prepare_test_fixtures.py"
            ))
            .status()
            .expect("python3 is required to prepare recorded regression fixtures");
        assert!(
            status.success(),
            "recorded regression fixture preparation failed"
        );
    });
    let root = std::env::temp_dir();
    match name {
        "recorded-groove.json" => root.join("recorded-beat-project").join(name),
        "keys-demo.json" | "v202-gui-after.json" | "roll-empty-fixture.json" | "room-demo.json" => {
            root.join(name)
        }
        _ => panic!("unknown recorded regression fixture: {name}"),
    }
}
