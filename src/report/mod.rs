//! The audit report. It reads from the whole to the detail: figures by section
//! (Summary), everyone against every test system (Matrix), then what stands
//! behind them: how sound the records are (Data checks), how due
//! dates come about (Schedule) and who changed what (Change log). Each part
//! can be left out. The content is put together here; `xlsx` and `pdf` write
//! it out.
//!
//! Figures follow the Summary page: an assessment is one assigned test system
//! for one active person, and is *current* when completed and not overdue.

mod pdf;
mod xlsx;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::NaiveDate;

use crate::dates::fmt_date;
use crate::model::*;
use crate::query::{due_items, DueItem, Index};
use crate::schedule::{stage_label, DueStatus};

/// The parts a report is made of, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Summary,
    Matrix,
    Checks,
    Schedule,
    ChangeLog,
}

impl Part {
    pub const ALL: [Part; 5] = [
        Part::Summary,
        Part::Matrix,
        Part::Checks,
        Part::Schedule,
        Part::ChangeLog,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Part::Summary => "Summary",
            Part::Matrix => "Matrix",
            Part::Checks => "Data checks",
            Part::Schedule => "Schedule",
            Part::ChangeLog => "Change log",
        }
    }

    /// What it holds, in a few words.
    pub fn about(self) -> &'static str {
        match self {
            Part::Summary => "Figures by section, and sign-off",
            Part::Matrix => "Everyone against every test system",
            Part::Checks => "Gaps in the records to resolve or explain",
            Part::Schedule => "The rules that set the due dates",
            Part::ChangeLog => "Who changed what, and when",
        }
    }
}

/// The dates each cell of the matrix shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatrixDates {
    #[default]
    NextDue,
    Completed,
    Both,
}

impl MatrixDates {
    pub const ALL: [MatrixDates; 3] = [
        MatrixDates::NextDue,
        MatrixDates::Completed,
        MatrixDates::Both,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MatrixDates::NextDue => "Next due",
            MatrixDates::Completed => "Completed",
            MatrixDates::Both => "Both",
        }
    }
}

/// What the report covers. Standing is always as of the day it is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub parts: BTreeSet<Part>,
    pub dates: MatrixDates,
    pub section: Option<Id>,
    /// Also cover people who have been removed.
    pub removed: bool,
    /// The change log's dates.
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

impl Default for Options {
    /// Everything but the change log, for current staff.
    fn default() -> Options {
        Options {
            parts: Part::ALL
                .into_iter()
                .filter(|p| *p != Part::ChangeLog)
                .collect(),
            dates: MatrixDates::default(),
            section: None,
            removed: false,
            from: None,
            to: None,
        }
    }
}

impl Options {
    pub fn has(&self, part: Part) -> bool {
        self.parts.contains(&part)
    }

    /// Why a report can't be made from these, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        let backwards = matches!((self.from, self.to), (Some(f), Some(t)) if f > t);
        if self.parts.is_empty() {
            Some("Tick at least one part to include.")
        } else if backwards && self.has(Part::ChangeLog) {
            Some("The From date is after the To date.")
        } else {
            None
        }
    }

    fn in_period(&self, d: NaiveDate) -> bool {
        self.from.is_none_or(|f| d >= f) && self.to.is_none_or(|t| d <= t)
    }

    fn has_section(&self, id: &str) -> bool {
        self.section.as_deref().is_none_or(|s| s == id)
    }

    fn has_test(&self, data: &Data, test_id: &str) -> bool {
        self.section.is_none()
            || data
                .test(test_id)
                .is_some_and(|(s, _)| self.has_section(&s.id))
    }

    fn period_text(&self) -> String {
        match (self.from, self.to) {
            (None, None) => "All dates".to_owned(),
            (Some(f), None) => format!("From {}", fmt_date(f)),
            (None, Some(t)) => format!("Up to {}", fmt_date(t)),
            (Some(f), Some(t)) => format!("{} to {}", fmt_date(f), fmt_date(t)),
        }
    }

    fn section_text(&self, data: &Data) -> String {
        match &self.section {
            None => "All sections".to_owned(),
            Some(id) => data.section(id).map(|s| s.name.clone()).unwrap_or_default(),
        }
    }
}

/// Where an assessment stands, as on the Summary page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    UpToDate,
    DueSoon,
    Overdue,
    NotDone,
}

