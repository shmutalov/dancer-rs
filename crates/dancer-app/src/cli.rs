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
    /// The audio to dance to.
    ///
    /// With `--score`, this only has to exist — the file source decodes nothing,
    /// and the flag is for pointing at the real track when you want to play it
    /// yourself alongside. **Without** `--score` it is the track to analyse: the
    /// cache is consulted first, and the analyser runs only on a miss.
    pub audio: Option<PathBuf>,
    /// Directory holding the ONNX weights. Defaults to `models/` in the data
    /// directory. Not bundled — see `dancer_analyze::AnalyzeError::ModelsMissing`.
    pub models: Option<PathBuf>,
    /// Skip the score cache entirely: always re-analyse, never write.
    pub no_cache: bool,
    /// Start with anticipation off — M1's plain grid loop. Middle-click toggles it
    /// at runtime, which is how the M3 A/B is actually judged.
    pub no_anticipate: bool,
    /// Do not follow the system media session. Without this, and with no `--audio`
    /// or `--score`, the dancer follows whatever the user is playing.
    pub no_smtc: bool,
    /// Folders to analyse into the cache, then exit.
    ///
    /// The SMTC source can only recognise tracks the library already knows, so
    /// with an empty cache every track misses. Repeatable.
    pub scan: Vec<PathBuf>,
    /// Run the simulated transport off nominal speed, to give the clock real drift
    /// to absorb. Dev lever for exercising spec §9.1.
    pub rate: Option<f64>,
    /// Report every position this many seconds old, as SMTC does (Phase 0.5).
    pub staleness: Option<Duration>,
}

pub const USAGE: &str = "\
dancer-rs [SHEET.png] [options]

With no --audio or --score, follows whatever you are playing, via the system
media session. It can only recognise tracks already in the cache, so analyse
your music first:

  dancer-rs --scan D:/music

  --scan <DIR>          Analyse a music folder into the cache, then exit
  --audio <FILE>        Track to analyse and dance to (cached after the first run)
  --score <FILE.json>   Use this beat grid instead of analysing
  --models <DIR>        ONNX weights directory (default: <data dir>/models)
  --no-cache            Always re-analyse; do not read or write scores.db
  --no-anticipate       Start without anticipation (middle-click toggles it)
  --no-smtc             Do not follow the system media session
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
            "--models" => out.models = Some(PathBuf::from(value("--models")?)),
            "--no-cache" => out.no_cache = true,
            "--no-anticipate" => out.no_anticipate = true,
            "--no-smtc" => out.no_smtc = true,
            "--scan" => out.scan.push(PathBuf::from(value("--scan")?)),
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
    fn analysis_flags_parse() {
        let a = parse_str("--audio track.mp3 --models m --no-cache").unwrap();
        assert_eq!(a.audio, Some(PathBuf::from("track.mp3")));
        assert_eq!(a.models, Some(PathBuf::from("m")));
        assert!(a.no_cache);
        assert!(a.score.is_none(), "audio alone means analyse");
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
