//! The vendored certification assets must equal the sources they were copied
//! from.
//!
//! `hcr` embeds the V4 Profile and the conformance vectors because a published
//! crate carries only its own directory — it cannot read the frontend
//! repository or a sibling crate's fixtures. That copy is exactly the kind of
//! duplicate that drifts silently, so this test compares it against the
//! original whenever the checkout that owns the original is present. Consumers
//! who only have the published crate see the checks skip, not fail.

use std::path::Path;

fn compare(vendored: &str, source: &Path, label: &str) {
    if !source.exists() {
        eprintln!("skipping {label}: {} is not in this checkout", source.display());
        return;
    }
    let original = std::fs::read_to_string(source).expect("source asset is readable");
    assert_eq!(
        vendored,
        original,
        "{label} has drifted from {}; re-copy it after regenerating the fixture",
        source.display(),
    );
}

#[test]
fn assets_match_their_sources() {
    compare(
        include_str!("../assets/cutter-grid-profile-v4.json"),
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../HCR_Simulator_Frontend/tests/fixtures/cutter-grid-profile-v4.json",
        )),
        "assets/cutter-grid-profile-v4.json",
    );
    compare(
        include_str!("../assets/vectors.json"),
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../hcr_sim/tests/fixtures/vectors.json",
        )),
        "assets/vectors.json",
    );
}
