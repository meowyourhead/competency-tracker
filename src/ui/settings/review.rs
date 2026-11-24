//! Settings → Import & export: the list of records imported as "unconfirmed –
//! from Excel". Filters narrow the list; the actions then apply to the ticked
//! rows, or to every row shown when none is ticked.

use std::collections::HashSet;

use chrono::NaiveDate;
use egui::{Align, Button, ComboBox, Label, Layout, RichText, ScrollArea, TextEdit, Vec2};
use egui_phosphor::regular as icon;

use super::apply;
use crate::dates::fmt_date;
use crate::model::{Data, Id, StaffStatus};
use crate::ops;
use crate::schedule::stage_label;
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::Palette;
use crate::ui::widgets::{chip_select, date_picker, empty_state, search_box, segmented, SEARCH_W};

const ROW_H: f32 = 30.0;
/// Column widths: checkbox, staff, section, test, stage, date (comment takes the rest).
const COLS: [f32; 6] = [28.0, 150.0, 150.0, 190.0, 100.0, 100.0];
/// Repeats of the last stage offered when batch editing.
const MORE_ANNUALS: u32 = 8;

/// Whose records are listed.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Scope {
    /// Active staff and current tests: the ones that matter day to day.
    #[default]
    Current,
    RemovedStaff,
    RetiredTests,
    Ignored,
    /// All of them but the ignored.
    All,
}

impl Scope {
    const ALL: [Scope; 5] = [
        Scope::Current,
        Scope::RemovedStaff,
        Scope::RetiredTests,
        Scope::Ignored,
        Scope::All,
    ];

    fn label(self) -> &'static str {
        match self {
            Scope::Current => "To review",
            Scope::RemovedStaff => "Removed staff",
            Scope::RetiredTests => "Retired tests",
            Scope::Ignored => "Ignored",
            Scope::All => "All",
        }
    }

    fn keeps(self, r: &Row) -> bool {
        match self {
            Scope::All => r.kind != Scope::Ignored,
            s => r.kind == s,
        }
    }
}

/// Where a record came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Cell,
    Remark,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum SortBy {
    #[default]
    Section,
    Staff,
    Test,
    Stage,
    Date,
    Comment,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Confirm,
    Due,
    Delete,
    /// Leave them all out of the review (no rows ticked).
    Ignore,
}

/// The batch edit form; `None` / unticked keeps what each record has.
#[derive(Default)]
struct EditForm {
    stage: Option<u32>,
    date: Option<NaiveDate>,
    set_comment: bool,
    comment: String,
    confirm: bool,
}

#[derive(Default)]
pub(super) struct Review {
    scope: Scope,
    text: String,
    section: Option<Id>,
    staff: Option<Id>,
    test: Option<Id>,
    stage: Option<String>,
    source: Option<Source>,
    year: Option<i32>,
    sort: SortBy,
    descending: bool,
    selected: HashSet<Id>,
    /// The row last ticked, for ticking a range with Shift.
    anchor: Option<Id>,
    confirm: Option<Pending>,
    edit: Option<EditForm>,
    /// Unconfirmed records for a data revision.
    cache: Option<(u64, Vec<Row>)>,
}

struct Row {
    id: Id,
    staff_id: Id,
    section_id: Id,
    test_id: Id,
    staff: String,
    section: String,
    test: String,
    stage: String,
    stage_index: u32,
    date: NaiveDate,
    comment: String,
    /// Never `All`.
    kind: Scope,
    /// Section and test order, for the usual sort.
    order: (u32, u32),
}

impl Row {
    fn source(&self) -> Source {
        if self.comment.starts_with("From remark") {
            Source::Remark
        } else {
            Source::Cell
        }
    }
}

enum Act {
    Ask(Pending),
    Cancel,
    Confirm,
    Due,
    Delete,
    Ignore(bool),
    Edit,
    Open(Id, Id),
}