impl Standing {
    fn of(i: &DueItem) -> Standing {
        match i.status {
            DueStatus::Overdue => Standing::Overdue,
            DueStatus::DueSoon => Standing::DueSoon,
            _ if i.info.last_completed_on.is_none() => Standing::NotDone,
            _ => Standing::UpToDate,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Standing::UpToDate => "Up to date",
            Standing::DueSoon => "Due soon",
            Standing::Overdue => "Overdue",
            Standing::NotDone => "Not done yet",
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Tally {
    total: i64,
    current: i64,
    up_to_date: i64,
    due_soon: i64,
    overdue: i64,
    not_done: i64,
}

impl Tally {
    fn of(items: &[&DueItem]) -> Tally {
        let mut t = Tally::default();
        for i in items {
            t.total += 1;
            let s = Standing::of(i);
            match s {
                Standing::UpToDate => t.up_to_date += 1,
                Standing::DueSoon => t.due_soon += 1,
                Standing::Overdue => t.overdue += 1,
                Standing::NotDone => t.not_done += 1,
            }
            if i.info.last_completed_on.is_some() && s != Standing::Overdue {
                t.current += 1;
            }
        }
        t
    }

    fn percent(&self) -> Option<f64> {
        (self.total > 0).then(|| self.current as f64 / self.total as f64)
    }
}

fn distinct_staff(items: &[&DueItem]) -> usize {
    let ids: BTreeSet<&str> = items.iter().map(|i| i.staff_id.as_str()).collect();
    ids.len()
}

/// What a table cell holds; each output shows it its own way.
enum Cell {
    Text(String),
    Date(Option<NaiveDate>),
    Stamp(Ts),
    Num(i64),
    Percent(Option<f64>),
    Standing(Standing),
}

impl Cell {
    /// As words, for outputs without cell types.
    fn words(&self) -> String {
        match self {
            Cell::Text(s) => s.clone(),
            Cell::Date(d) => d.map(fmt_date).unwrap_or_default(),
            Cell::Stamp(t) => fmt_stamp(*t),
            Cell::Num(n) => n.to_string(),
            Cell::Percent(p) => p.map(|p| format!("{:.0}%", p * 100.0)).unwrap_or_default(),
            Cell::Standing(s) => s.label().to_owned(),
        }
    }
}

fn fmt_stamp(t: Ts) -> String {
    t.format("%-d %b %Y %H:%M").to_string()
}

fn text(s: impl Into<String>) -> Cell {
    Cell::Text(s.into())
}

/// Tinted cells: a standing's colours behind other words.
struct Tinted {
    cell: Cell,
    tint: Option<Standing>,
}

impl From<Cell> for Tinted {
    fn from(cell: Cell) -> Tinted {
        let tint = match cell {
            Cell::Standing(s) => Some(s),
            _ => None,
        };
        Tinted { cell, tint }
    }
}

#[derive(Default)]
struct Table {
    /// (heading, width in characters)
    heads: Vec<(String, f64)>,
    rows: Vec<Vec<Tinted>>,
    /// The sheet is this one table: it can be filtered, and its headings stay.
    main: bool,
    /// Too many columns for a page may continue below, after the first.
    split: bool,
    /// On paper, the first column becomes a band over the rows sharing it.
    grouped: bool,
    /// The last row is a total.
    total: bool,
}

impl Table {
    fn new(heads: &[(&str, f64)], rows: Vec<Vec<Cell>>) -> Table {
        Table {
            heads: heads.iter().map(|(h, w)| ((*h).to_owned(), *w)).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(Tinted::from).collect())
                .collect(),
            ..Table::default()
        }
    }
}

enum Block {
    Heading(String),
    /// Labels and values.
    Facts(Vec<(&'static str, String)>),
    Table(Table),
    /// What the tints mean.
    Key,
    /// The other parts and what they hold.
    Contents,
    /// A line to sign for each role.
    SignOff([&'static str; 2]),
}

struct Sheet {
    part: Part,
    title: &'static str,
    /// Says what it shows, under the title.
    intro: String,
    blocks: Vec<Block>,
}

impl Sheet {
    fn new(part: Part, intro: impl Into<String>, blocks: Vec<Block>) -> Sheet {
        Sheet {
            part,
            title: part.label(),
            intro: intro.into(),
            blocks,
        }
    }
}

/// The whole report, ready to write out.
struct Document {
    /// Repeated on every printed page.
    stamp: String,
    author: String,
    sheets: Vec<Sheet>,
}

const TITLE: &str = "Competency audit report";

/// (fill, text) behind a standing, as `0xRRGGBB`.
fn tint_rgb(s: Standing) -> (u32, u32) {
    match s {
        Standing::UpToDate => (0xE3EFE0, 0x2F6B3A),
        Standing::DueSoon => (0xF6EBC8, 0x7A5A0B),
        Standing::Overdue => (0xF6DEDA, 0x9B2C1F),
        Standing::NotDone => (0xEDEBE4, 0x5F5E59),
    }
}

const STANDINGS: [Standing; 4] = [
    Standing::UpToDate,
    Standing::DueSoon,
    Standing::Overdue,
    Standing::NotDone,
];

/// Everything the parts share.
struct Report<'a> {
    data: &'a Data,
    idx: Index<'a>,
    opts: &'a Options,
    today: NaiveDate,
    /// The assessments in scope, by section, test, then name.
    items: Vec<DueItem>,
    /// Each live or retired test's place: (section order, test order).
    place: HashMap<&'a str, (u32, u32)>,
}

impl<'a> Report<'a> {
    fn new(data: &'a Data, opts: &'a Options, today: NaiveDate) -> Report<'a> {
        let idx = Index::build(data);
        let place: HashMap<&str, (u32, u32)> = data
            .sections
            .iter()
            .flat_map(|s| {
                s.tests
                    .iter()
                    .map(move |t| (t.id.as_str(), (s.order, t.order)))
            })
            .collect();
        let at = |test: &str| place.get(test).copied().unwrap_or((u32::MAX, u32::MAX));
        let mut items: Vec<DueItem> = due_items(data, &idx, today)
            .into_iter()
            .filter(|i| opts.has_section(&i.section_id))
            .collect();
        items.sort_by_key(|i| (at(&i.test_id), i.staff_name.to_lowercase()));
        Report {
            data,
            idx,
            opts,
            today,
            items,
            place,
        }
    }

    fn at(&self, test: &str) -> (u32, u32) {
        self.place
            .get(test)
            .copied()
            .unwrap_or((u32::MAX, u32::MAX))
    }

    /// (section, test) names; retired tests say so.
    fn test_names(&self, id: &str) -> (String, String) {
        match self.data.test(id) {
            Some((s, t)) if t.retired || s.retired => {
                (s.name.clone(), format!("{} (retired)", t.name))
            }
            Some((s, t)) => (s.name.clone(), t.name.clone()),
            None => (String::new(), "(deleted test system)".to_owned()),
        }
    }

    /// The person's name; removed staff say so.
    fn staff_name(&self, id: &str) -> String {
        match self.data.staff(id) {
            Some(s) if s.status != StaffStatus::Active => format!("{} (removed)", s.name),
            Some(s) => s.name.clone(),
            None => "(deleted person)".to_owned(),
        }
    }

    /// Whether the report covers this person: only active staff, unless
    /// removed ones are asked for.
    fn covers(&self, staff_id: &str) -> bool {
        self.opts.removed
            || self
                .data
                .staff(staff_id)
                .is_some_and(|s| s.status == StaffStatus::Active)
    }

    fn record_label(&self, r: &Record) -> String {
        if r.stage_label.is_empty() {
            stage_label(self.data.rule_for_test(&r.test_id), r.stage_index)
        } else {
            r.stage_label.clone()
        }
    }

    /// The record a pair's last completion date comes from.
    fn last_record(&self, staff: &str, test: &str) -> Option<&'a Record> {
        self.idx
            .pair(staff, test)
            .records
            .into_iter()
            .max_by_key(|r| (r.completed_on, r.stage_index))
    }

    fn unconfirmed(&self, i: &DueItem) -> bool {
        self.last_record(&i.staff_id, &i.test_id)
            .is_some_and(|r| r.unconfirmed)
    }

    fn live_sections(&self) -> Vec<&'a Section> {
        self.data
            .sections_sorted()
            .into_iter()
            .filter(|s| !s.retired && self.opts.has_section(&s.id))
            .collect()
    }

    /// A section's live tests, in order.
    fn tests_of(&self, sec: &'a Section) -> Vec<&'a TestSystem> {
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired).collect();
        tests.sort_by_key(|t| t.order);
        tests
    }

    fn summary(&self, by: &str, now: Ts, checks: usize) -> Sheet {
        let data = self.data;
        let all: Vec<&DueItem> = self.items.iter().collect();
        let figures = |name: &str, tests: usize, items: &[&DueItem]| {
            let t = Tally::of(items);
            vec![
                text(name),
                Cell::Num(tests as i64),
                Cell::Num(distinct_staff(items) as i64),
                Cell::Num(t.total),
                Cell::Num(t.current),
                Cell::Percent(t.percent()),
                Cell::Num(t.up_to_date),
                Cell::Num(t.due_soon),
                Cell::Num(t.overdue),
                Cell::Num(t.not_done),
            ]
        };
        let sections = self.live_sections();
        let mut rows: Vec<Vec<Cell>> = sections
            .iter()
            .map(|sec| {
                let of = |i: &&&DueItem| i.section_id == sec.id;
                let items: Vec<&DueItem> = all.iter().filter(of).copied().collect();
                figures(&sec.name, self.tests_of(sec).len(), &items)
            })
            .collect();
        let total = self.opts.section.is_none();
        if total {
            let tests = sections.iter().map(|s| self.tests_of(s).len()).sum();
            rows.push(figures("All sections", tests, &all));
        }
        let mut table = Table::new(
            &[
                ("Section", 30.0),
                ("Test systems", 14.0),
                ("Staff", 10.0),
                ("Assessments", 14.0),
                ("Current", 11.0),
                ("% current", 12.0),
                ("Up to date", 12.0),
                ("Due soon", 11.0),
                ("Overdue", 11.0),
                ("Not done yet", 14.0),
            ],
            rows,
        );
        table.total = total;
        let overdue: Vec<&&DueItem> = all
            .iter()
            .filter(|i| Standing::of(i) == Standing::Overdue)
            .collect();
        let noted = overdue.iter().filter(|i| i.delay_note.is_some()).count();
        let unconfirmed = all.iter().filter(|i| self.unconfirmed(i)).count();
        let mut notes = vec![
            (
                "Overdue with a reason",
                format!("{noted} of {}", overdue.len()),
            ),
            (
                "Not confirmed",
                format!(
                    "{unconfirmed} of {} rest on an imported date nobody has confirmed",
                    all.len()
                ),
            ),
        ];
        if self.opts.has(Part::Checks) {
            notes.push(("Data checks", format!("{checks} to resolve or explain")));
        }
        let mut blocks = vec![
            Block::Facts(vec![
                (
                    "Covers",
                    format!(
                        "{}, {}",
                        self.opts.section_text(data),
                        if self.opts.removed {
                            "active and removed staff (figures count active staff only)"
                        } else {
                            "active staff only"
                        }
                    ),
                ),
                ("Status as of", fmt_date(self.today)),
                ("Made by", format!("{by}, {}", fmt_stamp(now))),
                (
                    "Data last saved",
                    format!("{}, by {}", fmt_stamp(data.saved_at), data.saved_by),
                ),
            ]),
            Block::Heading("By section".to_owned()),
            Block::Table(table),
            Block::Heading("To note".to_owned()),
            Block::Facts(notes),
        ];
        if self.opts.parts.len() > 1 {
            blocks.push(Block::Heading("In this report".to_owned()));
            blocks.push(Block::Contents);
        }
        blocks.push(Block::Heading("Sign-off".to_owned()));
        blocks.push(Block::SignOff(["Prepared by", "Reviewed by"]));
        Sheet {
            title: TITLE,
            ..Sheet::new(
                Part::Summary,
                format!(
                    "An assessment is one test system assigned to one person. It is current when done and not overdue, and due soon within {} days.",
                    data.settings.due_soon_days
                ),
                blocks,
            )
        }
    }

    /// Each section as people against test systems, with the dates picked in
    /// each cell, tinted by standing.
    fn matrix(&self) -> Sheet {
        let dates = self.opts.dates;
        // Two dates make a wider cell.
        let width = if dates == MatrixDates::Both {
            18.0
        } else {
            15.0
        };
        let mut blocks = vec![Block::Key];
        for sec in self.live_sections() {
            let tests = self.tests_of(sec);
            let mut staff: Vec<&Staff> = self
                .data
                .staff
                .iter()
                // Nothing is due from removed staff, only done by them.
                .filter(|s| {
                    s.status == StaffStatus::Active
                        || (self.opts.removed && dates != MatrixDates::NextDue)
                })
                .filter(|s| tests.iter().any(|t| s.assignments.contains_key(&t.id)))
                .collect();
            if staff.is_empty() {
                continue;
            }
            staff.sort_by_key(|s| (s.status != StaffStatus::Active, s.name.to_lowercase()));
            let items: HashMap<(&str, &str), &DueItem> = self
                .items
                .iter()
                .filter(|i| i.section_id == sec.id)
                .map(|i| ((i.staff_id.as_str(), i.test_id.as_str()), i))
                .collect();
            // Names the section on every page the table runs on to.
            let mut heads = vec![(sec.name.clone(), 26.0)];
            heads.extend(tests.iter().map(|t| (t.name.clone(), width)));
            let rows = staff
                .into_iter()
                .map(|s| {
                    let mut row = vec![Tinted::from(text(self.staff_name(&s.id)))];
                    row.extend(tests.iter().map(|t| {
                        let state = s.assignments.get(&t.id).map(|a| a.state);
                        match (items.get(&(s.id.as_str(), t.id.as_str())), state) {
                            (Some(i), _) => Tinted {
                                cell: self.matrix_cell(i),
                                tint: Some(Standing::of(i)),
                            },
                            (None, Some(AssignState::Na)) => text("N/A").into(),
                            // Removed: what they had last done.
                            (None, Some(AssignState::Assigned)) => {
                                let last = self.last_record(&s.id, &t.id);
                                Cell::Date(last.map(|r| r.completed_on)).into()
                            }
                            (None, None) => text("").into(),
                        }
                    }));
                    row
                })
                .collect();
            blocks.push(Block::Heading(sec.name.clone()));
            blocks.push(Block::Table(Table {
                heads,
                rows,
                split: true,
                ..Table::default()
            }));
        }
        let what = match dates {
            MatrixDates::NextDue => "when it is next due",
            MatrixDates::Completed => "when it was last completed",
            MatrixDates::Both => "last completed, then next due",
        };
        Sheet::new(
            Part::Matrix,
            format!("Each cell: {what}. * = from an imported date, not confirmed. Blank = not assigned."),
            blocks,
        )
    }

    fn matrix_cell(&self, i: &DueItem) -> Cell {
        let star = if self.unconfirmed(i) { "*" } else { "" };
        let done = i
            .info
            .last_completed_on
            .map_or("Not done".to_owned(), |d| format!("{}{star}", fmt_date(d)));
        // A single plain date stays a date in Excel.
        let single = match self.opts.dates {
            MatrixDates::NextDue => i.info.due_on,
            MatrixDates::Completed => i.info.last_completed_on,
            MatrixDates::Both => None,
        };
        if single.is_some() && star.is_empty() {
            return Cell::Date(single);
        }
        match (self.opts.dates, i.info.due_on) {
            (MatrixDates::NextDue, Some(d)) => text(format!("{}{star}", fmt_date(d))),
            (MatrixDates::NextDue, None) if i.info.last_completed_on.is_some() => {
                text("No due date")
            }
            (MatrixDates::Both, Some(d)) => text(format!("{done}\nDue {}", fmt_date(d))),
            _ => text(done),
        }
    }
    /// Gaps and oddities in the records, and how many.
    fn checks(&self) -> (Sheet, usize) {
        let data = self.data;
        let mut rows: Vec<(&str, Vec<Cell>)> = Vec::new();
        let mut add =
            |check: &'static str, section: &str, test: &str, staff: &str, detail: String| {
                let row = vec![
                    text(check),
                    text(section),
                    text(test),
                    text(staff),
                    text(detail),
                ];
                rows.push((check, row));
            };
        let mut records: Vec<&Record> = data
            .records
            .iter()
            .filter(|r| self.opts.has_test(data, &r.test_id) && self.covers(&r.staff_id))
            // Like the rest of the report, not retired tests.
            .filter(|r| {
                data.test(&r.test_id)
                    .is_none_or(|(s, t)| !s.retired && !t.retired)
            })
            .collect();
        records.sort_by_key(|r| {
            (
                self.at(&r.test_id),
                self.staff_name(&r.staff_id).to_lowercase(),
                r.completed_on,
            )
        });
        let mut stages: HashMap<(&str, &str, u32), u32> = HashMap::new();
        for r in &records {
            *stages
                .entry((&r.staff_id, &r.test_id, r.stage_index))
                .or_default() += 1;
        }
        for r in records {
            let staff = data.staff(&r.staff_id);
            let mut found: Vec<(&'static str, String)> = Vec::new();
            let done = format!("{}, {}", self.record_label(r), fmt_date(r.completed_on));
            if r.unconfirmed {
                found.push(("Imported, not confirmed", done.clone()));
            }
            if r.completed_on > self.today {
                found.push(("Completed in the future", done.clone()));
            }
            if let Some(j) = staff.and_then(|s| s.joined).filter(|j| r.completed_on < *j) {
                found.push((
                    "Completed before joining",
                    format!("{done}; joined {}", fmt_date(j)),
                ));
            }
            // Said once per stage, at the first of its records.
            if let Some(n) = stages
                .remove(&(&r.staff_id, &r.test_id, r.stage_index))
                .filter(|n| *n > 1)
            {
                found.push((
                    "Same stage recorded more than once",
                    format!("{}: {n} records", self.record_label(r)),
                ));
            }
            if staff.is_none() || data.test(&r.test_id).is_none() {
                found.push(("Record of something deleted", done.clone()));
            }
            for (check, detail) in found {
                let (section, test) = self.test_names(&r.test_id);
                add(
                    check,
                    &section,
                    &test,
                    &self.staff_name(&r.staff_id),
                    detail,
                );
            }
        }
        let mut people: Vec<&Staff> = data
            .staff
            .iter()
            .filter(|s| s.status == StaffStatus::Active)
            .collect();
        people.sort_by_key(|s| s.name.to_lowercase());
        for s in people {
            let assigned = self.items.iter().any(|i| i.staff_id == s.id);
            if s.joined.is_none() && (assigned || self.opts.section.is_none()) {
                add("No join date", "", "", &s.name, String::new());
            }
            if !assigned && self.opts.section.is_none() {
                add("No test systems assigned", "", "", &s.name, String::new());
            }
        }
        for sec in self.live_sections() {
            for t in self.tests_of(sec) {
                if !self.items.iter().any(|i| i.test_id == t.id) {
                    add(
                        "Nobody assigned to the test system",
                        &sec.name,
                        &t.name,
                        "",
                        String::new(),
                    );
                }
            }
        }
        // Stable: within a check, as found.
        rows.sort_by_key(|r| r.0);
        let found = rows.len();
        let mut t = Table::new(
            &[
                ("Check", 34.0),
                ("Section", 20.0),
                ("Test system", 34.0),
                ("Staff", 26.0),
                ("Detail", 44.0),
            ],
            rows.into_iter().map(|r| r.1).collect(),
        );
        t.main = true;
        t.grouped = true;
        let sheet = Sheet::new(
            Part::Checks,
            "Found automatically. Each is something to resolve or explain, not necessarily an error.",
            vec![Block::Table(t)],
        );
        (sheet, found)
    }

    fn schedule(&self) -> Sheet {
        let s = &self.data.settings;
        let live: Vec<&TestSystem> = self
            .live_sections()
            .into_iter()
            .flat_map(|sec| self.tests_of(sec))
            .collect();
        let rows = self
            .data
            .rules
            .iter()
            .map(|rule| {
                let used: Vec<&str> = live
                    .iter()
                    .filter(|t| self.data.rule_for_test(&t.id).id == rule.id)
                    .map(|t| t.name.as_str())
                    .collect();
                let used = match used.len() {
                    0 => "None".to_owned(),
                    n if n == live.len() => format!("All {n}"),
                    _ => used.join(", "),
                };
                let name = if rule.id == self.data.default_rule_id {
                    format!("{} (default)", rule.name)
                } else {
                    rule.name.clone()
                };
                vec![text(name), text(rule_text(rule)), text(used)]
            })
            .collect();
        Sheet::new(
            Part::Schedule,
            "How due dates are worked out. Each stage counts from the last completion.",
            vec![
                Block::Table(Table::new(
                    &[("Rule", 30.0), ("Stages", 62.0), ("Test systems", 60.0)],
                    rows,
                )),
                Block::Facts(vec![
                    ("Due soon means within", format!("{} days", s.due_soon_days)),
                    (
                        "First one due",
                        s.initial_due_days.map_or("No set date".to_owned(), |n| {
                            format!("{n} days after being assigned")
                        }),
                    ),
                ]),
            ],
        )
    }

    /// Whether the change log may show this entry: nothing about people the
    /// report doesn't cover.
    fn shows(&self, e: &AuditEntry, gone: &[String]) -> bool {
        if self.opts.removed {
            return true;
        }
        match e.refs.get("staff_id") {
            Some(id) => self.covers(id),
            // These name people without pointing at them.
            None => {
                let names = ["history.", "import.", "staff."]
                    .iter()
                    .any(|p| e.action.starts_with(p));
                !(names && gone.iter().any(|n| mentions(&e.detail, n)))
            }
        }
    }

    fn change_log(&self) -> Sheet {
        // People removed, or deleted altogether.
        let mut gone: Vec<String> = self
            .data
            .staff
            .iter()
            .filter(|s| s.status != StaffStatus::Active)
            .map(|s| s.name.clone())
            .collect();
        gone.extend(
            self.data
                .audit
                .iter()
                .filter(|e| e.action == "staff.delete")
                .filter_map(|e| {
                    let rest = e.detail.strip_prefix("Permanently deleted ")?;
                    rest.strip_suffix(" and all their records")
                        .map(str::to_owned)
                }),
        );
        let mut entries: Vec<&AuditEntry> = self
            .data
            .audit
            .iter()
            .filter(|e| self.opts.in_period(e.at.date_naive()))
            .filter(|e| {
                self.opts.section.is_none()
                    || self.opts.section.as_ref() == e.refs.get("section_id")
                    || e.refs
                        .get("test_id")
                        .is_some_and(|t| self.opts.has_test(self.data, t))
            })
            .filter(|e| self.shows(e, &gone))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.at));
        let rows = entries
            .into_iter()
            .map(|e| {
                let test = e.refs.get("test_id").map(|t| self.test_names(t));
                let section = e
                    .refs
                    .get("section_id")
                    .and_then(|s| self.data.section(s))
                    .map(|s| s.name.clone())
                    .or_else(|| test.as_ref().map(|t| t.0.clone()))
                    .unwrap_or_default();
                let staff = e.refs.get("staff_id").map(|s| self.staff_name(s));
                vec![
                    Cell::Stamp(e.at),
                    text(&e.by),
                    text(section),
                    text(test.map(|t| t.1).unwrap_or_default()),
                    text(staff.unwrap_or_default()),
                    text(if e.reverted {
                        format!("{} (reverted later)", e.detail)
                    } else {
                        e.detail.clone()
                    }),
                ]
            })
            .collect();
        let which = if self.opts.section.is_some() {
            " Changes tied to this section only."
        } else {
            ""
        };
        let mut t = Table::new(
            &[
                ("When", 18.0),
                ("By", 14.0),
                ("Section", 20.0),
                ("Test system", 28.0),
                ("Staff", 24.0),
                ("Change", 66.0),
            ],
            rows,
        );
        t.main = true;
        Sheet::new(
            Part::ChangeLog,
            format!("Newest first. {}.{which}", self.opts.period_text()),
            vec![Block::Table(t)],
        )
    }
}

