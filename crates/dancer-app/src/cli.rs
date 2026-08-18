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
    /// Start with the anticipation lead removed: same choreography, but each move
    /// begins on its beat rather than early, so accents land late. Middle-click
    /// toggles it at runtime, which is how the M3 A/B is actually judged.
    pub no_anticipate: bool,
    /// Do not follow the system media session. Without this, and with no `--audio`
    /// or `--score`, the dancer follows whatever the user is playing.
    pub no_smtc: bool,
    /// Never fetch a streamed track, whatever the config says.
    pub no_fetch: bool,
    /// Write a complete config file, filling in every key with its current or
    /// default value, then exit. Existing settings are preserved.
    pub write_config: bool,
    /// Sign in to Yandex via the OAuth device flow, store the token, then exit.
    pub yandex_login: bool,
    /// Load the sheet, print how every row resolved, then exit.
    ///
    /// The manifest is the only part of a sheet that cannot be checked by looking
    /// at it, and a mistyped `motif` is a warning rather than an error (spec
    /// §4.2.1) — so without this the only way to find one is to notice the dancer
    /// behaving oddly.
    pub check_sheet: bool,
    /// Folders to analyse into the cache, then exit.
    ///
    /// The SMTC source can only recognise tracks the library already knows, so
    /// with an empty cache every track misses. Repeatable, and it may be given with
    /// no value at all — bare `--scan` uses `[library] folders` from the config.
    pub scan: Vec<PathBuf>,
    /// Whether `--scan` appeared, even with no folder after it.
    ///
    /// Separate from `scan` being non-empty because those mean different things:
    /// no flag is "run the dancer", and the flag with no folder is "scan what the
    /// config says".
    pub scan_flag: bool,
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

  --scan [DIR]          Analyse a music folder into the cache, then exit
                        (no DIR: uses [library] folders from config.toml)
  --audio <FILE>        Track to analyse and dance to (cached after the first run)
  --score <FILE.json>   Use this beat grid instead of analysing
  --models <DIR>        ONNX weights directory (default: <data dir>/models)
  --no-cache            Always re-analyse; do not read or write scores.db
  --no-anticipate       Start with the anticipation lead off (middle-click toggles)
  --no-smtc             Do not follow the system media session
  --no-fetch            Never fetch a streamed track for analysis
  --write-config        Write a complete config.toml and exit
  --yandex-login        Sign in to Yandex Music, store the token, and exit
  --check-sheet         Print how every row of the sheet resolved, then exit
  --rate <F>            Simulated transport speed, 1.0 nominal
  --stale <SECS>        Report positions this many seconds old
  -h, --help            This text
";

impl Args {
    /// Did the user ask for a scan, whether or not they named a folder?
    pub fn scan_requested(&self) -> bool {
        self.scan_flag
    }
}

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
            "--scan" => {
                out.scan_flag = true;
                // The value is optional, so it must not swallow the next flag.
                if it.peek().is_some_and(|v| !v.starts_with('-')) {
                    out.scan.push(PathBuf::from(it.next().expect("peeked")));
                }
            }
            "--no-fetch" => out.no_fetch = true,
            "--write-config" => out.write_config = true,
            "--yandex-login" => out.yandex_login = true,
            "--check-sheet" => out.check_sheet = true,
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
    fn check_sheet_takes_the_positional_sheet() {
        let a = parse_str("flchan/Dance_Large.png --check-sheet").unwrap();
        assert!(a.check_sheet);
        assert_eq!(a.sheet, Some(PathBuf::from("flchan/Dance_Large.png")));
    }

    #[test]
    fn scan_takes_a_folder_or_none_at_all() {
        let named = parse_str("--scan D:/music --scan E:/more").unwrap();
        assert!(named.scan_requested());
        assert_eq!(named.scan.len(), 2);

        // Bare `--scan` means "use the configured folders", which is a request in
        // its own right and must not read as "no scan".
        let bare = parse_str("--scan").unwrap();
        assert!(bare.scan_requested());
        assert!(bare.scan.is_empty());

        assert!(!parse_str("sheet.png").unwrap().scan_requested());
    }

    #[test]
    fn scan_does_not_swallow_the_flag_after_it() {
        // The regression an optional positional invites: `--scan --no-cache` must
        // not treat `--no-cache` as a folder name.
        let a = parse_str("--scan --no-cache").unwrap();
        assert!(a.scan_requested());
        assert!(a.scan.is_empty(), "took {:?} as a folder", a.scan);
        assert!(a.no_cache);
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
