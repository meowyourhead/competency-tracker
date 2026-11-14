use chrono::{Local, NaiveDate};

use crate::model::Ts;

/// Format as `d MMM yyyy` (no leading zero on the day).
pub fn fmt_date(d: NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

/// Parse `d/m/yyyy` or `dd/mm/yyyy` (4-digit year required); `None` for anything invalid.
pub fn parse_dmy(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    // chrono's %Y accepts "25" as year 25; insist on a genuine 4-digit year.
    let year = s.rsplit('/').next()?;
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) || year.starts_with('0') {
        return None;
    }
    NaiveDate::parse_from_str(s, "%d/%m/%Y").ok()
}

pub fn now_ts() -> Ts {
    Local::now().fixed_offset()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn fmt_date_has_no_leading_zero() {
        assert_eq!(fmt_date(ymd(2026, 12, 2)), "2 Dec 2026");
    }

    #[test]
    fn parse_dmy_cases() {
        assert_eq!(parse_dmy("16/9/2024"), Some(ymd(2024, 9, 16)));
        assert_eq!(parse_dmy("01/02/2025"), Some(ymd(2025, 2, 1)));
        assert_eq!(parse_dmy("31/06/2025"), None);
        assert_eq!(parse_dmy("Not Done"), None);
    }

    #[test]
    fn parse_dmy_rejects_two_digit_years() {
        assert_eq!(parse_dmy("1/2/25"), None);
        assert_eq!(parse_dmy("01/02/0025"), None);
        assert_eq!(parse_dmy("1/2/25 "), None);
    }

    #[test]
    fn now_ts_is_recent() {
        let diff = (chrono::Utc::now() - now_ts().with_timezone(&chrono::Utc)).num_seconds();
        assert!(diff.abs() < 5);
    }
}