fn rows(data: &Data) -> Vec<Row> {
    let ignored: HashSet<&Id> = data.review_ignored.iter().collect();
    data.records
        .iter()
        .filter(|r| r.unconfirmed)
        .map(|r| {
            let found = data.test(&r.test_id);
            let person = data.staff(&r.staff_id);
            let active = person.is_some_and(|s| s.status == StaffStatus::Active);
            let current = found.is_some_and(|(s, t)| !s.retired && !t.retired);
            let kind = if ignored.contains(&r.id) {
                Scope::Ignored
            } else if !active {
                Scope::RemovedStaff
            } else if !current {
                Scope::RetiredTests
            } else {
                Scope::Current
            };
            Row {
                id: r.id.clone(),
                staff_id: r.staff_id.clone(),
                test_id: r.test_id.clone(),
                section_id: found.map(|(s, _)| s.id.clone()).unwrap_or_default(),
                staff: person.map(|s| s.name.clone()).unwrap_or_default(),
                section: found.map(|(s, _)| s.name.clone()).unwrap_or_default(),
                test: found.map(|(_, t)| t.name.clone()).unwrap_or_default(),
                stage: r.stage_label.clone(),
                stage_index: r.stage_index,
                date: r.completed_on,
                comment: r.comment.clone(),
                kind,
                order: found.map_or((u32::MAX, u32::MAX), |(s, t)| (s.order, t.order)),
            }
        })
        .collect()
}

