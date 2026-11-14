use chrono::{Months, NaiveDate};

use crate::model::{DueOverride, OverrideSource, Record, Rule, Settings, Stage};

/// 1 -> "1st", 2 -> "2nd", 11 -> "11th", 111 -> "111th".
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

static EMPTY_STAGE: Stage = Stage { label: String::new(), interval_months: None, repeats: false, ordinal: false };

/// The stage at `idx`; indexes past the end return the last stage (which is
/// expected to repeat). An empty rule yields an empty placeholder stage.
pub fn stage_at(rule: &Rule, idx: u32) -> &Stage {
    let i = (idx as usize).min(rule.stages.len().saturating_sub(1));
    rule.stages.get(i).unwrap_or(&EMPTY_STAGE)
}

pub fn stage_label(rule: &Rule, idx: u32) -> String {
    let stage = stage_at(rule, idx);
    if stage.ordinal {
        let pos = (rule.stages.len().saturating_sub(1)) as u32;
        let k = idx.saturating_sub(pos);
        format!("{} {}", ordinal(k + 1), stage.label)
    } else {
        stage.label.clone()
    }
}

pub struct PairState<'a> {
    pub records: Vec<&'a Record>,
    pub skipped: Vec<u32>,
    pub override_: Option<&'a DueOverride>,
    pub assigned_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueSource {
    Rule,
    Excel,
    Manual,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DueInfo {
    pub next_stage_index: u32,
    pub next_stage_label: String,
    pub due_on: Option<NaiveDate>,
    pub source: DueSource,
    pub last_completed_on: Option<NaiveDate>,
}

fn add_interval(rule: &Rule, stage_idx: u32, from: NaiveDate) -> Option<NaiveDate> {
    let months = stage_at(rule, stage_idx).interval_months?;
    from.checked_add_months(Months::new(months))
}

pub fn next_due(rule: &Rule, s: &PairState, settings: &Settings) -> DueInfo {
    let latest = s.records.iter().copied().max_by_key(|r| (r.completed_on, r.stage_index));
    let mut next = latest.map_or(0, |r| r.stage_index + 1);
    while s.skipped.contains(&next) {
        next += 1;
    }
    let mut due_on = match latest {
        Some(l) => add_interval(rule, next, l.completed_on),
        None => match (s.assigned_on, settings.initial_due_days) {
            (Some(a), Some(d)) => a.checked_add_signed(chrono::Duration::days(d)),
            _ => None,
        },
    };
    let mut source = if due_on.is_some() { DueSource::Rule } else { DueSource::None };
    if let Some(o) = s.override_ {
        due_on = Some(o.due_on);
        source = match o.source {
            OverrideSource::Excel => DueSource::Excel,
            OverrideSource::Manual => DueSource::Manual,
        };
    }
    DueInfo {
        next_stage_index: next,
        next_stage_label: stage_label(rule, next),
        due_on,
        source,
        last_completed_on: latest.map(|r| r.completed_on),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DueStatus {
    Overdue,
    DueSoon,
    OnTrack,
    NotStarted,
}

pub fn classify(due: Option<NaiveDate>, today: NaiveDate, due_soon_days: i64) -> DueStatus {
    match due {
        None => DueStatus::NotStarted,
        Some(d) => {
            let days = (d - today).num_days();
            if days < 0 {
                DueStatus::Overdue
            } else if days <= due_soon_days {
                DueStatus::DueSoon
            } else {
                DueStatus::OnTrack
            }
        }
    }
}

/// Days late (+) or early (-) against the due date this completion answered:
/// the previous completion plus the interval of this record's stage.
pub fn completion_variance(rule: &Rule, s: &PairState, record_id: &str) -> Option<i64> {
    let rec = s.records.iter().copied().find(|r| r.id == record_id)?;
    let key = (rec.completed_on, rec.stage_index);
    let prev = s
        .records
        .iter()
        .copied()
        .filter(|r| (r.completed_on, r.stage_index) < key)
        .max_by_key(|r| (r.completed_on, r.stage_index))?;
    let due = add_interval(rule, rec.stage_index, prev.completed_on)?;
    Some((rec.completed_on - due).num_days())
}

pub fn variance_tag(days: i64, tolerance_days: i64) -> Option<String> {
    if days.abs() <= tolerance_days {
        return None;
    }
    let n = days.abs();
    let unit = if n == 1 { "day" } else { "days" };
    let dir = if days > 0 { "late" } else { "early" };
    Some(format!("{dir} by {n} {unit}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn rec(id: usize, stage: u32, on: NaiveDate) -> Record {
        Record {
            id: format!("rec-{id}"),
            staff_id: "st".into(),
            test_id: "t".into(),
            stage_index: stage,
            stage_label: String::new(),
            completed_on: on,
            comment: String::new(),
            by: "u".into(),
            at: chrono::FixedOffset::east_opt(0).unwrap().with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            imported: false,
            unconfirmed: false,
        }
    }

    fn recs(items: &[(u32, NaiveDate)]) -> Vec<Record> {
        items.iter().enumerate().map(|(i, (s, d))| rec(i, *s, *d)).collect()
    }

    fn pair(v: &[Record]) -> PairState<'_> {
        PairState { records: v.iter().collect(), skipped: vec![], override_: None, assigned_on: None }
    }

    #[test]
    fn labels() {
        let r = Rule::default_global();
        assert_eq!(stage_label(&r, 0), "Initial");
        assert_eq!(stage_label(&r, 1), "Semi-annual");
        assert_eq!(stage_label(&r, 2), "1st Annual");
        assert_eq!(stage_label(&r, 3), "2nd Annual");
        assert_eq!(stage_label(&r, 13), "12th Annual");
        assert_eq!(ordinal(22), "22nd");
    }

    #[test]
    fn ordinals() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 111].iter().map(|n| ordinal(*n)).collect();
        assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "111th"]);
    }

    #[test]
    fn stage_at_past_end_returns_last() {
        let r = Rule::default_global();
        assert_eq!(stage_at(&r, 0).label, "Initial");
        assert_eq!(stage_at(&r, 99).label, "Annual");
    }

    #[test]
    fn xin_yi_semi_then_annual() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2)), (1, ymd(2024, 11, 2))]);
        let i = next_due(&r, &pair(&v), &Settings::default());
        assert_eq!(i.next_stage_label, "1st Annual");
        assert_eq!(i.due_on, Some(ymd(2025, 11, 2)));
        assert_eq!(i.source, DueSource::Rule);
        assert_eq!(i.last_completed_on, Some(ymd(2024, 11, 2)));
    }

    #[test]
    fn semi_due_six_months_after_initial() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2))]);
        let i = next_due(&r, &pair(&v), &Settings::default());
        assert_eq!(i.next_stage_index, 1);
        assert_eq!(i.next_stage_label, "Semi-annual");
        assert_eq!(i.due_on, Some(ymd(2024, 11, 2)));
    }

    #[test]
    fn skipped_semi_counts_from_initial() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 7, 2))]);
        let mut p = pair(&v);
        p.skipped = vec![1];
        let i = next_due(&r, &p, &Settings::default());
        assert_eq!(i.next_stage_index, 2);
        assert_eq!(i.next_stage_label, "1st Annual");
        assert_eq!(i.due_on, Some(ymd(2025, 7, 2)));
    }

    #[test]
    fn month_end_clamps() {
        let r = Rule::default_global();
        let mut p;
        let v = recs(&[(0, ymd(2024, 8, 31))]);
        p = pair(&v);
        p.skipped = vec![1];
        assert_eq!(next_due(&r, &p, &Settings::default()).due_on, Some(ymd(2025, 8, 31)));
        // semi-annual: 2024-08-31 + 6 months = 2025-02-28
        assert_eq!(next_due(&r, &pair(&v), &Settings::default()).due_on, Some(ymd(2025, 2, 28)));
        let v2 = recs(&[(0, ymd(2023, 8, 31))]);
        assert_eq!(next_due(&r, &pair(&v2), &Settings::default()).due_on, Some(ymd(2024, 2, 29)));
    }

    #[test]
    fn late_completion_counts_from_actual_date() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2)), (1, ymd(2025, 1, 20))]);
        let i = next_due(&r, &pair(&v), &Settings::default());
        assert_eq!(i.due_on, Some(ymd(2026, 1, 20)));
    }

    #[test]
    fn no_records_initial_pending() {
        let r = Rule::default_global();
        let v: Vec<Record> = vec![];
        let i = next_due(&r, &pair(&v), &Settings::default());
        assert_eq!(i.next_stage_index, 0);
        assert_eq!(i.next_stage_label, "Initial");
        assert_eq!(i.due_on, None);
        assert_eq!(i.source, DueSource::None);
        assert_eq!(i.last_completed_on, None);
    }

    #[test]
    fn initial_due_days_setting() {
        let r = Rule::default_global();
        let v: Vec<Record> = vec![];
        let mut p = pair(&v);
        p.assigned_on = Some(ymd(2026, 1, 1));
        let s = Settings { initial_due_days: Some(30), ..Settings::default() };
        let i = next_due(&r, &p, &s);
        assert_eq!(i.due_on, Some(ymd(2026, 1, 31)));
        assert_eq!(i.source, DueSource::Rule);
        // no setting -> no due date
        assert_eq!(next_due(&r, &p, &Settings::default()).due_on, None);
    }

    #[test]
    fn override_wins() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2))]);
        let mut o = DueOverride {
            staff_id: "st".into(),
            test_id: "t".into(),
            due_on: ymd(2027, 3, 24),
            source: OverrideSource::Excel,
            by: "u".into(),
            at: v[0].at,
        };
        let mut p = pair(&v);
        p.override_ = Some(&o);
        let i = next_due(&r, &p, &Settings::default());
        assert_eq!(i.due_on, Some(ymd(2027, 3, 24)));
        assert_eq!(i.source, DueSource::Excel);
        assert_eq!(i.next_stage_index, 1);
        o.source = OverrideSource::Manual;
        let mut p = pair(&v);
        p.override_ = Some(&o);
        assert_eq!(next_due(&r, &p, &Settings::default()).source, DueSource::Manual);
    }

    #[test]
    fn latest_record_by_date_then_stage() {
        let r = Rule::default_global();
        // out-of-order insertion
        let v = recs(&[(1, ymd(2024, 11, 2)), (0, ymd(2024, 5, 2))]);
        assert_eq!(next_due(&r, &pair(&v), &Settings::default()).next_stage_index, 2);
    }

    #[test]
    fn classify_boundaries() {
        let t = ymd(2026, 11, 14);
        assert_eq!(classify(Some(ymd(2026, 11, 13)), t, 30), DueStatus::Overdue);
        assert_eq!(classify(Some(t), t, 30), DueStatus::DueSoon);
        assert_eq!(classify(Some(ymd(2026, 12, 14)), t, 30), DueStatus::DueSoon);
        assert_eq!(classify(Some(ymd(2026, 12, 15)), t, 30), DueStatus::OnTrack);
        assert_eq!(classify(None, t, 30), DueStatus::NotStarted);
        assert!(DueStatus::Overdue < DueStatus::DueSoon && DueStatus::OnTrack < DueStatus::NotStarted);
    }

    #[test]
    fn variance() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2)), (1, ymd(2024, 12, 19))]);
        let p = pair(&v);
        assert_eq!(completion_variance(&r, &p, "rec-0"), None);
        assert_eq!(completion_variance(&r, &p, "rec-1"), Some(47));
        assert_eq!(completion_variance(&r, &p, "nope"), None);
        assert_eq!(variance_tag(47, 30).as_deref(), Some("late by 47 days"));
        assert_eq!(variance_tag(20, 30), None);
        assert_eq!(variance_tag(30, 30), None);
        assert_eq!(variance_tag(-40, 30).as_deref(), Some("early by 40 days"));
        assert_eq!(variance_tag(1, 0).as_deref(), Some("late by 1 day"));
        assert_eq!(variance_tag(-1, 0).as_deref(), Some("early by 1 day"));
    }

    #[test]
    fn variance_early() {
        let r = Rule::default_global();
        let v = recs(&[(0, ymd(2024, 5, 2)), (1, ymd(2024, 9, 23))]);
        // due 2024-11-02, done 2024-09-23 -> 40 days early
        assert_eq!(completion_variance(&r, &pair(&v), "rec-1"), Some(-40));
    }
}

