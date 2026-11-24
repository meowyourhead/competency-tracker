//! Audited data operations. Every UI mutation goes through here: validate first,
//! then mutate `Data` and append exactly one `AuditEntry`. A failed operation
//! leaves `Data` (including the audit log) untouched.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::dates::fmt_date;
use crate::model::*;
use crate::schedule::stage_label;

pub struct Actor {
    pub user: String,
    pub now: Ts,
}

impl Actor {
    pub fn today(&self) -> NaiveDate {
        self.now.date_naive()
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum OpError {
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("date is in the future")]
    FutureDate,
    #[error("name cannot be empty")]
    EmptyName,
    #[error("has completion records")]
    HasRecords,
    #[error("person must be removed first")]
    NotRemoved,
    #[error("confirmation text does not match")]
    ConfirmMismatch,
    #[error("the default rule cannot be deleted")]
    IsDefaultRule,
    #[error("can't merge a person with themselves")]
    SamePerson,
}

type Res<T = ()> = Result<T, OpError>;

fn log(d: &mut Data, a: &Actor, action: &str, detail: String, refs: &[(&str, &str)]) {
    d.audit.push(AuditEntry {
        at: a.now,
        by: a.user.clone(),
        action: action.into(),
        detail,
        refs: refs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    });
}

fn staff_name(d: &Data, id: &str) -> Res<String> {
    d.staff(id)
        .map(|s| s.name.clone())
        .ok_or(OpError::NotFound("staff"))
}

fn test_info(d: &Data, id: &str) -> Res<(String, String)> {
    d.test(id)
        .map(|(s, t)| (s.id.clone(), t.name.clone()))
        .ok_or(OpError::NotFound("test"))
}

fn non_empty(s: &str) -> Res<String> {
    let t = s.trim();
    if t.is_empty() {
        Err(OpError::EmptyName)
    } else {
        Ok(t.to_string())
    }
}

// ---------------------------------------------------------------- completions

pub fn add_completion(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    test_id: &str,
    stage_index: u32,
    completed_on: NaiveDate,
    comment: &str,
) -> Res<Id> {
    if completed_on > a.today() {
        return Err(OpError::FutureDate);
    }
    let name = staff_name(d, staff_id)?;
    let (section_id, test_name) = test_info(d, test_id)?;
    let label = stage_label(d.rule_for_test(test_id), stage_index);
    let id = new_id("c");
    d.records.push(Record {
        id: id.clone(),
        staff_id: staff_id.into(),
        test_id: test_id.into(),
        stage_index,
        stage_label: label.clone(),
        completed_on,
        comment: comment.into(),
        by: a.user.clone(),
        at: a.now,
        imported: false,
        unconfirmed: false,
    });
    d.due_overrides
        .retain(|o| !(o.staff_id == staff_id && o.test_id == test_id));
    log(
        d,
        a,
        "record.add",
        format!(
            "Marked {test_name} {label} complete for {name} on {}",
            fmt_date(completed_on)
        ),
        &[
            ("staff_id", staff_id),
            ("test_id", test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(id)
}

pub fn edit_record(
    d: &mut Data,
    a: &Actor,
    record_id: &str,
    completed_on: NaiveDate,
    stage_index: u32,
    comment: &str,
) -> Res {
    if completed_on > a.today() {
        return Err(OpError::FutureDate);
    }
    let rec = d
        .records
        .iter()
        .find(|r| r.id == record_id)
        .ok_or(OpError::NotFound("record"))?;
    let (staff_id, test_id) = (rec.staff_id.clone(), rec.test_id.clone());
    let name = d
        .staff(&staff_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let (section_id, test_name) = d
        .test(&test_id)
        .map(|(s, t)| (s.id.clone(), t.name.clone()))
        .unwrap_or_default();
    let label = stage_label(d.rule_for_test(&test_id), stage_index);
    let rec = d
        .records
        .iter_mut()
        .find(|r| r.id == record_id)
        .expect("checked above");
    rec.completed_on = completed_on;
    rec.stage_index = stage_index;
    rec.stage_label = label.clone();
    rec.comment = comment.into();
    log(
        d,
        a,
        "record.edit",
        format!(
            "Edited {test_name} {label} for {name}: completed {}",
            fmt_date(completed_on)
        ),
        &[
            ("staff_id", &staff_id),
            ("test_id", &test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

pub fn delete_record(d: &mut Data, a: &Actor, record_id: &str) -> Res {
    let pos = d
        .records
        .iter()
        .position(|r| r.id == record_id)
        .ok_or(OpError::NotFound("record"))?;
    let rec = d.records.remove(pos);
    let name = d
        .staff(&rec.staff_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let (section_id, test_name) = d
        .test(&rec.test_id)
        .map(|(s, t)| (s.id.clone(), t.name.clone()))
        .unwrap_or_default();
    log(
        d,
        a,
        "record.delete",
        format!(
            "Deleted {test_name} {} completion for {name} on {}",
            rec.stage_label,
            fmt_date(rec.completed_on)
        ),
        &[
            ("staff_id", &rec.staff_id),
            ("test_id", &rec.test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

/// Clears the `unconfirmed` flag. Unknown or already-confirmed ids are ignored; when
/// nothing changes, nothing is audited. Returns the number of records confirmed.
pub fn confirm_records(d: &mut Data, a: &Actor, ids: &[Id]) -> Res<usize> {
    let mut n = 0;
    for r in d
        .records
        .iter_mut()
        .filter(|r| r.unconfirmed && ids.contains(&r.id))
    {
        r.unconfirmed = false;
        n += 1;
    }
    if n > 0 {
        let s = if n == 1 { "" } else { "s" };
        log(
            d,
            a,
            "record.confirm",
            format!("Confirmed {n} imported record{s}"),
            &[],
        );
    }
    Ok(n)
}

pub fn add_delay_note(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    test_id: &str,
    for_stage_index: u32,
    note: &str,
    expected_on: Option<NaiveDate>,
) -> Res<Id> {
    let name = staff_name(d, staff_id)?;
    let (section_id, test_name) = test_info(d, test_id)?;
    let label = stage_label(d.rule_for_test(test_id), for_stage_index);
    let id = new_id("d");
    d.delay_notes.push(DelayNote {
        id: id.clone(),
        staff_id: staff_id.into(),
        test_id: test_id.into(),
        for_stage_index,
        note: note.into(),
        expected_on,
        by: a.user.clone(),
        at: a.now,
    });
    let mut detail = format!("Added delay note for {name}, {test_name} {label}: {note}");
    if let Some(e) = expected_on {
        detail.push_str(&format!(" (expected {})", fmt_date(e)));
    }
    log(
        d,
        a,
        "delay.add",
        detail,
        &[
            ("staff_id", staff_id),
            ("test_id", test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(id)
}

pub fn delete_delay_note(d: &mut Data, a: &Actor, id: &str) -> Res {
    let pos = d
        .delay_notes
        .iter()
        .position(|n| n.id == id)
        .ok_or(OpError::NotFound("delay note"))?;
    let n = d.delay_notes.remove(pos);
    let name = d
        .staff(&n.staff_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let (section_id, test_name) = d
        .test(&n.test_id)
        .map(|(s, t)| (s.id.clone(), t.name.clone()))
        .unwrap_or_default();
    log(
        d,
        a,
        "delay.delete",
        format!("Deleted delay note for {name}, {test_name}"),
        &[
            ("staff_id", &n.staff_id),
            ("test_id", &n.test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

pub fn set_skipped(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    test_id: &str,
    stage_index: u32,
    skipped: bool,
) -> Res {
    let name = staff_name(d, staff_id)?;
    let (section_id, test_name) = test_info(d, test_id)?;
    let label = stage_label(d.rule_for_test(test_id), stage_index);
    d.skipped.retain(|s| {
        !(s.staff_id == staff_id && s.test_id == test_id && s.stage_index == stage_index)
    });
    if skipped {
        d.skipped.push(Skipped {
            staff_id: staff_id.into(),
            test_id: test_id.into(),
            stage_index,
            by: a.user.clone(),
            at: a.now,
        });
    }
    let (action, verb) = if skipped {
        ("stage.skip", "Skipped")
    } else {
        ("stage.unskip", "Restored")
    };
    log(
        d,
        a,
        action,
        format!("{verb} {test_name} {label} for {name}"),
        &[
            ("staff_id", staff_id),
            ("test_id", test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

pub fn set_due_override(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    test_id: &str,
    due_on: Option<NaiveDate>,
) -> Res {
    let name = staff_name(d, staff_id)?;
    let (section_id, test_name) = test_info(d, test_id)?;
    d.due_overrides
        .retain(|o| !(o.staff_id == staff_id && o.test_id == test_id));
    let detail = match due_on {
        Some(due) => {
            d.due_overrides.push(DueOverride {
                staff_id: staff_id.into(),
                test_id: test_id.into(),
                due_on: due,
                source: OverrideSource::Manual,
                by: a.user.clone(),
                at: a.now,
            });
            format!("Set {test_name} due date for {name} to {}", fmt_date(due))
        }
        None => format!("Removed {test_name} due date override for {name}"),
    };
    log(
        d,
        a,
        "due.override",
        detail,
        &[
            ("staff_id", staff_id),
            ("test_id", test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

// ---------------------------------------------------------------------- staff

pub fn add_staff(
    d: &mut Data,
    a: &Actor,
    name: &str,
    title: &str,
    joined: Option<NaiveDate>,
) -> Res<Id> {
    let name = non_empty(name)?;
    let id = new_id("p");
    d.staff.push(Staff {
        id: id.clone(),
        name: name.clone(),
        title: title.trim().into(),
        joined,
        status: StaffStatus::Active,
        assignments: BTreeMap::new(),
    });
    log(
        d,
        a,
        "staff.add",
        format!("Added {name} to personnel"),
        &[("staff_id", &id)],
    );
    Ok(id)
}

pub fn update_staff(
    d: &mut Data,
    a: &Actor,
    id: &str,
    name: &str,
    title: &str,
    joined: Option<NaiveDate>,
) -> Res {
    let name = non_empty(name)?;
    let s = d.staff_mut(id).ok_or(OpError::NotFound("staff"))?;
    let old = std::mem::replace(&mut s.name, name.clone());
    s.title = title.trim().into();
    s.joined = joined;
    let detail = if old == name {
        format!("Updated details for {name}")
    } else {
        format!("Updated details for {old} (now {name})")
    };
    log(d, a, "staff.update", detail, &[("staff_id", id)]);
    Ok(())
}

pub fn set_staff_status(d: &mut Data, a: &Actor, id: &str, status: StaffStatus) -> Res {
    let s = d.staff_mut(id).ok_or(OpError::NotFound("staff"))?;
    s.status = status;
    let name = s.name.clone();
    let word = match status {
        StaffStatus::Active => "active",
        StaffStatus::Hidden => "hidden",
        StaffStatus::Removed => "removed",
    };
    log(
        d,
        a,
        "staff.status",
        format!("Set {name} to {word}"),
        &[("staff_id", id)],
    );
    Ok(())
}

pub fn delete_staff_permanently(d: &mut Data, a: &Actor, id: &str, confirm_text: &str) -> Res {
    let s = d.staff(id).ok_or(OpError::NotFound("staff"))?;
    if s.status != StaffStatus::Removed {
        return Err(OpError::NotRemoved);
    }
    if confirm_text.trim() != s.name {
        return Err(OpError::ConfirmMismatch);
    }
    let name = s.name.clone();
    d.staff.retain(|s| s.id != id);
    d.records.retain(|r| r.staff_id != id);
    d.skipped.retain(|r| r.staff_id != id);
    d.due_overrides.retain(|r| r.staff_id != id);
    d.delay_notes.retain(|r| r.staff_id != id);
    d.notes.retain(|r| r.staff_id != id);
    for s in &mut d.sections {
        s.staff_order.retain(|x| x != id);
    }
    log(
        d,
        a,
        "staff.delete",
        format!("Permanently deleted {name} and all their records"),
        &[("staff_id", id)],
    );
    Ok(())
}

/// Merges `merge_id` into `keep_id` (two records of the same person) and
/// deletes `merge_id`. The kept person gets the union of both people's
/// assignments, completions, skips, due dates, delay notes and notes; where
/// both have one for the same test, the kept person's wins (an assignment
/// beats N/A). Completions that are exact duplicates (same test, stage and
/// date) are kept once. Title and join date are filled in from the merged
/// person only when the kept one has none. History entries that named the
/// merged person now point at the kept one.
pub fn merge_staff(d: &mut Data, a: &Actor, keep_id: &str, merge_id: &str) -> Res<usize> {
    if keep_id == merge_id {
        return Err(OpError::SamePerson);
    }
    let keep_name = staff_name(d, keep_id)?;
    let gone = d
        .staff(merge_id)
        .cloned()
        .ok_or(OpError::NotFound("staff"))?;
    let keep = d.staff_mut(keep_id).expect("checked above");
    for (test, asg) in gone.assignments {
        match keep.assignments.get_mut(&test) {
            None => {
                keep.assignments.insert(test, asg);
            }
            Some(k) => {
                if k.state == AssignState::Na && asg.state == AssignState::Assigned {
                    *k = asg;
                } else if k.state == asg.state && asg.since < k.since {
                    k.since = asg.since;
                }
            }
        }
    }
    if keep.title.trim().is_empty() {
        keep.title = gone.title.clone();
    }
    keep.joined = match (keep.joined, gone.joined) {
        (Some(k), Some(g)) => Some(k.min(g)),
        (k, g) => k.or(g),
    };
    if gone.status == StaffStatus::Active {
        keep.status = StaffStatus::Active;
    }

    // Completions: move over, dropping exact duplicates of the kept person's.
    let existing: std::collections::HashSet<(Id, u32, NaiveDate)> = d
        .records
        .iter()
        .filter(|r| r.staff_id == keep_id)
        .map(|r| (r.test_id.clone(), r.stage_index, r.completed_on))
        .collect();
    let mut moved = 0;
    d.records.retain_mut(|r| {
        if r.staff_id != merge_id {
            return true;
        }
        if existing.contains(&(r.test_id.clone(), r.stage_index, r.completed_on)) {
            return false;
        }
        r.staff_id = keep_id.to_owned();
        moved += 1;
        true
    });

    let kept_skips: std::collections::HashSet<(Id, u32)> = d
        .skipped
        .iter()
        .filter(|x| x.staff_id == keep_id)
        .map(|x| (x.test_id.clone(), x.stage_index))
        .collect();
    d.skipped.retain_mut(|x| {
        if x.staff_id != merge_id {
            return true;
        }
        if kept_skips.contains(&(x.test_id.clone(), x.stage_index)) {
            return false;
        }
        x.staff_id = keep_id.to_owned();
        true
    });

    let kept_overrides: std::collections::HashSet<Id> = d
        .due_overrides
        .iter()
        .filter(|x| x.staff_id == keep_id)
        .map(|x| x.test_id.clone())
        .collect();
    d.due_overrides.retain_mut(|x| {
        if x.staff_id != merge_id {
            return true;
        }
        if kept_overrides.contains(&x.test_id) {
            return false;
        }
        x.staff_id = keep_id.to_owned();
        true
    });

    for n in d.delay_notes.iter_mut().filter(|n| n.staff_id == merge_id) {
        n.staff_id = keep_id.to_owned();
    }
    for n in d.notes.iter_mut().filter(|n| n.staff_id == merge_id) {
        n.staff_id = keep_id.to_owned();
    }
    for e in &mut d.audit {
        if let Some(v) = e.refs.get_mut("staff_id").filter(|v| *v == merge_id) {
            *v = keep_id.to_owned();
        }
    }
    d.staff.retain(|s| s.id != merge_id);
    // In each section's order the kept person takes the merged one's place,
    // unless they already have their own.
    for s in &mut d.sections {
        if s.staff_order.iter().any(|x| x == keep_id) {
            s.staff_order.retain(|x| x != merge_id);
        } else if let Some(x) = s.staff_order.iter_mut().find(|x| *x == merge_id) {
            *x = keep_id.to_owned();
        }
    }
    let records = if moved == 1 {
        "1 completion".to_owned()
    } else {
        format!("{moved} completions")
    };
    log(
        d,
        a,
        "staff.merge",
        format!("Merged {} into {keep_name} ({records} moved)", gone.name),
        &[("staff_id", keep_id)],
    );
    Ok(moved)
}

pub fn set_assignment(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    test_id: &str,
    state: Option<AssignState>,
) -> Res {
    let name = staff_name(d, staff_id)?;
    let (section_id, test_name) = test_info(d, test_id)?;
    let today = a.today();
    let s = d.staff_mut(staff_id).expect("checked above");
    let detail = match state {
        Some(st) => {
            if s.assignments.get(test_id).map(|x| x.state) != Some(st) {
                s.assignments.insert(
                    test_id.into(),
                    Assignment {
                        state: st,
                        since: today,
                    },
                );
            }
            match st {
                AssignState::Assigned => format!("Assigned {test_name} to {name}"),
                AssignState::Na => format!("Marked {test_name} not applicable for {name}"),
            }
        }
        None => {
            s.assignments.remove(test_id);
            format!("Removed {test_name} assignment for {name}")
        }
    };
    log(
        d,
        a,
        "assignment.set",
        detail,
        &[
            ("staff_id", staff_id),
            ("test_id", test_id),
            ("section_id", &section_id),
        ],
    );
    Ok(())
}

pub fn assign_section(
    d: &mut Data,
    a: &Actor,
    staff_id: &str,
    section_id: &str,
    assigned: bool,
) -> Res {
    let name = staff_name(d, staff_id)?;
    let sec = d.section(section_id).ok_or(OpError::NotFound("section"))?;
    let sec_name = sec.name.clone();
    let test_ids: Vec<Id> = sec
        .tests
        .iter()
        .filter(|t| !t.retired)
        .map(|t| t.id.clone())
        .collect();
    let today = a.today();
    let s = d.staff_mut(staff_id).expect("checked above");
    for t in &test_ids {
        if assigned {
            if s.assignments.get(t).map(|x| x.state) != Some(AssignState::Assigned) {
                s.assignments.insert(
                    t.clone(),
                    Assignment {
                        state: AssignState::Assigned,
                        since: today,
                    },
                );
            }
        } else {
            s.assignments.remove(t);
        }
    }
    let detail = if assigned {
        format!("Assigned all {sec_name} tests to {name}")
    } else {
        format!("Removed all {sec_name} test assignments for {name}")
    };
    log(
        d,
        a,
        "assignment.set",
        detail,
        &[("staff_id", staff_id), ("section_id", section_id)],
    );
    Ok(())
}

// ------------------------------------------------------------------- sections

pub fn add_section(d: &mut Data, a: &Actor, name: &str) -> Res<Id> {
    let name = non_empty(name)?;
    let id = new_id("s");
    let order = d.sections.iter().map(|s| s.order + 1).max().unwrap_or(0);
    d.sections.push(Section {
        id: id.clone(),
        name: name.clone(),
        order,
        retired: false,
        tests: vec![],
        color: None,
        staff_order: vec![],
    });
    log(
        d,
        a,
        "section.add",
        format!("Added section {name}"),
        &[("section_id", &id)],
    );
    Ok(id)
}

pub fn rename_section(d: &mut Data, a: &Actor, id: &str, name: &str) -> Res {
    let name = non_empty(name)?;
    let s = d
        .sections
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or(OpError::NotFound("section"))?;
    let old = std::mem::replace(&mut s.name, name.clone());
    log(
        d,
        a,
        "section.rename",
        format!("Renamed section {old} to {name}"),
        &[("section_id", id)],
    );
    Ok(())
}

/// Index after moving `idx` by `delta` within `len` items, clamped to the ends.
fn clamped(idx: usize, delta: i32, len: usize) -> usize {
    (idx as i64 + delta as i64).clamp(0, len as i64 - 1) as usize
}

pub fn move_section(d: &mut Data, a: &Actor, id: &str, delta: i32) -> Res {
    if d.section(id).is_none() {
        return Err(OpError::NotFound("section"));
    }
    let mut ids: Vec<Id> = d.sections_sorted().iter().map(|s| s.id.clone()).collect();
    let from = ids.iter().position(|x| x == id).expect("checked above");
    let to = clamped(from, delta, ids.len());
    if to == from {
        return Ok(());
    }
    let moved = ids.remove(from);
    ids.insert(to, moved);
    for (i, sid) in ids.iter().enumerate() {
        d.sections
            .iter_mut()
            .find(|s| &s.id == sid)
            .expect("id from list")
            .order = i as u32;
    }
    let name = d.section(id).expect("checked above").name.clone();
    log(
        d,
        a,
        "section.move",
        format!("Moved section {name} to position {}", to + 1),
        &[("section_id", id)],
    );
    Ok(())
}

/// Sets (`Some("#RRGGBB")`) or clears (`None`, back to the default) a
/// section's colour.
pub fn set_section_color(d: &mut Data, a: &Actor, id: &str, color: Option<u32>) -> Res {
    let s = d
        .sections
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or(OpError::NotFound("section"))?;
    s.color = color.map(fmt_hex_color);
    let name = s.name.clone();
    let detail = match color {
        Some(c) => format!("Set the colour of section {name} to {}", fmt_hex_color(c)),
        None => format!("Reset the colour of section {name}"),
    };
    log(d, a, "section.color", detail, &[("section_id", id)]);
    Ok(())
}

pub fn set_section_retired(d: &mut Data, a: &Actor, id: &str, retired: bool) -> Res {
    let s = d
        .sections
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or(OpError::NotFound("section"))?;
    s.retired = retired;
    let name = s.name.clone();
    let verb = if retired { "Retired" } else { "Restored" };
    log(
        d,
        a,
        "section.retire",
        format!("{verb} section {name}"),
        &[("section_id", id)],
    );
    Ok(())
}

pub fn delete_section(d: &mut Data, a: &Actor, id: &str) -> Res {
    let sec = d.section(id).ok_or(OpError::NotFound("section"))?;
    if d.records
        .iter()
        .any(|r| sec.tests.iter().any(|t| t.id == r.test_id))
    {
        return Err(OpError::HasRecords);
    }
    let name = sec.name.clone();
    let test_ids: Vec<Id> = sec.tests.iter().map(|t| t.id.clone()).collect();
    for t in &test_ids {
        purge_test_refs(d, t);
    }
    d.sections.retain(|s| s.id != id);
    // Keep order values contiguous.
    let ids: Vec<Id> = d.sections_sorted().iter().map(|s| s.id.clone()).collect();
    for (i, sid) in ids.iter().enumerate() {
        d.sections
            .iter_mut()
            .find(|s| &s.id == sid)
            .expect("id from list")
            .order = i as u32;
    }
    log(
        d,
        a,
        "section.delete",
        format!("Deleted section {name}"),
        &[("section_id", id)],
    );
    Ok(())
}

// ---------------------------------------------------------------------- tests

/// Drops every non-record reference to a test (records are guarded by callers).
fn purge_test_refs(d: &mut Data, test_id: &str) {
    for s in &mut d.staff {
        s.assignments.remove(test_id);
    }
    d.skipped.retain(|x| x.test_id != test_id);
    d.due_overrides.retain(|x| x.test_id != test_id);
    d.delay_notes.retain(|x| x.test_id != test_id);
    d.notes.retain(|x| x.test_id.as_deref() != Some(test_id));
}

fn test_mut<'a>(d: &'a mut Data, id: &str) -> Res<&'a mut TestSystem> {
    d.sections
        .iter_mut()
        .find_map(|s| s.tests.iter_mut().find(|t| t.id == id))
        .ok_or(OpError::NotFound("test"))
}

pub fn add_test(d: &mut Data, a: &Actor, section_id: &str, name: &str) -> Res<Id> {
    let name = non_empty(name)?;
    let id = new_id("t");
    let sec = d
        .sections
        .iter_mut()
        .find(|s| s.id == section_id)
        .ok_or(OpError::NotFound("section"))?;
    let order = sec.tests.iter().map(|t| t.order + 1).max().unwrap_or(0);
    sec.tests.push(TestSystem {
        id: id.clone(),
        name: name.clone(),
        order,
        retired: false,
        rule_id: None,
    });
    let sec_name = sec.name.clone();
    log(
        d,
        a,
        "test.add",
        format!("Added test {name} to {sec_name}"),
        &[("test_id", &id), ("section_id", section_id)],
    );
    Ok(id)
}

pub fn rename_test(d: &mut Data, a: &Actor, test_id: &str, name: &str) -> Res {
    let name = non_empty(name)?;
    let (section_id, _) = test_info(d, test_id)?;
    let old = std::mem::replace(&mut test_mut(d, test_id)?.name, name.clone());
    log(
        d,
        a,
        "test.rename",
        format!("Renamed test {old} to {name}"),
        &[("test_id", test_id), ("section_id", &section_id)],
    );
    Ok(())
}

pub fn move_test(d: &mut Data, a: &Actor, test_id: &str, delta: i32) -> Res {
    let (section_id, name) = test_info(d, test_id)?;
    let sec = d
        .sections
        .iter_mut()
        .find(|s| s.id == section_id)
        .expect("test's section");
    let mut ids: Vec<Id> = {
        let mut v: Vec<&TestSystem> = sec.tests.iter().collect();
        v.sort_by_key(|t| t.order);
        v.iter().map(|t| t.id.clone()).collect()
    };
    let from = ids
        .iter()
        .position(|x| x == test_id)
        .expect("test in section");
    let to = clamped(from, delta, ids.len());
    if to == from {
        return Ok(());
    }
    let moved = ids.remove(from);
    ids.insert(to, moved);
    for (i, tid) in ids.iter().enumerate() {
        sec.tests
            .iter_mut()
            .find(|t| &t.id == tid)
            .expect("id from list")
            .order = i as u32;
    }
    log(
        d,
        a,
        "test.move",
        format!("Moved test {name} to position {}", to + 1),
        &[("test_id", test_id), ("section_id", &section_id)],
    );
    Ok(())
}

pub fn set_test_retired(d: &mut Data, a: &Actor, test_id: &str, retired: bool) -> Res {
    let (section_id, name) = test_info(d, test_id)?;
    test_mut(d, test_id)?.retired = retired;
    let verb = if retired { "Retired" } else { "Restored" };
    log(
        d,
        a,
        "test.retire",
        format!("{verb} test {name}"),
        &[("test_id", test_id), ("section_id", &section_id)],
    );
    Ok(())
}

pub fn delete_test(d: &mut Data, a: &Actor, test_id: &str) -> Res {
    let (section_id, name) = test_info(d, test_id)?;
    if d.records.iter().any(|r| r.test_id == test_id) {
        return Err(OpError::HasRecords);
    }
    purge_test_refs(d, test_id);
    for s in &mut d.sections {
        s.tests.retain(|t| t.id != test_id);
    }
    log(
        d,
        a,
        "test.delete",
        format!("Deleted test {name}"),
        &[("test_id", test_id), ("section_id", &section_id)],
    );
    Ok(())
}

pub fn set_test_rule(d: &mut Data, a: &Actor, test_id: &str, rule_id: Option<Id>) -> Res {
    let (section_id, name) = test_info(d, test_id)?;
    let rule_name = match &rule_id {
        Some(r) => d
            .rules
            .iter()
            .find(|x| &x.id == r)
            .map(|x| x.name.clone())
            .ok_or(OpError::NotFound("rule"))?,
        None => "the default rule".into(),
    };
    test_mut(d, test_id)?.rule_id = rule_id;
    log(
        d,
        a,
        "test.rule",
        format!("Set {name} to use {rule_name}"),
        &[("test_id", test_id), ("section_id", &section_id)],
    );
    Ok(())
}

// ---------------------------------------------------------- rules and settings

pub fn add_rule(d: &mut Data, a: &Actor, name: &str, stages: Vec<Stage>) -> Res<Id> {
    let name = non_empty(name)?;
    let id = new_id("r");
    d.rules.push(Rule {
        id: id.clone(),
        name: name.clone(),
        stages,
    });
    log(d, a, "rule.add", format!("Added schedule rule {name}"), &[]);
    Ok(id)
}

pub fn update_rule(d: &mut Data, a: &Actor, id: &str, name: &str, stages: Vec<Stage>) -> Res {
    let name = non_empty(name)?;
    let r = d
        .rules
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or(OpError::NotFound("rule"))?;
    r.name = name.clone();
    r.stages = stages;
    log(
        d,
        a,
        "rule.update",
        format!("Updated schedule rule {name}"),
        &[],
    );
    Ok(())
}

pub fn delete_rule(d: &mut Data, a: &Actor, id: &str) -> Res {
    let r = d
        .rules
        .iter()
        .find(|r| r.id == id)
        .ok_or(OpError::NotFound("rule"))?;
    if id == d.default_rule_id {
        return Err(OpError::IsDefaultRule);
    }
    let name = r.name.clone();
    d.rules.retain(|r| r.id != id);
    for t in d.sections.iter_mut().flat_map(|s| s.tests.iter_mut()) {
        if t.rule_id.as_deref() == Some(id) {
            t.rule_id = None;
        }
    }
    log(
        d,
        a,
        "rule.delete",
        format!("Deleted schedule rule {name}"),
        &[],
    );
    Ok(())
}

pub fn update_settings(d: &mut Data, a: &Actor, s: Settings) -> Res {
    fn opt(v: Option<i64>) -> String {
        v.map(|x| x.to_string()).unwrap_or_else(|| "off".into())
    }
    let o = d.settings.clone();
    let mut changes = vec![];
    if o.due_soon_days != s.due_soon_days {
        changes.push(format!(
            "due soon days {} to {}",
            o.due_soon_days, s.due_soon_days
        ));
    }
    if o.variance_tolerance_days != s.variance_tolerance_days {
        changes.push(format!(
            "variance tolerance days {} to {}",
            o.variance_tolerance_days, s.variance_tolerance_days
        ));
    }
    if o.inactivity_minutes != s.inactivity_minutes {
        changes.push(format!(
            "inactivity minutes {} to {}",
            o.inactivity_minutes, s.inactivity_minutes
        ));
    }
    if o.inactivity_grace_minutes != s.inactivity_grace_minutes {
        changes.push(format!(
            "inactivity prompt minutes {} to {}",
            o.inactivity_grace_minutes, s.inactivity_grace_minutes
        ));
    }
    if o.initial_due_days != s.initial_due_days {
        changes.push(format!(
            "initial due days {} to {}",
            opt(o.initial_due_days),
            opt(s.initial_due_days)
        ));
    }
    if o.archive_after_days != s.archive_after_days {
        changes.push(format!(
            "archive after days {} to {}",
            opt(o.archive_after_days),
            opt(s.archive_after_days)
        ));
    }
    let detail = if changes.is_empty() {
        "Saved settings (no changes)".to_string()
    } else {
        format!("Changed settings: {}", changes.join(", "))
    };
    d.settings = s;
    log(d, a, "settings.update", detail, &[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn actor() -> Actor {
        Actor {
            user: "jtan".into(),
            now: chrono::FixedOffset::east_opt(8 * 3600)
                .unwrap()
                .with_ymd_and_hms(2026, 11, 14, 9, 30, 0)
                .unwrap(),
        }
    }

    fn day(y: i32, m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }

    /// One section, two tests, one staff member. Returns (data, section, test1, test2, staff).
    fn fixture() -> (Data, Id, Id, Id, Id) {
        let a = actor();
        let mut d = Data::new_empty("jtan", a.now);
        d.sections.truncate(1);
        let sec = d.sections[0].id.clone();
        let t1 = add_test(&mut d, &a, &sec, "Cell-DYN Ruby").unwrap();
        let t2 = add_test(&mut d, &a, &sec, "Sysmex").unwrap();
        let st = add_staff(&mut d, &a, "Agnes", "MLT", None).unwrap();
        d.audit.clear();
        (d, sec, t1, t2, st)
    }

    #[test]
    fn add_completion_records_user_and_audits() {
        let (mut d, sec, t1, _, st) = fixture();
        let a = actor();
        let id = add_completion(&mut d, &a, &st, &t1, 0, day(2026, 10, 12), "ok").unwrap();
        let r = d.records.iter().find(|r| r.id == id).unwrap();
        assert_eq!((r.by.as_str(), r.stage_label.as_str()), ("jtan", "Initial"));
        assert_eq!(d.audit.len(), 1);
        let e = &d.audit[0];
        assert_eq!((e.action.as_str(), e.by.as_str()), ("record.add", "jtan"));
        assert_eq!(e.refs["staff_id"], st);
        assert_eq!(e.refs["test_id"], t1);
        assert_eq!(e.refs["section_id"], sec);
        assert_eq!(
            e.detail,
            "Marked Cell-DYN Ruby Initial complete for Agnes on 12 Oct 2026"
        );
    }

    #[test]
    fn merge_staff_keeps_both_peoples_competencies() {
        let (mut d, _, t1, t2, st) = fixture();
        let a = actor();
        let dup = add_staff(&mut d, &a, "Agnes Tan", "", Some(day(2020, 1, 1))).unwrap();
        set_assignment(&mut d, &a, &st, &t1, Some(AssignState::Na)).unwrap();
        set_assignment(&mut d, &a, &dup, &t1, Some(AssignState::Assigned)).unwrap();
        set_assignment(&mut d, &a, &dup, &t2, Some(AssignState::Assigned)).unwrap();
        add_completion(&mut d, &a, &st, &t1, 0, day(2026, 1, 5), "").unwrap();
        // The same completion twice, and one only the duplicate has.
        add_completion(&mut d, &a, &dup, &t1, 0, day(2026, 1, 5), "").unwrap();
        add_completion(&mut d, &a, &dup, &t2, 0, day(2026, 2, 5), "").unwrap();
        set_due_override(&mut d, &a, &dup, &t2, Some(day(2027, 3, 1))).unwrap();

        let moved = merge_staff(&mut d, &a, &st, &dup).unwrap();
        assert_eq!(moved, 1);
        assert!(d.staff(&dup).is_none());
        let s = d.staff(&st).unwrap();
        assert_eq!(s.title, "MLT", "the kept title wins");
        assert_eq!(
            s.joined,
            Some(day(2020, 1, 1)),
            "filled from the merged one"
        );
        assert_eq!(s.assignments[&t1].state, AssignState::Assigned);
        assert_eq!(s.assignments[&t2].state, AssignState::Assigned);
        assert_eq!(d.records.iter().filter(|r| r.staff_id == st).count(), 2);
        assert!(d.records.iter().all(|r| r.staff_id != dup));
        assert_eq!(d.due_overrides[0].staff_id, st);
        assert!(d
            .audit
            .iter()
            .all(|e| e.refs.get("staff_id").map(String::as_str) != Some(dup.as_str())));
        assert_eq!(
            d.audit.last().unwrap().detail,
            "Merged Agnes Tan into Agnes (1 completion moved)"
        );
        assert_eq!(merge_staff(&mut d, &a, &st, &st), Err(OpError::SamePerson));
    }

    #[test]
    fn section_color_set_and_reset() {
        let (mut d, sec, ..) = fixture();
        let a = actor();
        set_section_color(&mut d, &a, &sec, Some(0x112233)).unwrap();
        assert_eq!(d.sections[0].color.as_deref(), Some("#112233"));
        set_section_color(&mut d, &a, &sec, None).unwrap();
        assert_eq!(d.sections[0].color, None);
        assert_eq!(d.audit.len(), 2);
    }

    #[test]
    fn future_completion_rejected() {
        let (mut d, _, t1, _, st) = fixture();
        let r = add_completion(&mut d, &actor(), &st, &t1, 0, day(2026, 11, 15), "");
        assert_eq!(r, Err(OpError::FutureDate));
        assert!(d.records.is_empty() && d.audit.is_empty());
        // Today itself is fine.
        add_completion(&mut d, &actor(), &st, &t1, 0, day(2026, 11, 14), "").unwrap();
    }

    #[test]
    fn completion_clears_override() {
        let (mut d, _, t1, t2, st) = fixture();
        let a = actor();
        set_due_override(&mut d, &a, &st, &t1, Some(day(2027, 1, 1))).unwrap();
        set_due_override(&mut d, &a, &st, &t2, Some(day(2027, 2, 1))).unwrap();
        assert_eq!(d.due_overrides[0].source, OverrideSource::Manual);
        add_completion(&mut d, &a, &st, &t1, 0, day(2026, 11, 1), "").unwrap();
        assert_eq!(d.due_overrides.len(), 1);
        assert_eq!(d.due_overrides[0].test_id, t2);
        set_due_override(&mut d, &a, &st, &t2, None).unwrap();
        assert!(d.due_overrides.is_empty());
    }

    #[test]
    fn edit_and_delete_record() {
        let (mut d, _, t1, _, st) = fixture();
        let a = actor();
        let id = add_completion(&mut d, &a, &st, &t1, 0, day(2026, 10, 1), "").unwrap();
        assert_eq!(
            edit_record(&mut d, &a, &id, day(2026, 12, 1), 1, ""),
            Err(OpError::FutureDate)
        );
        edit_record(&mut d, &a, &id, day(2026, 10, 2), 1, "fixed").unwrap();
        let r = &d.records[0];
        assert_eq!(
            (r.stage_index, r.stage_label.as_str(), r.comment.as_str()),
            (1, "Semi-annual", "fixed")
        );
        delete_record(&mut d, &a, &id).unwrap();
        assert!(d.records.is_empty());
        assert_eq!(
            d.audit
                .iter()
                .map(|e| e.action.as_str())
                .collect::<Vec<_>>(),
            ["record.add", "record.edit", "record.delete"]
        );
        assert_eq!(
            delete_record(&mut d, &a, &id),
            Err(OpError::NotFound("record"))
        );
        assert_eq!(d.audit.len(), 3);
    }

    #[test]
    fn delete_staff_requires_removed_and_name() {
        let (mut d, _, t1, _, st) = fixture();
        let a = actor();
        add_completion(&mut d, &a, &st, &t1, 0, day(2026, 10, 1), "").unwrap();
        add_delay_note(&mut d, &a, &st, &t1, 1, "sick", None).unwrap();
        set_skipped(&mut d, &a, &st, &t1, 1, true).unwrap();
        let n = d.audit.len();
        assert_eq!(
            delete_staff_permanently(&mut d, &a, &st, "Agnes"),
            Err(OpError::NotRemoved)
        );
        set_staff_status(&mut d, &a, &st, StaffStatus::Removed).unwrap();
        let n = n + 1;
        assert_eq!(
            delete_staff_permanently(&mut d, &a, &st, "Bob"),
            Err(OpError::ConfirmMismatch)
        );
        assert_eq!(d.audit.len(), n);
        delete_staff_permanently(&mut d, &a, &st, "  Agnes ").unwrap();
        assert!(
            d.staff.is_empty()
                && d.records.is_empty()
                && d.delay_notes.is_empty()
                && d.skipped.is_empty()
        );
        assert_eq!(d.audit.len(), n + 1);
        assert_eq!(d.audit.last().unwrap().action, "staff.delete");
    }

    #[test]
    fn delete_test_blocked_with_records() {
        let (mut d, _, t1, t2, st) = fixture();
        let a = actor();
        add_completion(&mut d, &a, &st, &t1, 0, day(2026, 10, 1), "").unwrap();
        let n = d.audit.len();
        assert_eq!(delete_test(&mut d, &a, &t1), Err(OpError::HasRecords));
        assert_eq!(d.audit.len(), n);
        assert_eq!(d.sections[0].tests.len(), 2);
        set_assignment(&mut d, &a, &st, &t2, Some(AssignState::Assigned)).unwrap();
        delete_test(&mut d, &a, &t2).unwrap();
        assert_eq!(d.sections[0].tests.len(), 1);
        assert!(d.staff[0].assignments.is_empty());
    }

    #[test]
    fn delete_section_blocked_with_records_else_removes_tests() {
        let (mut d, sec, t1, _, st) = fixture();
        let a = actor();
        add_completion(&mut d, &a, &st, &t1, 0, day(2026, 10, 1), "").unwrap();
        assert_eq!(delete_section(&mut d, &a, &sec), Err(OpError::HasRecords));
        let rid = d.records[0].id.clone();
        delete_record(&mut d, &a, &rid).unwrap();
        delete_section(&mut d, &a, &sec).unwrap();
        assert!(d.sections.is_empty());
    }

    #[test]
    fn move_section_reorders() {
        let a = actor();
        let mut d = Data::new_empty("jtan", a.now);
        d.sections.clear();
        let ids: Vec<Id> = ["A", "B", "C"]
            .iter()
            .map(|n| add_section(&mut d, &a, n).unwrap())
            .collect();
        move_section(&mut d, &a, &ids[1], -1).unwrap();
        let sorted = d.sections_sorted();
        assert_eq!(
            sorted.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["B", "A", "C"]
        );
        assert_eq!(
            sorted.iter().map(|s| s.order).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        let n = d.audit.len();
        move_section(&mut d, &a, &ids[1], -5).unwrap(); // already first: clamped no-op
        assert_eq!(d.audit.len(), n);
        move_section(&mut d, &a, &ids[1], 9).unwrap(); // to the end
        assert_eq!(d.sections_sorted().last().unwrap().name, "B");
        assert_eq!(d.audit.len(), n + 1);
    }

    #[test]
    fn move_test_within_section() {
        let (mut d, _, t1, t2, _) = fixture();
        let a = actor();
        move_test(&mut d, &a, &t2, -1).unwrap();
        let tests = &d.sections[0].tests;
        assert_eq!(tests.iter().find(|t| t.id == t2).unwrap().order, 0);
        assert_eq!(tests.iter().find(|t| t.id == t1).unwrap().order, 1);
        assert_eq!(d.audit.last().unwrap().action, "test.move");
    }

    #[test]
    fn delete_rule_resets_tests() {
        let (mut d, _, t1, t2, _) = fixture();
        let a = actor();
        let r = add_rule(&mut d, &a, "Quarterly", vec![]).unwrap();
        set_test_rule(&mut d, &a, &t1, Some(r.clone())).unwrap();
        set_test_rule(&mut d, &a, &t2, Some(r.clone())).unwrap();
        assert_eq!(
            set_test_rule(&mut d, &a, &t2, Some("r-nope".into())),
            Err(OpError::NotFound("rule"))
        );
        delete_rule(&mut d, &a, &r).unwrap();
        assert!(d.sections[0].tests.iter().all(|t| t.rule_id.is_none()));
        assert_eq!(d.rules.len(), 1);
        let n = d.audit.len();
        let def = d.default_rule_id.clone();
        assert_eq!(delete_rule(&mut d, &a, &def), Err(OpError::IsDefaultRule));
        assert_eq!(d.audit.len(), n);
    }

    #[test]
    fn assign_section_assigns_all_live_tests() {
        let (mut d, sec, t1, t2, st) = fixture();
        let a = actor();
        set_test_retired(&mut d, &a, &t2, true).unwrap();
        assign_section(&mut d, &a, &st, &sec, true).unwrap();
        let asg = &d.staff[0].assignments;
        assert_eq!(asg.len(), 1);
        assert_eq!(
            asg[&t1],
            Assignment {
                state: AssignState::Assigned,
                since: day(2026, 11, 14)
            }
        );
        assign_section(&mut d, &a, &st, &sec, false).unwrap();
        assert!(d.staff[0].assignments.is_empty());
    }

    #[test]
    fn confirm_records_clears_flag() {
        let (mut d, _, t1, _, st) = fixture();
        let a = actor();
        let r1 = add_completion(&mut d, &a, &st, &t1, 0, day(2026, 1, 1), "").unwrap();
        let r2 = add_completion(&mut d, &a, &st, &t1, 1, day(2026, 7, 1), "").unwrap();
        for r in &mut d.records {
            r.unconfirmed = true;
        }
        let n = d.audit.len();
        assert_eq!(
            confirm_records(&mut d, &a, &[r1.clone(), r2.clone()]).unwrap(),
            2
        );
        assert!(d.records.iter().all(|r| !r.unconfirmed));
        assert_eq!(d.audit.len(), n + 1);
        assert_eq!(d.audit.last().unwrap().action, "record.confirm");
    }

    #[test]
    fn empty_name_rejected_and_untouched() {
        let (mut d, sec, _, _, _) = fixture();
        let a = actor();
        assert_eq!(
            add_staff(&mut d, &a, "  ", "", None),
            Err(OpError::EmptyName)
        );
        assert_eq!(add_section(&mut d, &a, ""), Err(OpError::EmptyName));
        assert_eq!(add_test(&mut d, &a, &sec, " "), Err(OpError::EmptyName));
        assert!(d.audit.is_empty());
    }

    #[test]
    fn skip_notes_and_settings() {
        let (mut d, _, t1, _, st) = fixture();
        let a = actor();
        set_skipped(&mut d, &a, &st, &t1, 1, true).unwrap();
        assert_eq!(d.skipped.len(), 1);
        set_skipped(&mut d, &a, &st, &t1, 1, false).unwrap();
        assert!(d.skipped.is_empty());
        let nid =
            add_delay_note(&mut d, &a, &st, &t1, 1, "waiting", Some(day(2027, 1, 5))).unwrap();
        delete_delay_note(&mut d, &a, &nid).unwrap();
        let actions: Vec<_> = d.audit.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(
            actions,
            ["stage.skip", "stage.unskip", "delay.add", "delay.delete"]
        );
        let s = Settings {
            due_soon_days: 45,
            ..Settings::default()
        };
        update_settings(&mut d, &a, s).unwrap();
        assert!(d
            .audit
            .last()
            .unwrap()
            .detail
            .contains("due soon days 30 to 45"));
        assert_eq!(d.settings.due_soon_days, 45);
    }
}
