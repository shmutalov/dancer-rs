//! The hand-written fixtures, as permanent regression tests (ROADMAP M1).
//!
//! Their value is being known-correct by construction: when M2's analyzer output
//! disagrees with the clock, these say which one is wrong. Regenerate with
//! `cargo run -p dancer-score --example gen-fixtures`.

use std::path::PathBuf;

use dancer_score::{Score, ScoreSource};

fn fixture(name: &str) -> Score {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    Score::load(&path).unwrap_or_else(|e| panic!("loading {name}: {e}"))
}

#[test]
fn all_fixtures_load_and_validate() {
    for name in [
        "steady-120.json",
        "waltz-90.json",
        "drifting-128.json",
        "labelled-124.json",
    ] {
        let s = fixture(name);
        s.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!s.beats.is_empty(), "{name} has no beats");
    }
}

#[test]
fn steady_120_is_arithmetically_exact() {
    // The baseline is only useful if it is exactly what it claims. Beat every
    // 0.5 s, bar every 2 s, for three minutes.
    let s = fixture("steady-120.json");
    assert_eq!(s.bpm, 120.0);
    assert_eq!(s.meter, 4);
    assert_eq!(s.beats.len(), 360);
    for (i, &b) in s.beats.iter().enumerate() {
        assert!((b - i as f64 * 0.5).abs() < 1e-12, "beat {i} at {b}");
    }
    assert!((s.next_bar_after(0.1).unwrap() - 2.0).abs() < 1e-12);
    assert_eq!(s.confidence, 1.0);
}

#[test]
fn waltz_is_in_three() {
    // Guards anything that quietly assumes 4/4 (spec §5).
    let s = fixture("waltz-90.json");
    assert_eq!(s.meter, 3);
    assert_eq!(s.bar_beat(0), 1);
    assert_eq!(s.bar_beat(3), 1);
    // Bars are three beats of 2/3 s each.
    assert!((s.next_bar_after(0.5).unwrap() - 2.0).abs() < 1e-9);
}

#[test]
fn drifting_fixture_actually_drifts() {
    // If this stopped being true it would silently stop testing spec §11.1's
    // "local interval, not global BPM" rule.
    let s = fixture("drifting-128.json");
    let first = s.interval_at(0);
    let last = s.interval_at(s.beats.len() - 2);
    assert!(last < first * 0.99, "intervals {first} -> {last} barely moved");

    // The stored bpm is the starting tempo and is wrong by the end — deliberately.
    let global = 60.0 / s.bpm;
    assert!(
        (last - global).abs() > 0.005,
        "global bpm should disagree with the local interval at the end"
    );
}

#[test]
fn only_the_labelled_fixture_has_segments() {
    // Empty segments are the normal case (spec §5, §8.1); everything downstream
    // must tolerate it, so most fixtures deliberately have none.
    assert!(fixture("steady-120.json").segments.is_empty());
    assert!(fixture("waltz-90.json").segments.is_empty());
    assert!(fixture("drifting-128.json").segments.is_empty());

    let s = fixture("labelled-124.json");
    assert_eq!(s.source, ScoreSource::Allin1);
    assert_eq!(s.segment_at(50.0).map(|g| g.label.as_str()), Some("chorus"));
    assert_eq!(s.segment_at(1.0).map(|g| g.label.as_str()), Some("intro"));
    assert!(!s.cues.is_empty());
}
