//! Yandex Music: resolve a streamed track, fetch it, analyse it, **delete it**
//! (spec §6.4).
//!
//! # What this is for
//!
//! Streaming has no file, so it has no grid, so the dancer can only ever sit in
//! `Unscored` (§8.3). Every other part of this project works on owned music; this
//! is the one path that closes the gap for streamed music, and it closes it only
//! for Yandex.
//!
//! # The shape of it, which is the point
//!
//! ```text
//! (title, artist)  ->  search  ->  fetch  ->  analyse  ->  DELETE  ->  grid
//! ```
//!
//! The audio is a **means, not an artifact**. It exists on disk for the seconds it
//! takes beat-this to read it and is removed before the score is written — see
//! [`TempAudio`], which deletes on drop, including on panic and on the error paths.
//! What is retained is a few kilobytes of beat timings: facts about when the
//! drums hit, not a copy of the recording.
//!
//! Three rules make that structural rather than a claim:
//!
//! 1. **The user initiates.** Nothing here runs on its own. There is no crawler, no
//!    "pre-analyse my library", no background sweep of a playlist. A track is
//!    fetched because the user is playing *that track* and asked for it to be
//!    analysed.
//! 2. **The audio never outlives the analysis.** No cache of downloaded files, no
//!    resumable partials, no configurable "keep" flag. The path is a temp file that
//!    deletes itself.
//! 3. **The worst copy that decodes is the right one.** [`pick_variant`] takes the
//!    *lowest* bitrate on offer, not the lossless one. beat-this resamples to
//!    22.05 kHz regardless, so a grid from a 64 kbps stream is identical to a grid
//!    from FLAC — the extra bytes would buy nothing except a better copy of
//!    somebody's master, which is not what this is for.
//!
//! # Cost, honestly
//!
//! It wraps an undocumented internal API and needs an OAuth token, so it will break
//! when Yandex changes something. It is feature-gated and every failure is a
//! degradation to `Unscored`, never a crash: the dancer keeps dancing.

use std::path::{Path, PathBuf};

use dancer_analyze::Analyzer;
use dancer_score::{Score, TrackId, TrackMeta};
use yamuse::client::Client;
use yamuse::models::search::Search;
use yamuse::models::track::Track;
use yamuse::{SearchQuery, SearchType};

pub mod auth;
pub mod match_track;
pub use auth::{login, DeviceLogin};
pub use match_track::{score_candidate, Candidate};

/// Namespace for scores that came from here (spec §5.1).
///
/// Deliberately not `file:` — this is a different master from a local rip, and a
/// grid off by 40 ms looks broken. Two entries for the same song is the intent.
pub const SOURCE: &str = "yandex";

#[derive(Debug, thiserror::Error)]
pub enum YandexError {
    #[error("no OAuth token configured")]
    NoToken,
    #[error("yandex api: {0}")]
    Api(String),
    #[error("no match for {title} — {artist}")]
    NoMatch { title: String, artist: String },
    #[error("no downloadable variant for track {id}")]
    NoVariant { id: String },
    #[error("fetching audio: {0}")]
    Fetch(String),
    #[error("analysing: {0}")]
    Analyze(String),
}

/// A downloaded file that deletes itself.
///
/// The deletion is in `Drop` rather than at the end of a function so it also runs
/// on the error paths and on unwind. The whole design rests on the audio not
/// outliving the analysis, and "remember to delete it" is not a design.
pub struct TempAudio {
    path: PathBuf,
}

impl TempAudio {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => tracing::debug!(path = %self.path.display(), "temporary audio deleted"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // Worth shouting about: the one invariant this module has is that
                // the audio does not survive.
                tracing::error!(
                    path = %self.path.display(),
                    error = %e,
                    "COULD NOT DELETE downloaded audio — remove it manually"
                );
            }
        }
    }
}

pub struct Yandex {
    client: Client,
    /// Where temporary audio lands. Cleared per track, never accumulated.
    scratch: PathBuf,
}

impl Yandex {
    pub fn new(token: &str, scratch: PathBuf) -> Result<Self, YandexError> {
        if token.trim().is_empty() {
            return Err(YandexError::NoToken);
        }
        let client = Client::builder()
            .token(token)
            .build()
            .map_err(|e| YandexError::Api(e.to_string()))?;
        Ok(Self { client, scratch })
    }

    /// Find the Yandex track that best matches what SMTC reported.
    ///
    /// SMTC gives strings, not identifiers, so this is a search and it can be
    /// wrong. [`match_track`] scores candidates on title, artist and duration and
    /// refuses anything weak — a confidently wrong grid is worse than none
    /// (spec §8.3), and that applies with double force here, where being wrong
    /// means having fetched somebody else's song for nothing.
    pub async fn resolve(&self, meta: &TrackMeta) -> Result<Track, YandexError> {
        let query = format!("{} {}", meta.artist, meta.title);
        let found: Search = self
            .client
            .search(
                query.trim(),
                SearchQuery::default().of_type(SearchType::Track),
            )
            .await
            .map_err(|e| YandexError::Api(e.to_string()))?;

        let candidates: Vec<Track> = found.tracks.map(|r| r.results).unwrap_or_default();
        tracing::debug!(query = %query, candidates = candidates.len(), "yandex search");

        match_track::best(&candidates, meta).ok_or_else(|| YandexError::NoMatch {
            title: meta.title.clone(),
            artist: meta.artist.clone(),
        })
    }

