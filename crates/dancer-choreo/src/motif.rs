//! What a move *is*, in Motif vocabulary (spec §11.3).
//!
//! # Where this comes from
//!
//! Motif Notation is Rudolf Laban's own simplified subset of Labanotation: instead
//! of prescribing every limb, it names the *essence* of an action — travel, turn,
//! spring, gesture, stillness. Full Labanotation is no use to us, because it drives
//! a skeleton and we have eight pre-drawn cells; you cannot synthesise "left arm,
//! forward high" out of somebody's artwork. Motif is exactly the level that does
//! transfer, because it describes a move rather than a body.
//!
//! # The problem it solves
//!
//! Selection used to run on one scalar, `energy`. That cannot distinguish a small
//! travelling step from a small gesture, and — the reported complaint — nothing
//! stopped a full spin from being chosen for a quiet passage as long as its declared
//! energy happened to land in the window. The user's description of what should have
//! happened was itself a Motif statement:
//!
//! > human will just stay in beat only by moving its feet, and maybe its hands a
//! > little
//!
//! That is [`Motif::Step`] and [`Motif::Gesture`], and no [`Motif::Turn`]. The
//! manifest had no way to say it.
//!
//! # How it gates selection
//!
//! Every motif carries an intrinsic [`exertion`](Motif::exertion) — how much of a
//! body it takes to do. A row's exertion is the *largest* of its motifs, because the
//! biggest action in a move is what the move reads as. Each energy tier then has a
//! ceiling, so a turn is inadmissible in a calm passage **whatever energy the sheet
//! declared for it**. That is the part `energy` alone could not express.
//!
//! Rows that declare no motif stay admissible. Every inherited sheet is untagged,
//! and a rule that excluded them would leave nothing to dance.

use crate::energy::Tier;

/// A move's essential action.
///
/// Deliberately small. Motif's full symbol set is larger, but these are the ones a
/// sprite sheet can actually distinguish and a sheet author can tag without
/// training.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Motif {
    /// No action: holding a shape. Laban's retention.
    Stillness,
    /// Limbs that carry no weight — arms, head. The "hands a little".
    Gesture,
    /// Weight transferred on the spot; feet keeping time.
    Step,
    /// Locomotion: the figure covers ground.
    Travel,
    /// Rotation about the body's own axis.
    Turn,
    /// Both feet leave the floor. Laban calls this a spring.
    Jump,
    /// Level rises.
    Rise,
    /// Level sinks.
    Sink,
    /// The body spreads — Laban Shape's spreading.
    Expand,
    /// The body encloses — Laban Shape's enclosing.
    Contract,
}

impl Motif {
    /// How much body the action takes, `0.0..=1.0`.
    ///
    /// Not a measurement of anything; a ranking of actions against each other, which
    /// is all the tier ceilings need. The ordering is the load-bearing part — that
    /// a jump costs more than a step, and a turn more than a travel — rather than
    /// any individual number.
    pub const fn exertion(self) -> f32 {
        match self {
            Motif::Stillness => 0.00,
            Motif::Gesture => 0.20,
            Motif::Step => 0.30,
            Motif::Contract => 0.35,
            // Sinking is a giving-in to weight; rising works against it.
            Motif::Sink => 0.40,
            Motif::Rise => 0.50,
            Motif::Expand => 0.55,
            Motif::Travel => 0.70,
            Motif::Turn => 0.85,
            Motif::Jump => 1.00,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Motif::Stillness => "stillness",
            Motif::Gesture => "gesture",
            Motif::Step => "step",
            Motif::Travel => "travel",
            Motif::Turn => "turn",
            Motif::Jump => "jump",
            Motif::Rise => "rise",
            Motif::Sink => "sink",
            Motif::Expand => "expand",
            Motif::Contract => "contract",
        }
    }

    /// Parse a manifest tag.
    ///
    /// Synonyms are accepted because sheet authors are not notators: the Laban term
    /// and the obvious English word should both work, so nobody has to look up that
    /// a jump is called a spring.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "stillness" | "still" | "hold" | "pause" | "balance" => Motif::Stillness,
            "gesture" | "gestures" | "arms" => Motif::Gesture,
            "step" | "steps" | "support" | "weight" => Motif::Step,
            "travel" | "travelling" | "traveling" | "walk" | "run" | "path" => Motif::Travel,
            "turn" | "turning" | "spin" | "rotate" | "rotation" => Motif::Turn,
            "jump" | "spring" | "leap" | "hop" => Motif::Jump,
            "rise" | "rising" | "up" => Motif::Rise,
            "sink" | "sinking" | "down" | "crouch" => Motif::Sink,
            "expand" | "spread" | "open" | "grow" => Motif::Expand,
            "contract" | "enclose" | "close" | "shrink" => Motif::Contract,
            _ => return None,
        })
    }
}

