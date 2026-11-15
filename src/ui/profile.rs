//! One person's profile: header with the status strip and next-up line,
//! then their tests as section cards or as one table.

use chrono::NaiveDate;
use egui::Color32;

use crate::model::{AssignState, Data, Id, Staff};
use crate::query::Index;
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::ui::widgets::rgb;

/// One of the person's tests, ready to draw.
pub(crate) struct ProfileRow {
    pub section_id: Id,
    pub section_name: String,
    pub color: Color32,
    pub test_id: Id,
    pub test_name: String,
    /// `None` when the test is N/A for this person.
    pub due: Option<(DueInfo, DueStatus, bool)>,
    /// The last completion; `None` for N/A and never-done tests.
    pub last_done: Option<NaiveDate>,
}

impl ProfileRow {
    pub fn status(&self) -> Option<DueStatus> {
        self.due.as_ref().map(|d| d.1)
    }
}

/// The strip's statuses, in order.
pub(crate) const STATUSES: [DueStatus; 4] = [
    DueStatus::Overdue,
    DueStatus::DueSoon,
    DueStatus::OnTrack,
    DueStatus::NotStarted,
];

/// Every live test the person has an assignment for (assigned or N/A), by
/// section order then test order.
pub(crate) fn rows(data: &Data, staff: &Staff, today: NaiveDate) -> Vec<ProfileRow> {
    let idx = Index::build(data);
    let mut out = Vec::new();
    for (pos, sec) in data.sections_sorted().into_iter().enumerate() {
        if sec.retired {
            continue;
        }
        let color = rgb(sec.color_rgb(pos));
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired).collect();
        tests.sort_by_key(|t| t.order);
        for t in tests {
            let Some(a) = staff.assignments.get(&t.id) else {
                continue;
            };
            let due = (a.state == AssignState::Assigned).then(|| {
                let info = next_due(
                    data.rule_for_test(&t.id),
                    &idx.pair(&staff.id, &t.id),
                    &data.settings,
                );
                let status = classify(info.due_on, today, data.settings.due_soon_days);
                let delayed = idx
                    .delay_notes(&staff.id, &t.id)
                    .iter()
                    .any(|n| n.for_stage_index == info.next_stage_index);
                (info, status, delayed)
            });
            out.push(ProfileRow {
                section_id: sec.id.clone(),
                section_name: sec.name.clone(),
                color,
                test_id: t.id.clone(),
                test_name: t.name.clone(),
                last_done: due.as_ref().and_then(|d| d.0.last_completed_on),
                due,
            });
        }
    }
    out
}

/// How many assigned tests have each status, in [`STATUSES`] order.
pub(crate) fn counts(rows: &[ProfileRow]) -> [usize; 4] {
    let mut out = [0; 4];
    for s in rows.iter().filter_map(ProfileRow::status) {
        if let Some(i) = STATUSES.iter().position(|x| *x == s) {
            out[i] += 1;
        }
    }
    out
}

/// The assigned test with the earliest due date.
pub(crate) fn next_up(rows: &[ProfileRow]) -> Option<&ProfileRow> {
    rows.iter()
        .filter_map(|r| Some((r, r.due.as_ref()?.0.due_on?)))
        .min_by_key(|(_, d)| *d)
        .map(|(r, _)| r)
}