/// "Initial → Semi-annual after 6 months → Annual every 12 months".
fn rule_text(rule: &Rule) -> String {
    let stages: Vec<String> = rule
        .stages
        .iter()
        .map(|s| match s.interval_months {
            None => s.label.clone(),
            Some(n) => {
                let word = if s.repeats { "every" } else { "after" };
                let unit = if n == 1 { "month" } else { "months" };
                format!("{} {word} {n} {unit}", s.label)
            }
        })
        .collect();
    stages.join(" \u{2192} ")
}

/// Whether `text` has `name` as whole words.
fn mentions(text: &str, name: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !name.is_empty()
        && text.match_indices(name).any(|(i, _)| {
            !word(text[..i].chars().next_back()) && !word(text[i + name.len()..].chars().next())
        })
}

/// What a report would hold, for showing before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preview {
    pub staff: usize,
    pub assessments: usize,
}

pub fn preview(data: &Data, opts: &Options, today: NaiveDate) -> Preview {
    let r = Report::new(data, opts, today);
    let all: Vec<&DueItem> = r.items.iter().collect();
    Preview {
        staff: distinct_staff(&all),
        assessments: all.len(),
    }
}

/// The file a report is saved as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Excel,
    Pdf,
}

impl FileType {
    pub fn extension(self) -> &'static str {
        match self {
            FileType::Excel => "xlsx",
            FileType::Pdf => "pdf",
        }
    }
}

