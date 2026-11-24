//! Bringing the current data up to date with a newer copy of the workbook.
//!
//! Unlike a full import, nothing is replaced: staff, tests and records keep
//! their ids, and what people did in the app wins over the workbook (removed
//! or merged staff, renamed and retired tests, assignments, edited records,
//! due dates). The workbook adds what it has that the app doesn't: new
//! completions, ticks, corrected dates, new people, and its order of names.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::dates::fmt_date;
use crate::import::mapping::{name_aliases, normalize_name, ImportPlan, IMPORT_BY};
use crate::model::{
    new_id, AssignState, AuditEntry, Data, DueOverride, Id, OverrideSource, Record, Section, Ts,
};
use crate::schedule::stage_label;

/// An Annual this many days after the one it would replace is the next
/// annual, not a corrected date.
const NEXT_ANNUAL_DAYS: i64 = 180;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateReport {
    /// "person – section / test: stage date" lines.
    pub added: Vec<String>,
    /// Records that were unconfirmed and are ticked in the workbook now.
    pub confirmed: Vec<String>,
    /// Records whose date was corrected.
    pub changed: Vec<String>,
    /// Unconfirmed records taken out: coloured and unticked in the workbook,
    /// so due rather than done.
    pub not_done: Vec<String>,
    pub new_staff: Vec<String>,
    pub new_tests: Vec<String>,
    pub assigned: Vec<String>,
    /// Excel due dates set, moved or dropped.
    pub due_dates: Vec<String>,
    pub notes: usize,
    /// Where the workbook differs but the app's own change was kept.
    pub kept: Vec<String>,
    /// Imported earlier, no longer in the workbook; left as they are.
    pub stale: Vec<String>,
}

impl UpdateReport {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.confirmed.is_empty()
            && self.changed.is_empty()
            && self.not_done.is_empty()
            && self.new_staff.is_empty()
            && self.new_tests.is_empty()
            && self.assigned.is_empty()
            && self.due_dates.is_empty()
            && self.notes == 0
    }

    /// Plain text for Notepad (CRLF line ends).
    pub fn to_text(&self) -> String {
        let mut s = String::from("Excel update report\r\n");
        let lists = [
            ("Completions added", &self.added),
            ("Completions confirmed (ticked in Excel)", &self.confirmed),
            ("Completion dates corrected", &self.changed),
            ("Due, not done (coloured and unticked)", &self.not_done),
            ("New staff", &self.new_staff),
            ("New test systems", &self.new_tests),
            ("Newly assigned", &self.assigned),
            ("Due dates from Excel", &self.due_dates),
            ("Kept as they are in the app", &self.kept),
            ("No longer in Excel (left as they are)", &self.stale),
        ];
        for (title, items) in lists {
            s += &format!("\r\n{title} ({})\r\n", items.len());
            if items.is_empty() {
                s += "  (none)\r\n";
            }
            for i in items {
                s += &format!("  {i}\r\n");
            }
        }
        s += &format!("\r\nNotes added: {}\r\n", self.notes);
        s
    }
}

/// The text between `prefix` and `sep` in an audit line, and what follows.
fn between<'a>(s: &'a str, prefix: &str, sep: &str) -> Option<(&'a str, &'a str)> {
    s.strip_prefix(prefix)?.split_once(sep)
}

/// Plan staff id → current staff id; `None` for someone deleted in the app.
/// People are matched by name, then by names they had before a merge or a
/// rename in the app.
fn match_staff(data: &Data, plan: &Data) -> HashMap<Id, Option<Id>> {
    let mut former: HashMap<String, Id> = HashMap::new();
    let mut deleted: HashSet<String> = HashSet::new();
    for e in &data.audit {
        let old = match e.action.as_str() {
            "staff.merge" => between(&e.detail, "Merged ", " into ").map(|x| x.0),
            "staff.update" => between(&e.detail, "Updated details for ", " (now ").map(|x| x.0),
            "staff.delete" => {
                if let Some((n, _)) = between(&e.detail, "Permanently deleted ", " and all") {
                    deleted.insert(normalize_name(n));
                }
                None
            }
            _ => None,
        };
        if let (Some(old), Some(id)) = (old, e.refs.get("staff_id")) {
            former.insert(normalize_name(old), id.clone());
        }
    }
    plan.staff
        .iter()
        .filter_map(|p| {
            let norm = normalize_name(&p.name);
            let found = data
                .staff
                .iter()
                .find(|s| normalize_name(&s.name) == norm)
                .map(|s| s.id.clone())
                .or_else(|| {
                    former
                        .get(&norm)
                        .filter(|id| data.staff(id).is_some())
                        .cloned()
                })
                .or_else(|| {
                    let mine = name_aliases(&p.name);
                    data.staff
                        .iter()
                        .find(|s| name_aliases(&s.name).iter().any(|a| mine.contains(a)))
                        .map(|s| s.id.clone())
                });
            match found {
                Some(id) => Some((p.id.clone(), Some(id))),
                None if deleted.contains(&norm) => Some((p.id.clone(), None)),
                None => None,
            }
        })
        .collect()
}