impl Review {
    fn refresh(&mut self, session: &Session) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, _)) if *r == rev) {
            return;
        }
        let rows = rows(session.data());
        let ids: HashSet<&Id> = rows.iter().map(|r| &r.id).collect();
        self.selected.retain(|id| ids.contains(id));
        self.cache = Some((rev, rows));
        // The question was about the rows as they were.
        self.confirm = None;
    }

    fn all(&self) -> &[Row] {
        self.cache.as_ref().map_or(&[][..], |(_, r)| r.as_slice())
    }

    fn filtered(&self) -> bool {
        self.section.is_some()
            || self.staff.is_some()
            || self.test.is_some()
            || self.stage.is_some()
            || self.source.is_some()
            || self.year.is_some()
            || !self.text.trim().is_empty()
    }

    fn clear_filters(&mut self) {
        self.section = None;
        self.staff = None;
        self.test = None;
        self.stage = None;
        self.source = None;
        self.year = None;
        self.text.clear();
    }

    /// The rows left by the filters, sorted.
    fn shown(&self) -> Vec<&Row> {
        use chrono::Datelike;
        let words: Vec<String> = self
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut out: Vec<&Row> = self
            .all()
            .iter()
            .filter(|r| self.scope.keeps(r))
            .filter(|r| self.section.as_ref().is_none_or(|x| *x == r.section_id))
            .filter(|r| self.staff.as_ref().is_none_or(|x| *x == r.staff_id))
            .filter(|r| self.test.as_ref().is_none_or(|x| *x == r.test_id))
            .filter(|r| self.stage.as_ref().is_none_or(|x| *x == r.stage))
            .filter(|r| self.source.is_none_or(|x| x == r.source()))
            .filter(|r| self.year.is_none_or(|y| y == r.date.year()))
            .filter(|r| {
                // Every word somewhere in the row.
                let hay = format!(
                    "{} {} {} {} {} {}",
                    r.staff,
                    r.section,
                    r.test,
                    r.stage,
                    fmt_date(r.date),
                    r.comment
                )
                .to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        out.sort_by(|a, b| {
            let usual = |r: &Row| (r.order.0, r.staff.to_lowercase(), r.order.1, r.date);
            let by = match self.sort {
                SortBy::Section => usual(a).cmp(&usual(b)),
                SortBy::Staff => a.staff.to_lowercase().cmp(&b.staff.to_lowercase()),
                SortBy::Test => a.test.to_lowercase().cmp(&b.test.to_lowercase()),
                SortBy::Stage => a.stage_index.cmp(&b.stage_index),
                SortBy::Date => a.date.cmp(&b.date),
                SortBy::Comment => a.comment.to_lowercase().cmp(&b.comment.to_lowercase()),
            };
            let by = if self.descending { by.reverse() } else { by };
            by.then_with(|| usual(a).cmp(&usual(b)))
        });
        out
    }

    /// What an action applies to: the ticked rows shown, or all shown.
    fn targets(&self) -> (Vec<Id>, bool) {
        let shown: Vec<Id> = self.shown().iter().map(|r| r.id.clone()).collect();
        let picked: Vec<Id> = shown
            .iter()
            .filter(|id| self.selected.contains(*id))
            .cloned()
            .collect();
        if picked.is_empty() {
            (shown, false)
        } else {
            (picked, true)
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut Review) {
    let editing = cx.session.is_editing();
    if !editing {
        st.confirm = None;
        st.edit = None;
    }
    st.refresh(cx.session);
    if let Some(a) = body(ui, cx.pal, editing, cx.today, cx.session.data(), st) {
        run(cx, st, a);
    }
}

fn body(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    today: NaiveDate,
    data: &Data,
    st: &mut Review,
) -> Option<Act> {
    if st.all().is_empty() {
        empty_state(
            ui,
            "Nothing to review",
            "Records imported as \u{201c}unconfirmed \u{2013} from Excel\u{201d} appear here.",
        );
        return None;
    }
    ui.label(
        RichText::new(
            "Records marked \u{201c}unconfirmed \u{2013} from Excel\u{201d}. Check them against the workbook, then confirm them or take them out. Ignored ones stay unconfirmed but leave this list.",
        )
        .color(pal.muted),
    );
    ui.add_space(6.0);
    scopes(ui, pal, st);
    ui.add_space(6.0);
    filters(ui, pal, st);
    ui.add_space(6.0);
    let mut act = None;
    let (ids, picked) = st.targets();
    actions(ui, pal, editing, st, &ids, picked, &mut act);
    if st.edit.is_some() {
        edit_form(ui, pal, data, today, st, ids.len(), &mut act);
    }
    ui.add_space(4.0);
    if st.shown().is_empty() {
        let body = match st.scope {
            Scope::Current if !st.filtered() => "Everything here has been reviewed.",
            _ => "No records match.",
        };
        ui.add_space(12.0);
        ui.label(RichText::new(body).color(pal.muted));
        ui.add_space(12.0);
    } else {
        table(ui, pal, editing, st, &mut act);
    }
    act
}

fn scopes(ui: &mut egui::Ui, pal: &Palette, st: &mut Review) {
    let labels: Vec<String> = Scope::ALL
        .iter()
        .map(|s| {
            let n = st.all().iter().filter(|r| s.keeps(r)).count();
            format!("{} ({n})", s.label())
        })
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let at = Scope::ALL.iter().position(|s| *s == st.scope).unwrap_or(0);
    if let Some(i) = segmented(ui, pal, at, &refs) {
        if Scope::ALL[i] != st.scope {
            st.scope = Scope::ALL[i];
            st.selected.clear();
            st.confirm = None;
            st.edit = None;
            st.clear_filters();
        }
    }
}

fn filters(ui: &mut egui::Ui, pal: &Palette, st: &mut Review) {
    use chrono::Datelike;
    // Only what the rows of this scope hold.
    let in_scope: Vec<&Row> = st.all().iter().filter(|r| st.scope.keeps(r)).collect();
    let options = |f: &dyn Fn(&Row) -> (Id, String)| -> Vec<(Id, String)> {
        let mut seen = HashSet::new();
        let mut v: Vec<(Id, String)> = in_scope
            .iter()
            .map(|r| f(r))
            .filter(|(id, _)| seen.insert(id.clone()))
            .collect();
        v.sort_by_key(|(_, n)| n.to_lowercase());
        v
    };
    let sections = options(&|r| (r.section_id.clone(), r.section.clone()));
    let staff = options(&|r| (r.staff_id.clone(), r.staff.clone()));
    let tests = options(&|r| (r.test_id.clone(), r.test.clone()));
    let mut stages: Vec<(u32, String)> = in_scope
        .iter()
        .map(|r| (r.stage_index, r.stage.clone()))
        .collect();
    stages.sort();
    stages.dedup_by(|a, b| a.1 == b.1);
    let stages: Vec<(String, String)> = stages.into_iter().map(|(_, s)| (s.clone(), s)).collect();
    let mut years: Vec<i32> = in_scope.iter().map(|r| r.date.year()).collect();
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    let years: Vec<(i32, String)> = years.into_iter().map(|y| (y, y.to_string())).collect();
    let sources = [
        (Source::Cell, "Date cell".to_owned()),
        (Source::Remark, "Remark".to_owned()),
    ];
    ui.horizontal_wrapped(|ui| {
        search_box(ui, &mut st.text, "Search these records", SEARCH_W);
        chip_select(ui, "Section", &mut st.section, &sections);
        chip_select(ui, "Staff", &mut st.staff, &staff);
        chip_select(ui, "Test system", &mut st.test, &tests);
        chip_select(ui, "Stage", &mut st.stage, &stages);
        chip_select(ui, "Year", &mut st.year, &years);
        chip_select(ui, "From", &mut st.source, &sources);
        if st.filtered()
            && ui
                .add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false))
                .clicked()
        {
            st.clear_filters();
        }
    });
}

fn actions(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    st: &mut Review,
    ids: &[Id],
    picked: bool,
    act: &mut Option<Act>,
) {
    let n = ids.len();
    let on = editing && n > 0;
    let which = if picked { "selected" } else { "all" };
    ui.horizontal_wrapped(|ui| {
        let text = if picked {
            format!("{n} selected")
        } else {
            format!("{n} shown")
        };
        ui.label(RichText::new(text).color(pal.muted));
        if st.scope == Scope::Ignored {
            let put = Button::new(format!("Put {which} back"));
            if ui.add_enabled(on, put).clicked() {
                *act = Some(Act::Ignore(false));
            }
        } else {
            if ui
                .add_enabled(on, Button::new(format!("Confirm {which}")))
                .on_hover_text("They were done on these dates")
                .clicked()
            {
                *act = Some(if picked {
                    Act::Confirm
                } else {
                    Act::Ask(Pending::Confirm)
                });
            }
            if ui
                .add_enabled(on, Button::new(format!("Mark {which} as due")))
                .on_hover_text("They weren't done: the dates become due dates")
                .clicked()
            {
                *act = Some(Act::Ask(Pending::Due));
            }
            if ui
                .add_enabled(on, Button::new(format!("Edit {which}\u{2026}")))
                .on_hover_text("Change their stage, date or comment together")
                .clicked()
            {
                st.edit = Some(EditForm::default());
                st.confirm = None;
            }
            if ui
                .add_enabled(on, Button::new(format!("Ignore {which}")))
                .on_hover_text("Leave them out of this list; they stay unconfirmed")
                .clicked()
            {
                *act = Some(if picked {
                    Act::Ignore(true)
                } else {
                    Act::Ask(Pending::Ignore)
                });
            }
        }
        if ui
            .add_enabled(
                on,
                Button::new(RichText::new(format!("Delete {which}")).color(pal.red_fg)),
            )
            .clicked()
        {
            *act = Some(Act::Ask(Pending::Delete));
        }
    });
    let Some(pending) = st.confirm else { return };
    let s = plural(n);
    let (question, yes, then) = match pending {
        Pending::Confirm => (
            format!("Confirm {which} {n} record{s}?"),
            "Confirm",
            Act::Confirm,
        ),
        Pending::Due => (
            format!("Mark {n} record{s} as due, not done? They stop counting as completed."),
            "Mark as due",
            Act::Due,
        ),
        Pending::Ignore => (
            format!("Leave all {n} record{s} out of the review?"),
            "Ignore",
            Act::Ignore(true),
        ),
        Pending::Delete => (
            format!("Delete {n} record{s}? You can revert this from History."),
            "Delete",
            Act::Delete,
        ),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(question).color(if pending == Pending::Delete {
                pal.red_fg
            } else {
                pal.text
            }),
        );
        if ui.add_enabled(on, Button::new(yes)).clicked() {
            *act = Some(then);
        }
        if ui.button("Cancel").clicked() {
            *act = Some(Act::Cancel);
        }
    });
}

