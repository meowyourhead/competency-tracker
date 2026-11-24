//! Reverting the latest change. Every change made through the session leaves
//! an [`UndoEntry`]: what to put back to get exactly the data before it. They
//! are kept in the data itself, so a change can be reverted later, by anyone,
//! latest first. The audit log is never shortened: a reverted change stays in
//! it, marked, and the revert is recorded as a change of its own.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{AuditEntry, Data, Ts};

/// How many changes can be reverted, and roughly how much room (in bytes)
/// they may take in the data file. The latest one is always kept.
const MAX_ENTRIES: usize = 100;
const MAX_BYTES: usize = 2_000_000;

/// What to put back into one field of [`Data`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldPatch {
    Whole(Value),
    /// Remove the items at `del`, then insert each of `ins` at its index.
    List {
        del: Vec<usize>,
        ins: Vec<(usize, Value)>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub at: Ts,
    pub by: String,
    /// What the change did, one line per audit entry it made.
    pub details: Vec<String>,
    /// Its audit entries: `audit[from..to]`.
    from: usize,
    to: usize,
    /// Fingerprint of the data right after the change.
    after: String,
    /// Rough size in the data file.
    size: usize,
    patch: BTreeMap<String, FieldPatch>,
}

/// The fields a change can touch (the audit log is handled separately).
macro_rules! lists {
    ($m:ident) => {
        $m!(
            rules,
            sections,
            staff,
            records,
            skipped,
            due_overrides,
            delay_notes,
            notes,
            review_ignored
        )
    };
}

