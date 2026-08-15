//! Phase 0.5 — does WinRT/SMTC work on the GNU toolchain?
//!
//! With MSVC ruled out (ROADMAP §0.4), SMTC is the last unproven ABI surface, and
//! it is the whole content of M4. This exercises every call the source adapter
//! needs: session enumeration, media properties, playback info, and — most
//! importantly — `LastUpdatedTime`, which spec §6.2 requires as the clock anchor.

use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Mgr;

fn main() -> windows::core::Result<()> {
    let mgr = Mgr::RequestAsync()?.join()?;
    println!("session manager: OK");

    let sessions = mgr.GetSessions()?;
    println!("sessions: {}", sessions.Size()?);

    match mgr.GetCurrentSession() {
        Ok(s) => report(&s, "current")?,
        Err(e) => println!("no current session ({})", e.code().0),
    }

    for s in &sessions {
        report(&s, "  ")?;
    }
    Ok(())
}

fn report(
    s: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
    tag: &str,
) -> windows::core::Result<()> {
    let app = s.SourceAppUserModelId()?;
    println!("\n[{tag}] {app}");

    match s.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
        Ok(p) => println!(
            "  title  : {:?}\n  artist : {:?}",
            p.Title().unwrap_or_default().to_string(),
            p.Artist().unwrap_or_default().to_string()
        ),
        Err(e) => println!("  media properties unavailable ({})", e.code().0),
    }

    match s.GetPlaybackInfo() {
        Ok(i) => println!(
            "  status : {:?}  rate {:?}",
            i.PlaybackStatus().map(|v| v.0),
            i.PlaybackRate().and_then(|r| r.Value()).unwrap_or(f64::NAN),
        ),
        Err(e) => println!("  playback info unavailable ({})", e.code().0),
    }

    // The anchor. Spec §6.2: Position only refreshes on state change, so
    // LastUpdatedTime is the timestamp that matters — never Instant::now().
    match s.GetTimelineProperties() {
        Ok(t) => {
            let pos = t.Position()?.Duration as f64 / 1e7;
            let end = t.EndTime()?.Duration as f64 / 1e7;
            let upd = t.LastUpdatedTime()?.UniversalTime;
            println!("  position: {pos:.3}s / {end:.3}s  (as reported)");

            if upd == 0 {
                println!("  anchor  : ZERO — unusable, no timeline");
            } else {
                // FILETIME (100 ns ticks since 1601) -> seconds since UNIX epoch.
                let anchor_unix = (upd - 116_444_736_000_000_000) as f64 / 1e7;
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                let staleness = now_unix - anchor_unix;
                println!("  anchor  : LastUpdatedTime is {staleness:.3}s old");
                println!(
                    "  EXTRAPOLATED position = {:.3}s   <- what the clock must use",
                    pos + staleness
                );
                if staleness > 0.5 {
                    println!(
                        "            reported position is stale by {staleness:.1}s — \
                         confirms spec §6.2: anchor on LastUpdatedTime, never Instant::now()"
                    );
                }
            }
        }
        Err(e) => println!("  NO TIMELINE ({}) — would drop to Unscored", e.code().0),
    }
    Ok(())
}
