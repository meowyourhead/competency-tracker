use std::collections::HashMap;

use chrono::NaiveDate;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::model::*;
use crate::schedule::{classify, next_due, DueInfo, DueStatus, PairState};

type PairKey = (Id, Id);

fn key(staff_id: &str, test_id: &str) -> PairKey {
    (staff_id.to_string(), test_id.to_string())
}

/// Per-(staff, test) grouping of everything the schedule needs, built in one pass.
pub struct Index<'a> {
    records: HashMap<PairKey, Vec<&'a Record>>,
    skipped: HashMap<PairKey, Vec<u32>>,
    overrides: HashMap<PairKey, &'a DueOverride>,
    delay_notes: HashMap<PairKey, Vec<&'a DelayNote>>,
    notes: HashMap<PairKey, Vec<&'a Note>>,
    data: &'a Data,
}

impl<'a> Index<'a> {
    pub fn build(data: &'a Data) -> Index<'a> {
        let mut records: HashMap<PairKey, Vec<&'a Record>> = HashMap::new();
        for r in &data.records {
            records
                .entry(key(&r.staff_id, &r.test_id))
                .or_default()
                .push(r);
        }
        let mut skipped: HashMap<PairKey, Vec<u32>> = HashMap::new();
        for s in &data.skipped {
            skipped
                .entry(key(&s.staff_id, &s.test_id))
                .or_default()
                .push(s.stage_index);
        }
        let mut overrides: HashMap<PairKey, &'a DueOverride> = HashMap::new();
        for o in &data.due_overrides {
            let slot = overrides.entry(key(&o.staff_id, &o.test_id)).or_insert(o);
            if o.at >= slot.at {
                *slot = o;
            }
        }
        let mut delay_notes: HashMap<PairKey, Vec<&'a DelayNote>> = HashMap::new();
        for n in &data.delay_notes {
            delay_notes
                .entry(key(&n.staff_id, &n.test_id))
                .or_default()
                .push(n);
        }
        let mut notes: HashMap<PairKey, Vec<&'a Note>> = HashMap::new();
        for n in &data.notes {
            if let Some(t) = &n.test_id {
                notes.entry(key(&n.staff_id, t)).or_default().push(n);
            }
        }
        Index {
            records,
            skipped,
            overrides,
            delay_notes,
            notes,
            data,
        }
    }

    pub fn pair(&self, staff_id: &str, test_id: &str) -> PairState<'a> {
        let k = key(staff_id, test_id);
        PairState {
            records: self.records.get(&k).cloned().unwrap_or_default(),
            skipped: self.skipped.get(&k).cloned().unwrap_or_default(),
            override_: self.overrides.get(&k).copied(),
            assigned_on: self
                .data
                .staff(staff_id)
                .and_then(|s| s.assignments.get(test_id))
                .map(|a| a.since),
        }
    }

    /// Delay notes for the pair, in data order.
    pub fn delay_notes(&self, staff_id: &str, test_id: &str) -> &[&'a DelayNote] {
        self.delay_notes
            .get(&key(staff_id, test_id))
            .map_or(&[], |v| v.as_slice())
    }

    /// Imported notes attached to the pair, in data order.
    pub fn notes(&self, staff_id: &str, test_id: &str) -> &[&'a Note] {
        self.notes
            .get(&key(staff_id, test_id))
            .map_or(&[], |v| v.as_slice())
    }

    pub fn data(&self) -> &'a Data {
        self.data
    }
}

#[derive(Debug, Clone)]
pub struct DueItem {
    pub staff_id: Id,
    pub staff_name: String,
    pub title: String,
    pub section_id: Id,
    pub section_name: String,
    pub test_id: Id,
    pub test_name: String,
    pub info: DueInfo,
    pub status: DueStatus,
    pub delay_note: Option<DelayNote>,
}

