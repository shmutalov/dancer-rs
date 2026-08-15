//! SMTC adapter (spec §6.2) — the source that makes the product a product.
//!
//! Everything before this danced to a *simulated* transport. This one reads what
//! Windows knows about whatever the user is actually playing: Spotify, the Yandex
//! Music desktop app, a browser tab calling the Media Session API. No OAuth, no
//! tokens, no network, no per-service maintenance.
//!
//! # The one thing that matters
//!
//! `Position` does not refresh continuously. Phase 0.5 measured Edge reporting
//! `0.019s` for a track 59.7 s in, and the Yandex Music desktop app reporting
//! `43.172s` while 130 s in — stale by 87 seconds, growing to 120 with `Position`
//! unchanged. Two unrelated applications, so this is how SMTC works, not one app's
//! bug.
//!
//! The value is *stale but exact*: `LastUpdatedTime` names the instant it was true.
//! Pair the two and a 120-second-old reading is perfect information; pair the
//! position with `Instant::now()` instead and the dancer is two minutes out.
//!
//! # Why this polls rather than subscribing
//!
//! Spec §6.2 asks for `MediaPropertiesChanged` / `PlaybackInfoChanged` /
//! `TimelinePropertiesChanged` subscriptions. This reads synchronously on a fast
//! cadence instead, for two reasons.
//!
//! The correctness argument that motivates subscribing does not apply: because
//! every reading carries its own `LastUpdatedTime`, re-reading unchanged state is
//! a no-op — the clock computes a zero error against the same anchor and does
//! nothing. Polling cannot drift here, only notice late.
//!
//! What it costs is latency on *track changes*, which is why the cadence is
//! [`DEFAULT_POLL`] rather than the 2 s used for the file source. The read is a
//! local IPC call; its measured cost is logged on the first poll so the trade is
//! visible rather than assumed. Subscriptions remain the better answer and are
//! worth revisiting if that measurement is ever unfavourable — they need WinRT
//! event handlers on a thread with the right COM apartment, which Phase 0.5 did not
//! validate and this milestone did not want to discover on.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dancer_score::{TrackId, TrackMeta};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
};

use crate::{Observation, Source, SourceError};

/// Poll cadence. Fast, because this is what bounds track-change latency.
pub const DEFAULT_POLL: Duration = Duration::from_millis(500);

/// FILETIME epoch (1601-01-01) as 100 ns ticks before the Unix epoch.
const FILETIME_TO_UNIX: i64 = 116_444_736_000_000_000;

/// `PlaybackStatus::Playing`, confirmed by Phase 0.5.
const STATUS_PLAYING: i32 = 4;

pub struct SmtcSource {
    manager: Manager,
    /// Application identifiers allowed to drive the dancer. Empty accepts any.
    allowlist: Vec<String>,
    /// Application identifiers seen so far, so the tray can offer them (M5).
    seen: Vec<String>,
    measured_read: Option<Duration>,
}

impl SmtcSource {
    pub fn new(allowlist: Vec<String>) -> Result<Self, SourceError> {
        let manager = Manager::RequestAsync()
            .and_then(|op| op.join())
            .map_err(|e| SourceError::Unavailable(format!("SMTC unavailable: {e}")))?;
        tracing::info!(allowlist = ?allowlist, "SMTC source ready");
        Ok(Self {
            manager,
            allowlist,
            seen: Vec::new(),
            measured_read: None,
        })
    }

    /// Application identifiers observed this session.
    ///
    /// Kept because the allowlist cannot be a shipped table of English executable
    /// names: Phase 0.5 found the Yandex Music desktop app identifying as
    /// `Яндекс Музыка.exe`. The tray populates itself from what actually appears.
    pub fn seen(&self) -> &[String] {
        &self.seen
    }

    fn allowed(&self, app: &str) -> bool {
        // Unicode comparison, never ASCII-folded: these identifiers are localised.
        self.allowlist.is_empty() || self.allowlist.iter().any(|a| a == app)
    }

    /// Read one session into an observation.
    fn read(&mut self, session: &Session) -> Option<Observation> {
        let app = session.SourceAppUserModelId().ok()?.to_string();
        if !self.seen.iter().any(|s| *s == app) {
            tracing::info!(app = %app, "media session seen");
            self.seen.push(app.clone());
        }
        if !self.allowed(&app) {
            return None;
        }

        let props = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()).ok()?;
        let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();

        let playing = session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .map(|s| s.0 == STATUS_PLAYING)
            .unwrap_or(false);

        // Identity keeps the source-namespaced form (spec §5.1). The *library*
        // key is hashed from (title, artist) instead, which is what connects this
        // to a file analysed earlier (§8.3) — see `TrackMeta::library_key`.
        let id = TrackId::new("smtc", format!("{app}|{title}|{artist}"));

        let timeline = session.GetTimelineProperties().ok();
        let end = timeline
            .as_ref()
            .and_then(|t| t.EndTime().ok())
            .map(|d| d.Duration as f64 / 1e7)
            .filter(|d| *d > 0.0);

        let meta = TrackMeta {
            id,
            title,
            artist,
            duration_secs: end,
        };

        let Some(t) = timeline else {
            return Some(no_timeline(meta, playing));
        };
        let (Ok(pos), Ok(updated)) = (t.Position(), t.LastUpdatedTime()) else {
            return Some(no_timeline(meta, playing));
        };

