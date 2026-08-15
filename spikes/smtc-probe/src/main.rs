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
            println!("  position: {pos:.3}s / {end:.3}s");
            println!(
                "  anchor  : LastUpdatedTime = {upd} {}",
                if upd == 0 { "<-- ZERO: unusable, no timeline" } else { "" }
            );
        }
        Err(e) => println!("  NO TIMELINE ({}) — would drop to Unscored", e.code().0),
    }
    Ok(())
}