    /// Fetch, analyse, delete. Returns only the grid.
    pub async fn score_for(
        &self,
        meta: &TrackMeta,
        analyzer: &mut Analyzer,
    ) -> Result<Score, YandexError> {
        let track = self.resolve(meta).await?;
        let id = track
            .id
            .as_ref()
            .map(|i| i.to_string())
            .ok_or_else(|| YandexError::NoMatch {
                title: meta.title.clone(),
                artist: meta.artist.clone(),
            })?;

        let audio = self.fetch(&id).await?;

        // Analysis is CPU-bound and synchronous; keep it off the async executor.
        let track_id = TrackId::new(SOURCE, &id);
        let score = analyzer
            .analyze_file(audio.path(), &track_id)
            .map_err(|e| YandexError::Analyze(e.to_string()))?;

        // `audio` drops here, deleting the file, before the caller ever sees the
        // score. Explicit rather than implicit because the ordering is the point.
        drop(audio);

        tracing::info!(
            track = %track_id,
            beats = score.beats.len(),
            confidence = score.confidence,
            "grid built from a streamed track; audio deleted"
        );
        Ok(score)
    }

    /// Download the smallest usable variant to a self-deleting temp file.
    async fn fetch(&self, id: &str) -> Result<TempAudio, YandexError> {
        let variants = self
            .client
            .track_download_info(id)
            .await
            .map_err(|e| YandexError::Api(e.to_string()))?;

        let variant = pick_variant(&variants).ok_or_else(|| YandexError::NoVariant {
            id: id.to_string(),
        })?;
        tracing::info!(
            id,
            codec = ?variant.codec,
            kbps = ?variant.bitrate_in_kbps,
            "fetching the lowest-bitrate variant — the grid does not need more"
        );

        // Valid for about a minute, so resolve immediately before downloading.
        let url = variant
            .direct_link(self.client.transport())
            .await
            .map_err(|e| YandexError::Fetch(e.to_string()))?;

        std::fs::create_dir_all(&self.scratch).ok();
        let path = self.scratch.join(format!("yandex-{id}.tmp"));
        // Constructed *before* the download so an interrupted or failed fetch
        // still cleans up after itself.
        let audio = TempAudio::new(path.clone());

        let bytes = yamuse::download::Downloader::new(self.client.transport(), [url])
            .to_file(&path)
            .await
            .map_err(|e| YandexError::Fetch(e.to_string()))?;

        tracing::debug!(bytes, path = %path.display(), "fetched");
        Ok(audio)
    }
}

/// The **lowest**-bitrate variant, not the highest.
///
/// beat-this resamples everything to 22.05 kHz, so bitrate has no effect on the
/// grid. Taking the smallest download is faster, kinder to the CDN, and keeps this
/// path honest about what it is for: timings, not a copy of the recording.
pub fn pick_variant(variants: &[yamuse::models::track::DownloadInfo]) -> Option<&yamuse::models::track::DownloadInfo> {
    variants
        .iter()
        // Previews are clipped, so their grid would describe a different length.
        .filter(|v| v.preview != Some(true))
        .min_by_key(|v| v.bitrate_in_kbps.unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_audio_deletes_itself() {
        let path = std::env::temp_dir().join(format!("dancer-temp-{}.bin", std::process::id()));
        std::fs::write(&path, b"audio").unwrap();
        assert!(path.exists());
        {
            let _guard = TempAudio::new(path.clone());
        }
        assert!(!path.exists(), "the audio must not outlive the analysis");
    }

    #[test]
    fn temp_audio_deletes_on_unwind_too() {
        // The error paths are exactly when it would be forgotten.
        let path = std::env::temp_dir().join(format!("dancer-panic-{}.bin", std::process::id()));
        std::fs::write(&path, b"audio").unwrap();
        let p = path.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = TempAudio::new(p);
            panic!("analysis blew up");
        });
        assert!(!path.exists(), "a panic must not leave audio on disk");
    }

    #[test]
    fn deleting_something_already_gone_is_not_an_error() {
        let path = std::env::temp_dir().join(format!("dancer-gone-{}.bin", std::process::id()));
        drop(TempAudio::new(path));
    }

    #[test]
    fn an_empty_token_is_rejected_before_any_request() {
        assert!(matches!(
            Yandex::new("   ", PathBuf::from(".")),
            Err(YandexError::NoToken)
        ));
    }
}
