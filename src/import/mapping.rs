use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::NaiveDate;

use crate::dates::fmt_date;
use crate::import::excel::{Cell, RawBlock, RawSheet, SHEETS};
use crate::model::{
    new_id, AssignState, Assignment, AuditEntry, Data, DueOverride, Id, Note, OverrideSource, Record, Section,
    Skipped, Stage, Staff, StaffStatus, TestSystem, Ts,
};
use crate::schedule::{next_due, stage_label, PairState};

const IMPORT_BY: &str = "excel-import";

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn normalize_name(s: &str) -> String {
    collapse(s).to_lowercase()
}

/// `(text outside brackets, [text inside each bracket pair])`; an unclosed
/// bracket runs to the end of the string.
fn split_brackets(s: &str) -> (String, Vec<String>) {
    let mut outside = String::new();
    let mut inside = Vec::new();
    let mut cur: Option<String> = None;
    for ch in s.chars() {
        match (ch, cur.as_mut()) {
            ('(', None) => cur = Some(String::new()),
            (')', Some(_)) => inside.extend(cur.take()),
            (_, Some(buf)) => buf.push(ch),
            (_, None) => outside.push(ch),
        }
    }
    inside.extend(cur);
    (outside, inside)
}

pub fn name_aliases(s: &str) -> Vec<String> {
    let (outside, inside) = split_brackets(s);
    let mut out: Vec<String> = Vec::new();
    let all = [normalize_name(s), normalize_name(&outside)].into_iter().chain(inside.iter().map(|i| normalize_name(i)));
    for a in all {
        if !a.is_empty() && !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// Shortest bracket-free name; if every name has brackets, the outside part of the shortest.
fn pick_name<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let clean: Vec<String> = names.map(collapse).collect();
    if let Some(n) = clean.iter().filter(|n| !n.contains('(')).min_by_key(|n| n.chars().count()) {
        return n.clone();
    }
    let Some(s) = clean.iter().min_by_key(|n| n.chars().count()) else {
        return String::new();
    };
    let outside = collapse(&split_brackets(s).0);
    if outside.is_empty() {
        s.clone()
    } else {
        outside
    }
}

fn group_of(members: &[(&str, &RawBlock)]) -> MergeGroup {
    let name = pick_name(members.iter().map(|(_, b)| b.name.as_str()));
    let mut joined: Vec<NaiveDate> = members.iter().filter_map(|(_, b)| b.joined).collect();
    joined.sort();
    joined.dedup();
    let mut titles: Vec<(String, usize)> = Vec::new();
    for (_, b) in members {
        let t = collapse(&b.title);
        if t.is_empty() {
            continue;
        }
        match titles.iter_mut().find(|(x, _)| *x == t) {
            Some(e) => e.1 += 1,
            None => titles.push((t, 1)),
        }
    }
    // Most frequent; ties go to the first seen.
    let title = titles
        .iter()
        .fold(None::<&(String, usize)>, |best, e| match best {
            Some(b) if b.1 >= e.1 => Some(b),
            _ => Some(e),
        })
        .map(|e| e.0.clone())
        .unwrap_or_default();
    let mut conflicts = Vec::new();
    if joined.len() > 1 {
        let ds: Vec<String> = joined.iter().map(|d| fmt_date(*d)).collect();
        conflicts.push(format!("Joined dates differ: {}", ds.join(", ")));
    }
    if titles.len() > 1 {
        let ts: Vec<&str> = titles.iter().map(|t| t.0.as_str()).collect();
        conflicts.push(format!("Titles differ: {}", ts.join(", ")));
    }
    MergeGroup {
        name,
        members: members.iter().map(|(s, b)| (s.to_string(), b.row, b.name.clone())).collect(),
        joined: joined.first().copied(),
        title,
        conflicts,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeGroup {
    pub name: String,
    /// `(sheet, row, raw name)`
    pub members: Vec<(String, usize, String)>,
    pub joined: Option<NaiveDate>,
    pub title: String,
    pub conflicts: Vec<String>,
}

fn find_root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Union-find over every named block: blocks sharing any alias are one person.
/// Groups come out in order of first appearance.
pub fn propose_merges(sheets: &[RawSheet]) -> Vec<MergeGroup> {
    let blocks: Vec<(&str, &RawBlock)> = sheets
        .iter()
        .flat_map(|s| s.blocks.iter().filter(|b| !b.name.trim().is_empty()).map(move |b| (s.name.as_str(), b)))
        .collect();
    let mut parent: Vec<usize> = (0..blocks.len()).collect();
    let mut first_with: HashMap<String, usize> = HashMap::new();
    for (i, (_, b)) in blocks.iter().enumerate() {
        for alias in name_aliases(&b.name) {
            match first_with.get(&alias) {
                Some(&j) => {
                    let (ri, rj) = (find_root(&mut parent, i), find_root(&mut parent, j));
                    // The smaller index stays root, so a root is its group's first block.
                    parent[ri.max(rj)] = ri.min(rj);
                }
                None => {
                    first_with.insert(alias, i);
                }
            }
        }
    }
    let mut by_root: BTreeMap<usize, Vec<(&str, &RawBlock)>> = BTreeMap::new();
    for (i, &m) in blocks.iter().enumerate() {
        by_root.entry(find_root(&mut parent, i)).or_default().push(m);
    }
    by_root.values().map(|m| group_of(m)).collect()
}

/// Every member of `groups[idx]` becomes its own group, named after that
/// member's own raw name (whitespace collapsed), not the old group's name.
///
/// `MergeGroup` keeps no per-member joined date or title, so each split group
/// inherits the original group's. The review UI MUST let the user edit each
/// split member's joined date and title before importing.
pub fn split_group(groups: &mut Vec<MergeGroup>, idx: usize) {
    if groups.get(idx).is_none_or(|g| g.members.len() < 2) {
        return;
    }
    let g = groups.remove(idx);
    let parts: Vec<MergeGroup> = g
        .members
        .into_iter()
        .map(|m| MergeGroup {
            name: collapse(&m.2),
            members: vec![m],
            joined: g.joined,
            title: g.title.clone(),
            conflicts: Vec::new(),
        })
        .collect();
    groups.splice(idx..idx, parts);
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub user: String,
    pub now: Ts,
    pub tolerance_days: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub sheets: usize,
    pub staff: usize,
    pub records: usize,
    pub unconfirmed: usize,
    pub planned: usize,
    pub notes: usize,
    pub excel_overrides: Vec<String>,
    pub excel_earlier: Vec<String>,
    /// Excel next-due ignored because the pair has a planned (future) date.
    pub excel_ignored_planned: Vec<String>,
    /// Excel next-due ignored because it is a record date + 12·k months, k ≥ 2 (drag-fill).
    pub excel_ignored_years: Vec<String>,
    pub flagged_stage: Vec<String>,
    pub merges: Vec<String>,
    /// One line per planned (future, unticked) date.
    pub planned_dates: Vec<String>,
    /// Names of staff set to Removed.
    pub removed: Vec<String>,
    /// "person – section" unassigned by a remark or cell.
    pub unassigned: Vec<String>,
    /// Things the user should know about the import as a whole.
    pub warnings: Vec<String>,
}

impl ImportReport {
    /// Plain text for Notepad (CRLF line ends).
    pub fn to_text(&self) -> String {
        let mut s = String::from("Excel import report\r\n\r\n");
        let counts = [
            ("Sheets", self.sheets),
            ("Staff", self.staff),
            ("Records", self.records),
            ("Unconfirmed records", self.unconfirmed),
            ("Planned dates", self.planned),
            ("Notes", self.notes),
        ];
        for (label, n) in counts {
            s += &format!("{label}: {n}\r\n");
        }
        for w in &self.warnings {
            s += &format!("\r\nNote: {w}\r\n");
        }
        let lists = [
            ("Excel due dates used (later than calculated)", &self.excel_overrides),
            ("Excel due dates earlier than calculated", &self.excel_earlier),
            ("Excel due dates ignored (planned date kept)", &self.excel_ignored_planned),
            ("Excel due dates ignored (looks like +N years)", &self.excel_ignored_years),
            ("Check annual numbering", &self.flagged_stage),
            ("Merged names", &self.merges),
            ("Planned dates", &self.planned_dates),
            ("Removed staff", &self.removed),
            ("Unassigned (person – section)", &self.unassigned),
        ];
        for (title, items) in lists {
            s += &format!("\r\n{title} ({})\r\n", items.len());
            if items.is_empty() {
                s += "  (none)\r\n";
            }
            for item in items {
                s += &format!("  {item}\r\n");
            }
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub data: Data,
    pub report: ImportReport,
}

pub const SCHEDULE_DIFFERS: &str =
    "Your current schedule differs from the standard one; imported records use the standard schedule.";

/// Same number of stages with the same intervals and repeat/numbering flags
/// (labels may differ), so records built for one fit the other.
fn same_shape(a: &[Stage], b: &[Stage]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.interval_months == y.interval_months && x.repeats == y.repeats && x.ordinal == y.ordinal
        })
}

/// Keeps what an import must not reset: the settings, the default rule's
/// stages and the whole audit history (placed before the import's own entry).
/// The current stages are only kept when they have the same shape as the ones
/// the import was built with; otherwise the import's stay and `report` says so.
pub fn carry_over(plan: &mut Data, report: &mut ImportReport, current: &Data) {
    plan.settings = current.settings.clone();
    let wanted = current.rules.iter().find(|r| r.id == current.default_rule_id).map(|r| &r.stages);
    let own = plan.default_rule_id.clone();
    if let (Some(stages), Some(rule)) = (wanted, plan.rules.iter_mut().find(|r| r.id == own)) {
        if same_shape(stages, &rule.stages) {
            rule.stages = stages.clone();
        } else {
            report.warnings.push(SCHEDULE_DIFFERS.to_owned());
        }
    }
    let mut audit = current.audit.clone();
    audit.append(&mut plan.audit);
    plan.audit = audit;
}

fn is_na_text(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "-" | "na" | "n/a")
}

fn is_not_done(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("not done")
}

fn says_unassigned(s: &str) -> bool {
    let l = s.to_lowercase();
    ["not assigned to", "no longer in", "no longer working in"].iter().any(|p| l.contains(p))
}

fn says_removed(s: &str) -> bool {
    let l = s.to_lowercase();
    ["left the company", "no longer working at"].iter().any(|p| l.contains(p))
}

/// Lowercase alphanumeric words.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The first `(\d+)\s*(st|nd|rd|th)` in `s` that stands as its own word.
fn find_ordinal(s: &str) -> Option<u32> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if !(c[i].is_ascii_digit() && (i == 0 || !c[i - 1].is_alphanumeric())) {
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && c[i].is_ascii_digit() {
            i += 1;
        }
        let mut j = i;
        while j < c.len() && c[j].is_whitespace() {
            j += 1;
        }
        if j + 2 > c.len() {
            continue;
        }
        let suffix: String = c[j..j + 2].iter().collect::<String>().to_lowercase();
        let word_end = c.get(j + 2).is_none_or(|x| !x.is_alphabetic());
        if word_end && ["st", "nd", "rd", "th"].contains(&suffix.as_str()) {
            if let Ok(n) = c[start..i].iter().collect::<String>().parse::<u32>() {
                if n >= 1 {
                    return Some(n);
                }
            }
        }
    }
    None
}

const MONTHS: [&str; 24] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec", "january", "february",
    "march", "april", "june", "july", "august", "sept", "september", "october", "november", "december",
];