/// Writes the report, made by `by` at `now`, to `path`. Both file types hold
/// the same.
pub fn export_audit_report(
    data: &Data,
    opts: &Options,
    by: &str,
    now: Ts,
    path: &Path,
    file: FileType,
) -> Result<(), String> {
    if let Some(p) = opts.problem() {
        return Err(p.to_owned());
    }
    let r = Report::new(data, opts, now.date_naive());
    let (checks, found) = r.checks();
    let mut checks = Some(checks);
    let sheets = opts
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Summary => Some(r.summary(by, now, found)),
            Part::Matrix => Some(r.matrix()),
            Part::Checks => checks.take(),
            Part::Schedule => Some(r.schedule()),
            Part::ChangeLog => Some(r.change_log()),
        })
        .collect();
    let doc = Document {
        stamp: format!(
            "{TITLE}, {}, {}",
            opts.section_text(data),
            fmt_date(r.today)
        ),
        author: by.to_owned(),
        sheets,
    };
    match file {
        FileType::Excel => xlsx::write(&doc, path).map_err(|e| e.to_string()),
        FileType::Pdf => pdf::write(&doc, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{open_workbook, Reader, Xlsx};
    use chrono::TimeZone;

    type Book = Xlsx<std::io::BufReader<std::fs::File>>;

    fn ts(y: i32, m: u32, day: u32) -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, m, day, 9, 5, 0)
            .unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn entry(action: &str, detail: &str, refs: &[(&str, &str)], at: Ts) -> AuditEntry {
        AuditEntry {
            at,
            by: "jtan".into(),
            action: action.into(),
            detail: detail.into(),
            refs: refs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            reverted: false,
        }
    }

    /// Chemistry (Cobas), Haematology (Ruby). Amy: Cobas, overdue. Zed: Cobas
    /// N/A, Ruby never done. Olaf (removed): a Cobas completion.
    fn sample() -> Data {
        let mut data = Data::new_empty("jtan", ts(2026, 1, 1));
        for (i, (id, name)) in [("t-cobas", "Cobas"), ("t-ruby", "Ruby")]
            .into_iter()
            .enumerate()
        {
            data.sections[i].tests.push(TestSystem {
                id: id.into(),
                name: name.into(),
                order: 0,
                retired: false,
                rule_id: None,
            });
        }
        let person = |id: &str, name: &str, status, tests: &[(&str, AssignState)]| Staff {
            id: id.into(),
            name: name.into(),
            title: "MLT".into(),
            joined: Some(d(2025, 1, 1)),
            status,
            assignments: tests
                .iter()
                .map(|(t, state)| {
                    let since = d(2025, 1, 1);
                    let state = *state;
                    ((*t).to_string(), Assignment { state, since })
                })
                .collect(),
        };
        use AssignState::{Assigned, Na};
        use StaffStatus::{Active, Removed};
        data.staff = vec![
            person(
                "p-zed",
                "Zed",
                Active,
                &[("t-cobas", Na), ("t-ruby", Assigned)],
            ),
            person("p-amy", "Amy", Active, &[("t-cobas", Assigned)]),
            person("p-olaf", "Olaf", Removed, &[("t-cobas", Assigned)]),
        ];
        let rec = |id: &str, staff: &str, idx: u32, label: &str, on| Record {
            id: id.into(),
            staff_id: staff.into(),
            test_id: "t-cobas".into(),
            stage_index: idx,
            stage_label: label.into(),
            completed_on: on,
            comment: String::new(),
            by: "jtan".into(),
            at: ts(2026, 1, 2),
            imported: true,
            unconfirmed: false,
        };
        data.records = vec![
            rec("r-1", "p-amy", 0, "Initial", d(2025, 2, 1)),
            rec("r-2", "p-amy", 1, "Semi-annual", d(2025, 10, 1)),
            // Before joining.
            rec("r-3", "p-olaf", 0, "Initial", d(2024, 6, 1)),
        ];
        data.records[1].unconfirmed = true;
        data.audit = vec![
            entry(
                "record.add",
                "=Did Cobas",
                &[("staff_id", "p-amy"), ("test_id", "t-cobas")],
                ts(2026, 1, 2),
            ),
            entry("settings.update", "Changed", &[], ts(2026, 3, 1)),
            entry(
                "staff.status",
                "Set Olaf to removed",
                &[("staff_id", "p-olaf")],
                ts(2026, 3, 2),
            ),
            entry(
                "history.revert",
                "Reverted: Set Olaf to hidden",
                &[],
                ts(2026, 3, 3),
            ),
            entry(
                "staff.delete",
                "Permanently deleted Pia and all their records",
                &[("staff_id", "p-pia")],
                ts(2026, 3, 4),
            ),
            entry(
                "history.revert",
                "Reverted: Added Pia to personnel",
                &[],
                ts(2026, 3, 5),
            ),
        ];
        data
    }

    const NOW: (i32, u32, u32) = (2026, 11, 25);

    fn everything() -> Options {
        Options {
            parts: Part::ALL.into_iter().collect(),
            removed: true,
            ..Options::default()
        }
    }

    fn made(data: &Data, opts: &Options) -> (tempfile::TempDir, Book) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.xlsx");
        let now = ts(NOW.0, NOW.1, NOW.2);
        export_audit_report(data, opts, "R&D tester", now, &path, FileType::Excel).unwrap();
        let wb = open_workbook(&path).unwrap();
        (dir, wb)
    }

    /// A sheet's rows as text (dates as "yyyy-mm-dd").
    fn rows(wb: &mut Book, sheet: &str) -> Vec<Vec<String>> {
        let range = wb.worksheet_range(sheet).unwrap();
        range
            .rows()
            .map(|r| {
                r.iter()
                    .map(|c| match c {
                        calamine::Data::DateTime(dt) => {
                            let (y, m, d, ..) = dt.to_ymd_hms_milli();
                            format!("{y}-{m:02}-{d:02}")
                        }
                        c => c.to_string(),
                    })
                    .collect()
            })
            .collect()
    }

    fn row<'a>(rows: &'a [Vec<String>], first: &str) -> &'a [String] {
        rows.iter()
            .find(|r| r[0] == first)
            .unwrap_or_else(|| panic!("no row {first}"))
    }

    /// Everything the workbook says, on any sheet.
    fn all_text(wb: &mut Book) -> String {
        let names = wb.sheet_names();
        let mut out = String::new();
        for n in names {
            for r in rows(wb, &n) {
                out.push_str(&r.join("\t"));
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn by_default_everything_but_the_change_log() {
        let (_dir, wb) = made(&sample(), &Options::default());
        assert_eq!(
            wb.sheet_names(),
            ["Summary", "Matrix", "Data checks", "Schedule"]
        );
    }

    #[test]
    fn only_the_parts_picked_in_order() {
        let opts = Options {
            parts: [Part::ChangeLog, Part::Matrix].into(),
            ..Options::default()
        };
        let (_dir, wb) = made(&sample(), &opts);
        assert_eq!(wb.sheet_names(), ["Matrix", "Change log"]);
        // The summary lists just what came along.
        let opts = Options {
            parts: [Part::Summary, Part::Schedule].into(),
            ..Options::default()
        };
        let (_dir, mut wb) = made(&sample(), &opts);
        let said = all_text(&mut wb);
        assert!(said.contains("The rules that"));
        for gone in ["Everyone against", "Data checks"] {
            assert!(!said.contains(gone), "{gone}");
        }
        let none = Options {
            parts: BTreeSet::new(),
            ..Options::default()
        };
        assert!(none.problem().is_some());
    }

    #[test]
    fn removed_staff_appear_only_when_asked_for() {
        let mut opts = Options {
            parts: Part::ALL.into_iter().collect(),
            ..Options::default()
        };
        let (_dir, mut wb) = made(&sample(), &opts);
        let said = all_text(&mut wb);
        for gone in ["Olaf", "Pia", "deleted person"] {
            assert!(!said.contains(gone), "{gone}");
        }
        assert!(said.contains("Amy") && said.contains("active staff only"));
        // The rest of the change log is still there.
        assert_eq!(rows(&mut wb, "Change log").len(), 4 + 2);

        opts.removed = true;
        let (_dir, mut wb) = made(&sample(), &opts);
        assert_eq!(rows(&mut wb, "Change log").len(), 4 + 6);
        assert!(rows(&mut wb, "Data checks")
            .iter()
            .any(|r| r[0] == "Completed before joining"));
        // Figures count active staff either way.
        assert_eq!(row(&rows(&mut wb, "Summary"), "All sections")[2], "2");
    }

    #[test]
    fn summary_figures_match_the_data() {
        let (_dir, mut wb) = made(&sample(), &Options::default());
        let r = rows(&mut wb, "Summary");
        // Test systems, staff, assessments, current, %, then by status.
        assert_eq!(
            row(&r, "Chemistry")[1..],
            ["1", "1", "1", "0", "0", "0", "0", "1", "0"]
        );
        assert_eq!(
            row(&r, "All sections")[1..],
            ["2", "2", "2", "0", "0", "0", "0", "1", "1"]
        );
        assert_eq!(row(&r, "Overdue with a reason")[1], "0 of 1");
        assert!(row(&r, "Not confirmed")[1].starts_with("1 of 2"));
        assert_eq!(row(&r, "Data checks")[1], "1 to resolve or explain");
        assert!(row(&r, "Made by")[1].starts_with("R&D tester, 25 Nov 2026"));
    }

    #[test]
    fn change_log_dates_and_text() {
        let opts = Options {
            parts: [Part::ChangeLog].into(),
            from: Some(d(2025, 9, 1)),
            to: Some(d(2026, 1, 31)),
            ..Options::default()
        };
        let (_dir, mut wb) = made(&sample(), &opts);
        let r = rows(&mut wb, "Change log");
        assert_eq!(r.len(), 4 + 1);
        assert_eq!(r[4][5], "=Did Cobas", "text, never a formula");
        let backwards = Options {
            from: opts.to,
            to: opts.from,
            ..opts
        };
        assert!(backwards.problem().is_some());
        // The dates don't matter without the change log.
        let unused = Options {
            parts: [Part::Matrix].into(),
            ..backwards
        };
        assert_eq!(unused.problem(), None);
    }

    #[test]
    fn section_narrows_every_part() {
        let data = sample();
        let opts = Options {
            section: Some(data.sections[1].id.clone()),
            ..everything()
        };
        let want = Preview {
            staff: 1,
            assessments: 1,
        };
        assert_eq!(preview(&data, &opts, d(2026, 11, 25)), want);
        let (_dir, mut wb) = made(&data, &opts);
        let said = all_text(&mut wb);
        assert!(said.contains("Zed") && !said.contains("Amy") && !said.contains("Olaf"));
        assert!(
            !said.contains("All sections"),
            "no total over a single section"
        );
        assert_eq!(rows(&mut wb, "Change log")[4][0], "None");
    }

    #[test]
    fn checks_find_gaps() {
        let mut data = sample();
        data.staff[0].joined = None;
        let again = Record {
            id: "r-4".into(),
            completed_on: d(2027, 1, 1),
            ..data.records[1].clone()
        };
        data.records.push(again);
        let found = |opts: &Options| -> Vec<String> {
            let (_dir, mut wb) = made(&data, opts);
            rows(&mut wb, "Data checks")[4..]
                .iter()
                .map(|r| format!("{}: {}", r[0], r[3]))
                .collect()
        };
        let default = [
            "Completed in the future: Amy",
            "Imported, not confirmed: Amy",
            "Imported, not confirmed: Amy",
            "No join date: Zed",
            "Same stage recorded more than once: Amy",
        ];
        assert_eq!(found(&Options::default()), default);
        let all = found(&everything());
        assert_eq!(all[0], "Completed before joining: Olaf (removed)");
        assert_eq!(all[1..], default);
    }

    #[test]
    fn checks_leave_out_retired_tests() {
        let mut data = sample();
        data.sections[0].tests[0].retired = true;
        let (_dir, mut wb) = made(&data, &everything());
        let said: Vec<String> = rows(&mut wb, "Data checks").concat();
        assert!(!said.iter().any(|c| c.contains("Cobas")));
    }

    #[test]
    fn schedule_says_each_rule_in_a_line() {
        let mut data = sample();
        let mut short = Rule::default_global();
        short.id = "r-short".into();
        short.name = "No semi-annual".into();
        short.stages.remove(1);
        data.rules.push(short);
        data.sections[1].tests[0].rule_id = Some("r-short".into());
        let (_dir, mut wb) = made(&data, &Options::default());
        let r = rows(&mut wb, "Schedule");
        assert_eq!(
            row(&r, "Global (default)")[1..],
            [
                "Initial \u{2192} Semi-annual after 6 months \u{2192} Annual every 12 months",
                "Cobas"
            ]
        );
        assert_eq!(
            row(&r, "No semi-annual")[1..],
            ["Initial \u{2192} Annual every 12 months", "Ruby"]
        );
    }

    #[test]
    fn matrix_shows_the_dates_picked() {
        let cell = |dates, removed| {
            let opts = Options {
                dates,
                removed,
                ..Options::default()
            };
            let (_dir, mut wb) = made(&sample(), &opts);
            let r = rows(&mut wb, "Matrix");
            let of = |name: &str| -> Vec<String> {
                let theirs = r.iter().filter(|r| r[0] == name);
                theirs.map(|r| r[1].clone()).collect()
            };
            (of("Amy"), of("Zed"), of("Olaf (removed)"))
        };
        use MatrixDates::{Both, Completed, NextDue};
        let none: [&str; 0] = [];
        let (amy, zed, olaf) = cell(NextDue, true);
        assert_eq!(amy, ["1 Oct 2026*"]);
        assert_eq!(zed, ["N/A", "Not done"]);
        assert_eq!(olaf, none, "nothing is due from removed staff");
        let (amy, zed, olaf) = cell(Completed, true);
        assert_eq!(amy, ["1 Oct 2025*"]);
        assert_eq!(zed, ["N/A", "Not done"]);
        assert_eq!(olaf, ["2024-06-01"]);
        let (amy, _, olaf) = cell(Both, false);
        assert_eq!(amy, ["1 Oct 2025*\nDue 1 Oct 2026"]);
        assert_eq!(olaf, none);
        assert_eq!(Options::default().dates, NextDue);
    }
    #[test]
    fn names_are_matched_as_whole_words() {
        assert!(mentions("Set Ann to removed", "Ann"));
        assert!(mentions("Added Ann", "Ann"));
        assert!(!mentions("Set Annual to removed", "Ann"));
        assert!(!mentions("JoAnn", "Ann"));
        assert!(!mentions("anything", ""));
        assert!(mentions("Set 陈丽 to removed", "陈丽"));
    }

    #[test]
    fn pdf_is_written_for_any_data_and_parts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        let now = ts(NOW.0, NOW.1, NOW.2);
        let mut odd = sample();
        odd.records[0].completed_on = d(1899, 1, 1);
        odd.records[1].staff_id = "gone".into();
        for data in [sample(), odd, Data::new_empty("jtan", now)] {
            let mut picks = vec![everything(), Options::default()];
            picks.extend(Part::ALL.map(|p| Options {
                parts: [p].into(),
                ..Options::default()
            }));
            for opts in picks {
                export_audit_report(&data, &opts, "jtan", now, &path, FileType::Pdf).unwrap();
                assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF-"));
            }
        }
        let nowhere = dir.path().join("none").join("report.pdf");
        for file in [FileType::Pdf, FileType::Excel] {
            let opts = Options::default();
            assert!(export_audit_report(&sample(), &opts, "j", now, &nowhere, file).is_err());
        }
    }

    #[test]
    fn copes_with_no_data_and_odd_values() {
        made(&Data::new_empty("jtan", ts(2026, 1, 1)), &everything());
        let mut data = sample();
        data.records[0].completed_on = d(1899, 1, 1);
        data.audit[0].detail = "x".repeat(40_000);
        data.records[1].staff_id = "gone".into();
        data.records[2].test_id = "gone".into();
        let (_dir, mut wb) = made(&data, &everything());
        assert!(rows(&mut wb, "Data checks")
            .iter()
            .any(|r| r[0] == "Record of something deleted"));
    }
}