/// What a section or test is called now, by the names it had before.
fn renamed(data: &Data, action: &str, prefix: &str, key: &str) -> HashMap<String, Id> {
    data.audit
        .iter()
        .filter(|e| e.action == action)
        .filter_map(|e| {
            let (old, _) = between(&e.detail, prefix, " to ")?;
            Some((normalize_name(old), e.refs.get(key)?.clone()))
        })
        .collect()
}

/// Adds the sections and tests only the workbook has; returns plan test id →
/// current test id and plan section id → current section id.
fn match_tests(
    data: &mut Data,
    plan: &Data,
    report: &mut UpdateReport,
) -> (HashMap<Id, Id>, HashMap<Id, Id>) {
    let old_sections = renamed(data, "section.rename", "Renamed section ", "section_id");
    let old_tests = renamed(data, "test.rename", "Renamed test ", "test_id");
    let mut tests = HashMap::new();
    let mut sections = HashMap::new();
    for ps in plan.sections_sorted() {
        let norm = normalize_name(&ps.name);
        let at = data
            .sections
            .iter()
            .position(|s| normalize_name(&s.name) == norm)
            .or_else(|| {
                let id = old_sections.get(&norm)?;
                data.sections.iter().position(|s| s.id == *id)
            });
        let at = at.unwrap_or_else(|| {
            data.sections.push(Section {
                order: data.sections.iter().map(|s| s.order + 1).max().unwrap_or(0),
                tests: vec![],
                staff_order: vec![],
                ..ps.clone()
            });
            data.sections.len() - 1
        });
        sections.insert(ps.id.clone(), data.sections[at].id.clone());
        for pt in &ps.tests {
            let norm = normalize_name(&pt.name);
            let here = data.sections[at]
                .tests
                .iter()
                .find(|t| normalize_name(&t.name) == norm)
                .map(|t| t.id.clone());
            let found = here
                .or_else(|| {
                    old_tests
                        .get(&norm)
                        .filter(|t| data.test(t).is_some())
                        .cloned()
                })
                .or_else(|| {
                    let mut all = data.sections.iter().flat_map(|s| &s.tests);
                    all.find(|t| normalize_name(&t.name) == norm)
                        .map(|t| t.id.clone())
                });
            let id = found.unwrap_or_else(|| {
                let sec = &mut data.sections[at];
                let mut t = pt.clone();
                t.order = sec.tests.iter().map(|t| t.order + 1).max().unwrap_or(0);
                report.new_tests.push(format!("{} / {}", sec.name, pt.name));
                sec.tests.push(t);
                pt.id.clone()
            });
            tests.insert(pt.id.clone(), id);
        }
    }
    (tests, sections)
}

type Pair = (Id, Id);

/// What people did in the app, per (staff, test).
#[derive(Default)]
struct Manual {
    records: HashSet<Pair>,
    assignments: HashSet<Pair>,
    /// (staff, section) assigned or unassigned as a whole.
    sections: HashSet<Pair>,
    due: HashSet<Pair>,
    stages: HashSet<Pair>,
}

impl Manual {
    fn of(data: &Data) -> Manual {
        let mut m = Manual::default();
        for e in &data.audit {
            let Some(staff) = e.refs.get("staff_id") else {
                continue;
            };
            let Some(test) = e.refs.get("test_id") else {
                if let (true, Some(sec)) = (e.action == "assignment.set", e.refs.get("section_id"))
                {
                    m.sections.insert((staff.clone(), sec.clone()));
                }
                continue;
            };
            let pair = (staff.clone(), test.clone());
            match e.action.as_str() {
                "record.add" | "record.edit" | "record.delete" => m.records.insert(pair),
                "assignment.set" => m.assignments.insert(pair),
                "due.override" => m.due.insert(pair),
                "stage.skip" | "stage.unskip" => m.stages.insert(pair),
                _ => false,
            };
        }
        // Covers data whose audit log was trimmed.
        for r in data.records.iter().filter(|r| !r.imported) {
            m.records.insert((r.staff_id.clone(), r.test_id.clone()));
        }
        m
    }
}