pub fn due_items(data: &Data, idx: &Index, today: NaiveDate) -> Vec<DueItem> {
    let mut out = Vec::new();
    for st in data
        .staff
        .iter()
        .filter(|s| s.status == StaffStatus::Active)
    {
        for sec in data.sections.iter().filter(|s| !s.retired) {
            for t in sec.tests.iter().filter(|t| !t.retired) {
                let assigned = st
                    .assignments
                    .get(&t.id)
                    .is_some_and(|a| a.state == AssignState::Assigned);
                if !assigned {
                    continue;
                }
                let info = next_due(
                    data.rule_for_test(&t.id),
                    &idx.pair(&st.id, &t.id),
                    &data.settings,
                );
                let status = classify(info.due_on, today, data.settings.due_soon_days);
                let delay_note = idx
                    .delay_notes(&st.id, &t.id)
                    .iter()
                    .filter(|n| n.for_stage_index == info.next_stage_index)
                    .max_by_key(|n| n.at)
                    .map(|n| (*n).clone());
                out.push(DueItem {
                    staff_id: st.id.clone(),
                    staff_name: st.name.clone(),
                    title: st.title.clone(),
                    section_id: sec.id.clone(),
                    section_name: sec.name.clone(),
                    test_id: t.id.clone(),
                    test_name: t.name.clone(),
                    info,
                    status,
                    delay_note,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.status
            .cmp(&b.status)
            .then_with(|| a.info.due_on.cmp(&b.info.due_on))
            .then_with(|| {
                a.staff_name
                    .to_lowercase()
                    .cmp(&b.staff_name.to_lowercase())
            })
    });
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub overdue: usize,
    pub due_soon: usize,
    pub on_track: usize,
    pub delayed: usize,
}

pub fn counts<'a>(items: impl IntoIterator<Item = &'a DueItem>) -> Counts {
    let mut c = Counts::default();
    for i in items {
        match i.status {
            DueStatus::Overdue => c.overdue += 1,
            DueStatus::DueSoon => c.due_soon += 1,
            DueStatus::OnTrack => c.on_track += 1,
            DueStatus::NotStarted => {}
        }
        if is_delayed(i) {
            c.delayed += 1;
        }
    }
    c
}

fn is_delayed(i: &DueItem) -> bool {
    i.delay_note.is_some() && matches!(i.status, DueStatus::Overdue | DueStatus::DueSoon)
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.to_lowercase().contains(needle_lower)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    Overdue,
    DueSoon,
    Delayed,
    OnTrack,
    NotStarted,
}

#[derive(Debug, Clone, Default)]
pub struct DueFilter {
    pub section: Option<Id>,
    pub test: Option<Id>,
    pub title: Option<String>,
    pub status: StatusFilter,
    pub text: String,
}

pub fn filter_items<'a>(items: &'a [DueItem], f: &DueFilter) -> Vec<&'a DueItem> {
    let text = f.text.trim().to_lowercase();
    items
        .iter()
        .filter(|i| f.section.as_ref().is_none_or(|s| *s == i.section_id))
        .filter(|i| f.test.as_ref().is_none_or(|t| *t == i.test_id))
        .filter(|i| {
            f.title
                .as_ref()
                .is_none_or(|t| t.eq_ignore_ascii_case(&i.title))
        })
        .filter(|i| match f.status {
            StatusFilter::All => true,
            StatusFilter::Overdue => i.status == DueStatus::Overdue,
            StatusFilter::DueSoon => i.status == DueStatus::DueSoon,
            StatusFilter::Delayed => is_delayed(i),
            StatusFilter::OnTrack => i.status == DueStatus::OnTrack,
            StatusFilter::NotStarted => i.status == DueStatus::NotStarted,
        })
        .filter(|i| {
            text.is_empty()
                || contains_ci(&i.staff_name, &text)
                || contains_ci(&i.test_name, &text)
                || contains_ci(&i.section_name, &text)
        })
        .collect()
}

fn group_by<'a>(
    items: &[&'a DueItem],
    id: impl Fn(&DueItem) -> &Id,
) -> Vec<(Id, Vec<&'a DueItem>)> {
    let mut groups: Vec<(Id, Vec<&'a DueItem>)> = Vec::new();
    let mut pos: HashMap<Id, usize> = HashMap::new();
    for &i in items {
        let g = *pos.entry(id(i).clone()).or_insert_with(|| {
            groups.push((id(i).clone(), Vec::new()));
            groups.len() - 1
        });
        groups[g].1.push(i);
    }
    groups
}

pub fn group_by_person<'a>(items: &[&'a DueItem]) -> Vec<(Id, Vec<&'a DueItem>)> {
    group_by(items, |i| &i.staff_id)
}

pub fn group_by_test<'a>(items: &[&'a DueItem]) -> Vec<(Id, Vec<&'a DueItem>)> {
    group_by(items, |i| &i.test_id)
}