/// Whether a remark token names this header: a prefix of one of its words
/// ("RET" → "Reticulocyte"), of two adjacent words joined ("u411" → "u 411"),
/// or the header's initials ("UM" → "Urinalysis Microscopy").
fn token_names_header(token: &str, header_words: &[String]) -> bool {
    let initials: String = header_words.iter().filter_map(|w| w.chars().next()).collect();
    header_words.iter().any(|w| w.starts_with(token))
        || header_words.windows(2).any(|p| format!("{}{}", p[0], p[1]).starts_with(token))
        || initials == token
}

/// Tokens that never name a test in an ordinal remark.
const STOPWORDS: [&str; 13] = ["by", "and", "of", "the", "test", "to", "in", "for", "with", "on", "done", "annual", "at"];

fn month_num(w: &str) -> Option<u32> {
    if !MONTHS.contains(&w) {
        return None;
    }
    MONTHS[..12].iter().position(|m| w.starts_with(m)).map(|i| i as u32 + 1)
}

/// A date written in a remark.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ClauseDate {
    None,
    Date(NaiveDate),
    /// Something date-like that is not a valid date (e.g. "31/06/2025", "Sep 2024").
    Unparseable,
}

/// First date in `s`: `d/m/yy`, `d/m/yyyy`, or `d MMM yyyy` (any case, full or short month).
fn clause_date(s: &str) -> ClauseDate {
    let mut date_like = false;
    for chunk in s.split(|c: char| !(c.is_ascii_digit() || c == '/')) {
        if !chunk.contains('/') || !chunk.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        date_like = true;
        let parts: Vec<&str> = chunk.split('/').collect();
        if let [d, m, y] = parts[..] {
            let year = match y.len() {
                2 => y.parse::<i32>().ok().map(|y| 2000 + y),
                4 => y.parse::<i32>().ok(),
                _ => None,
            };
            if let (Some(y), Ok(m), Ok(d)) = (year, m.parse::<u32>(), d.parse::<u32>()) {
                if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                    return ClauseDate::Date(date);
                }
            }
        }
    }
    let w = words(s);
    for (i, word) in w.iter().enumerate() {
        let Some(m) = month_num(word) else { continue };
        date_like = true;
        let day = i.checked_sub(1).and_then(|j| w.get(j)).filter(|d| d.len() <= 2).and_then(|d| d.parse::<u32>().ok());
        let year = w.get(i + 1).filter(|y| y.len() == 4).and_then(|y| y.parse::<i32>().ok());
        if let (Some(d), Some(y)) = (day, year) {
            if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                return ClauseDate::Date(date);
            }
        }
    }
    if date_like {
        ClauseDate::Unparseable
    } else {
        ClauseDate::None
    }
}

/// An ordinal remark clause that names a test.
#[derive(Debug, Clone, PartialEq)]
struct OrdinalHit {
    n: u32,
    /// The clause's date, else the first date elsewhere in the same remark.
    date: ClauseDate,
    clause: String,
}

/// Ordinal remark hits per test, and whether some ordinal clause matched no test.
///
/// Each `,`/`;` clause with an ordinal applies to the tests its other tokens
/// name. A token whose matches strictly contain another token's matches is
/// ignored, so "ABO Manual" picks "ABO Manual" but not "ABO Echo Lumina".
fn annual_hits(remarks: &[String], headers: &[&str]) -> (Vec<Vec<OrdinalHit>>, bool) {
    let mut hits = vec![Vec::new(); headers.len()];
    let header_words: Vec<Vec<String>> = headers.iter().map(|h| words(h)).collect();
    let mut unmatched = false;
    for remark in remarks {
        for clause in remark.split([',', ';']) {
            let Some(n) = find_ordinal(clause) else { continue };
            let matches: Vec<BTreeSet<usize>> = words(clause)
                .into_iter()
                .filter(|t| {
                    t.chars().count() >= 2
                        && !STOPWORDS.contains(&t.as_str())
                        && !MONTHS.contains(&t.as_str())
                        && !t.starts_with(|c: char| c.is_ascii_digit())
                })
                .map(|t| (0..headers.len()).filter(|&i| token_names_header(&t, &header_words[i])).collect())
                .filter(|m: &BTreeSet<usize>| !m.is_empty())
                .collect();
            let date = match clause_date(clause) {
                ClauseDate::None => clause_date(remark),
                d => d,
            };
            let mut hit = false;
            for m in &matches {
                if matches.iter().any(|o| o.len() < m.len() && o.is_subset(m)) {
                    continue;
                }
                for &i in m {
                    let h = OrdinalHit { n, date, clause: clause.trim().to_string() };
                    if !hits[i].contains(&h) {
                        hits[i].push(h);
                    }
                    hit = true;
                }
            }
            unmatched |= !hit;
        }
    }
    (hits, unmatched)
}

/// How an Annual-row record dated `row` is staged given the ordinal hits for
/// its test: `(row stage index, extra unconfirmed records (stage, date, clause), flag)`.
///
/// - Hit without a date, or dated within tolerance of `row` → relabel the row
///   record with that ordinal.
/// - Hit with a past date later than `row` beyond tolerance → keep the row
///   record (1st Annual, or the annual just before the hit's when it is a year
///   earlier) and add an unconfirmed record at the hit's date.
/// - Anything else (future, earlier, unparseable) → leave as is and flag.
fn resolve_annual(
    hits: &[OrdinalHit],
    row: NaiveDate,
    today: NaiveDate,
    tol: chrono::Duration,
) -> (u32, Vec<(u32, NaiveDate, String)>, bool) {
    let mut relabel: Option<u32> = None;
    let mut later: Vec<(u32, NaiveDate, String)> = Vec::new();
    let mut flag = false;
    for h in hits {
        match h.date {
            ClauseDate::None => relabel = relabel.max(Some(h.n)),
            ClauseDate::Date(d) if (d - row).num_days().abs() <= tol.num_days() => relabel = relabel.max(Some(h.n)),
            ClauseDate::Date(d) if d <= today && d > row + tol => later.push((h.n + 1, d, h.clause.clone())),
            _ => flag = true,
        }
    }
    let row_stage = match relabel {
        Some(n) => n + 1,
        None => later
            .iter()
            .min_by_key(|l| l.0)
            .filter(|(stage, d, _)| {
                // The row is the annual just before the hit's if it is a year earlier.
                *stage >= 3
                    && row.checked_add_months(chrono::Months::new(12)).is_some_and(|y| (*d - y).num_days().abs() <= tol.num_days())
            })
            .map_or(2, |l| l.0 - 1),
    };
    (row_stage, later, flag)
}

