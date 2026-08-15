//! The `Source` trait and its adapters (spec §6).
//!
//! A source answers one question on a poll cadence: what is playing, where is it,
//! and is it moving. Everything else in the runtime is downstream of that.
//!
//! # Why this trait is synchronous, unlike spec §6.1
//!
//! The spec sketches an `#[async_trait]` interface. Nothing that exists needs it,
//! and it is not free: `async_trait` boxes every call, and an async trait implies a
//! tokio runtime in the workspace.
//!
//! The adapters divide cleanly. SMTC (M4) is WinRT, whose async operations expose a
//! blocking `join()` — Phase 0.5 used exactly that — and it runs on its own thread
//! anyway (spec §3.2), where blocking is the point. Spotify and Yandex (M6) are
//! HTTP and genuinely want async, but they will bring their own runtime with them.
//!
//! So the cost of deferring is one `Source` impl wrapping `block_on` when an HTTP
//! adapter first appears, and the cost of not deferring is a runtime dependency
//! carried from M1 to M6 for nothing. Revisit at M6, not before.

use std::time::{Duration, Instant};

use dancer_score::TrackMeta;

pub mod file;
pub use file::FileSource;

#[cfg(windows)]
pub mod smtc;
#[cfg(windows)]
pub use smtc::SmtcSource;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source unavailable: {0}")]
    Unavailable(String),
    #[error("reading from {source_name}: {message}")]
    Read {
        source_name: &'static str,
        message: String,
    },
}

/// One reading from a player.
#[derive(Debug, Clone)]
pub struct Observation {
    pub track: TrackMeta,
    /// Position in media time, as reported.
    pub position: Duration,
    pub playing: bool,
    /// The local monotonic instant at which `position` was true.
    ///
    /// **Not the moment of the read.** Phase 0.5 measured SMTC returning a position
    /// 87 seconds old, with `LastUpdatedTime` naming when it had been correct.
    /// Pairing the value with that instant makes a stale reading exact rather than
    /// wrong; pairing it with `Instant::now()` would put the dancer 87 s out.
    pub observed_at: Instant,
    /// Whether `position` and `observed_at` mean anything.
    ///
    /// Some sessions publish identity but no timeline at all (spec §6.2). Encoding
    /// that as a sentinel position would be a lie the clock cannot detect, so it is
    /// a flag: the caller reports the track and stays `Unscored`.
    pub timeline: bool,
}

impl Observation {
    pub fn position_secs(&self) -> f64 {
        self.position.as_secs_f64()
    }
}

pub trait Source: Send {
    fn name(&self) -> &'static str;

    /// Cheap check — is this source usable right now?
    fn available(&self) -> bool;

    /// One observation, or `None` when nothing is playing.
    fn poll(&mut self) -> Result<Option<Observation>, SourceError>;

    /// How coarse this source's position reporting is, for drift tuning.
    fn position_granularity(&self) -> Duration;
}