fn edit_form(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    today: NaiveDate,
    st: &mut Review,
    n: usize,
    act: &mut Option<Act>,
) {
    let Some(f) = &mut st.edit else { return };
    let rule = data
        .rules
        .iter()
        .find(|r| r.id == data.default_rule_id)
        .or(data.rules.first());
    egui::Frame::new()
        .fill(pal.bg)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("Change {n} record{} together", plural(n))).color(pal.text),
            );
            ui.horizontal_wrapped(|ui| {
                let label = |i: u32| rule.map(|r| stage_label(r, i)).unwrap_or_default();
                ui.label(RichText::new("Stage").color(pal.muted));
                ComboBox::from_id_salt("review_stage")
                    .selected_text(f.stage.map_or("Keep".to_owned(), label))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.stage, None, "Keep");
                        let count = rule.map_or(0, |r| r.stages.len() as u32);
                        for i in 0..count + MORE_ANNUALS {
                            ui.selectable_value(&mut f.stage, Some(i), label(i));
                        }
                    });
                ui.add_space(8.0);
                let mut set = f.date.is_some();
                if ui.checkbox(&mut set, "Set the date").changed() {
                    f.date = set.then_some(today);
                }
                if let Some(d) = &mut f.date {
                    date_picker(ui, d, "review_date");
                }
                ui.add_space(8.0);
                ui.checkbox(&mut f.set_comment, "Set the comment");
                if f.set_comment {
                    ui.add(TextEdit::singleline(&mut f.comment).desired_width(200.0));
                }
                ui.add_space(8.0);
                ui.checkbox(&mut f.confirm, "Confirm them too");
            });
            let any = f.stage.is_some() || f.date.is_some() || f.set_comment || f.confirm;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(any && n > 0, Button::new("Apply changes"))
                    .clicked()
                {
                    *act = Some(Act::Edit);
                }
                if ui.button("Cancel editing").clicked() {
                    *act = Some(Act::Cancel);
                }
            });
        });
}