/// Everything gathered for one (staff, test) pair across that person's blocks.
#[derive(Default)]
struct PairAcc {
    /// A date anywhere in the 4 rows.
    any_date: bool,
    /// A date in stage rows 0-2.
    stage_date: bool,
    /// Some block had `-`/`NA`/`N/A` in all three stage rows.
    all_na: bool,
    skipped: BTreeSet<u32>,
    /// Stage indexes that have a date in some merged block.
    dated_stages: BTreeSet<u32>,
    planned: Option<NaiveDate>,
    excel_due: Option<NaiveDate>,
}

impl PairAcc {
    fn is_na(&self) -> bool {
        self.all_na && !self.stage_date
    }
}

struct Records {
    list: Vec<Record>,
    at: HashMap<(Id, Id, NaiveDate), usize>,
}

impl Records {
    /// Dedupes on (staff, test, date), preferring a confirmed record.
    fn add(&mut self, r: Record) {
        let key = (r.staff_id.clone(), r.test_id.clone(), r.completed_on);
        match self.at.get(&key) {
            Some(&i) => {
                if self.list[i].unconfirmed && !r.unconfirmed {
                    self.list[i] = r;
                }
            }
            None => {
                self.at.insert(key, self.list.len());
                self.list.push(r);
            }
        }
    }
}

fn push_test(section: &mut Section, name: String) -> Id {
    let id = new_id("t");
    let order = section.tests.len() as u32;
    section.tests.push(TestSystem { id: id.clone(), name, order, retired: false, rule_id: None });
    id
}

fn staff_index(staff: &[Staff], id: &str) -> Option<usize> {
    staff.iter().position(|s| s.id == id)
}

fn new_staff(name: String, title: String, joined: Option<NaiveDate>) -> Staff {
    Staff { id: new_id("p"), name, title, joined, status: StaffStatus::Active, assignments: BTreeMap::new() }
}

