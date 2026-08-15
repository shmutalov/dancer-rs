//! Messages into the render thread (spec §3.2).
//!
//! The render thread owns all authoritative state; everything else sends messages
//! in. No shared mutable state, no locks in the render path.
//!
//! These are drained in `about_to_wait` rather than waking the event loop through
//! an `EventLoopProxy`. That costs up to one frame of latency and it does not
//! matter: every timing-critical value travels with its own instant — `at` here,
//! `observed_at` in an `Observation` — so a message handled 16 ms late is still
//! folded in against the moment it was true. Latency would only matter if the
//! render thread timestamped on receipt, which is exactly what spec §6.1 forbids.

use std::sync::Arc;
use std::time::Instant;

use dancer_score::{Score, TrackId, TrackMeta};

#[derive(Debug, Clone)]
pub enum AppEvent {
    TrackChanged {
        id: TrackId,
        meta: TrackMeta,
    },
    PositionReport {
        pos_secs: f64,
        playing: bool,
        /// The instant `pos_secs` was true — not the instant it was read.
        at: Instant,
    },
    PlaybackStopped,
    ScoreReady {
        id: TrackId,
        score: Arc<Score>,
    },
    /// The polling source failed. Carried rather than logged at the origin so the
    /// state machine can decide whether it is fatal.
    SourceLost(String),
}