/// A fixed-width, single-line table cell.
fn cell(ui: &mut egui::Ui, width: f32, text: RichText) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(Label::new(text).truncate())
        },
    )
    .inner
}

fn table(ui: &mut egui::Ui, pal: &Palette, editing: bool, st: &mut Review, act: &mut Option<Act>) {
    let ids: Vec<Id> = st.shown().iter().map(|r| r.id.clone()).collect();
    let picked = ids.iter().filter(|id| st.selected.contains(*id)).count();
    ui.horizontal(|ui| {
        let mut all = !ids.is_empty() && picked == ids.len();
        ui.allocate_ui_with_layout(
            Vec2::new(COLS[0], ROW_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                if ui
                    .add_enabled(editing, egui::Checkbox::without_text(&mut all))
                    .on_hover_text("Select all shown")
                    .changed()
                {
                    for id in &ids {
                        if all {
                            st.selected.insert(id.clone());
                        } else {
                            st.selected.remove(id);
                        }
                    }
                }
            },
        );
        let heads = [
            ("Staff", SortBy::Staff),
            ("Section", SortBy::Section),
            ("Test system", SortBy::Test),
            ("Stage", SortBy::Stage),
            ("Completed", SortBy::Date),
            ("Comment", SortBy::Comment),
        ];
        for (i, (head, key)) in heads.into_iter().enumerate() {
            let w = COLS.get(i + 1).copied();
            let w = w.unwrap_or_else(|| ui.available_width());
            ui.allocate_ui_with_layout(
                Vec2::new(w, ROW_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(w);
                    let on = st.sort == key;
                    let text = match (on, st.descending) {
                        (false, _) => head.to_owned(),
                        (true, false) => format!("{head} {}", icon::CARET_UP),
                        (true, true) => format!("{head} {}", icon::CARET_DOWN),
                    };
                    let color = if on { pal.text } else { pal.muted };
                    let b = Button::new(RichText::new(text).size(12.5).color(color)).frame(false);
                    if ui.add(b).on_hover_text("Sort").clicked() {
                        st.descending = on && !st.descending;
                        st.sort = key;
                    }
                },
            );
        }
    });
    let rows = st.shown();
    let mut toggle: Option<(Id, bool)> = None;
    let mut open = None;
    ScrollArea::vertical()
        .id_salt("unconfirmed_rows")
        .max_height(460.0)
        .auto_shrink([false, true])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for r in &rows[range] {
                ui.horizontal(|ui| {
                    let mut on = st.selected.contains(&r.id);
                    ui.allocate_ui_with_layout(
                        Vec2::new(COLS[0], ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            if ui
                                .add_enabled(editing, egui::Checkbox::without_text(&mut on))
                                .on_hover_text("Shift-click selects everything in between")
                                .changed()
                            {
                                toggle = Some((r.id.clone(), on));
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(COLS[1], ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_width(COLS[1]);
                            let b = Button::new(RichText::new(&r.staff).color(pal.text))
                                .frame(false)
                                .truncate();
                            if ui
                                .add(b)
                                .on_hover_text("Open this person's record of the test")
                                .clicked()
                            {
                                open = Some((r.staff_id.clone(), r.test_id.clone()));
                            }
                        },
                    );
                    cell(ui, COLS[2], RichText::new(&r.section).color(pal.muted));
                    cell(ui, COLS[3], RichText::new(&r.test).color(pal.text));
                    cell(ui, COLS[4], RichText::new(&r.stage).color(pal.text));
                    cell(ui, COLS[5], RichText::new(fmt_date(r.date)).color(pal.text));
                    cell(
                        ui,
                        ui.available_width(),
                        RichText::new(&r.comment).color(pal.muted),
                    );
                });
            }
        });
    if let Some((s, t)) = open {
        *act = Some(Act::Open(s, t));
    }
    if let Some((id, on)) = toggle {
        let pos = |x: &Id| ids.iter().position(|i| i == x);
        let shift = ui.input(|i| i.modifiers.shift);
        let range = match (shift, st.anchor.as_ref().and_then(pos), pos(&id)) {
            (true, Some(a), Some(b)) => a.min(b)..=a.max(b),
            (_, _, Some(b)) => b..=b,
            _ => return,
        };
        for x in &ids[range] {
            if on {
                st.selected.insert(x.clone());
            } else {
                st.selected.remove(x);
            }
        }
        st.anchor = Some(id);
    }
}