/// `current` brought up to date with the workbook in `plan`, and what changed.
pub fn update_existing(
    current: &Data,
    plan: &ImportPlan,
    user: &str,
    now: Ts,
) -> (Data, UpdateReport) {
    let mut data = current.clone();
    let mut report = UpdateReport::default();
    let manual = Manual::of(current);
    let coloured = &plan.not_done;
    let plan = &plan.data;

    let (tests, sections) = match_tests(&mut data, plan, &mut report);
    let mut staff = match_staff(&data, plan);
    for p in &plan.staff {
        match staff.get(&p.id) {
            Some(Some(id)) => {
                let s = data.staff_mut(id).expect("matched above");
                if s.title.trim().is_empty() {
                    s.title = p.title.clone();
                }
                s.joined = s.joined.or(p.joined);
            }
            Some(None) => report
                .kept
                .push(format!("{}: deleted in the app, not added again", p.name)),
            None => {
                let mut s = p.clone();
                s.assignments = BTreeMap::new();
                report.new_staff.push(s.name.clone());
                data.staff.push(s);
                staff.insert(p.id.clone(), Some(p.id.clone()));
            }
        }
    }
    let staff: HashMap<Id, Id> = staff
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect();
    let pair_of =
        |s: &Id, t: &Id| -> Option<Pair> { Some((staff.get(s)?.clone(), tests.get(t)?.clone())) };
    let known_before: HashSet<Pair> = current
        .records
        .iter()
        .map(|r| (r.staff_id.clone(), r.test_id.clone()))
        .chain(
            current
                .due_overrides
                .iter()
                .map(|o| (o.staff_id.clone(), o.test_id.clone())),
        )
        .chain(
            current
                .skipped
                .iter()
                .map(|s| (s.staff_id.clone(), s.test_id.clone())),
        )
        .collect();

    // Records, pair by pair.
    let wanted_dates: HashSet<(Id, Id, chrono::NaiveDate)> = plan
        .records
        .iter()
        .filter_map(|r| {
            let (s, t) = pair_of(&r.staff_id, &r.test_id)?;
            Some((s, t, r.completed_on))
        })
        .collect();
    let mut wanted: BTreeMap<Pair, Vec<&Record>> = BTreeMap::new();
    let mut still_planned: HashSet<Pair> = HashSet::new();
    // Dates that are due in the workbook aren't completions: unconfirmed
    // ones imported earlier go. Confirmed ones and edits made in the app stay.
    for n in coloured {
        let Some(pair) = pair_of(&n.staff_id, &n.test_id) else {
            continue;
        };
        if manual.records.contains(&pair) {
            continue;
        }
        let name = what(&data, &pair);
        let in_plan =
            |d: chrono::NaiveDate| wanted_dates.contains(&(pair.0.clone(), pair.1.clone(), d));
        data.records.retain(|r| {
            let same = r.staff_id == pair.0 && r.test_id == pair.1 && r.imported;
            let gone = same
                && !in_plan(r.completed_on)
                && (r.completed_on == n.on || r.stage_index == n.stage_index);
            if gone && !r.unconfirmed {
                report.kept.push(format!(
                    "{name}: {} {} is confirmed here but due in Excel",
                    r.stage_label,
                    fmt_date(r.completed_on)
                ));
            } else if gone {
                report.not_done.push(format!(
                    "{name}: {} {}",
                    r.stage_label,
                    fmt_date(r.completed_on)
                ));
            }
            !gone || !r.unconfirmed
        });
    }
    for r in &plan.records {
        if let Some(pair) = pair_of(&r.staff_id, &r.test_id) {
            wanted.entry(pair).or_default().push(r);
        }
    }
    for (pair, mut records) in wanted {
        records.sort_by_key(|r| (r.completed_on, r.stage_index));
        // A planned date that has passed without a tick is still planned (and
        // now overdue), not done.
        let planned = current
            .due_overrides
            .iter()
            .find(|o| o.staff_id == pair.0 && o.test_id == pair.1)
            .map(|o| o.due_on);
        records.retain(|r| {
            let missed = r.unconfirmed && planned == Some(r.completed_on);
            if missed {
                still_planned.insert(pair.clone());
                report.kept.push(format!(
                    "{}: {} was planned for {} and isn't ticked in Excel, so it is still due",
                    what(&data, &pair),
                    r.stage_label,
                    fmt_date(r.completed_on)
                ));
            }
            !missed
        });
        update_pair(
            &mut data,
            &pair,
            &records,
            manual.records.contains(&pair),
            now,
            &mut report,
        );
    }

    // Stages skipped in the workbook; one that was done isn't skipped.
    let mut skipped: BTreeSet<(Id, Id, u32)> = data
        .skipped
        .iter()
        .map(|s| (s.staff_id.clone(), s.test_id.clone(), s.stage_index))
        .collect();
    for s in &plan.skipped {
        let Some((sid, tid)) = pair_of(&s.staff_id, &s.test_id) else {
            continue;
        };
        if manual.stages.contains(&(sid.clone(), tid.clone()))
            || manual.records.contains(&(sid.clone(), tid.clone()))
        {
            continue;
        }
        if skipped.insert((sid.clone(), tid.clone(), s.stage_index)) {
            let mut s = s.clone();
            s.staff_id = sid;
            s.test_id = tid;
            data.skipped.push(s);
        }
    }
    let done: HashSet<(Id, Id, u32)> = data
        .records
        .iter()
        .map(|r| (r.staff_id.clone(), r.test_id.clone(), r.stage_index))
        .collect();
    data.skipped
        .retain(|s| !done.contains(&(s.staff_id.clone(), s.test_id.clone(), s.stage_index)));

    // Assignments: new pairs only, unless the app's was imported as N/A.
    for p in &plan.staff {
        for (t, a) in &p.assignments {
            let Some(pair) = pair_of(&p.id, t) else {
                continue;
            };
            let section = data.test(&pair.1).map(|(s, _)| s.id.clone());
            let by_hand = manual.assignments.contains(&pair)
                || section.is_some_and(|s| manual.sections.contains(&(pair.0.clone(), s)));
            let name = what(&data, &pair);
            let s = data.staff_mut(&pair.0).expect("mapped to current staff");
            match s.assignments.get_mut(&pair.1) {
                None if by_hand || known_before.contains(&pair) => {}
                None => {
                    s.assignments.insert(pair.1.clone(), a.clone());
                    if a.state == AssignState::Assigned {
                        report.assigned.push(name);
                    }
                }
                Some(cur)
                    if cur.state == AssignState::Na
                        && a.state == AssignState::Assigned
                        && !by_hand =>
                {
                    *cur = a.clone();
                    report.assigned.push(name);
                }
                Some(_) => {}
            }
        }
    }

    update_due_dates(
        &mut data,
        plan,
        &manual,
        &still_planned,
        &pair_of,
        &mut report,
    );

    // Notes.
    let mut seen: HashSet<(Id, Option<Id>, Option<Id>, String)> = data
        .notes
        .iter()
        .map(|n| {
            (
                n.staff_id.clone(),
                n.test_id.clone(),
                n.section_id.clone(),
                n.text.clone(),
            )
        })
        .collect();
    for n in &plan.notes {
        let Some(sid) = staff.get(&n.staff_id) else {
            continue;
        };
        let mut n = n.clone();
        n.staff_id = sid.clone();
        n.test_id = n.test_id.and_then(|t| tests.get(&t).cloned());
        n.section_id = n.section_id.and_then(|s| sections.get(&s).cloned());
        let key = (
            n.staff_id.clone(),
            n.test_id.clone(),
            n.section_id.clone(),
            n.text.clone(),
        );
        if seen.insert(key) {
            data.notes.push(n);
            report.notes += 1;
        }
    }

    // The workbook's order of names.
    for ps in &plan.sections {
        let Some(sec) = data.sections.iter_mut().find(|s| s.id == sections[&ps.id]) else {
            continue;
        };
        let mut order: Vec<Id> = Vec::new();
        let listed = ps.staff_order.iter().filter_map(|p| staff.get(p));
        for id in listed.chain(&sec.staff_order) {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        sec.staff_order = order;
    }

    data.audit.push(AuditEntry {
        at: now,
        by: user.to_owned(),
        action: "import.update".into(),
        detail: format!(
            "Updated from Excel: {} completions added, {} confirmed, {} dates corrected, {} set to due, {} new staff, {} due dates changed",
            report.added.len(),
            report.confirmed.len(),
            report.changed.len(),
            report.not_done.len(),
            report.new_staff.len(),
            report.due_dates.len()
        ),
        refs: BTreeMap::new(),
    });
    (data, report)
}

/// "person – section / test".
fn what(data: &Data, (staff, test): &Pair) -> String {
    let person = data.staff(staff).map_or("", |s| s.name.as_str());
    match data.test(test) {
        Some((s, t)) => format!("{person} \u{2013} {} / {}", s.name, t.name),
        None => person.to_owned(),
    }
}

/// Brings one person's records of one test up to date with the workbook's
/// (`wanted`, oldest first). `by_hand`: someone changed these records in the
/// app, so nothing already there is touched.
fn update_pair(
    data: &mut Data,
    pair: &Pair,
    wanted: &[&Record],
    by_hand: bool,
    now: Ts,
    report: &mut UpdateReport,
) {
    let name = what(data, pair);
    let rule = data.rule_for_test(&pair.1).clone();
    let first_annual = rule.stages.len().saturating_sub(1) as u32;
    let mine: Vec<usize> = (0..data.records.len())
        .filter(|&i| data.records[i].staff_id == pair.0 && data.records[i].test_id == pair.1)
        .collect();
    let mut matched: HashSet<usize> = HashSet::new();
    let mut left: Vec<&Record> = Vec::new();
    // Same date: the same completion, whatever stage the app calls it.
    for w in wanted {
        let same = mine
            .iter()
            .copied()
            .filter(|i| !matched.contains(i))
            .filter(|&i| data.records[i].completed_on == w.completed_on)
            .min_by_key(|&i| data.records[i].stage_index.abs_diff(w.stage_index));
        let Some(i) = same else {
            left.push(w);
            continue;
        };
        matched.insert(i);
        let r = &mut data.records[i];
        if r.unconfirmed && !w.unconfirmed {
            r.unconfirmed = false;
            report.confirmed.push(format!(
                "{name}: {} {}",
                r.stage_label,
                fmt_date(r.completed_on)
            ));
        }
    }
    for w in left {
        let free = mine
            .iter()
            .copied()
            .filter(|i| !matched.contains(i))
            .find(|&i| data.records[i].stage_index == w.stage_index);
        if by_hand {
            let newest = mine.iter().map(|&i| data.records[i].completed_on).max();
            if newest.is_some_and(|d| w.completed_on <= d) {
                report.kept.push(format!(
                    "{name}: Excel has {} on {}; your own entries were kept",
                    w.stage_label,
                    fmt_date(w.completed_on)
                ));
                continue;
            }
        } else if let Some(i) = free {
            // The Annual row moved on by a year: that's the next annual, and
            // the one it replaced in the workbook stays in the history.
            let old = data.records[i].completed_on;
            let next_annual = w.stage_index >= first_annual
                && !data.records[i].unconfirmed
                && (w.completed_on - old).num_days() > NEXT_ANNUAL_DAYS;
            if !next_annual {
                matched.insert(i);
                let r = &mut data.records[i];
                r.completed_on = w.completed_on;
                r.unconfirmed = w.unconfirmed;
                report.changed.push(format!(
                    "{name}: {} {} \u{2192} {}{}",
                    r.stage_label,
                    fmt_date(old),
                    fmt_date(w.completed_on),
                    if w.unconfirmed { " (unconfirmed)" } else { "" }
                ));
                continue;
            }
        }
        // A new completion. Its stage, unless something already fills it.
        let mut stage = w.stage_index;
        let taken = |s: u32| {
            data.records
                .iter()
                .any(|r| r.staff_id == pair.0 && r.test_id == pair.1 && r.stage_index == s)
        };
        if stage >= first_annual {
            while taken(stage) {
                stage += 1;
            }
        }
        let label = stage_label(&rule, stage);
        report.added.push(format!(
            "{name}: {label} {}{}",
            fmt_date(w.completed_on),
            if w.unconfirmed { " (unconfirmed)" } else { "" }
        ));
        data.records.push(Record {
            id: new_id("c"),
            staff_id: pair.0.clone(),
            test_id: pair.1.clone(),
            stage_index: stage,
            stage_label: label,
            completed_on: w.completed_on,
            comment: w.comment.clone(),
            by: IMPORT_BY.into(),
            at: now,
            imported: true,
            unconfirmed: w.unconfirmed,
        });
    }
    if !by_hand {
        for i in mine {
            if !matched.contains(&i) {
                let r = &data.records[i];
                report.stale.push(format!(
                    "{name}: {} {}",
                    r.stage_label,
                    fmt_date(r.completed_on)
                ));
            }
        }
    }
}

/// Due dates that came from the workbook follow it; ones set in the app stay.
fn update_due_dates(
    data: &mut Data,
    plan: &Data,
    manual: &Manual,
    still_planned: &HashSet<Pair>,
    pair_of: &impl Fn(&Id, &Id) -> Option<Pair>,
    report: &mut UpdateReport,
) {
    let mut wanted: BTreeMap<Pair, &DueOverride> = BTreeMap::new();
    for o in &plan.due_overrides {
        if let Some(pair) = pair_of(&o.staff_id, &o.test_id) {
            let slot = wanted.entry(pair).or_insert(o);
            if o.due_on > slot.due_on {
                *slot = o;
            }
        }
    }
    let mut pairs: BTreeSet<Pair> = wanted.keys().cloned().collect();
    for p in &plan.staff {
        pairs.extend(p.assignments.keys().filter_map(|t| pair_of(&p.id, t)));
    }
    for pair in pairs {
        let is = |o: &DueOverride| o.staff_id == pair.0 && o.test_id == pair.1;
        let cur = data.due_overrides.iter().find(|o| is(o));
        if manual.records.contains(&pair)
            || manual.due.contains(&pair)
            || still_planned.contains(&pair)
            || cur.is_some_and(|o| o.source == OverrideSource::Manual)
        {
            continue;
        }
        let cur = cur.map(|o| o.due_on);
        let want = wanted.get(&pair);
        if cur == want.map(|o| o.due_on) {
            continue;
        }
        let show = |d: Option<chrono::NaiveDate>| d.map_or("calculated".to_owned(), fmt_date);
        report.due_dates.push(format!(
            "{}: {} \u{2192} {}",
            what(data, &pair),
            show(cur),
            show(want.map(|o| o.due_on))
        ));
        data.due_overrides.retain(|o| !is(o));
        if let Some(o) = want {
            data.due_overrides.push(DueOverride {
                staff_id: pair.0,
                test_id: pair.1,
                ..(*o).clone()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::excel::{Cell, RawBlock, RawSheet, RawTest, StageCell};
    use crate::import::mapping::{build_import, propose_merges, ImportOptions};
    use crate::model::StaffStatus;
    use crate::ops::{self, Actor};
    use chrono::{NaiveDate, TimeZone};

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn now() -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 24, 9, 0, 0)
            .unwrap()
    }

    fn actor() -> Actor {
        Actor {
            user: "agnes".into(),
            now: now(),
        }
    }

    fn on(y: i32, m: u32, d: u32, ticked: bool) -> StageCell {
        StageCell {
            cell: Cell::Date(ymd(y, m, d)),
            ticked,
            due: false,
        }
    }

    fn none() -> StageCell {
        StageCell {
            cell: Cell::Empty,
            ticked: false,
            due: false,
        }
    }

    type Cells = [StageCell; 4];

    fn plan_of(people: &[(&str, Cells)]) -> ImportPlan {
        let blocks = people
            .iter()
            .enumerate()
            .map(|(i, (name, cells))| RawBlock {
                row: 3 + 4 * i,
                name: (*name).into(),
                title: "MT".into(),
                joined: None,
                stages: cells.clone().map(|c| vec![c]),
                remarks: vec![],
            })
            .collect();
        let sheets = [RawSheet {
            name: "Urinalysis".into(),
            tests: vec![RawTest {
                col: 5,
                header: "Microscopy".into(),
                tick_col: Some(6),
                section_hint: "Urinalysis".into(),
            }],
            blocks,
        }];
        let opts = ImportOptions {
            user: "jtan".into(),
            now: now(),
            tolerance_days: 30,
        };
        build_import(&sheets, &propose_merges(&sheets), &opts)
    }

    fn base() -> Cells {
        [on(2024, 1, 10, true), on(2024, 7, 10, true), none(), none()]
    }

    fn update(cur: &Data, plan: &ImportPlan) -> (Data, UpdateReport) {
        update_existing(cur, plan, "jtan", now())
    }

    fn dates(d: &Data, name: &str) -> Vec<(String, NaiveDate, bool)> {
        let id = &d.staff.iter().find(|s| s.name == name).unwrap().id;
        let mut v: Vec<_> = d
            .records
            .iter()
            .filter(|r| r.staff_id == *id)
            .map(|r| (r.stage_label.clone(), r.completed_on, r.unconfirmed))
            .collect();
        v.sort_by_key(|x| x.1);
        v
    }

    fn pair(d: &Data, name: &str) -> Pair {
        (
            d.staff.iter().find(|s| s.name == name).unwrap().id.clone(),
            d.sections[0].tests[0].id.clone(),
        )
    }

    #[test]
    fn the_same_workbook_changes_nothing() {
        let plan = plan_of(&[("Amy", base())]);
        let (d, r) = update(&plan.data, &plan_of(&[("Amy", base())]));
        assert!(r.is_empty(), "{r:?}");
        assert!(r.kept.is_empty() && r.stale.is_empty());
        assert_eq!(d.staff, plan.data.staff);
        assert_eq!(d.records, plan.data.records);
        assert_eq!(d.sections, plan.data.sections);
        assert_eq!(d.audit.len(), plan.data.audit.len() + 1);
    }

    #[test]
    fn adds_new_completions_and_ticks_and_moves_dates() {
        let cur = plan_of(&[
            ("Amy", base()),
            (
                "Bea",
                [
                    on(2024, 1, 10, true),
                    on(2024, 7, 10, false),
                    none(),
                    none(),
                ],
            ),
            (
                "Cal",
                [
                    on(2026, 2, 16, true),
                    on(2026, 8, 16, false),
                    none(),
                    none(),
                ],
            ),
        ])
        .data;
        let mut amy = base();
        amy[2] = on(2025, 7, 1, true);
        amy[3] = on(2026, 7, 2, true);
        let (d, r) = update(
            &cur,
            &plan_of(&[
                ("Amy", amy),
                ("Bea", base()),
                (
                    "Cal",
                    [on(2026, 2, 16, true), on(2026, 9, 30, true), none(), none()],
                ),
            ]),
        );
        let labels =
            |n: &str| -> Vec<String> { dates(&d, n).into_iter().map(|(l, ..)| l).collect() };
        assert_eq!(
            labels("Amy"),
            ["Initial", "Semi-annual", "1st Annual", "2nd Annual"]
        );
        assert_eq!(r.added.len(), 2);
        assert_eq!(
            dates(&d, "Bea")[1],
            ("Semi-annual".into(), ymd(2024, 7, 10), false)
        );
        assert_eq!(r.confirmed.len(), 1);
        assert_eq!(
            dates(&d, "Cal")[1],
            ("Semi-annual".into(), ymd(2026, 9, 30), false)
        );
        assert_eq!(r.changed.len(), 1);
        assert_eq!(d.staff.len(), 3);
    }

    #[test]
    fn an_annual_row_moved_on_a_year_keeps_the_old_annual() {
        let mut c = base();
        c[2] = on(2025, 7, 1, true);
        let cur = plan_of(&[("Amy", c.clone())]).data;
        c[2] = on(2026, 7, 2, true);
        let plan = plan_of(&[("Amy", c)]);
        let (d, r) = update(&cur, &plan);
        let got = dates(&d, "Amy");
        assert_eq!(got[2], ("1st Annual".into(), ymd(2025, 7, 1), false));
        assert_eq!(got[3], ("2nd Annual".into(), ymd(2026, 7, 2), false));
        assert_eq!(r.stale.len(), 1, "listed, not removed");
        // And again: nothing more.
        let (d2, r2) = update(&d, &plan);
        assert_eq!(d2.records, d.records);
        assert!(r2.added.is_empty());
    }

    #[test]
    fn what_was_done_in_the_app_is_kept() {
        let mut cur = plan_of(&[("Amy", base()), ("JH", base()), ("Old", base())]).data;
        let a = actor();
        let (amy, test) = pair(&cur, "Amy");
        // Amy's semi-annual date was corrected by hand.
        let semi = cur
            .records
            .iter()
            .find(|r| r.staff_id == amy && r.stage_index == 1)
            .unwrap()
            .id
            .clone();
        ops::edit_record(&mut cur, &a, &semi, ymd(2024, 7, 15), 1, "").unwrap();
        // JH was merged into Jo Hill, and Old removed and unassigned.
        let jia = ops::add_staff(&mut cur, &a, "Jo Hill", "MT", None).unwrap();
        let jh = pair(&cur, "JH").0;
        ops::merge_staff(&mut cur, &a, &jia, &jh).unwrap();
        let old = pair(&cur, "Old").0;
        ops::set_staff_status(&mut cur, &a, &old, StaffStatus::Removed).unwrap();
        ops::set_assignment(&mut cur, &a, &old, &test, None).unwrap();

        let mut newer = base();
        newer[2] = on(2025, 7, 1, true);
        let (d, r) = update(
            &cur,
            &plan_of(&[
                ("Amy", newer.clone()),
                ("JH", newer.clone()),
                ("Old", newer),
            ]),
        );
        assert_eq!(d.staff.len(), 3, "JH isn't added again");
        let got = dates(&d, "Amy");
        assert_eq!(got[1].1, ymd(2024, 7, 15), "the edit stays");
        assert_eq!(
            got.last().unwrap().1,
            ymd(2025, 7, 1),
            "the newer one is added"
        );
        assert_eq!(r.kept.len(), 1, "{:?}", r.kept);
        assert_eq!(dates(&d, "Jo Hill").len(), 3);
        let old = d.staff.iter().find(|s| s.name == "Old").unwrap();
        assert_eq!(old.status, StaffStatus::Removed);
        assert!(old.assignments.is_empty());
        assert_eq!(dates(&d, "Old").len(), 3, "the history is still kept");
    }

    #[test]
    fn renamed_tests_and_sections_still_match() {
        let mut cur = plan_of(&[("Amy", base())]).data;
        let a = actor();
        let (_, test) = pair(&cur, "Amy");
        let sec = cur.sections[0].id.clone();
        ops::rename_test(&mut cur, &a, &test, "Urine microscopy").unwrap();
        ops::rename_section(&mut cur, &a, &sec, "Urines").unwrap();
        let (d, r) = update(&cur, &plan_of(&[("Amy", base())]));
        assert!(r.is_empty(), "{r:?}");
        assert_eq!((d.sections.len(), d.sections[0].tests.len()), (1, 1));
    }

    #[test]
    fn new_people_and_the_order_of_names_come_from_the_workbook() {
        let mut cur = plan_of(&[("Amy", base()), ("Bea", base())]).data;
        cur.sections[0].staff_order.clear(); // saved before orders were kept
        let (d, r) = update(
            &cur,
            &plan_of(&[("Zoe", base()), ("Bea", base()), ("Amy", base())]),
        );
        assert_eq!(r.new_staff, ["Zoe"]);
        assert_eq!(r.assigned.len(), 1);
        let names: Vec<&str> = d.sections[0]
            .staff_order
            .iter()
            .map(|id| d.staff(id).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["Zoe", "Bea", "Amy"]);
    }

    #[test]
    fn excel_due_dates_follow_the_workbook_and_manual_ones_stay() {
        let mut planned = base();
        planned[2] = on(2027, 3, 1, false);
        let mut cur = plan_of(&[("Amy", planned.clone()), ("Bea", planned.clone())]).data;
        assert_eq!(cur.due_overrides.len(), 2);
        let (bea, test) = pair(&cur, "Bea");
        ops::set_due_override(&mut cur, &actor(), &bea, &test, Some(ymd(2027, 1, 1))).unwrap();
        planned[2] = on(2026, 11, 2, true);
        let (d, r) = update(
            &cur,
            &plan_of(&[("Amy", planned.clone()), ("Bea", planned)]),
        );
        assert_eq!(r.due_dates.len(), 1, "{:?}", r.due_dates);
        assert_eq!(d.due_overrides.len(), 1);
        assert_eq!(d.due_overrides[0].due_on, ymd(2027, 1, 1));
    }

    #[test]
    fn a_planned_date_that_passed_unticked_is_still_due() {
        let mut planned = base();
        planned[2] = on(2026, 11, 18, false);
        let mut cur = plan_of(&[("Amy", planned.clone())]).data;
        // Imported on the 17th, when the date was still to come.
        cur.records.retain(|r| r.stage_index < 2);
        let (amy, test) = pair(&cur, "Amy");
        cur.due_overrides.push(DueOverride {
            staff_id: amy,
            test_id: test,
            due_on: ymd(2026, 11, 18),
            source: OverrideSource::Excel,
            by: IMPORT_BY.into(),
            at: now(),
        });
        let (d, r) = update(&cur, &plan_of(&[("Amy", planned.clone())]));
        assert_eq!(dates(&d, "Amy").len(), 2);
        assert_eq!(d.due_overrides, cur.due_overrides);
        assert_eq!(r.kept.len(), 1);
        // Ticked: done, and no longer planned.
        planned[2].ticked = true;
        let (d, _) = update(&cur, &plan_of(&[("Amy", planned)]));
        assert_eq!(dates(&d, "Amy").len(), 3);
        assert!(d.due_overrides.is_empty());
    }

    #[test]
    fn coloured_dates_stop_being_completions() {
        let mut c = base();
        c[2] = on(2025, 7, 1, false);
        let people = |c: &Cells| {
            plan_of(&[
                ("Amy", c.clone()),
                ("Bea", c.clone()),
                ("Cal", c.clone()),
                ("Dee", c.clone()),
            ])
        };
        let mut cur = people(&c).data;
        let annual = |d: &Data, n: &str| {
            let s = pair(d, n).0;
            d.records
                .iter()
                .find(|r| r.staff_id == s && r.stage_index == 2)
                .map(|r| r.id.clone())
        };
        // Bea's was confirmed and Cal's edited in the app.
        let (bea, cal) = (annual(&cur, "Bea").unwrap(), annual(&cur, "Cal").unwrap());
        ops::confirm_records(&mut cur, &actor(), &[bea]).unwrap();
        ops::edit_record(&mut cur, &actor(), &cal, ymd(2025, 7, 1), 2, "ok").unwrap();
        // Dee's has another date in the workbook now.
        let dee = annual(&cur, "Dee").unwrap();
        let r = cur.records.iter_mut().find(|r| r.id == dee).unwrap();
        r.completed_on = ymd(2025, 6, 1);
        c[2].due = true;
        let plan = people(&c);
        let (d, r) = update(&cur, &plan);
        assert!(annual(&d, "Amy").is_none() && annual(&d, "Dee").is_none());
        assert!(annual(&d, "Bea").is_some() && annual(&d, "Cal").is_some());
        assert_eq!(r.not_done.len(), 2, "{:?}", r.not_done);
        let (amy, _) = pair(&d, "Amy");
        let o = d.due_overrides.iter().find(|o| o.staff_id == amy).unwrap();
        assert_eq!(o.due_on, ymd(2025, 7, 1));
        let (_, again) = update(&d, &plan);
        assert!(again.is_empty(), "{again:?}");
    }

    /// Updates a real snapshot from a real workbook:
    /// `TRACKER_JSON`, `TRACKER_XLSM` in, `TRACKER_OUT` (json) and a report out.
    #[test]
    #[ignore]
    fn real_update() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let mut cur: Data =
            serde_json::from_str(&std::fs::read_to_string(var("TRACKER_JSON")).unwrap()).unwrap();
        cur.ensure_valid();
        let sheets =
            crate::import::excel::read_workbook(std::path::Path::new(&var("TRACKER_XLSM")))
                .unwrap();
        let now = crate::dates::now_ts();
        let opts = ImportOptions {
            user: cur.saved_by.clone(),
            now,
            tolerance_days: cur.settings.variance_tolerance_days,
        };
        let plan = build_import(&sheets, &propose_merges(&sheets), &opts);
        let (mut d, r) = update_existing(&cur, &plan, &opts.user, now);
        d.saved_at = now;
        let out = var("TRACKER_OUT");
        std::fs::write(&out, serde_json::to_string_pretty(&d).unwrap()).unwrap();
        std::fs::write(format!("{out}.report.txt"), r.to_text()).unwrap();
        // Running it again changes nothing.
        let (d2, r2) = update_existing(&d, &plan, &opts.user, now);
        assert!(r2.is_empty(), "{}", r2.to_text());
        assert_eq!(d2.records, d.records);
    }
}
