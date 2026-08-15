//! Local file adapter (spec §6.5) — a simulated transport, no playback.
//!
//! Exists so the clock, scheduler and renderer can be exercised deterministically
//! with no streaming service in the loop. It plays nothing and decodes nothing:
//! duration is supplied by the caller, normally from the paired score, which keeps
//! an audio-decoding dependency out of M1 entirely.
//!
//! It deliberately reports *badly* by default, in the specific way real sources do.
//! [`FileSource::with_staleness`] makes every reading a fixed age, reproducing what
//! Phase 0.5 measured on SMTC. A transport that reported perfectly would exercise
//! none of the clock's correction path, and the correction path is the milestone.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dancer_score::{TrackId, TrackMeta};

use crate::{Observation, Source, SourceError};

/// A simulated player: free-running, seekable, pausable.
#[derive(Debug, Clone)]
pub struct FileSource {
    path: PathBuf,
    meta: TrackMeta,
    /// Media time at the last transport change.
    base: f64,
    /// Local instant of that change.
    since: Instant,
    /// When the transport was created. Nothing was playing before this.
    origin: Instant,
    playing: bool,
    /// Playback rate. Nudge off 1.0 to give the clock real drift to correct.
    rate: f64,
    /// How old each reading is (spec §6.2's `LastUpdatedTime` behaviour).
    staleness: Duration,
    granularity: Duration,
    looping: bool,
}

impl FileSource {
    /// `duration_secs` normally comes from the paired score — nothing here decodes
    /// audio.
    pub fn new(path: impl AsRef<Path>, duration_secs: f64, now: Instant) -> Self {
        let path = path.as_ref().to_owned();
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());

