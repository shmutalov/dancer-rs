//! An RFC 3339 timestamp without a date-time dependency.
//!
//! `analyzed_at` is informational — nothing branches on it — so pulling in `chrono`
//! or `time` for one string would be a poor trade. The civil-date conversion is
//! Howard Hinnant's `civil_from_days`, which is short, exact, and has no era
//! restrictions worth worrying about here.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_rfc3339() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => from_unix(d.as_secs() as i64),
        // Clock before 1970. Nothing reads this, so an empty string beats a panic.
        Err(_) => String::new(),
    }
}

pub fn from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a civil date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_format_correctly() {
        assert_eq!(from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(from_unix(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which is where naive date maths goes wrong.
        assert_eq!(from_unix(1_582_934_400), "2020-02-29T00:00:00Z");
        assert_eq!(from_unix(1_755_216_000), "2025-08-15T00:00:00Z");
    }

    #[test]
    fn now_is_plausible() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'));
        assert!(s.starts_with("20"), "{s}");
    }
}