/// Builds the tracker data from the parsed workbook (spec §12).
pub fn build_import(sheets: &[RawSheet], groups: &[MergeGroup], opts: &ImportOptions) -> ImportPlan {
    let today = opts.now.date_naive();
    let mut data = Data::new_empty(&opts.user, opts.now);
    data.settings.variance_tolerance_days = opts.tolerance_days;
    let rule = data.rules[0].clone();
    let mut report = ImportReport { sheets: sheets.len(), ..ImportReport::default() };

    // 1. One section per sheet, in SHEETS order.
    let mut order: Vec<usize> = (0..sheets.len()).collect();
    order.sort_by_key(|&i| SHEETS.iter().position(|n| *n == sheets[i].name).unwrap_or(usize::MAX));
    let mut sheet_section = vec![0usize; sheets.len()];
    data.sections = order
        .iter()
        .enumerate()
        .map(|(pos, &si)| {
            sheet_section[si] = pos;
            Section { id: new_id("s"), name: sheets[si].name.clone(), order: pos as u32, retired: false, tests: vec![] }
        })
        .collect();
    let target_section = |si: usize, hint: &str| -> usize {
        let h = normalize_name(hint);
        sheets
            .iter()
            .position(|s| !h.is_empty() && normalize_name(&s.name) == h)
            .map_or(sheet_section[si], |i| sheet_section[i])
    };

    // 2. Tests: own-section columns first (column order, duplicates suffixed),
    //    then cross-section columns mapped by normalised name.
    let mut test_map: Vec<Vec<Id>> = sheets.iter().map(|s| vec![Id::new(); s.tests.len()]).collect();
    let mut name_count: HashMap<(usize, String), usize> = HashMap::new();
    let mut first_by_name: HashMap<(usize, String), Id> = HashMap::new();
    let mut cross = Vec::new();
    for &si in &order {
        let mut cols: Vec<usize> = (0..sheets[si].tests.len()).collect();
        cols.sort_by_key(|&ti| sheets[si].tests[ti].col);
        for ti in cols {
            let t = &sheets[si].tests[ti];
            let sec = target_section(si, &t.section_hint);
            if sec != sheet_section[si] {
                cross.push((si, ti, sec));
                continue;
            }
            let norm = normalize_name(&t.header);
            let n = name_count.entry((sec, norm.clone())).or_insert(0);
            *n += 1;
            let name = if *n == 1 { t.header.clone() } else { format!("{} ({n})", t.header) };
            let id = push_test(&mut data.sections[sec], name);
            first_by_name.entry((sec, norm)).or_insert_with(|| id.clone());
            test_map[si][ti] = id;
        }
    }
    for (si, ti, sec) in cross {
        let t = &sheets[si].tests[ti];
        let key = (sec, normalize_name(&t.header));
        let id = match first_by_name.get(&key) {
            Some(id) => id.clone(),
            None => {
                let id = push_test(&mut data.sections[sec], t.header.clone());
                name_count.insert(key.clone(), 1);
                first_by_name.insert(key, id.clone());
                id
            }
        };
        test_map[si][ti] = id;
    }
    // test id -> (section index, test order, test name)
    let mut test_info: HashMap<Id, (usize, u32, String)> = HashMap::new();
    for (si, s) in data.sections.iter().enumerate() {
        for t in &s.tests {
            test_info.insert(t.id.clone(), (si, t.order, t.name.clone()));
        }
    }

    // Staff from the (possibly user-edited) merge groups; any block no group
    // covers becomes its own person so nothing is dropped.
    let mut staff_of: HashMap<(String, usize), usize> = HashMap::new();
    for g in groups {
        for m in &g.members {
            staff_of.insert((m.0.clone(), m.1), data.staff.len());
        }
        data.staff.push(new_staff(g.name.clone(), g.title.clone(), g.joined));
        if g.members.len() > 1 {
            let ms: Vec<String> = g.members.iter().map(|m| format!("{} [{}]", m.2, m.0)).collect();
            let mut line = format!("{}: {}", g.name, ms.join(", "));
            if !g.conflicts.is_empty() {
                line += &format!(" ({})", g.conflicts.join("; "));
            }
            report.merges.push(line);
        }
    }
    for &si in &order {
        for b in &sheets[si].blocks {
            let key = (sheets[si].name.clone(), b.row);
            if b.name.trim().is_empty() || staff_of.contains_key(&key) {
                continue;
            }
            staff_of.insert(key, data.staff.len());
            data.staff.push(new_staff(collapse(&b.name), collapse(&b.title), b.joined));
        }
    }

    // 3-5. Cells and remarks.
    let tol = chrono::Duration::days(opts.tolerance_days);
    let mut records = Records { list: Vec::new(), at: HashMap::new() };
    let mut pairs: BTreeMap<(usize, Id), PairAcc> = BTreeMap::new();
    let mut unassigned: HashSet<(usize, usize)> = HashSet::new();
    let mut removed: HashSet<usize> = HashSet::new();
    for &si in &order {
        let sheet = &sheets[si];
        let own_sec = sheet_section[si];
        let section_id = data.sections[own_sec].id.clone();
        let headers: Vec<&str> = sheet.tests.iter().map(|t| t.header.as_str()).collect();
        for b in &sheet.blocks {
            let Some(&p) = staff_of.get(&(sheet.name.clone(), b.row)) else { continue };
            let staff_id = data.staff[p].id.clone();
            let mut status_text = |s: &str| {
                if says_unassigned(s) {
                    unassigned.insert((p, own_sec));
                }
                if says_removed(s) {
                    removed.insert(p);
                }
            };
            let (hits, unmatched) = annual_hits(&b.remarks, &headers);
            let flag_line = format!("{} – {}: check annual numbering", data.staff[p].name, sheet.name);
            let flag = |report: &mut ImportReport| {
                if !report.flagged_stage.contains(&flag_line) {
                    report.flagged_stage.push(flag_line.clone());
                }
            };
            if unmatched {
                flag(&mut report);
            }
            for remark in &b.remarks {
                status_text(remark);
                data.notes.push(Note {
                    id: new_id("n"),
                    staff_id: staff_id.clone(),
                    test_id: None,
                    section_id: Some(section_id.clone()),
                    text: remark.clone(),
                    by: IMPORT_BY.into(),
                    at: opts.now,
                });
            }
            for ti in 0..sheet.tests.len() {
                let test_id = &test_map[si][ti];
                let acc = pairs.entry((p, test_id.clone())).or_default();
                let mut na_stages = Vec::new();
                let record = |stage: u32, on: NaiveDate, unconfirmed: bool, comment: String| Record {
                    id: new_id("c"),
                    staff_id: staff_id.clone(),
                    test_id: test_id.clone(),
                    stage_index: stage,
                    stage_label: stage_label(&rule, stage),
                    completed_on: on,
                    comment,
                    by: IMPORT_BY.into(),
                    at: opts.now,
                    imported: true,
                    unconfirmed,
                };
                for (row, cells) in b.stages.iter().enumerate() {
                    let Some(c) = cells.get(ti) else { continue };
                    let mut stage = row as u32;
                    match &c.cell {
                        Cell::Empty => {}
                        Cell::Date(d) => {
                            acc.any_date = true;
                            if row == 3 {
                                acc.excel_due = acc.excel_due.max(Some(*d));
                            } else if c.ticked || *d <= today {
                                if row == 2 {
                                    let (s, extra, flagged) = resolve_annual(&hits[ti], *d, today, tol);
                                    stage = s;
                                    if flagged {
                                        flag(&mut report);
                                    }
                                    for (xs, xd, clause) in extra {
                                        acc.dated_stages.insert(xs);
                                        records.add(record(xs, xd, true, format!("From remark: {clause}")));
                                    }
                                }
                                acc.stage_date = true;
                                acc.dated_stages.insert(stage);
                                records.add(record(stage, *d, !c.ticked, String::new()));
                            } else {
                                // Unticked future date (tick column or not): planned.
                                acc.stage_date = true;
                                acc.dated_stages.insert(stage);
                                acc.planned = acc.planned.max(Some(*d));
                                report.planned += 1;
                                report.planned_dates.push(format!(
                                    "{} – {} / {}: {} planned {}",
                                    data.staff[p].name,
                                    data.sections[test_info[test_id].0].name,
                                    test_info[test_id].2,
                                    stage_label(&rule, stage),
                                    fmt_date(*d)
                                ));
                            }
                        }
                        Cell::Text(s) => {
                            if is_na_text(s) {
                                if row < 3 {
                                    na_stages.push(stage);
                                }
                                continue;
                            }
                            if is_not_done(s) {
                                continue;
                            }
                            status_text(s);
                            let label = if row == 3 { "Next Annual Due".to_string() } else { stage_label(&rule, stage) };
                            data.notes.push(Note {
                                id: new_id("n"),
                                staff_id: staff_id.clone(),
                                test_id: Some(test_id.clone()),
                                section_id: None,
                                text: format!("{label}: {s}"),
                                by: IMPORT_BY.into(),
                                at: opts.now,
                            });
                        }
                    }
                }
                if na_stages.len() == 3 {
                    acc.all_na = true;
                } else {
                    acc.skipped.extend(na_stages);
                }
            }
        }
    }
    let mut removed: Vec<usize> = removed.into_iter().collect();
    removed.sort_unstable();
    for p in removed {
        data.staff[p].status = StaffStatus::Removed;
        report.removed.push(data.staff[p].name.clone());
    }
    let mut unassigned_lines: Vec<&(usize, usize)> = unassigned.iter().collect();
    unassigned_lines.sort_unstable();
    for (p, sec) in unassigned_lines {
        report.unassigned.push(format!("{} – {}", data.staff[*p].name, data.sections[*sec].name));
    }
    data.records = records.list;
    // Stages that really happened are never skipped.
    for r in &data.records {
        if let Some(acc) = staff_index(&data.staff, &r.staff_id).and_then(|p| pairs.get_mut(&(p, r.test_id.clone()))) {
            acc.dated_stages.insert(r.stage_index);
        }
    }
    for acc in pairs.values_mut() {
        let dated = acc.dated_stages.clone();
        acc.skipped.retain(|s| !dated.contains(s));
    }

    // 6. Assignments.
    for ((p, tid), acc) in &pairs {
        let sec = test_info[tid].0;
        if unassigned.contains(&(*p, sec)) {
            continue;
        }
        let state = if acc.is_na() {
            AssignState::Na
        } else if acc.any_date {
            AssignState::Assigned
        } else {
            continue;
        };
        let since = data.staff[*p].joined.unwrap_or(today);
        data.staff[*p].assignments.insert(tid.clone(), Assignment { state, since });
    }
    for ((p, tid), acc) in &pairs {
        if acc.is_na() {
            continue;
        }
        for &stage_index in &acc.skipped {
            data.skipped.push(Skipped {
                staff_id: data.staff[*p].id.clone(),
                test_id: tid.clone(),
                stage_index,
                by: IMPORT_BY.into(),
                at: opts.now,
            });
        }
    }

    // 7. Planned dates and the Excel next-due row.
    let mut by_pair: HashMap<(&str, &str), Vec<&Record>> = HashMap::new();
    for r in &data.records {
        by_pair.entry((r.staff_id.as_str(), r.test_id.as_str())).or_default().push(r);
    }
    let mut overrides = Vec::new();
    // Report lines in person, section, test order.
    let mut ordered: Vec<(&(usize, Id), &PairAcc)> = pairs.iter().collect();
    ordered.sort_by_key(|((p, tid), _)| (*p, test_info[tid].0, test_info[tid].1));
    for ((p, tid), acc) in ordered {
        let (sec, _, test_name) = &test_info[tid];
        if acc.is_na() || unassigned.contains(&(*p, *sec)) {
            continue;
        }
        let staff = &data.staff[*p];
        let due = acc.planned;
        if let Some(x) = acc.excel_due {
            let pair_records = by_pair.get(&(staff.id.as_str(), tid.as_str())).cloned().unwrap_or_default();
            let state = PairState {
                records: pair_records.clone(),
                skipped: acc.skipped.iter().copied().collect(),
                override_: None,
                assigned_on: staff.assignments.get(tid).map(|a| a.since),
            };
            let computed = next_due(&rule, &state, &data.settings).due_on;
            let what = format!("{} – {} / {}", staff.name, data.sections[*sec].name, test_name);
            let vs = computed.map_or("nothing calculated".to_string(), |c| format!("calculated {}", fmt_date(c)));
            let line = format!("{what}: Excel {} vs {vs}", fmt_date(x));
            match computed {
                Some(c) if x < c - tol => report.excel_earlier.push(line),
                Some(c) if x <= c + tol => {}
                // Excel is later beyond tolerance, or nothing was calculated.
                _ => {
                    // Drag-fill: a record date plus exactly 12·k months, k ≥ 2.
                    let years = pair_records.iter().find_map(|r| {
                        (2..=50u32).find(|k| r.completed_on.checked_add_months(chrono::Months::new(12 * k)) == Some(x))
                    });
                    if let Some(planned) = acc.planned {
                        report.excel_ignored_planned.push(format!("{line}; planned {} kept", fmt_date(planned)));
                    } else if let Some(k) = years {
                        report.excel_ignored_years.push(format!("{line} (+{k} years from a record)"));
                    } else {
                        overrides.push(DueOverride {
                            staff_id: staff.id.clone(),
                            test_id: tid.clone(),
                            due_on: x,
                            source: OverrideSource::Excel,
                            by: IMPORT_BY.into(),
                            at: opts.now,
                        });
                        report.excel_overrides.push(line);
                        continue;
                    }
                }
            }
        }
        if let Some(due_on) = due {
            overrides.push(DueOverride {
                staff_id: staff.id.clone(),
                test_id: tid.clone(),
                due_on,
                source: OverrideSource::Excel,
                by: IMPORT_BY.into(),
                at: opts.now,
            });
        }
    }
    data.due_overrides = overrides;

    report.staff = data.staff.len();
    report.records = data.records.len();
    report.unconfirmed = data.records.iter().filter(|r| r.unconfirmed).count();
    report.notes = data.notes.len();

    // 8. Audit.
    data.audit.push(AuditEntry {
        at: opts.now,
        by: opts.user.clone(),
        action: "import.excel".into(),
        detail: format!(
            "Imported {} sheets: {} staff, {} records ({} unconfirmed), {} planned dates, {} notes",
            report.sheets, report.staff, report.records, report.unconfirmed, report.planned, report.notes
        ),
        refs: BTreeMap::new(),
    });
    ImportPlan { data, report }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::excel::{Cell, RawBlock, RawTest, StageCell};
    use crate::model::{AssignState, OverrideSource, Record, StaffStatus};
    use chrono::TimeZone;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Highest ordinal per test as a stage index (2 when none).
    fn annual_indices(remarks: &[String], headers: &[&str]) -> (Vec<u32>, bool) {
        let (hits, unmatched) = annual_hits(remarks, headers);
        (hits.iter().map(|h| h.iter().map(|h| h.n + 1).max().unwrap_or(2)).collect(), unmatched)
    }

    #[test]
    fn clause_dates() {
        assert_eq!(clause_date("DONE on 27 Dec 2023"), ClauseDate::Date(ymd(2023, 12, 27)));
        assert_eq!(clause_date("on 07 May 2023"), ClauseDate::Date(ymd(2023, 5, 7)));
        assert_eq!(clause_date("e411 (07/8/2024)"), ClauseDate::Date(ymd(2024, 8, 7)));
        assert_eq!(clause_date("e411 (16/9/24)"), ClauseDate::Date(ymd(2024, 9, 16)));
        assert_eq!(clause_date("2 may 2025"), ClauseDate::Date(ymd(2025, 5, 2)));
        assert_eq!(clause_date("2 September 2025"), ClauseDate::Date(ymd(2025, 9, 2)));
        assert_eq!(clause_date("(31/06/2025)"), ClauseDate::Unparseable);
        assert_eq!(clause_date("UM (Sep 2024)"), ClauseDate::Unparseable);
        assert_eq!(clause_date("2nd UM/u411"), ClauseDate::None);
    }

    fn now() -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600).unwrap().with_ymd_and_hms(2026, 11, 14, 9, 0, 0).unwrap()
    }

    fn opts() -> ImportOptions {
        ImportOptions { user: "jtan".into(), now: now(), tolerance_days: 30 }
    }

    fn date(y: i32, m: u32, d: u32, ticked: bool) -> StageCell {
        StageCell { cell: Cell::Date(ymd(y, m, d)), ticked }
    }

    fn text(s: &str) -> StageCell {
        StageCell { cell: Cell::Text(s.into()), ticked: false }
    }

    fn empty() -> StageCell {
        StageCell { cell: Cell::Empty, ticked: false }
    }

    fn rt(col: usize, header: &str, tick: bool, hint: &str) -> RawTest {
        RawTest { col, header: header.into(), tick_col: tick.then_some(col + 1), section_hint: hint.into() }
    }

    /// A block with one stage cell per test in each of the 4 rows.
    fn block(row: usize, name: &str, rows: [Vec<StageCell>; 4]) -> RawBlock {
        RawBlock {
            row,
            name: name.into(),
            title: "MT".into(),
            joined: Some(ymd(2020, 1, 1)),
            stages: rows,
            remarks: vec![],
        }
    }

    /// Block for a single-test sheet.
    fn block1(row: usize, name: &str, cells: [StageCell; 4]) -> RawBlock {
        let [a, b, c, d] = cells;
        block(row, name, [vec![a], vec![b], vec![c], vec![d]])
    }

    fn sheet(name: &str, tests: Vec<RawTest>, blocks: Vec<RawBlock>) -> RawSheet {
        RawSheet { name: name.into(), tests, blocks }
    }

    fn one_test(name: &str, tick: bool, blocks: Vec<RawBlock>) -> RawSheet {
        sheet(name, vec![rt(5, "Test A", tick, name)], blocks)
    }

    fn run(sheets: &[RawSheet]) -> ImportPlan {
        let groups = propose_merges(sheets);
        build_import(sheets, &groups, &opts())
    }

    fn staff_id(p: &ImportPlan, name: &str) -> String {
        p.data.staff.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no staff {name}")).id.clone()
    }

    fn test_id(p: &ImportPlan, section: &str, name: &str) -> String {
        let sec = p.data.sections.iter().find(|s| s.name == section).unwrap();
        sec.tests.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no test {name}")).id.clone()
    }

    fn recs<'a>(p: &'a ImportPlan, staff: &str, test: &str) -> Vec<&'a Record> {
        let mut v: Vec<&Record> =
            p.data.records.iter().filter(|r| r.staff_id == staff && r.test_id == test).collect();
        v.sort_by_key(|r| r.completed_on);
        v
    }

    fn default_stages(d: &Data) -> Vec<crate::model::Stage> {
        d.rules.iter().find(|r| r.id == d.default_rule_id).unwrap().stages.clone()
    }

    fn default_stages_mut(d: &mut Data) -> &mut Vec<crate::model::Stage> {
        let rid = d.default_rule_id.clone();
        &mut d.rules.iter_mut().find(|r| r.id == rid).unwrap().stages
    }

    #[test]
    fn carry_over_keeps_settings_rule_and_audit() {
        let mut plan = run(&[one_test("Hematology", true, vec![block1(4, "A", [date(2025, 1, 10, true), empty(), empty(), empty()])])]).data;
        let mut current = plan.clone();
        current.settings.due_soon_days = 45;
        current.settings.initial_due_days = Some(14);
        // Same shape, renamed stages: carried over.
        default_stages_mut(&mut current)[1].label = "Six-monthly".into();
        let old = AuditEntry { at: now(), by: "agnes".into(), action: "staff.add".into(), detail: "old".into(), refs: Default::default() };
        current.audit = vec![old.clone()];
        assert_eq!(plan.audit.len(), 1);

        let mut report = ImportReport::default();
        carry_over(&mut plan, &mut report, &current);
        assert_eq!(plan.settings, current.settings);
        assert_eq!(default_stages(&plan), default_stages(&current));
        assert!(report.warnings.is_empty());
        assert_eq!(plan.audit.len(), 2);
        assert_eq!(plan.audit[0], old);
        assert_eq!(plan.audit[1].action, "import.excel");
    }

    #[test]
    fn carry_over_keeps_the_standard_schedule_when_the_current_one_differs() {
        let base = run(&[one_test("Hematology", true, vec![block1(4, "A", [date(2025, 1, 10, true), empty(), empty(), empty()])])]).data;
        let standard = default_stages(&base);
        let changes: [fn(&mut Vec<crate::model::Stage>); 4] = [
            |s| s.truncate(1),
            |s| s[1].interval_months = Some(3),
            |s| s[2].repeats = false,
            |s| s[2].ordinal = false,
        ];
        for change in changes {
            let mut plan = base.clone();
            let mut current = base.clone();
            current.settings.due_soon_days = 45;
            change(default_stages_mut(&mut current));
            let mut report = ImportReport::default();
            carry_over(&mut plan, &mut report, &current);
            assert_eq!(default_stages(&plan), standard, "imported records keep the schedule they were built with");
            assert_eq!(plan.settings.due_soon_days, 45, "settings still carried");
            assert_eq!(
                report.warnings,
                ["Your current schedule differs from the standard one; imported records use the standard schedule."]
            );
            assert!(report.to_text().contains("Your current schedule differs from the standard one"));
        }
    }

    #[test]
    fn normalize_and_aliases() {
        assert_eq!(normalize_name("  Shawn \n  TAN "), "shawn tan");
        assert_eq!(name_aliases("Christine (Li Yun)"), ["christine (li yun)", "christine", "li yun"]);
        assert_eq!(name_aliases("Shawn"), ["shawn"]);
    }

    #[test]
    fn aliases_merge_bracket_names() {
        let e = || [empty(), empty(), empty(), empty()];
        let sheets = vec![
            one_test("Chemistry & Immunology", true, vec![block1(3, "Shawn", e()), block1(7, "Christine (Li Yun)", e())]),
            one_test("Hematology", true, vec![block1(3, "Shawn (Weixiang)", e()), block1(7, "Li Yun", e())]),
            one_test("Urinalysis", true, vec![block1(3, "Weixiang", e()), block1(7, "Agnes", e())]),
        ];
        let g = propose_merges(&sheets);
        assert_eq!(g.len(), 3);
        let shawn = g.iter().find(|g| g.name == "Shawn").unwrap();
        assert_eq!(shawn.members.len(), 3);
        assert!(shawn.members.contains(&("Hematology".into(), 3, "Shawn (Weixiang)".into())));
        let li = g.iter().find(|g| g.members.iter().any(|m| m.2 == "Li Yun")).unwrap();
        assert_eq!(li.members.len(), 2);
        assert!(li.members.iter().any(|m| m.2 == "Christine (Li Yun)"));
        assert_eq!(li.name, "Li Yun");
        assert!(g.iter().any(|g| g.name == "Agnes" && g.members.len() == 1));

        let mut g = g;
        let idx = g.iter().position(|g| g.name == "Shawn").unwrap();
        split_group(&mut g, idx);
        assert_eq!(g.len(), 5);
        assert!(g.iter().all(|g| g.members.len() == 1 || g.name == "Li Yun"));
    }

    #[test]
    fn same_tab_duplicates_merge() {
        let sheets = vec![one_test(
            "Urinalysis",
            true,
            vec![
                block1(3, "Shermaine", [date(2024, 1, 5, true), empty(), empty(), empty()]),
                block1(7, "Shermaine ", [empty(), date(2024, 7, 5, true), empty(), empty()]),
                block1(11, "  ", [date(2024, 1, 5, true), empty(), empty(), empty()]),
            ],
        )];
        let g = propose_merges(&sheets);
        assert_eq!(g.len(), 1, "blank names are skipped");
        assert_eq!(g[0].members.len(), 2);
        let p = build_import(&sheets, &g, &opts());
        assert_eq!(p.data.staff.len(), 1);
        assert_eq!(p.data.records.len(), 2);
        assert_eq!(p.report.merges.len(), 1);
    }

    #[test]
    fn merge_conflicts_listed_earliest_joined() {
        let mut a = block1(3, "Agnes", [empty(), empty(), empty(), empty()]);
        a.joined = Some(ymd(2020, 5, 18));
        let mut b = a.clone();
        b.joined = Some(ymd(2019, 1, 1));
        b.title = "Senior MT".into();
        let mut c = a.clone();
        c.name = "agnes".into();
        let sheets = vec![
            one_test("Chemistry & Immunology", true, vec![a]),
            one_test("Hematology", true, vec![b]),
            one_test("Urinalysis", true, vec![c]),
        ];
        let g = propose_merges(&sheets);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].joined, Some(ymd(2019, 1, 1)));
        assert_eq!(g[0].title, "MT");
        assert_eq!(g[0].conflicts.len(), 2, "{:?}", g[0].conflicts);
        assert!(g[0].conflicts.iter().any(|c| c.contains("1 Jan 2019") && c.contains("18 May 2020")));
        assert!(g[0].conflicts.iter().any(|c| c.contains("Senior MT")));

        // Edited group fields are used for the staff member.
        let mut g = g;
        g[0].name = "Agnes Lim".into();
        g[0].title = "Senior MT".into();
        let p = build_import(&sheets, &g, &opts());
        assert_eq!(p.data.staff.len(), 1);
        assert_eq!(p.data.staff[0].name, "Agnes Lim");
        assert_eq!(p.data.staff[0].title, "Senior MT");
        assert_eq!(p.data.staff[0].joined, Some(ymd(2019, 1, 1)));
    }

    #[test]
    fn ticked_date_is_confirmed_record() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(3, "Alice", [date(2024, 5, 2, true), empty(), empty(), empty()])],
        )];
        let p = run(&sheets);
        assert_eq!(p.data.sections.len(), 1);
        assert_eq!(p.data.sections[0].name, "Serology");
        let (s, t) = (staff_id(&p, "Alice"), test_id(&p, "Serology", "Test A"));
        let r = recs(&p, &s, &t);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].stage_index, r[0].stage_label.as_str()), (0, "Initial"));
        assert_eq!(r[0].completed_on, ymd(2024, 5, 2));
        assert!(r[0].imported && !r[0].unconfirmed);
        assert_eq!((r[0].by.as_str(), r[0].at), ("excel-import", now()));
        let a = &p.data.staff[0].assignments[&t];
        assert_eq!((a.state, a.since), (AssignState::Assigned, ymd(2020, 1, 1)));
        assert_eq!(p.data.audit.len(), 1);
        assert_eq!((p.data.audit[0].action.as_str(), p.data.audit[0].by.as_str()), ("import.excel", "jtan"));
        assert_eq!((p.report.sheets, p.report.staff, p.report.records, p.report.unconfirmed), (1, 1, 1, 0));
    }

    #[test]
    fn unticked_past_date_is_unconfirmed() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(3, "Alice", [date(2024, 5, 2, true), date(2024, 11, 2, false), empty(), empty()])],
        )];
        let p = run(&sheets);
        let r = recs(&p, &staff_id(&p, "Alice"), &test_id(&p, "Serology", "Test A"));
        assert_eq!(r.len(), 2);
        assert_eq!((r[1].stage_index, r[1].stage_label.as_str()), (1, "Semi-annual"));
        assert!(r[1].unconfirmed && r[1].imported);
        assert!(!r[0].unconfirmed);
        assert_eq!(p.report.unconfirmed, 1);
        assert!(p.data.due_overrides.is_empty());
    }

    #[test]
    fn tickless_column_is_unconfirmed() {
        let sheets = vec![one_test(
            "Molecular Biology",
            false,
            vec![block1(3, "Bob", [date(2024, 1, 3, false), date(2027, 1, 3, false), empty(), empty()])],
        )];
        let p = run(&sheets);
        let r = recs(&p, &staff_id(&p, "Bob"), &test_id(&p, "Molecular Biology", "Test A"));
        assert_eq!(r.len(), 1, "a future date in a tick-less column is planned, not a record");
        assert!(r[0].unconfirmed && r[0].completed_on == ymd(2024, 1, 3));
        assert_eq!(p.report.planned, 1);
        assert_eq!(p.data.due_overrides.len(), 1);
        assert_eq!(p.data.due_overrides[0].due_on, ymd(2027, 1, 3));
        assert_eq!(p.report.planned_dates.len(), 1);
        assert!(p.report.planned_dates[0].contains("3 Jan 2027"));
    }

    // ---- Fix round 1 ----

    #[test]
    fn planned_date_kept_over_later_excel() {
        let sheets = vec![one_test(
            "Urinalysis",
            true,
            vec![block1(
                3,
                "Mashaida",
                [date(2026, 7, 15, true), empty(), date(2027, 7, 15, false), date(2028, 7, 15, false)],
            )],
        )];
        let p = run(&sheets);
        assert_eq!(p.data.due_overrides.len(), 1);
        assert_eq!(p.data.due_overrides[0].due_on, ymd(2027, 7, 15));
        assert!(p.report.excel_overrides.is_empty(), "{:?}", p.report.excel_overrides);
        assert_eq!(p.report.excel_ignored_planned.len(), 1);
        assert!(p.report.excel_ignored_planned[0].contains("15 Jul 2028"));
        assert!(p.report.to_text().contains("Excel due dates ignored (planned date kept) (1)"));
    }

    #[test]
    fn excel_drag_fill_ignored() {
        // 1st Annual 11 Feb 2026 -> computed 11 Feb 2027; Excel 11 Feb 2028 = record + 24 months.
        let p = due_case(date(2026, 2, 11, true), ymd(2028, 2, 11));
        assert!(p.data.due_overrides.is_empty());
        assert!(p.report.excel_overrides.is_empty());
        assert_eq!(p.report.excel_ignored_years.len(), 1);
        assert!(p.report.excel_ignored_years[0].contains("+2 years"), "{:?}", p.report.excel_ignored_years);
        assert!(p.report.to_text().contains("Excel due dates ignored (looks like +N years) (1)"));
        // Not an exact multiple: still overrides.
        let p = due_case(date(2026, 2, 11, true), ymd(2028, 2, 12));
        assert_eq!(p.data.due_overrides.len(), 1);
    }

    fn hema_ruby(annual: NaiveDate, remark: &str) -> ImportPlan {
        let mut b = block1(3, "Frankie", [empty(), empty(), StageCell { cell: Cell::Date(annual), ticked: true }, empty()]);
        b.remarks = vec![remark.into()];
        run(&[sheet("Hematology", vec![rt(5, "Cell-DYN Ruby", true, "Hematology")], vec![b])])
    }

    #[test]
    fn ordinal_clause_date_matches_row_relabels() {
        // (a) clause date within tolerance of the Annual-row date.
        let p = hema_ruby(ymd(2023, 12, 27), "Cell-Dyn Ruby 2nd Annual DONE on 27 Dec 2023");
        assert_eq!(p.data.records.len(), 1);
        assert_eq!((p.data.records[0].stage_index, p.data.records[0].stage_label.as_str()), (3, "2nd Annual"));
        assert!(p.report.flagged_stage.is_empty());
        // Date in another clause of the same remark (real shape).
        let mut b = block1(3, "Valerie", [empty(), empty(), date(2025, 5, 2, true), empty()]);
        b.remarks = vec!["2 may 2025, 2nd annual (u411 & microscopy)".into()];
        let tests = vec![rt(5, "Urinalysis on cobas u 411 system", true, "Urinalysis")];
        let p = run(&[sheet("Urinalysis", tests, vec![b])]);
        assert_eq!(p.data.records.len(), 1);
        assert_eq!(p.data.records[0].stage_index, 3);
    }

    #[test]
    fn ordinal_clause_later_past_date_adds_unconfirmed_record() {
        // (b) Annual row 5 Dec 2022, remark says 2nd Annual on 27 Dec 2023.
        let remark = "Cell-Dyn Ruby 2nd Annual DONE on 27 Dec 2023";
        let p = hema_ruby(ymd(2022, 12, 5), remark);
        let mut r: Vec<&Record> = p.data.records.iter().collect();
        r.sort_by_key(|r| r.completed_on);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].completed_on, r[0].stage_index, r[0].unconfirmed), (ymd(2022, 12, 5), 2, false));
        assert_eq!((r[1].completed_on, r[1].stage_index, r[1].unconfirmed), (ymd(2023, 12, 27), 3, true));
        assert_eq!(r[1].stage_label, "2nd Annual");
        assert_eq!(r[1].comment, format!("From remark: {remark}"));

        // Urinalysis shape: row Jul 2022, "3rd UM/u411 (2 Sep 2024)" -> row stays 1st Annual.
        let tests = vec![
            rt(5, "Urinalysis on cobas u 411 system", true, "Urinalysis"),
            rt(7, "Urinalysis Microscopy", true, "Urinalysis"),
        ];
        let ann = vec![date(2022, 7, 15, true), date(2022, 7, 15, true)];
        let e2 = || vec![empty(), empty()];
        let mut b = block(3, "Frankie", [e2(), e2(), ann, e2()]);
        b.remarks = vec!["3rd UM/u411 (2 Sep 2024)".into()];
        let p = run(&[sheet("Urinalysis", tests.clone(), vec![b])]);
        assert_eq!(p.data.records.len(), 4);
        for r in &p.data.records {
            let want = if r.completed_on == ymd(2022, 7, 15) { (2, false) } else { (4, true) };
            assert_eq!((r.stage_index, r.unconfirmed), want, "{r:?}");
        }

        // Row exactly one year before a 3rd Annual -> the row is the 2nd Annual.
        let mut b = block(3, "Mashaida", [e2(), e2(), vec![date(2023, 9, 1, true), empty()], e2()]);
        b.remarks = vec!["3rd UM/u411 (2 Sep 2024)".into()];
        let p = run(&[sheet("Urinalysis", tests, vec![b])]);
        let mut r: Vec<&Record> = p.data.records.iter().collect();
        r.sort_by_key(|r| r.completed_on);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].stage_index, r[1].stage_index), (3, 4));
    }

    #[test]
    fn ordinal_clause_future_or_unparseable_flags() {
        // (c) future clause date.
        let p = hema_ruby(ymd(2025, 5, 2), "Cell-Dyn Ruby 2nd annual 2 may 2027");
        assert_eq!(p.data.records.len(), 1);
        assert_eq!(p.data.records[0].stage_index, 2);
        assert_eq!(p.report.flagged_stage, ["Frankie – Hematology: check annual numbering"]);
        // (c) date-like text that does not parse.
        let p = hema_ruby(ymd(2025, 5, 2), "Cell-Dyn Ruby 2nd annual (31/06/2025)");
        assert_eq!(p.data.records.len(), 1);
        assert_eq!(p.data.records[0].stage_index, 2);
        assert_eq!(p.report.flagged_stage.len(), 1);
    }

    #[test]
    fn skipped_dropped_when_merged_block_has_that_stage() {
        let chem = sheet(
            "Chemistry & Immunology",
            vec![rt(5, "Abbott Architect", true, "Chemistry & Immunology")],
            vec![block1(3, "Dan", [date(2024, 1, 1, true), date(2024, 7, 1, true), empty(), empty()])],
        );
        let hema = sheet(
            "Hematology",
            vec![rt(5, "Abbott Architect", true, "Chemistry & Immunology")],
            vec![block1(3, "Dan", [date(2024, 1, 1, true), text("-"), text("-"), empty()])],
        );
        let p = run(&[chem, hema]);
        let stages: Vec<u32> = p.data.skipped.iter().map(|s| s.stage_index).collect();
        assert_eq!(stages, [2], "semi-annual has a real date in Chemistry; only the annual skip stays");
    }

    #[test]
    fn unassigned_pair_gets_no_excel_override_and_report_lists() {
        let mut a = block1(3, "Alice", [date(2024, 7, 2, true), empty(), empty(), date(2027, 1, 1, false)]);
        a.remarks = vec!["Not assigned to Urinalysis".into()];
        let mut b = block1(7, "Bob", [date(2024, 7, 2, true), empty(), empty(), empty()]);
        b.remarks = vec!["Left the company".into()];
        let p = run(&[one_test("Urinalysis", true, vec![a, b])]);
        assert!(p.data.due_overrides.is_empty());
        assert!(p.report.excel_overrides.is_empty());
        assert_eq!(p.report.unassigned, ["Alice – Urinalysis"]);
        assert_eq!(p.report.removed, ["Bob"]);
        let t = p.report.to_text();
        assert!(t.contains("Removed staff (1)") && t.contains("Unassigned (person – section) (1)"));
        assert!(t.contains("Planned dates (0)"));
    }

    #[test]
    fn ordinal_stopwords_do_not_match() {
        let (idx, flag) = annual_indices(&["2nd test to HIV".to_string()], &["HIV testing", "Toxicology"]);
        assert_eq!((idx, flag), (vec![3, 2], false));
    }

    #[test]
    fn unticked_future_date_is_planned_override() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(3, "Alice", [date(2026, 6, 1, true), date(2026, 12, 1, false), empty(), empty()])],
        )];
        let p = run(&sheets);
        let (s, t) = (staff_id(&p, "Alice"), test_id(&p, "Serology", "Test A"));
        assert_eq!(recs(&p, &s, &t).len(), 1);
        assert_eq!(p.data.due_overrides.len(), 1);
        let o = &p.data.due_overrides[0];
        assert_eq!((o.staff_id.as_str(), o.test_id.as_str()), (s.as_str(), t.as_str()));
        assert_eq!((o.due_on, o.source), (ymd(2026, 12, 1), OverrideSource::Excel));
        assert_eq!(p.report.planned, 1);
    }

    #[test]
    fn dash_in_one_row_skips_stage() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(3, "Alice", [date(2024, 7, 2, true), text("-"), empty(), empty()])],
        )];
        let p = run(&sheets);
        assert_eq!(p.data.skipped.len(), 1);
        assert_eq!(p.data.skipped[0].stage_index, 1);
        let t = test_id(&p, "Serology", "Test A");
        assert_eq!(p.data.staff[0].assignments[&t].state, AssignState::Assigned);
    }

    #[test]
    fn all_dash_is_na() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(3, "Alice", [text("-"), text("NA"), text("N/A"), empty()])],
        )];
        let p = run(&sheets);
        assert!(p.data.skipped.is_empty());
        let t = test_id(&p, "Serology", "Test A");
        assert_eq!(p.data.staff[0].assignments[&t].state, AssignState::Na);
        assert!(p.data.notes.is_empty());
    }

    #[test]
    fn other_text_becomes_note() {
        let mut b = block1(3, "Alice", [date(2024, 7, 2, true), text("Not Done"), text("TO RETRAIN"), empty()]);
        b.remarks = vec!["printed".into()];
        let sheets = vec![one_test("Serology", true, vec![b])];
        let p = run(&sheets);
        let (s, t) = (staff_id(&p, "Alice"), test_id(&p, "Serology", "Test A"));
        assert_eq!(p.data.notes.len(), 2);
        let cell = p.data.notes.iter().find(|n| n.test_id.is_some()).unwrap();
        assert_eq!((cell.staff_id.as_str(), cell.test_id.as_deref()), (s.as_str(), Some(t.as_str())));
        assert_eq!(cell.text, "1st Annual: TO RETRAIN");
        assert_eq!((cell.by.as_str(), cell.at), ("excel-import", now()));
        let rem = p.data.notes.iter().find(|n| n.test_id.is_none()).unwrap();
        assert_eq!(rem.text, "printed");
        assert_eq!(rem.section_id.as_deref(), Some(p.data.sections[0].id.as_str()));
        assert_eq!(p.report.notes, 2);
        assert_eq!(p.data.records.len(), 1);
    }

    #[test]
    fn not_assigned_remark_unassigns_keeps_records() {
        let mut b = block1(3, "Alice", [date(2024, 7, 2, true), empty(), empty(), empty()]);
        b.remarks = vec!["Not assigned to Urinalysis".into()];
        let c = block1(7, "Bob", [date(2024, 7, 2, true), text("No longer in Urinalysis section"), empty(), empty()]);
        let sheets = vec![one_test("Urinalysis", true, vec![b, c])];
        let p = run(&sheets);
        assert_eq!(p.data.records.len(), 2);
        assert!(p.data.staff.iter().all(|s| s.assignments.is_empty()));
        assert!(p.data.staff.iter().all(|s| s.status == StaffStatus::Active));
    }

    #[test]
    fn left_company_removes_staff() {
        let mut a = block1(3, "Alice", [date(2024, 7, 2, true), empty(), empty(), empty()]);
        a.remarks = vec!["Left the company".into()];
        let mut b = block1(7, "Bob", [date(2024, 7, 2, true), empty(), empty(), empty()]);
        b.remarks = vec!["No longer working at Eurofins".into()];
        let c = block1(11, "Carol", [date(2024, 7, 2, true), empty(), empty(), empty()]);
        let sheets = vec![one_test("Serology", true, vec![a, b, c])];
        let p = run(&sheets);
        let status = |n: &str| p.data.staff.iter().find(|s| s.name == n).unwrap().status;
        assert_eq!(status("Alice"), StaffStatus::Removed);
        assert_eq!(status("Bob"), StaffStatus::Removed);
        assert_eq!(status("Carol"), StaffStatus::Active);
        assert_eq!(p.data.records.len(), 3);
    }

    #[test]
    fn annual_ordinal_from_remark() {
        let tests = vec![rt(5, "Reticulocyte Count", true, "Hematology"), rt(7, "Hemoglobin", true, "Hematology")];
        let ann = |m| vec![date(2024, m, 27, true), date(2024, m, 27, true)];
        let mut a = block(3, "Alice", [vec![empty(), empty()], vec![empty(), empty()], ann(3), vec![empty(), empty()]]);
        a.remarks = vec!["RET 2nd Annual DONE on 27 Mar 2024".into()];
        let mut b = block(7, "Bob", [vec![empty(), empty()], vec![empty(), empty()], ann(4), vec![empty(), empty()]]);
        b.remarks = vec!["3rd Annual xyz".into()];
        let sheets = vec![sheet("Hematology", tests, vec![a, b])];
        let p = run(&sheets);
        let (alice, ret, hb) =
            (staff_id(&p, "Alice"), test_id(&p, "Hematology", "Reticulocyte Count"), test_id(&p, "Hematology", "Hemoglobin"));
        let r = recs(&p, &alice, &ret);
        assert_eq!((r[0].stage_index, r[0].stage_label.as_str()), (3, "2nd Annual"));
        let r = recs(&p, &alice, &hb);
        assert_eq!((r[0].stage_index, r[0].stage_label.as_str()), (2, "1st Annual"));
        let bob = staff_id(&p, "Bob");
        assert_eq!(recs(&p, &bob, &ret)[0].stage_index, 2);
        assert_eq!(p.report.flagged_stage, ["Bob – Hematology: check annual numbering"]);
    }

    #[test]
    fn annual_ordinal_real_remark_shapes() {
        let hema = ["Abbott Architect", "Cell-DYN Ruby", "ESR ALIFAX Roller 20 PN", "ESR ALIFAX THL", "ABO Manual", "ABO Echo Lumina"];
        let r = |s: &str| vec![s.to_string()];
        let (idx, flag) = annual_indices(&r("Cell-Dyn Ruby & ESR 2nd Annual DONE on 02 Apr 2024"), &hema);
        assert_eq!((idx, flag), (vec![2, 3, 3, 3, 2, 2], false));
        let (idx, _) = annual_indices(&r("ABO Manual 2nd Annual DONE on 13 Dec 2024"), &hema);
        assert_eq!(idx, [2, 2, 2, 2, 3, 2], "ABO alone also names ABO Echo; the narrower match wins");
        let uri = ["Atellica", "Urinalysis on cobas u 411 system", "Urinalysis Microscopy"];
        assert_eq!(annual_indices(&r("3rd UM/u411 (2 Sep 2024)"), &uri), (vec![2, 4, 4], false));
        assert_eq!(annual_indices(&r("2 may 2025, 2nd annual (u411 & microscopy)"), &uri), (vec![2, 3, 3], false));
        assert_eq!(annual_indices(&r("2nd (22 May 2023)"), &uri), (vec![2, 2, 2], true), "month is not a test");
        assert_eq!(find_ordinal("e411 5th"), Some(5));
        assert_eq!(find_ordinal("e411 (07/8/2024)"), None);
        assert_eq!(find_ordinal("10 thyroid"), None);
    }

    fn due_case(annual: StageCell, excel: NaiveDate) -> ImportPlan {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(
                3,
                "Alice",
                [empty(), empty(), annual, StageCell { cell: Cell::Date(excel), ticked: false }],
            )],
        )];
        run(&sheets)
    }

    #[test]
    fn excel_due_within_tolerance_ignored() {
        let sheets = vec![one_test(
            "Serology",
            true,
            vec![block1(
                3,
                "Alice",
                [date(2024, 1, 18, true), date(2024, 7, 18, true), empty(), date(2025, 8, 10, false)],
            )],
        )];
        let p = run(&sheets);
        assert!(p.data.due_overrides.is_empty());
        assert!(p.report.excel_overrides.is_empty() && p.report.excel_earlier.is_empty());
    }

    #[test]
    fn excel_due_later_beyond_tolerance_overrides() {
        // 1st Annual done 7 Mar 2025 -> 2nd Annual computed 7 Mar 2026.
        let p = due_case(date(2025, 3, 7, true), ymd(2027, 3, 24));
        assert_eq!(p.data.due_overrides.len(), 1);
        let o = &p.data.due_overrides[0];
        assert_eq!((o.due_on, o.source), (ymd(2027, 3, 24), OverrideSource::Excel));
        assert_eq!(p.report.excel_overrides.len(), 1);
        assert!(p.report.excel_overrides[0].contains("Alice"));
        assert!(p.report.excel_overrides[0].contains("24 Mar 2027"));
        assert!(p.report.excel_earlier.is_empty());
    }

    #[test]
    fn excel_due_earlier_reported_only() {
        let p = due_case(date(2025, 3, 7, true), ymd(2025, 12, 1));
        assert!(p.data.due_overrides.is_empty());
        assert_eq!(p.report.excel_earlier.len(), 1);
        assert!(p.report.excel_earlier[0].contains("1 Dec 2025"));
        assert!(p.report.excel_overrides.is_empty());
    }

    #[test]
    fn cross_section_column_merges_into_chemistry() {
        let chem = sheet(
            "Chemistry & Immunology",
            vec![rt(5, "Abbott Architect", true, "Chemistry & Immunology")],
            vec![block1(3, "Dan", [date(2024, 1, 1, true), empty(), empty(), empty()])],
        );
        let hema = sheet(
            "Hematology",
            vec![
                rt(5, "abbott  ARCHITECT", true, "Chemistry & Immunology"),
                rt(7, "Alinity", true, "Chemistry & Immunology"),
                rt(9, "CBC", true, "Hematology"),
            ],
            vec![block(
                3,
                "Dan",
                [
                    vec![date(2024, 1, 1, false), date(2024, 2, 2, true), date(2024, 3, 3, true)],
                    vec![empty(), empty(), empty()],
                    vec![empty(), empty(), empty()],
                    vec![empty(), empty(), empty()],
                ],
            )],
        );
        // Sheets given out of SHEETS order: sections still follow SHEETS.
        let p = run(&[hema, chem]);
        let names: Vec<&str> = p.data.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Chemistry & Immunology", "Hematology"]);
        let chem_tests: Vec<&str> = p.data.sections[0].tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(chem_tests, ["Abbott Architect", "Alinity"]);
        let hema_tests: Vec<&str> = p.data.sections[1].tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(hema_tests, ["CBC"]);
        let dan = staff_id(&p, "Dan");
        let abbott = recs(&p, &dan, &test_id(&p, "Chemistry & Immunology", "Abbott Architect"));
        assert_eq!(abbott.len(), 1, "same date in both tabs is kept once");
        assert!(!abbott[0].unconfirmed, "ticked copy preferred");
        assert_eq!(recs(&p, &dan, &test_id(&p, "Chemistry & Immunology", "Alinity")).len(), 1);
        assert_eq!(p.data.records.len(), 3);
    }

    #[test]
    fn duplicate_headers_suffixed() {
        let h = "SARS-CoV-2 detection by TaqPath";
        let tests = vec![rt(9, h, false, "Molecular Biology"), rt(5, h, false, "Molecular Biology"), rt(7, "PCR", false, "Molecular Biology"), rt(11, h, false, "Molecular Biology")];
        let sheets = vec![sheet("Molecular Biology", tests, vec![])];
        let p = run(&sheets);
        let t: Vec<(&str, u32)> = p.data.sections[0].tests.iter().map(|t| (t.name.as_str(), t.order)).collect();
        assert_eq!(
            t,
            [
                (h, 0),
                ("PCR", 1),
                ("SARS-CoV-2 detection by TaqPath (2)", 2),
                ("SARS-CoV-2 detection by TaqPath (3)", 3)
            ]
        );
    }

    #[test]
    fn report_text_lists_sections() {
        let r = ImportReport {
            sheets: 6,
            staff: 2,
            records: 10,
            excel_overrides: vec!["Alice – Serology / Test A: x".into()],
            merges: vec!["Shawn: a, b".into()],
            ..ImportReport::default()
        };
        let t = r.to_text();
        let counts = t.find("Records: 10").unwrap();
        let later = t.find("Excel due dates used (later than calculated)").unwrap();
        let earlier = t.find("Excel due dates earlier than calculated").unwrap();
        let flagged = t.find("Check annual numbering").unwrap();
        let merges = t.find("Merged names").unwrap();
        assert!(counts < later && later < earlier && earlier < flagged && flagged < merges);
        assert!(t.contains("Alice – Serology / Test A: x\n") || t.contains("Alice – Serology / Test A: x\r\n"));
        assert!(t.contains("Shawn: a, b"));
    }

    #[test]
    #[ignore]
    fn real_workbook_import() {
        let path = std::env::var("TRACKER_XLSM").expect("set TRACKER_XLSM to the workbook path");
        let sheets = crate::import::excel::read_workbook(std::path::Path::new(&path)).unwrap();
        let total_blocks: usize = sheets.iter().map(|s| s.blocks.len()).sum();
        let groups = propose_merges(&sheets);
        let p = build_import(&sheets, &groups, &opts());
        let text = p.report.to_text();
        std::fs::create_dir_all("target").unwrap();
        std::fs::write("target/import-report-real.txt", &text).unwrap();
        println!("blocks={total_blocks}\n{text}");        assert!(p.data.staff.len() < total_blocks, "staff {} vs blocks {total_blocks}", p.data.staff.len());
        assert!(p.data.records.len() > 500, "records = {}", p.data.records.len());
        assert!(!text.trim().is_empty());
    }
}