        Self {
            meta: TrackMeta {
                // Namespaced per source (spec §5.1) — the same song from a file and
                // from Spotify are different masters and must not share a key.
                id: TrackId::new("file", path.to_string_lossy()),
                title,
                // No tag reading in M1. M2 can fill this in; leaving it blank is
                // honest, and a wrong guess would poison the library key.
                artist: String::new(),
                duration_secs: Some(duration_secs),
            },
            path,
            base: 0.0,
            since: now,
            origin: now,
            playing: true,
            rate: 1.0,
            staleness: Duration::ZERO,
            granularity: Duration::from_millis(1),
            looping: true,
        }
    }

    /// A transport for a file whose length is not known yet.
    ///
    /// The analyzer learns the duration by decoding, which happens off-thread and
    /// finishes after playback has already started (ROADMAP M2). Rather than guess,
    /// the transport reports no duration and does not loop — a wrong duration would
    /// reach the library index, where it gates cache hits (spec §5.1).
    pub fn with_unknown_duration(path: impl AsRef<Path>, now: Instant) -> Self {
        let mut s = Self::new(path, f64::INFINITY, now);
        s.meta.duration_secs = None;
        s.looping = false;
        s
    }

    /// Report every reading as this old, as a stale SMTC session does.
    pub fn with_staleness(mut self, staleness: Duration) -> Self {
        self.staleness = staleness;
        self
    }

    /// Run the transport off nominal speed, to give the clock drift to absorb.
    pub fn with_rate(mut self, rate: f64) -> Self {
        self.set_rate(rate);
        self
    }

    pub fn with_looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn meta(&self) -> &TrackMeta {
        &self.meta
    }

    pub fn duration(&self) -> f64 {
        self.meta.duration_secs.unwrap_or(f64::INFINITY)
    }

    /// True media position at `now`, before staleness is applied.
    pub fn position_at(&self, now: Instant) -> f64 {
        let p = if self.playing {
            self.base + now.saturating_duration_since(self.since).as_secs_f64() * self.rate
        } else {
            self.base
        };
        let d = self.duration();
        if self.looping && d.is_finite() && d > 0.0 {
            p.rem_euclid(d)
        } else {
            p.min(d)
        }
    }

    fn rebase(&mut self, now: Instant) {
        self.base = self.position_at(now);
        self.since = now;
    }

    pub fn pause(&mut self, now: Instant) {
        if self.playing {
            self.rebase(now);
            self.playing = false;
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if !self.playing {
            self.since = now;
            self.playing = true;
        }
    }

    pub fn seek(&mut self, secs: f64, now: Instant) {
        self.base = secs;
        self.since = now;
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Poll as of an explicit instant. Tests drive this; [`Source::poll`] uses now.
    pub fn poll_at(&self, now: Instant) -> Observation {
        // The reading is as of `now - staleness`, and it is reported paired with
        // that instant — precise but old, which is exactly SMTC's behaviour.
        //
        // Clamped to `origin`, because `position_at` saturates before the transport
        // started: without this the first poll claims "position 0 was true
        // `staleness` seconds ago", the clock extrapolates playback that never
        // happened, and the second poll looks like a backwards seek. A real SMTC
        // session cannot produce that — `LastUpdatedTime` is a real timestamp — so
        // it is an artifact of the simulation and does not belong in it.
        let observed_at = now
            .checked_sub(self.staleness)
            .unwrap_or(now)
            .max(self.origin);
        let raw = self.position_at(observed_at);

        let g = self.granularity.as_secs_f64();
        let position = if g > 0.0 { (raw / g).floor() * g } else { raw };

        Observation {
            track: self.meta.clone(),
            position: Duration::from_secs_f64(position.max(0.0)),
            playing: self.playing,
            observed_at,
            // A simulated transport always knows where it is.
            timeline: true,
        }
    }
}

impl Source for FileSource {
    fn name(&self) -> &'static str {
        "file"
    }

    fn available(&self) -> bool {
        // Simulated transport, but the file should at least exist — otherwise the
        // score paired with it is describing something that is not there.
        self.path.exists()
    }

    fn poll(&mut self) -> Result<Option<Observation>, SourceError> {
        if !self.available() {
            return Err(SourceError::Unavailable(format!(
                "{} does not exist",
                self.path.display()
            )));
        }
        Ok(Some(self.poll_at(Instant::now())))
    }

    fn position_granularity(&self) -> Duration {
        self.granularity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(now: Instant) -> FileSource {
        FileSource::new("test.wav", 100.0, now).with_looping(false)
    }

    #[test]
    fn transport_advances_pauses_and_seeks() {
        let t0 = Instant::now();
        let mut s = src(t0);
        assert!((s.position_at(t0 + Duration::from_secs(10)) - 10.0).abs() < 1e-6);

        s.pause(t0 + Duration::from_secs(10));
        assert!((s.position_at(t0 + Duration::from_secs(40)) - 10.0).abs() < 1e-6);

        s.resume(t0 + Duration::from_secs(40));
        assert!((s.position_at(t0 + Duration::from_secs(45)) - 15.0).abs() < 1e-6);

        s.seek(80.0, t0 + Duration::from_secs(45));
        assert!((s.position_at(t0 + Duration::from_secs(50)) - 85.0).abs() < 1e-6);
    }

    #[test]
    fn staleness_pairs_an_old_reading_with_its_own_instant() {
        // The Phase 0.5 shape: the value is old but not wrong, because the instant
        // it belongs to travels with it.
        let t0 = Instant::now();
        let s = src(t0).with_staleness(Duration::from_secs(87));
        let now = t0 + Duration::from_secs(120);
        let obs = s.poll_at(now);

        assert!((obs.position_secs() - 33.0).abs() < 0.01, "reading should be old");
        assert!((s.position_at(obs.observed_at) - obs.position_secs()).abs() < 0.01,
            "but exact for the instant it names");
        assert_eq!(now.duration_since(obs.observed_at).as_secs(), 87);
    }

    #[test]
    fn staleness_does_not_backdate_before_playback_began() {
        // Found by running the app with --stale 2: the first poll claimed position
        // 0 had been true two seconds earlier, so the clock extrapolated two
        // seconds of playback that never happened and the next poll read as a
        // backwards seek.
        let t0 = Instant::now();
        let s = src(t0).with_staleness(Duration::from_secs(2));
        let obs = s.poll_at(t0);
        assert_eq!(obs.observed_at, t0, "cannot have been observed before it existed");
        assert!(obs.position_secs() < 1e-6);

        // Once enough time has passed, staleness applies normally again.
        let obs = s.poll_at(t0 + Duration::from_secs(10));
        assert!((obs.position_secs() - 8.0).abs() < 0.01);
    }

    #[test]
    fn rate_gives_the_clock_something_to_correct() {
        let t0 = Instant::now();
        // Well inside the 100 s duration, so the end-of-track clamp does not
        // swallow the effect being measured.
        let s = src(t0).with_rate(1.01);
        assert!((s.position_at(t0 + Duration::from_secs(50)) - 50.5).abs() < 1e-6);
    }

    #[test]
    fn looping_wraps_and_clamping_does_not() {
        let t0 = Instant::now();
        let looped = FileSource::new("t.wav", 100.0, t0).with_looping(true);
        assert!((looped.position_at(t0 + Duration::from_secs(250)) - 50.0).abs() < 1e-6);

        let clamped = src(t0);
        assert!((clamped.position_at(t0 + Duration::from_secs(250)) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn track_id_is_namespaced() {
        let s = src(Instant::now());
        assert_eq!(s.meta().id.source, "file");
        assert_eq!(s.meta().title, "test");
    }

    #[test]
    fn missing_file_is_an_error_not_a_silent_none() {
        let mut s = FileSource::new("definitely-not-here.wav", 10.0, Instant::now());
        assert!(matches!(s.poll(), Err(SourceError::Unavailable(_))));
    }
}