fn run(cx: &mut Ctx, st: &mut Review, act: Act) {
    let (ids, _) = st.targets();
    let n = ids.len();
    let s = plural(n);
    let done = match act {
        Act::Ask(p) => {
            st.confirm = Some(p);
            st.edit = None;
            return;
        }
        Act::Cancel => {
            st.confirm = None;
            st.edit = None;
            return;
        }
        Act::Open(staff, test) => {
            *cx.open_detail = Some((staff, test));
            return;
        }
        Act::Confirm => {
            let res = apply(cx, "", "Couldn't confirm", |d, a| {
                ops::confirm_records(d, a, &ids)
            });
            if let Some(n) = res {
                let text = format!("Confirmed {n} record{}", plural(n));
                cx.toasts.push(Toast::new(text));
            }
            res.is_some()
        }
        Act::Due => apply(
            cx,
            &format!("Marked {n} record{s} as due"),
            "Couldn't mark as due",
            |d, a| ops::mark_records_due(d, a, &ids),
        )
        .is_some(),
        Act::Delete => apply(
            cx,
            &format!("Deleted {n} record{s}"),
            "Couldn't delete",
            |d, a| ids.iter().try_for_each(|id| ops::delete_record(d, a, id)),
        )
        .is_some(),
        Act::Ignore(on) => {
            let text = if on {
                format!("Ignored {n} record{s}")
            } else {
                format!("Put {n} record{s} back")
            };
            apply(cx, &text, "Couldn't change the list", |d, a| {
                ops::set_review_ignored(d, a, &ids, on)
            })
            .is_some()
        }
        Act::Edit => {
            let Some(f) = &st.edit else { return };
            if f.date.is_some_and(|d| d > cx.today) {
                cx.toasts
                    .push(Toast::new("Completion date can't be in the future"));
                return;
            }
            let comment = f.set_comment.then(|| f.comment.trim().to_owned());
            let (stage, date, confirm) = (f.stage, f.date, f.confirm);
            apply(
                cx,
                &format!("Changed {n} record{s}"),
                "Couldn't change the records",
                |d, a| {
                    if stage.is_some() || date.is_some() || comment.is_some() {
                        for id in &ids {
                            let Some(r) = d.records.iter().find(|r| r.id == *id) else {
                                continue;
                            };
                            let on = date.unwrap_or(r.completed_on);
                            let k = stage.unwrap_or(r.stage_index);
                            let text = comment.clone().unwrap_or_else(|| r.comment.clone());
                            ops::edit_record(d, a, id, on, k, &text)?;
                        }
                    }
                    if confirm {
                        ops::confirm_records(d, a, &ids)?;
                    }
                    Ok(())
                },
            )
            .is_some()
        }
    };
    if done {
        st.confirm = None;
        st.edit = None;
        for id in &ids {
            st.selected.remove(id);
        }
    }
}