#[derive(Debug, Clone, Copy)]
pub enum HistoryEntry<'a> {
    Completion(&'a Record),
    Delay(&'a DelayNote),
    Note(&'a Note),
}

impl HistoryEntry<'_> {
    pub fn date(&self) -> NaiveDate {
        match self {
            HistoryEntry::Completion(r) => r.completed_on,
            HistoryEntry::Delay(n) => n.at.date_naive(),
            HistoryEntry::Note(n) => n.at.date_naive(),
        }
    }

    fn kind_rank(&self) -> u8 {
        match self {
            HistoryEntry::Completion(_) => 0,
            HistoryEntry::Delay(_) => 1,
            HistoryEntry::Note(_) => 2,
        }
    }

    fn at(&self) -> Ts {
        match self {
            HistoryEntry::Completion(r) => r.at,
            HistoryEntry::Delay(n) => n.at,
            HistoryEntry::Note(n) => n.at,
        }
    }
}

pub const HISTORY_DEFAULT: usize = 4;

pub fn history<'a>(data: &'a Data, staff_id: &str, test_id: &str) -> Vec<HistoryEntry<'a>> {
    let pair = |s: &str, t: &str| s == staff_id && t == test_id;
    let mut out: Vec<HistoryEntry<'a>> = Vec::new();
    out.extend(
        data.records
            .iter()
            .filter(|r| pair(&r.staff_id, &r.test_id))
            .map(HistoryEntry::Completion),
    );
    out.extend(
        data.delay_notes
            .iter()
            .filter(|n| pair(&n.staff_id, &n.test_id))
            .map(HistoryEntry::Delay),
    );
    out.extend(
        data.notes
            .iter()
            .filter(|n| n.staff_id == staff_id && n.test_id.as_deref() == Some(test_id))
            .map(HistoryEntry::Note),
    );
    out.sort_by(|a, b| {
        b.date()
            .cmp(&a.date())
            .then_with(|| a.kind_rank().cmp(&b.kind_rank()))
            .then_with(|| b.at().cmp(&a.at()))
    });
    out
}

/// The visible slice and the number of hidden older entries.
pub fn split_history<'a, 'b>(
    h: &'b [HistoryEntry<'a>],
    expanded: bool,
) -> (&'b [HistoryEntry<'a>], usize) {
    if expanded || h.len() <= HISTORY_DEFAULT {
        (h, 0)
    } else {
        (&h[..HISTORY_DEFAULT], h.len() - HISTORY_DEFAULT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchHit {
    Staff(Id),
    Test { section_id: Id, test_id: Id },
    Section(Id),
}

pub fn search(data: &Data, q: &str, limit: usize) -> Vec<(SearchHit, String)> {
    let tokens: Vec<&str> = q.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    // Every whitespace-separated token must match; scores add up.
    let score = |label: &str| -> Option<i64> {
        tokens.iter().try_fold(0i64, |acc, t| {
            matcher.fuzzy_match(label, t).map(|s| acc + s)
        })
    };
    let mut hits: Vec<(i64, SearchHit, String)> = Vec::new();
    for s in data
        .staff
        .iter()
        .filter(|s| s.status == StaffStatus::Active)
    {
        if let Some(sc) = score(&s.name) {
            hits.push((sc, SearchHit::Staff(s.id.clone()), s.name.clone()));
        }
    }
    for sec in data.sections_sorted().into_iter().filter(|s| !s.retired) {
        if let Some(sc) = score(&sec.name) {
            hits.push((sc, SearchHit::Section(sec.id.clone()), sec.name.clone()));
        }
        for t in sec.tests.iter().filter(|t| !t.retired) {
            let label = format!("{} \u{b7} {}", t.name, sec.name);
            if let Some(sc) = score(&label) {
                hits.push((
                    sc,
                    SearchHit::Test {
                        section_id: sec.id.clone(),
                        test_id: t.id.clone(),
                    },
                    label,
                ));
            }
        }
    }
    hits.sort_by_key(|a| std::cmp::Reverse(a.0));
    hits.into_iter()
        .take(limit)
        .map(|(_, h, l)| (h, l))
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct StaffFilter {
    pub status: Option<StaffStatus>,
    pub section: Option<Id>,
    pub title: Option<String>,
    pub text: String,
}

pub fn filter_staff<'a>(data: &'a Data, f: &StaffFilter) -> Vec<&'a Staff> {
    let status = f.status.unwrap_or(StaffStatus::Active);
    let text = f.text.trim().to_lowercase();
    let mut out: Vec<&Staff> = data
        .staff
        .iter()
        // Hidden is no longer offered; staff hidden earlier are listed with
        // the removed ones so they can still be restored.
        .filter(|s| {
            s.status == status
                || (status == StaffStatus::Removed && s.status == StaffStatus::Hidden)
        })
        .filter(|s| {
            f.title
                .as_ref()
                .is_none_or(|t| t.eq_ignore_ascii_case(&s.title))
        })
        .filter(|s| {
            f.section.as_ref().is_none_or(|sec| {
                data.section(sec).is_some_and(|sec| {
                    sec.tests.iter().any(|t| {
                        s.assignments
                            .get(&t.id)
                            .is_some_and(|a| a.state == AssignState::Assigned)
                    })
                })
            })
        })
        .filter(|s| text.is_empty() || contains_ci(&s.name, &text))
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    out
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub user: Option<String>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub section: Option<Id>,
    pub staff: Option<Id>,
    pub action_prefix: Option<String>,
    pub text: String,
}

pub fn filter_audit<'a>(data: &'a Data, f: &AuditFilter) -> Vec<&'a AuditEntry> {
    let text = f.text.trim().to_lowercase();
    let mut out: Vec<&AuditEntry> = data
        .audit
        .iter()
        .filter(|e| f.user.as_ref().is_none_or(|u| *u == e.by))
        .filter(|e| f.from.is_none_or(|d| e.at.date_naive() >= d))
        .filter(|e| f.to.is_none_or(|d| e.at.date_naive() <= d))
        .filter(|e| {
            f.section
                .as_ref()
                .is_none_or(|s| e.refs.get("section_id") == Some(s))
        })
        .filter(|e| {
            f.staff
                .as_ref()
                .is_none_or(|s| e.refs.get("staff_id") == Some(s))
        })
        .filter(|e| {
            f.action_prefix
                .as_ref()
                .is_none_or(|p| e.action.starts_with(p.as_str()))
        })
        .filter(|e| text.is_empty() || contains_ci(&e.detail, &text))
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.at));
    out
}