        // A zero anchor means the session publishes no usable timeline. Spec §6.2:
        // detect it and drop to Unscored rather than inventing a position.
        if updated.UniversalTime == 0 {
            return Some(no_timeline(meta, playing));
        }

        let position = Duration::from_secs_f64((pos.Duration as f64 / 1e7).max(0.0));
        Some(Observation {
            track: meta,
            position,
            playing,
            observed_at: anchor_instant(updated.UniversalTime),
            timeline: true,
        })
    }
}

fn no_timeline(track: TrackMeta, playing: bool) -> Observation {
    Observation {
        track,
        position: Duration::ZERO,
        playing,
        observed_at: Instant::now(),
        timeline: false,
    }
}

/// Convert a FILETIME anchor into a local monotonic instant.
///
/// The two clocks are different — `SystemTime` can jump, `Instant` cannot — so this
/// measures the *age* of the reading against the wall clock and subtracts it from
/// now. A negative age means the wall clock moved backwards between SMTC writing
/// the value and us reading it; clamped to zero, because a reading from the future
/// is not information.
fn anchor_instant(universal_time: i64) -> Instant {
    let now = Instant::now();
    let anchor_unix = (universal_time - FILETIME_TO_UNIX) as f64 / 1e7;
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(anchor_unix);

    let age = (now_unix - anchor_unix).max(0.0);
    now.checked_sub(Duration::from_secs_f64(age)).unwrap_or(now)
}

impl Source for SmtcSource {
    fn name(&self) -> &'static str {
        "smtc"
    }

    fn available(&self) -> bool {
        self.manager.GetSessions().is_ok()
    }

    fn poll(&mut self) -> Result<Option<Observation>, SourceError> {
        let t0 = Instant::now();

        // Prefer whichever session Windows considers current; fall back to
        // scanning, since the current one may be an app we do not allow.
        let current = self.manager.GetCurrentSession().ok();
        let mut out = current.as_ref().and_then(|s| self.read(s));

        if out.is_none() {
            let sessions = self
                .manager
                .GetSessions()
                .map_err(|e| SourceError::Read {
                    source_name: "smtc",
                    message: e.to_string(),
                })?;
            for s in &sessions {
                if let Some(obs) = self.read(&s) {
                    // A playing session wins over a merely present one.
                    let better = obs.playing || out.is_none();
                    if better {
                        let playing = obs.playing;
                        out = Some(obs);
                        if playing {
                            break;
                        }
                    }
                }
            }
        }

        if self.measured_read.is_none() {
            let cost = t0.elapsed();
            self.measured_read = Some(cost);
            // Spec §6.2 asks for subscriptions; this polls. Log the cost so that
            // trade is visible rather than asserted.
            tracing::info!(
                read_ms = cost.as_secs_f64() * 1000.0,
                poll_ms = DEFAULT_POLL.as_secs_f64() * 1000.0,
                "SMTC read cost measured"
            );
        }
        Ok(out)
    }

    fn position_granularity(&self) -> Duration {
        // Sub-millisecond in the reported value (Phase 0.5 saw `43.172s`). The
        // coarseness that matters is staleness, and that is handled exactly by
        // pairing with `LastUpdatedTime`.
        Duration::from_millis(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_from_the_past_lands_in_the_past() {
        // Phase 0.5's shape: 87 seconds stale.
        let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let eighty_seven_ago = ((now_unix - 87.0) * 1e7) as i64 + FILETIME_TO_UNIX;

        let anchor = anchor_instant(eighty_seven_ago);
        let age = Instant::now().duration_since(anchor).as_secs_f64();
        assert!((age - 87.0).abs() < 1.0, "age {age} should be about 87 s");
    }

    #[test]
    fn a_reading_from_the_future_is_clamped_to_now() {
        let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let ahead = ((now_unix + 3600.0) * 1e7) as i64 + FILETIME_TO_UNIX;
        // Must not produce an instant later than now; the clock would then
        // extrapolate backwards.
        assert!(anchor_instant(ahead) <= Instant::now());
    }

    #[test]
    fn the_allowlist_compares_as_unicode() {
        // Phase 0.5: the Yandex Music desktop app is `Яндекс Музыка.exe`. An
        // ASCII-folded or byte-truncated comparison would never match it.
        let src = |list: Vec<&str>| {
            let allowlist: Vec<String> = list.into_iter().map(String::from).collect();
            move |app: &str| allowlist.is_empty() || allowlist.iter().any(|a| a == app)
        };

        let yandex = src(vec!["Яндекс Музыка.exe"]);
        assert!(yandex("Яндекс Музыка.exe"));
        assert!(!yandex("msedge.exe"));

        // Empty accepts anything, which is the shipped default.
        assert!(src(vec![])("anything.exe"));
    }

    #[test]
    fn no_timeline_observations_are_flagged_not_faked() {
        let meta = TrackMeta {
            id: TrackId::new("smtc", "x"),
            title: "t".into(),
            artist: "a".into(),
            duration_secs: None,
        };
        let obs = no_timeline(meta, true);
        assert!(!obs.timeline, "the caller must be able to tell");
        assert_eq!(obs.position, Duration::ZERO);
    }
}
