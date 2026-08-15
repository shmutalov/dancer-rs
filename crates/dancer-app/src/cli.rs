//! Command line. Deliberately hand-rolled — the surface is four flags and a
//! positional, and M5's tray is where configuration actually belongs.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Default)]
pub struct Args {
    /// Sheet PNG. Overrides the configured sheet; resolved against the working
    /// directory, not the data directory — anything else would surprise whoever
    /// typed it.
    pub sheet: Option<PathBuf>,
    /// Hand-written score JSON (M1). Its presence is what enables clock mode.
    pub score: Option<PathBuf>,
    /// The audio the score describes. Optional: the file source decodes nothing,
    /// so this only has to exist. Pass the real track when you want to play it
    /// yourself alongside and watch the dancer against it.
    pub audio: Option<PathBuf>,
    /// Run the simulated transport off nominal speed, to give the clock real drift
    /// to absorb. Dev lever for exercising spec §9.1.
    pub rate: Option<f64>,
    /// Report every position this many seconds old, as SMTC does (Phase 0.5).
    pub staleness: Option<Duration>,
}

pub const USAGE: &str = "\
dancer-rs [SHEET.png] [options]

  --score <FILE.json>   Beat grid to dance to (M1: hand-written)
  --audio <FILE>        Track the score describes; defaults to the score's path
  --rate <F>            Simulated transport speed, 1.0 nominal
  --stale <SECS>        Report positions this many seconds old
  -h, --help            This text
";

pub fn parse<I: Iterator<Item = String>>(args: I) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.peekable();

    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.into()),
            "--score" => out.score = Some(PathBuf::from(value("--score")?)),
            "--audio" => out.audio = Some(PathBuf::from(value("--audio")?)),
            "--rate" => {
                let v = value("--rate")?;
                out.rate = Some(v.parse().map_err(|_| format!("--rate: {v} is not a number"))?);
            }
            "--stale" => {
                let v = value("--stale")?;
                let secs: f64 = v.parse().map_err(|_| format!("--stale: {v} is not a number"))?;
                if secs < 0.0 {
                    return Err("--stale cannot be negative".into());
                }
                out.staleness = Some(Duration::from_secs_f64(secs));
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other if out.sheet.is_none() => out.sheet = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn positional_sheet_still_works() {
        // M0's only invocation form must not have been broken.
        let a = parse_str("sheet.png").unwrap();
        assert_eq!(a.sheet, Some(PathBuf::from("sheet.png")));
        assert!(a.score.is_none());
    }

    #[test]
    fn flags_and_positional_mix() {
        let a = parse_str("sheet.png --score s.json --rate 1.01 --stale 2.5").unwrap();
        assert_eq!(a.score, Some(PathBuf::from("s.json")));
        assert_eq!(a.rate, Some(1.01));
        assert_eq!(a.staleness, Some(Duration::from_millis(2500)));
    }

    #[test]
    fn bad_input_is_rejected_not_guessed() {
        assert!(parse_str("--score").is_err(), "missing value");
        assert!(parse_str("--rate abc").is_err(), "non-numeric");
        assert!(parse_str("--stale -1").is_err(), "negative staleness");
        assert!(parse_str("--nope").is_err(), "unknown flag");
        assert!(parse_str("a.png b.png").is_err(), "two positionals");
    }
}