fn distinct_sorted(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = values.filter(|s| !s.is_empty()).collect();
    v.sort_by_key(|s| s.to_lowercase());
    v.dedup();
    v
}

pub fn distinct_titles(data: &Data) -> Vec<String> {
    distinct_sorted(data.staff.iter().map(|s| s.title.clone()))
}

pub fn distinct_audit_users(data: &Data) -> Vec<String> {
    distinct_sorted(data.audit.iter().map(|e| e.by.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn ts(y: i32, m: u32, d: u32, h: u32) -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
    }

    const TODAY: (i32, u32, u32) = (2026, 11, 14);

    fn today() -> NaiveDate {
        ymd(TODAY.0, TODAY.1, TODAY.2)
    }

    fn staff(id: &str, name: &str, title: &str, tests: &[&str]) -> Staff {
        Staff {
            id: id.into(),
            name: name.into(),
            title: title.into(),
            joined: None,
            status: StaffStatus::Active,
            assignments: tests
                .iter()
                .map(|t| {
                    (
                        (*t).to_string(),
                        Assignment {
                            state: AssignState::Assigned,
                            since: ymd(2025, 1, 1),
                        },
                    )
                })
                .collect(),
        }
    }

    fn rec(id: &str, staff_id: &str, test_id: &str, stage: u32, on: NaiveDate) -> Record {
        Record {
            id: id.into(),
            staff_id: staff_id.into(),
            test_id: test_id.into(),
            stage_index: stage,
            stage_label: String::new(),
            completed_on: on,
            comment: String::new(),
            by: "jtan".into(),
            at: ts(2026, 1, 1, 9),
            imported: false,
            unconfirmed: false,
        }
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

    fn delay(id: &str, staff_id: &str, test_id: &str, stage: u32, at: Ts) -> DelayNote {
        DelayNote {
            id: id.into(),
            staff_id: staff_id.into(),
            test_id: test_id.into(),
            for_stage_index: stage,
            note: "waiting".into(),
            expected_on: None,
            by: "jtan".into(),
            at,
        }
    }

    /// Chemistry: t-ca (Cobas), t-cb (Architect). Haematology: t-h (Cell-DYN Ruby).
    fn base() -> Data {
        let mut d = Data::new_empty("jtan", ts(2026, 11, 14, 9));
        d.sections[0].id = "s-chem".into();
        d.sections[0].tests = vec![
            test_sys("t-ca", "Cobas", 0),
            test_sys("t-cb", "Architect", 1),
        ];
        d.sections[1].id = "s-haem".into();
        d.sections[1].tests = vec![test_sys("t-h", "Cell-DYN Ruby", 0)];
        d
    }

    fn items_for(d: &Data) -> Vec<DueItem> {
        let idx = Index::build(d);
        due_items(d, &idx, today())
    }

    #[test]
    fn excludes_hidden_removed_retired_and_na() {
        let mut d = base();
        d.staff
            .push(staff("p-1", "Alice", "MLT", &["t-ca", "t-cb", "t-h"]));
        let mut hidden = staff("p-2", "Bob", "MLT", &["t-ca"]);
        hidden.status = StaffStatus::Hidden;
        let mut removed = staff("p-3", "Cara", "MLT", &["t-ca"]);
        removed.status = StaffStatus::Removed;
        d.staff.push(hidden);
        d.staff.push(removed);
        d.staff[0].assignments.get_mut("t-cb").unwrap().state = AssignState::Na;
        d.sections[1].tests[0].retired = true;
        let items = items_for(&d);
        assert_eq!(items.len(), 1);
        assert_eq!(
            (items[0].staff_id.as_str(), items[0].test_id.as_str()),
            ("p-1", "t-ca")
        );
        assert_eq!(items[0].section_name, "Chemistry");
        assert_eq!(items[0].test_name, "Cobas");
        assert_eq!(items[0].title, "MLT");

        d.sections[0].retired = true;
        assert!(items_for(&d).is_empty());
    }

    /// Alice/Cobas overdue, Bob/Cobas due soon, Cara/Cobas on track, Dan/Cobas not started,
    /// Abe/Architect overdue earlier than Alice.
    fn urgency_data() -> Data {
        let mut d = base();
        for (id, name) in [
            ("p-a", "Alice"),
            ("p-b", "Bob"),
            ("p-c", "Cara"),
            ("p-d", "Dan"),
            ("p-e", "Abe"),
        ] {
            d.staff.push(staff(id, name, "MLT", &["t-ca", "t-cb"]));
        }
        // Semi-annual due = completed + 6 months after Initial.
        d.records.push(rec("r1", "p-a", "t-ca", 0, ymd(2026, 4, 1))); // due 2026-10-01 overdue
        d.records
            .push(rec("r2", "p-b", "t-ca", 0, ymd(2026, 5, 20))); // due 2026-11-20 soon
        d.records
            .push(rec("r3", "p-c", "t-ca", 0, ymd(2026, 10, 1))); // due 2027-04-01 on track
        d.records.push(rec("r4", "p-e", "t-cb", 0, ymd(2026, 3, 1))); // due 2026-09-01 overdue
        d
    }

    #[test]
    fn sorted_by_urgency() {
        let d = urgency_data();
        let items = items_for(&d);
        let order: Vec<(&str, &str)> = items
            .iter()
            .map(|i| (i.staff_name.as_str(), i.test_name.as_str()))
            .collect();
        // Overdue by due date (Abe/Architect 09-01, Alice/Cobas 10-01), then DueSoon, OnTrack,
        // then NotStarted (no due) ordered by staff name.
        assert_eq!(items[0].status, DueStatus::Overdue);
        assert_eq!(order[0], ("Abe", "Architect"));
        assert_eq!(order[1], ("Alice", "Cobas"));
        assert_eq!(order[2], ("Bob", "Cobas"));
        assert_eq!(items[2].status, DueStatus::DueSoon);
        assert_eq!(order[3], ("Cara", "Cobas"));
        assert_eq!(items[3].status, DueStatus::OnTrack);
        let rest = &items[4..];
        assert_eq!(rest.len(), 6);
        assert!(rest.iter().all(|i| i.status == DueStatus::NotStarted));
        let names: Vec<&str> = rest.iter().map(|i| i.staff_name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn delay_note_is_latest_for_next_stage_only() {
        let mut d = urgency_data();
        d.delay_notes
            .push(delay("n1", "p-a", "t-ca", 1, ts(2026, 10, 5, 9)));
        d.delay_notes
            .push(delay("n2", "p-a", "t-ca", 1, ts(2026, 10, 9, 9)));
        d.delay_notes
            .push(delay("n3", "p-a", "t-ca", 0, ts(2026, 10, 12, 9)));
        let items = items_for(&d);
        let alice = items
            .iter()
            .find(|i| i.staff_id == "p-a" && i.test_id == "t-ca")
            .unwrap();
        assert_eq!(alice.delay_note.as_ref().unwrap().id, "n2");
        let bob = items
            .iter()
            .find(|i| i.staff_id == "p-b" && i.test_id == "t-ca")
            .unwrap();
        assert!(bob.delay_note.is_none());
    }

    #[test]
    fn latest_override_wins_and_assigned_on_used() {
        let mut d = base();
        d.settings.initial_due_days = Some(30);
        d.staff.push(staff("p-1", "Alice", "MLT", &["t-ca"]));
        let mk = |due: NaiveDate, at: Ts| DueOverride {
            staff_id: "p-1".into(),
            test_id: "t-ca".into(),
            due_on: due,
            source: OverrideSource::Manual,
            by: "jtan".into(),
            at,
        };
        d.due_overrides.push(mk(ymd(2027, 1, 1), ts(2026, 5, 1, 9)));
        d.due_overrides.push(mk(ymd(2027, 2, 2), ts(2026, 6, 1, 9)));
        d.due_overrides.push(mk(ymd(2027, 3, 3), ts(2026, 3, 1, 9)));
        let idx = Index::build(&d);
        let p = idx.pair("p-1", "t-ca");
        assert_eq!(p.override_.unwrap().due_on, ymd(2027, 2, 2));
        assert_eq!(p.assigned_on, Some(ymd(2025, 1, 1)));
        assert!(idx.pair("p-1", "t-cb").assigned_on.is_none());
        d.due_overrides.clear();
        let items = items_for(&d);
        assert_eq!(items[0].info.due_on, Some(ymd(2025, 1, 31)));
    }

    #[test]
    fn counts_and_delayed() {
        let mut d = urgency_data();
        d.delay_notes
            .push(delay("n1", "p-a", "t-ca", 1, ts(2026, 10, 5, 9))); // overdue + note
        d.delay_notes
            .push(delay("n2", "p-b", "t-ca", 1, ts(2026, 10, 5, 9))); // soon + note
        d.delay_notes
            .push(delay("n3", "p-c", "t-ca", 1, ts(2026, 10, 5, 9))); // on track: not counted
        let items = items_for(&d);
        let c = counts(&items);
        assert_eq!(
            c,
            Counts {
                overdue: 2,
                due_soon: 1,
                on_track: 1,
                delayed: 2
            }
        );
    }

    #[test]
    fn filter_by_section_status_text() {
        let mut d = urgency_data();
        d.staff[0].assignments.insert(
            "t-h".into(),
            Assignment {
                state: AssignState::Assigned,
                since: ymd(2025, 1, 1),
            },
        );
        d.staff[0].title = "Senior MLT".into();
        d.delay_notes
            .push(delay("n1", "p-a", "t-ca", 1, ts(2026, 10, 5, 9)));
        let items = items_for(&d);

        let all = filter_items(&items, &DueFilter::default());
        assert_eq!(all.len(), items.len());

        let f = DueFilter {
            section: Some("s-haem".into()),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].test_id, "t-h");

        let f = DueFilter {
            test: Some("t-cb".into()),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 5);
        assert!(r.iter().all(|i| i.test_id == "t-cb"));

        let f = DueFilter {
            status: StatusFilter::Overdue,
            ..Default::default()
        };
        assert_eq!(filter_items(&items, &f).len(), 2);

        let f = DueFilter {
            status: StatusFilter::Delayed,
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].staff_id, "p-a");

        let f = DueFilter {
            status: StatusFilter::NotStarted,
            section: Some("s-haem".into()),
            ..Default::default()
        };
        assert_eq!(filter_items(&items, &f).len(), 1);

        let f = DueFilter {
            title: Some("senior mlt".into()),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|i| i.staff_id == "p-a"));

        let f = DueFilter {
            text: "ARCH".into(),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 5);
        assert!(r.iter().all(|i| i.test_name == "Architect"));
        let f = DueFilter {
            text: "chem".into(),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 10);
        assert!(r.iter().all(|i| i.section_name == "Chemistry"));
        let f = DueFilter {
            text: "bob".into(),
            ..Default::default()
        };
        let r = filter_items(&items, &f);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|i| i.staff_name == "Bob"));
        let f = DueFilter {
            text: "zzz".into(),
            ..Default::default()
        };
        assert!(filter_items(&items, &f).is_empty());
    }

    #[test]
    fn grouping_preserves_order() {
        let d = urgency_data();
        let items = items_for(&d);
        let refs: Vec<&DueItem> = items.iter().collect();
        let by_p = group_by_person(&refs);
        let ids: Vec<&str> = by_p.iter().map(|(id, _)| id.as_str()).collect();
        // first-seen order: Abe (first overall), Alice, Bob, Cara, then Dan
        assert_eq!(ids[0], "p-e");
        assert_eq!(by_p.iter().map(|(_, v)| v.len()).sum::<usize>(), refs.len());
        let alice = &by_p.iter().find(|(id, _)| id == "p-a").unwrap().1;
        assert_eq!(alice.len(), 2);
        let pos = |i: &DueItem| refs.iter().position(|r| std::ptr::eq(*r, i)).unwrap();
        assert!(pos(alice[0]) < pos(alice[1]));
        let by_t = group_by_test(&refs);
        assert_eq!(by_t.len(), 2);
        assert_eq!(by_t[0].0, items[0].test_id);
        assert!(by_t.iter().all(|(t, v)| v.iter().all(|i| &i.test_id == t)));
    }

    #[test]
    fn history_newest_first_and_split() {
        let mut d = base();
        d.staff.push(staff("p-1", "Alice", "MLT", &["t-ca"]));
        for i in 0..6u32 {
            d.records.push(rec(
                &format!("r{i}"),
                "p-1",
                "t-ca",
                i,
                ymd(2024, 1 + i, 10),
            ));
        }
        d.records
            .push(rec("other", "p-1", "t-cb", 0, ymd(2026, 1, 1)));
        let h = history(&d, "p-1", "t-ca");
        assert_eq!(h.len(), 6);
        assert!(h.windows(2).all(|w| w[0].date() >= w[1].date()));
        match h[0] {
            HistoryEntry::Completion(r) => assert_eq!(r.id, "r5"),
            _ => panic!("expected completion"),
        }
        let (shown, hidden) = split_history(&h, false);
        assert_eq!((shown.len(), hidden), (4, 2));
        let (shown, hidden) = split_history(&h, true);
        assert_eq!((shown.len(), hidden), (6, 0));
        let (shown, hidden) = split_history(&h[..3], false);
        assert_eq!((shown.len(), hidden), (3, 0));
    }

    #[test]
    fn history_mixes_entries_and_breaks_ties() {
        let mut d = base();
        d.staff.push(staff("p-1", "Alice", "MLT", &["t-ca"]));
        d.records.push(rec("r1", "p-1", "t-ca", 0, ymd(2026, 5, 1)));
        d.delay_notes
            .push(delay("d1", "p-1", "t-ca", 1, ts(2026, 5, 1, 10)));
        d.notes.push(Note {
            id: "n1".into(),
            staff_id: "p-1".into(),
            test_id: Some("t-ca".into()),
            section_id: None,
            text: "x".into(),
            by: "jtan".into(),
            at: ts(2026, 5, 1, 11),
        });
        d.notes.push(Note {
            id: "n2".into(),
            staff_id: "p-1".into(),
            test_id: Some("t-cb".into()),
            section_id: None,
            text: "elsewhere".into(),
            by: "jtan".into(),
            at: ts(2026, 5, 1, 11),
        });
        d.delay_notes
            .push(delay("d2", "p-1", "t-ca", 1, ts(2026, 6, 1, 10)));
        let h = history(&d, "p-1", "t-ca");
        assert_eq!(h.len(), 4);
        assert!(matches!(h[0], HistoryEntry::Delay(n) if n.id == "d2"));
        assert!(matches!(h[1], HistoryEntry::Completion(_)));
        assert!(matches!(h[2], HistoryEntry::Delay(n) if n.id == "d1"));
        assert!(matches!(h[3], HistoryEntry::Note(n) if n.id == "n1"));
    }

    #[test]
    fn search_finds_staff_and_tests_fuzzy() {
        let mut d = base();
        d.staff.push(staff("p-1", "Alice Tan", "MLT", &[]));
        let mut gone = staff("p-2", "Alicia Hidden", "MLT", &[]);
        gone.status = StaffStatus::Hidden;
        d.staff.push(gone);
        let r = search(&d, "cel dyn", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(
            r[0].0,
            SearchHit::Test {
                section_id: "s-haem".into(),
                test_id: "t-h".into()
            }
        );
        assert_eq!(r[0].1, "Cell-DYN Ruby \u{b7} Haematology");

        let r = search(&d, "alic", 10);
        assert_eq!(
            r,
            vec![(SearchHit::Staff("p-1".into()), "Alice Tan".to_string())]
        );

        let r = search(&d, "chem", 10);
        assert!(r.contains(&(SearchHit::Section("s-chem".into()), "Chemistry".to_string())));

        let r = search(&d, "CEL DYN", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "Cell-DYN Ruby \u{b7} Haematology");
        assert_eq!(
            search(&d, "Alic", 10),
            vec![(SearchHit::Staff("p-1".into()), "Alice Tan".to_string())]
        );

        assert!(search(&d, "", 10).is_empty());
        assert!(search(&d, "   ", 10).is_empty());
        assert_eq!(search(&d, "a", 2).len(), 2);

        d.sections[1].tests[0].retired = true;
        assert!(search(&d, "cel dyn", 10).is_empty());
        d.sections[1].retired = true;
        assert!(search(&d, "haem", 10).is_empty());
    }

    #[test]
    fn filter_staff_defaults_and_filters() {
        let mut d = base();
        d.staff.push(staff("p-1", "Zed", "MLT", &["t-h"]));
        d.staff.push(staff("p-2", "Amy", "Senior MLT", &["t-ca"]));
        let mut hid = staff("p-3", "Hal", "MLT", &["t-ca"]);
        hid.status = StaffStatus::Hidden;
        d.staff.push(hid);
        d.staff[1].assignments.insert(
            "t-h".into(),
            Assignment {
                state: AssignState::Na,
                since: ymd(2025, 1, 1),
            },
        );

        let names = |f: &StaffFilter| {
            filter_staff(&d, f)
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&StaffFilter::default()), ["Amy", "Zed"]);
        assert_eq!(
            names(&StaffFilter {
                status: Some(StaffStatus::Hidden),
                ..Default::default()
            }),
            ["Hal"]
        );
        assert_eq!(
            names(&StaffFilter {
                status: Some(StaffStatus::Removed),
                ..Default::default()
            }),
            ["Hal"],
            "staff hidden earlier are listed with the removed"
        );
        assert_eq!(
            names(&StaffFilter {
                section: Some("s-haem".into()),
                ..Default::default()
            }),
            ["Zed"]
        );
        assert_eq!(
            names(&StaffFilter {
                title: Some("senior mlt".into()),
                ..Default::default()
            }),
            ["Amy"]
        );
        assert_eq!(
            names(&StaffFilter {
                text: "ZE".into(),
                ..Default::default()
            }),
            ["Zed"]
        );
        assert_eq!(distinct_titles(&d), ["MLT", "Senior MLT"]);
    }

    fn audit(at: Ts, by: &str, action: &str, detail: &str, refs: &[(&str, &str)]) -> AuditEntry {
        AuditEntry {
            at,
            by: by.into(),
            action: action.into(),
            detail: detail.into(),
            refs: refs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn audit_filter_by_user_and_range() {
        let mut d = base();
        d.audit.push(audit(
            ts(2026, 11, 1, 9),
            "jtan",
            "completion.add",
            "Added Cobas",
            &[("staff_id", "p-1"), ("section_id", "s-chem")],
        ));
        d.audit.push(audit(
            ts(2026, 11, 10, 23),
            "mlee",
            "staff.add",
            "Added Bob",
            &[("staff_id", "p-2")],
        ));
        d.audit.push(audit(
            ts(2026, 11, 12, 9),
            "jtan",
            "completion.edit",
            "Edited",
            &[("staff_id", "p-1")],
        ));
        let ids = |f: &AuditFilter| {
            filter_audit(&d, f)
                .iter()
                .map(|e| e.detail.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(&AuditFilter::default()),
            ["Edited", "Added Bob", "Added Cobas"]
        );
        assert_eq!(
            ids(&AuditFilter {
                user: Some("jtan".into()),
                ..Default::default()
            }),
            ["Edited", "Added Cobas"]
        );
        // `to` is inclusive of the whole day
        let f = AuditFilter {
            from: Some(ymd(2026, 11, 2)),
            to: Some(ymd(2026, 11, 10)),
            ..Default::default()
        };
        assert_eq!(ids(&f), ["Added Bob"]);
        let f = AuditFilter {
            from: Some(ymd(2026, 11, 1)),
            to: Some(ymd(2026, 11, 1)),
            ..Default::default()
        };
        assert_eq!(ids(&f), ["Added Cobas"]);
        assert_eq!(
            ids(&AuditFilter {
                section: Some("s-chem".into()),
                ..Default::default()
            }),
            ["Added Cobas"]
        );
        assert_eq!(
            ids(&AuditFilter {
                staff: Some("p-1".into()),
                ..Default::default()
            }),
            ["Edited", "Added Cobas"]
        );
        assert_eq!(
            ids(&AuditFilter {
                action_prefix: Some("completion.".into()),
                ..Default::default()
            }),
            ["Edited", "Added Cobas"]
        );
        assert_eq!(
            ids(&AuditFilter {
                text: "BOB".into(),
                ..Default::default()
            }),
            ["Added Bob"]
        );
        assert_eq!(distinct_audit_users(&d), ["jtan", "mlee"]);
    }
}