/// FNV-1a over the data's content, so a file edited outside the app is never
/// "reverted" into nonsense.
fn fingerprint(d: &Data) -> String {
    let content = serde_json::to_vec(&(
        (&d.settings, &d.rules, &d.default_rule_id),
        (&d.sections, &d.staff, &d.records, &d.skipped),
        (
            &d.due_overrides,
            &d.delay_notes,
            &d.notes,
            &d.review_ignored,
        ),
    ))
    .unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in content {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn value<T: Serialize>(x: &T) -> Value {
    serde_json::to_value(x).unwrap_or(Value::Null)
}

/// How to turn `new` back into `old`.
fn diff_list<T: Serialize + PartialEq>(old: &[T], new: &[T]) -> Option<FieldPatch> {
    let pre = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if pre == old.len() && pre == new.len() {
        return None;
    }
    let (o, n) = (&old[pre..], &new[pre..]);
    let suf = o
        .iter()
        .rev()
        .zip(n.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (o, n) = (&o[..o.len() - suf], &n[..n.len() - suf]);
    // Items in both, in the same order; the rest is removed or put back.
    let text = |x: &T| serde_json::to_string(x).unwrap_or_default();
    let mut at: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (i, x) in n.iter().enumerate() {
        at.entry(text(x)).or_default().push_back(i);
    }
    let mut kept = vec![false; n.len()];
    let mut next = 0;
    let mut ins = Vec::new();
    for (i, x) in o.iter().enumerate() {
        let found = at.get_mut(&text(x)).and_then(|q| {
            while q.front().is_some_and(|&j| j < next) {
                q.pop_front();
            }
            q.pop_front()
        });
        match found {
            Some(j) => {
                kept[j] = true;
                next = j + 1;
            }
            None => ins.push((pre + i, value(x))),
        }
    }
    let del = (0..n.len())
        .filter(|&j| !kept[j])
        .map(|j| pre + j)
        .collect();
    Some(FieldPatch::List { del, ins })
}

fn apply_list<T: DeserializeOwned>(list: &mut Vec<T>, p: FieldPatch) -> Result<(), String> {
    let bad = || "its stored data can't be read".to_owned();
    let FieldPatch::List { del, ins } = p else {
        return Err(bad());
    };
    for i in del.into_iter().rev() {
        if i >= list.len() {
            return Err(bad());
        }
        list.remove(i);
    }
    for (i, v) in ins {
        if i > list.len() {
            return Err(bad());
        }
        list.insert(i, serde_json::from_value(v).map_err(|_| bad())?);
    }
    Ok(())
}

/// Adds the undo step that takes `data` back to `before`, if anything changed.
pub fn record(before: &Data, data: &mut Data, by: &str, at: Ts) {
    let mut patch = BTreeMap::new();
    macro_rules! diff {
        ($($f:ident),*) => {$(
            if let Some(p) = diff_list(&before.$f, &data.$f) {
                patch.insert(stringify!($f).to_owned(), p);
            }
        )*};
    }
    lists!(diff);
    if before.settings != data.settings {
        patch.insert(
            "settings".into(),
            FieldPatch::Whole(value(&before.settings)),
        );
    }
    if before.default_rule_id != data.default_rule_id {
        patch.insert(
            "default_rule_id".into(),
            FieldPatch::Whole(value(&before.default_rule_id)),
        );
    }
    if patch.is_empty() {
        return;
    }
    // Entries already there can change too (a merge points them at the kept
    // person); new ones follow them.
    let from = before.audit.len().min(data.audit.len());
    if let Some(p) = diff_list(&before.audit, &data.audit[..from]) {
        patch.insert("audit".into(), p);
    }
    let mut details: Vec<String> = data.audit[from..]
        .iter()
        .map(|e| e.detail.clone())
        .collect();
    if details.is_empty() {
        details.push("A change".to_owned());
    }
    let size = serde_json::to_string(&patch).map_or(0, |s| s.len());
    let entry = UndoEntry {
        at,
        by: by.to_owned(),
        details,
        from,
        to: data.audit.len(),
        after: fingerprint(data),
        size,
        patch,
    };
    data.undo.push(entry);
    let mut total: usize = data.undo.iter().map(|u| u.size).sum();
    while data.undo.len() > 1 && (data.undo.len() > MAX_ENTRIES || total > MAX_BYTES) {
        total -= data.undo.remove(0).size;
    }
}

/// Reverts the latest change. Returns what it was. Nothing changes on `Err`.
pub fn revert_latest(data: &mut Data, by: &str, now: Ts) -> Result<String, String> {
    let mut d = data.clone();
    let Some(entry) = d.undo.pop() else {
        return Err("There is nothing to revert".to_owned());
    };
    if fingerprint(&d) != entry.after {
        return Err(
            "The data was changed outside this version of the app since then, so it can't be reverted safely"
                .to_owned(),
        );
    }
    let mut patch = entry.patch;
    macro_rules! apply {
        ($($f:ident),*) => {$(
            if let Some(p) = patch.remove(stringify!($f)) {
                apply_list(&mut d.$f, p)?;
            }
        )*};
    }
    lists!(apply);
    let read = |_| "its stored data can't be read".to_owned();
    if let Some(FieldPatch::Whole(v)) = patch.remove("settings") {
        d.settings = serde_json::from_value(v).map_err(read)?;
    }
    if let Some(FieldPatch::Whole(v)) = patch.remove("default_rule_id") {
        d.default_rule_id = serde_json::from_value(v).map_err(read)?;
    }
    if let Some(p) = patch.remove("audit") {
        let later = d.audit.split_off(entry.from.min(d.audit.len()));
        apply_list(&mut d.audit, p)?;
        d.audit.extend(later);
    }
    for e in d
        .audit
        .iter_mut()
        .skip(entry.from)
        .take(entry.to.saturating_sub(entry.from))
    {
        e.reverted = true;
    }
    let what = match entry.details.as_slice() {
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} (and {} more)", rest.len()),
        [] => String::new(),
    };
    d.audit.push(AuditEntry {
        at: now,
        by: by.to_owned(),
        action: "history.revert".into(),
        detail: format!("Reverted: {what}"),
        refs: BTreeMap::new(),
        reverted: false,
    });
    *data = d;
    Ok(what)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssignState, StaffStatus};
    use crate::ops::{self, Actor};
    use chrono::{NaiveDate, TimeZone};

    fn now() -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 24, 9, 0, 0)
            .unwrap()
    }

    fn ymd(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 5, d).unwrap()
    }

    fn step<R>(d: &mut Data, f: impl FnOnce(&mut Data, &Actor) -> R) -> R {
        let before = d.clone();
        let a = Actor {
            user: "agnes".into(),
            now: now(),
        };
        let r = f(d, &a);
        record(&before, d, "agnes", now());
        r
    }

    /// Everything but the audit log and the undo steps.
    fn content(d: &Data) -> Data {
        Data {
            audit: vec![],
            undo: vec![],
            ..d.clone()
        }
    }

    #[test]
    fn list_diffs_go_back_exactly() {
        let cases: [(&[i32], &[i32]); 8] = [
            (&[1, 2, 3], &[1, 2, 3, 4]),
            (&[1, 2, 3], &[2]),
            (&[], &[1, 1]),
            (&[1, 2, 3, 4], &[4, 3, 2, 1]),
            (&[1, 1, 2, 1], &[1, 2, 1, 1]),
            (&[1, 2, 3, 4, 5], &[1, 9, 3, 8, 5]),
            (&[5, 6], &[]),
            (&[3, 1, 2], &[1, 2, 3, 3]),
        ];
        for (old, new) in cases {
            let p = diff_list(old, new).unwrap();
            let mut back = new.to_vec();
            apply_list(&mut back, p).unwrap();
            assert_eq!(back, old, "{old:?} {new:?}");
        }
        assert!(diff_list(&[1, 2], &[1, 2]).is_none());
    }

    #[test]
    fn changes_revert_latest_first_and_stay_in_the_audit_log() {
        let mut d = Data::new_empty("agnes", now());
        let mut states = vec![content(&d)];
        let sec = d.sections[0].id.clone();
        let test = step(&mut d, |d, a| ops::add_test(d, a, &sec, "Cobas")).unwrap();
        states.push(content(&d));
        let amy = step(&mut d, |d, a| ops::add_staff(d, a, "Amy", "MT", None)).unwrap();
        states.push(content(&d));
        let bo = step(&mut d, |d, a| ops::add_staff(d, a, "Bo", "MT", None)).unwrap();
        states.push(content(&d));
        step(&mut d, |d, a| {
            ops::set_assignment(d, a, &bo, &test, Some(AssignState::Assigned))
        })
        .unwrap();
        states.push(content(&d));
        step(&mut d, |d, a| {
            ops::add_completion(d, a, &bo, &test, 0, ymd(1), "")
        })
        .unwrap();
        states.push(content(&d));
        step(&mut d, |d, a| ops::merge_staff(d, a, &amy, &bo)).unwrap();
        states.push(content(&d));
        step(&mut d, |d, a| {
            ops::set_staff_status(d, a, &amy, StaffStatus::Removed)
        })
        .unwrap();
        states.push(content(&d));
        step(&mut d, |d, _| d.settings.due_soon_days = 60);
        assert_eq!(d.undo.len(), 8);
        // It survives a save and a load.
        d = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        let audit = d.audit.len();
        for (k, want) in states.iter().rev().enumerate() {
            revert_latest(&mut d, "bea", now()).unwrap();
            assert_eq!(&content(&d), want, "step {k}");
        }
        assert!(d.undo.is_empty());
        assert_eq!(d.audit.len(), audit + 8, "nothing is erased");
        assert!(d.audit[..audit].iter().all(|e| e.reverted));
        let last = d.audit.last().unwrap();
        assert_eq!(
            (last.action.as_str(), last.by.as_str()),
            ("history.revert", "bea")
        );
        // The merge had pointed Bo's entries at Amy; they name Bo again.
        assert!(d.audit.iter().any(|e| e.refs.get("staff_id") == Some(&bo)));
        assert!(revert_latest(&mut d, "bea", now()).is_err());
    }

    #[test]
    fn data_changed_elsewhere_is_not_reverted() {
        let mut d = Data::new_empty("agnes", now());
        step(&mut d, |d, a| ops::add_staff(d, a, "Amy", "MT", None)).unwrap();
        d.staff[0].name = "Edited in Notepad".into();
        let same = d.clone();
        assert!(revert_latest(&mut d, "bea", now()).is_err());
        assert_eq!(d, same);
    }

    #[test]
    fn old_steps_make_room_but_the_latest_stays() {
        let mut d = Data::new_empty("agnes", now());
        for i in 0..MAX_ENTRIES + 5 {
            step(&mut d, |d, _| d.settings.due_soon_days = i as i64 + 100);
        }
        assert_eq!(d.undo.len(), MAX_ENTRIES);
        let big = "x".repeat(MAX_BYTES + 1);
        step(&mut d, |d, a| ops::add_staff(d, a, &big, "MT", None)).unwrap();
        step(&mut d, |d, _| d.staff.clear());
        assert_eq!(d.undo.len(), 1, "the big step alone");
        revert_latest(&mut d, "a", now()).unwrap();
        assert_eq!(d.staff.len(), 1);
    }

    #[test]
    fn a_change_that_changed_nothing_leaves_no_step() {
        let mut d = Data::new_empty("agnes", now());
        step(&mut d, |_, _| ());
        assert!(d.undo.is_empty());
    }
}