impl std::str::FromStr for Motif {
    type Err = UnknownMotif;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Motif::parse(s).ok_or_else(|| UnknownMotif(s.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown motif `{0}`")]
pub struct UnknownMotif(pub String);

/// Exertion of a whole row: the largest of its motifs.
///
/// The maximum rather than the mean, because a move containing a jump *is* a jump
/// as far as a viewer is concerned — averaging it against a gesture in the same row
/// would let a big action in through the calm ceiling.
///
/// `None` when the row declares nothing, which is not the same as declaring
/// stillness: it means the sheet has no opinion and selection must not invent one.
pub fn exertion(motifs: &[Motif]) -> Option<f32> {
    motifs
        .iter()
        .map(|m| m.exertion())
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

/// The most a move may cost in this tier.
///
/// Calm stops below rising, so what is left is stillness, gesture, stepping and
/// enclosing — keeping time without doing anything. Steady stops below turning and
/// jumping, so the big moves are reserved for the loud tier and for drops, which
/// override this entirely.
pub const fn ceiling(tier: Tier) -> f32 {
    match tier {
        Tier::Calm => 0.40,
        Tier::Steady => 0.80,
        Tier::Loud => 1.00,
    }
}

/// Exertion at or above which a row counts as a big move, for drops.
pub const BIG_MOVE: f32 = 0.75;

/// Whether a row's motifs suit this tier.
///
/// Untagged rows are always admitted — see the module docs.
pub fn admits(tier: Tier, motifs: &[Motif]) -> bool {
    match exertion(motifs) {
        // A hair of slack so a motif sitting exactly on a ceiling is admitted
        // rather than excluded by float representation.
        Some(e) => e <= ceiling(tier) + 1e-6,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_is_not_admitted_by_a_quiet_passage() {
        // The complaint, as an assertion: no spinning through the silent beats,
        // regardless of what energy the sheet declared for the spin row.
        assert!(!admits(Tier::Calm, &[Motif::Turn]));
        assert!(!admits(Tier::Steady, &[Motif::Turn]));
        assert!(admits(Tier::Loud, &[Motif::Turn]));
    }

    #[test]
    fn keeping_time_with_feet_and_hands_is_admitted_by_a_quiet_passage() {
        // The described alternative, verbatim: feet, and maybe the hands a little.
        assert!(admits(Tier::Calm, &[Motif::Step, Motif::Gesture]));
        assert!(admits(Tier::Calm, &[Motif::Stillness]));
    }

    #[test]
    fn the_biggest_action_in_a_row_decides_it() {
        // A row that gestures *and* jumps reads as a jump. Averaging would let it
        // through the calm ceiling, which is exactly the failure being fixed.
        let both = [Motif::Gesture, Motif::Jump];
        assert_eq!(exertion(&both), Some(1.0));
        assert!(!admits(Tier::Calm, &both));
    }

    #[test]
    fn an_untagged_row_is_never_excluded() {
        // Every inherited sheet is untagged. Excluding them would leave the dancer
        // with nothing to pick.
        assert_eq!(exertion(&[]), None);
        for tier in [Tier::Calm, Tier::Steady, Tier::Loud] {
            assert!(admits(tier, &[]));
        }
    }

    #[test]
    fn tiers_widen_rather_than_shift() {
        // Anything a calm passage admits, a loud one must admit too — otherwise a
        // quiet move could not be used as contrast inside a loud section.
        for m in [
            Motif::Stillness,
            Motif::Gesture,
            Motif::Step,
            Motif::Travel,
            Motif::Turn,
            Motif::Jump,
            Motif::Rise,
            Motif::Sink,
            Motif::Expand,
            Motif::Contract,
        ] {
            if admits(Tier::Calm, &[m]) {
                assert!(admits(Tier::Steady, &[m]), "{m:?}");
            }
            if admits(Tier::Steady, &[m]) {
                assert!(admits(Tier::Loud, &[m]), "{m:?}");
            }
        }
    }

    #[test]
    fn laban_terms_and_plain_english_both_parse() {
        // Sheet authors are not notators.
        assert_eq!(Motif::parse("spring"), Some(Motif::Jump));
        assert_eq!(Motif::parse("jump"), Some(Motif::Jump));
        assert_eq!(Motif::parse("  Turn "), Some(Motif::Turn));
        assert_eq!(Motif::parse("enclose"), Some(Motif::Contract));
        assert_eq!(Motif::parse("plié"), None);
    }

    #[test]
    fn every_motif_round_trips_through_its_name() {
        for m in [
            Motif::Stillness,
            Motif::Gesture,
            Motif::Step,
            Motif::Travel,
            Motif::Turn,
            Motif::Jump,
            Motif::Rise,
            Motif::Sink,
            Motif::Expand,
            Motif::Contract,
        ] {
            assert_eq!(Motif::parse(m.as_str()), Some(m), "{m:?}");
        }
    }
}