/// All rows, or only the assigned ones with status `f`.
pub(crate) fn filtered(rows: &[ProfileRow], f: Option<DueStatus>) -> Vec<&ProfileRow> {
    rows.iter()
        .filter(|r| f.is_none() || r.status() == f)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::now_ts;
    use crate::model::{
        AssignState, Assignment, Data, DueOverride, OverrideSource, Record, Staff, StaffStatus,
        TestSystem,
    };

    fn today() -> NaiveDate {
        now_ts().date_naive()
    }

    fn test_sys(id: &str, name: &str, order: u32) -> TestSystem {
        TestSystem {
            id: id.into(),
            name: name.into(),
            order,
            retired: false,
            rule_id: None,
        }
    }

    /// Chemistry: B (order 1), A (order 0). Haematology: H.
    fn data() -> Data {
        let mut d = Data::new_empty("t", now_ts());
        d.settings.initial_due_days = None;
        d.sections[0].tests = vec![test_sys("t-b", "B", 1), test_sys("t-a", "A", 0)];
        d.sections[1].tests = vec![test_sys("t-h", "H", 0)];
        d
    }

    fn person(tests: &[(&str, AssignState)]) -> Staff {
        Staff {
            id: "p".into(),
            name: "P".into(),
            title: String::new(),
            joined: None,
            status: StaffStatus::Active,
            assignments: tests
                .iter()
                .map(|(t, state)| {
                    (
                        (*t).to_string(),
                        Assignment {
                            state: *state,
                            since: today() - chrono::Days::new(800),
                        },
                    )
                })
                .collect(),
        }
    }

    fn done(d: &mut Data, test: &str, days_ago: u64) {
        d.records.push(Record {
            id: format!("r-{test}"),
            staff_id: "p".into(),
            test_id: test.into(),
            stage_index: 0,
            stage_label: "Initial".into(),
            completed_on: today() - chrono::Days::new(days_ago),
            comment: String::new(),
            by: "t".into(),
            at: now_ts(),
            imported: false,
            unconfirmed: false,
        });
    }

    fn due_in(d: &mut Data, test: &str, days: u64) {
        d.due_overrides.push(DueOverride {
            staff_id: "p".into(),
            test_id: test.into(),
            due_on: today() + chrono::Days::new(days),
            source: OverrideSource::Manual,
            by: "t".into(),
            at: now_ts(),
        });
    }

    use AssignState::{Assigned, Na};

    #[test]
    fn rows_follow_section_and_test_order() {
        let d = data();
        let p = person(&[("t-h", Assigned), ("t-b", Assigned), ("t-a", Assigned)]);
        let names: Vec<_> = rows(&d, &p, today())
            .into_iter()
            .map(|r| r.test_name)
            .collect();
        assert_eq!(names, ["A", "B", "H"]);
    }

    #[test]
    fn counts_only_assigned() {
        let mut d = data();
        done(&mut d, "t-a", 400); // overdue
        let p = person(&[("t-a", Assigned), ("t-b", Assigned), ("t-h", Na)]);
        assert_eq!(counts(&rows(&d, &p, today())), [1, 0, 0, 1]);
    }

    #[test]
    fn next_up_skips_undated_and_na() {
        let mut d = data();
        due_in(&mut d, "t-h", 10);
        let p = person(&[("t-a", Assigned), ("t-b", Na), ("t-h", Assigned)]);
        let r = rows(&d, &p, today());
        assert_eq!(next_up(&r).map(|r| r.test_name.as_str()), Some("H"));
        let only_na = person(&[("t-b", Na)]);
        assert!(next_up(&rows(&d, &only_na, today())).is_none());
    }

    #[test]
    fn never_done_has_no_last_done() {
        let d = data();
        let p = person(&[("t-a", Assigned)]);
        let r = rows(&d, &p, today());
        assert_eq!(r[0].last_done, None);
        assert_eq!(r[0].due.as_ref().map(|d| d.1), Some(DueStatus::NotStarted));
    }

    #[test]
    fn only_na_counts_zero() {
        let d = data();
        let p = person(&[("t-a", Na), ("t-h", Na)]);
        let r = rows(&d, &p, today());
        assert_eq!(r.len(), 2);
        assert_eq!(counts(&r), [0, 0, 0, 0]);
    }

    #[test]
    fn filter_hides_na() {
        let mut d = data();
        done(&mut d, "t-a", 400);
        let p = person(&[("t-a", Assigned), ("t-b", Na)]);
        let r = rows(&d, &p, today());
        let over: Vec<_> = filtered(&r, Some(DueStatus::Overdue))
            .iter()
            .map(|r| r.test_name.as_str())
            .collect();
        assert_eq!(over, ["A"]);
        assert_eq!(filtered(&r, None).len(), 2);
    }
}
