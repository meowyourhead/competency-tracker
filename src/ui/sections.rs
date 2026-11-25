//! Sections page: one grid per lab section, staff (rows) × test systems
//! (columns). Two views of the same grid:
//!
//! - **Next due**: each cell shows the next-due pill and the stage it's for.
//! - **Completed**: like the master tracking workbook, each person gets one
//!   line per stage (Initial, Semi-annual, and every Annual they have done)
//!   with the date it was completed, then when the next one is due.
//!
//! People are listed in the section's default order (the workbook's) unless
//! another sort is picked.
//!
//! The header row and the name column stay put while the cells scroll; the
//! grid is exactly as tall as its rows, so it only scrolls when they don't fit.

use chrono::NaiveDate;
use egui::text::LayoutJob;
use std::collections::{BTreeMap, HashMap};

use egui::{
    Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Popup, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Record, Rule, Section};
use crate::ops;
use crate::query::{distinct_titles, Index, StatusFilter};
use crate::schedule::{
    classify, next_due, stage_in, stage_label, stages_without, DueInfo, DueStatus,
};
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::{
    chip_select, due_pill_text, empty_state, rgb, search_box, segmented, status_colors,
    status_label, SEARCH_W, SECTION_TINT,
};

/// The frozen name column.
const NAME_W: f32 = 200.0;
/// The stage-label column the Completed view adds beside the names.
const STAGE_W: f32 = 96.0;
const COL_W: f32 = 140.0;
const HEAD_H: f32 = 44.0;
/// One person's row in the Next due view.
const ROW_H: f32 = 44.0;
/// One stage line in the Completed view, and the space above and below a
/// person's block of lines.
const LINE_H: f32 = 20.0;
const BLOCK_PAD: f32 = 6.0;
/// The section band over the column headers of the combined grid.
const BAND_H: f32 = 26.0;
/// Room under the last row for the sideways scroll bar, when there is one.
const BAR_ROOM: f32 = 10.0;

/// Which picture of the grid is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum View {
    #[default]
    Due,
    Completed,
}

/// How the people in a grid are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RowSort {
    /// The section's own order, as in the workbook.
    #[default]
    Default,
    NameAsc,
    NameDesc,
    Title,
    /// Whoever's next assessment is due first.
    Due,
}

impl RowSort {
    const ALL: [RowSort; 5] = [
        RowSort::Default,
        RowSort::NameAsc,
        RowSort::NameDesc,
        RowSort::Title,
        RowSort::Due,
    ];

    fn label(self) -> &'static str {
        match self {
            RowSort::Default => "Default order",
            RowSort::NameAsc => "Name A\u{2013}Z",
            RowSort::NameDesc => "Name Z\u{2013}A",
            RowSort::Title => "Title",
            RowSort::Due => "Due date",
        }
    }
}

#[derive(Default)]
pub(crate) struct SectionsState {
    /// Every section at once, one grid per section.
    pub all: bool,
    /// Section shown; `None` (or a retired one) falls back to the first.
    pub section: Option<Id>,
    pub view: View,
    pub status: StatusFilter,
    /// Show only this test system's column.
    pub test: Option<Id>,
    pub title: Option<String>,
    /// Name search.
    pub text: String,
    /// Rows sorted by this test's due date (earliest first, no date last).
    pub sort_by_test: Option<Id>,
    /// The order of the rows otherwise.
    pub sort: RowSort,
    /// Every live section's grid, and all of them as one grouped grid, for
    /// (revision, date).
    cache: Option<(u64, NaiveDate, HashMap<Id, Grid>, Combined)>,
}

/// One section's run of columns in the combined grid.
struct Group {
    name: String,
    color: Color32,
    /// Its first column in the combined grid, and how many it has.
    start: usize,
    len: usize,
}

/// Every live section in one grid: their columns side by side, grouped by
/// section in the configured order; one row per person in any of them.
struct Combined {
    grid: Grid,
    groups: Vec<Group>,
}

/// One completion of a test by a person.
#[derive(Clone, Copy)]
struct Done {
    /// Its place in the schedule: 0 Initial, …, then one per repeat.
    stage: u32,
    on: NaiveDate,
    unconfirmed: bool,
}

/// One line of a person's block in the Completed view.
struct Line {
    /// `None` is the "Next due" line.
    stage: Option<u32>,
    /// Which of the completions recorded for that stage (normally the only one).
    nth: usize,
    label: String,
}

#[derive(Clone)]
enum CellView {
    Due {
        info: DueInfo,
        status: DueStatus,
        delayed: bool,
        /// Every completion, oldest first.
        done: Vec<Done>,
        /// Stages that don't apply: left out for this person, or not in the
        /// test's rule.
        na: Vec<u32>,
    },
    Na,
    Unassigned,
}

#[derive(Clone)]
struct Row {
    id: Id,
    name: String,
    title: String,
    cells: Vec<CellView>,
}

struct Grid {
    /// (test id, test name), in configured order.
    columns: Vec<(Id, String)>,
    /// The rule whose stages the Completed view lists (see `fullest_rule`).
    rule: Rule,
    rows: Vec<Row>,
}

/// What the user did in the grid this frame.
enum GridAction {
    Open(Id, Id),
    Assign(Id, Id),
    Sort(Id),
}

impl SectionsState {
    /// Shows everyone in the section again.
    pub(crate) fn clear_filters(&mut self) {
        self.status = StatusFilter::All;
        self.test = None;
        self.title = None;
        self.text.clear();
    }

    /// Rebuilds the grids only when the data revision or the date changed.
    fn refresh(&mut self, session: &Session, today: NaiveDate, sections: &[&Section]) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, d, ..)) if *r == rev && *d == today) {
            return;
        }
        let data = session.data();
        let idx = Index::build(data);
        let grids: HashMap<Id, Grid> = sections
            .iter()
            .map(|s| (s.id.clone(), build_grid(data, &idx, s, today)))
            .collect();
        let combined = combine(data, sections, &grids);
        self.cache = Some((rev, today, grids, combined));
    }
}

/// Joins the section grids side by side (see [`Combined`]). A person missing
/// from a section gets "—" across its columns.
fn combine(data: &Data, sections: &[&Section], grids: &HashMap<Id, Grid>) -> Combined {
    let all = data.sections_sorted();
    let mut columns = Vec::new();
    let mut groups = Vec::new();
    for s in sections {
        let g = &grids[&s.id];
        if g.columns.is_empty() {
            continue;
        }
        let position = all.iter().position(|x| x.id == s.id).unwrap_or(0);
        groups.push(Group {
            name: s.name.clone(),
            color: rgb(s.color_rgb(position)),
            start: columns.len(),
            len: g.columns.len(),
        });
        columns.extend(g.columns.iter().cloned());
    }
    // Everyone in any section: the first section's people in its order, then
    // whoever the next section adds, and so on.
    let mut people: Vec<(&str, &str, &str)> = Vec::new();
    for s in sections {
        for r in &grids[&s.id].rows {
            if !people.iter().any(|(id, ..)| *id == r.id) {
                people.push((&r.id, &r.name, &r.title));
            }
        }
    }
    let rows = people
        .into_iter()
        .map(|(id, name, title)| {
            let mut cells = Vec::with_capacity(columns.len());
            for s in sections {
                let g = &grids[&s.id];
                match g.rows.iter().find(|r| r.id == id) {
                    Some(r) => cells.extend(r.cells.iter().cloned()),
                    None => cells.extend((0..g.columns.len()).map(|_| CellView::Unassigned)),
                }
            }
            Row {
                id: id.to_owned(),
                name: name.to_owned(),
                title: title.to_owned(),
                cells,
            }
        })
        .collect();
    Combined {
        grid: Grid {
            columns,
            rule: fullest_rule(data),
            rows,
        },
        groups,
    }
}

/// The rule with the most stages among those in use: its stages are the lines
/// of the Completed view, and every test's own stages are placed on them.
fn fullest_rule(data: &Data) -> Rule {
    let live = data.sections.iter().filter(|s| !s.retired);
    let tests = live.flat_map(|s| s.tests.iter().filter(|t| !t.retired));
    let mut full = data.rule_for_test("");
    for t in tests {
        let r = data.rule_for_test(&t.id);
        if r.stages.len() > full.stages.len() {
            full = r;
        }
    }
    full.clone()
}

fn build_grid(data: &Data, idx: &Index, section: &Section, today: NaiveDate) -> Grid {
    let mut tests: Vec<_> = section.tests.iter().filter(|t| !t.retired).collect();
    tests.sort_by_key(|t| t.order);
    let full = fullest_rule(data);
    let mut staff: Vec<_> = data
        .staff
        .iter()
        .filter(|s| s.status == crate::model::StaffStatus::Active)
        .filter(|s| tests.iter().any(|t| s.assignments.contains_key(&t.id)))
        .collect();
    // The workbook's order; people it doesn't list follow by name.
    staff.sort_by_key(|s| {
        let place = section.staff_order.iter().position(|id| *id == s.id);
        (place.unwrap_or(usize::MAX), s.name.to_lowercase())
    });
    let rows = staff
        .into_iter()
        .map(|s| {
            let cells = tests
                .iter()
                .map(|t| match s.assignments.get(&t.id).map(|a| a.state) {
                    Some(AssignState::Assigned) => {
                        let rule = data.rule_for_test(&t.id);
                        let at = |stage| stage_in(&full, rule, stage);
                        // A completion keeps the stage it was recorded as,
                        // even if the test's rule lost a stage since.
                        let place = |r: &Record| {
                            let shorter = rule.stages.len() < full.stages.len();
                            (0..MAX_STAGES)
                                .find(|k| shorter && stage_label(&full, *k) == r.stage_label)
                                .unwrap_or_else(|| at(r.stage_index))
                        };
                        let pair = idx.pair(&s.id, &t.id);
                        let info = next_due(rule, &pair, &data.settings);
                        let status = classify(info.due_on, today, data.settings.due_soon_days);
                        let delayed = idx
                            .delay_notes(&s.id, &t.id)
                            .iter()
                            .any(|n| n.for_stage_index == info.next_stage_index);
                        let mut done: Vec<Done> = pair
                            .records
                            .iter()
                            .map(|r| Done {
                                stage: place(r),
                                on: r.completed_on,
                                unconfirmed: r.unconfirmed,
                            })
                            .collect();
                        done.sort_by_key(|d| (d.on, d.stage));
                        let mut na = stages_without(&full, rule);
                        na.extend(pair.skipped.iter().map(|s| at(*s)));
                        CellView::Due {
                            info,
                            status,
                            delayed,
                            done,
                            na,
                        }
                    }
                    Some(AssignState::Na) => CellView::Na,
                    None => CellView::Unassigned,
                })
                .collect();
            Row {
                id: s.id.clone(),
                name: s.name.clone(),
                title: s.title.clone(),
                cells,
            }
        })
        .collect();
    Grid {
        columns: tests
            .iter()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect(),
        rule: full,
        rows,
    }
}

fn matches_status(cell: &CellView, f: StatusFilter) -> bool {
    let CellView::Due {
        status, delayed, ..
    } = cell
    else {
        return false;
    };
    match f {
        StatusFilter::All => true,
        StatusFilter::Overdue => *status == DueStatus::Overdue,
        StatusFilter::DueSoon => *status == DueStatus::DueSoon,
        StatusFilter::OnTrack => *status == DueStatus::OnTrack,
        StatusFilter::NotStarted => *status == DueStatus::NotStarted,
        StatusFilter::Delayed => {
            *delayed && matches!(status, DueStatus::Overdue | DueStatus::DueSoon)
        }
    }
}

/// The columns and rows of `grid` left by the filters, and the sorted column.
fn visible(
    grid: &Grid,
    test: Option<&Id>,
    title: Option<&String>,
    needle: &str,
    status: StatusFilter,
    sort: RowSort,
    sort_by_test: Option<&Id>,
) -> (Vec<usize>, Vec<usize>, Option<usize>) {
    let cols: Vec<usize> = (0..grid.columns.len())
        .filter(|&c| test.is_none_or(|t| *t == grid.columns[c].0))
        .collect();
    let mut rows: Vec<usize> = (0..grid.rows.len())
        .filter(|&r| {
            let row = &grid.rows[r];
            // Nobody is listed only for "—" cells (someone outside the
            // chosen test system, or another section's person).
            cols.iter()
                .any(|&c| !matches!(row.cells[c], CellView::Unassigned))
                && title.is_none_or(|t| t.eq_ignore_ascii_case(&row.title))
                && (needle.is_empty() || row.name.to_lowercase().contains(needle))
                && (status == StatusFilter::All
                    || cols.iter().any(|&c| matches_status(&row.cells[c], status)))
        })
        .collect();
    let sort_col =
        sort_by_test.and_then(|t| cols.iter().copied().find(|&c| grid.columns[c].0 == *t));
    let due = |r: usize, c: usize| match &grid.rows[r].cells[c] {
        CellView::Due { info, .. } => info.due_on,
        _ => None,
    };
    // Each sort is stable, so ties keep the default order.
    let name = |r: usize| grid.rows[r].name.to_lowercase();
    match sort {
        RowSort::Default => {}
        RowSort::NameAsc => rows.sort_by_key(|&r| name(r)),
        RowSort::NameDesc => rows.sort_by_key(|&r| std::cmp::Reverse(name(r))),
        RowSort::Title => rows.sort_by_key(|&r| {
            let title = grid.rows[r].title.to_lowercase();
            (title.is_empty(), title)
        }),
        RowSort::Due => rows.sort_by_key(|&r| {
            let first = cols.iter().filter_map(|&c| due(r, c)).min();
            (first.is_none(), first)
        }),
    }
    if let Some(c) = sort_col {
        rows.sort_by_key(|&r| (due(r, c).is_none(), due(r, c)));
    }
    (cols, rows, sort_col)
}

/// The lines the Completed view shows for one person: every stage, a line for
/// every further annual they have done in any of `cols`, then "Next due".
fn stage_lines(grid: &Grid, cols: &[usize], row: &Row) -> Vec<Line> {
    let mut lines = Vec::new();
    if cols.is_empty() {
        return lines;
    }
    let rule = &grid.rule;
    // How many lines each stage needs (more than one only when a cell has
    // several completions of the same stage).
    let mut need: BTreeMap<u32, usize> = (0..rule.stages.len() as u32).map(|k| (k, 1)).collect();
    for &c in cols {
        if let CellView::Due { done, .. } = &row.cells[c] {
            for d in done {
                let n = done.iter().filter(|x| x.stage == d.stage).count();
                let slot = need.entry(d.stage).or_default();
                *slot = n.max(*slot);
            }
        }
    }
    for (stage, n) in need {
        for nth in 0..n {
            lines.push(Line {
                stage: Some(stage),
                nth,
                label: if nth == 0 {
                    stage_label(rule, stage)
                } else {
                    String::new()
                },
            });
        }
    }
    lines.push(Line {
        stage: None,
        nth: 0,
        label: NEXT_DUE.to_owned(),
    });
    lines
}

const NEXT_DUE: &str = "Next due";

/// How far along a schedule a recorded stage's name is looked for.
const MAX_STAGES: u32 = 60;

/// Applies what the user did in a grid.
fn apply_action(cx: &mut Ctx, sort_by_test: &mut Option<Id>, action: Option<GridAction>) {
    match action {
        Some(GridAction::Open(s, t)) => *cx.open_detail = Some((s, t)),
        Some(GridAction::Sort(t)) => {
            *sort_by_test = if sort_by_test.as_ref() == Some(&t) {
                None
            } else {
                Some(t)
            };
        }
        Some(GridAction::Assign(s, t)) => {
            let res = cx.session.mutate(cx.now, |d, a| {
                ops::set_assignment(d, a, &s, &t, Some(AssignState::Assigned))
            });
            let msg = match res {
                Some(Ok(())) => "Assigned".to_owned(),
                Some(Err(e)) => format!("Couldn't assign: {e}"),
                None => "Start editing to make changes".to_owned(),
            };
            cx.toasts.push(Toast::new(msg));
        }
        None => {}
    }
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SectionsState) {
    let pal = cx.pal;
    let session: &Session = cx.session;
    let data = session.data();
    let sections: Vec<&Section> = data
        .sections_sorted()
        .into_iter()
        .filter(|s| !s.retired)
        .collect();
    let Some(first) = sections.first() else {
        empty_state(ui, "No sections", "Add sections in Settings.");
        return;
    };
    let find = |id: Option<&str>| id.and_then(|id| sections.iter().copied().find(|s| s.id == id));
    let mut section = find(state.section.as_deref()).unwrap_or(first);
    let current = if state.all {
        None
    } else {
        Some(section.id.as_str())
    };
    if let Some(picked) = section_pills(ui, pal, &sections, current) {
        match picked {
            None => state.all = true,
            Some(id) => {
                state.all = false;
                section = find(Some(&id)).unwrap_or(section);
            }
        }
        state.test = None;
        state.sort_by_test = None;
    }
    if state.section.as_deref() != Some(section.id.as_str()) {
        state.section = Some(section.id.clone());
    }
    ui.add_space(6.0);
    state.refresh(session, cx.today, &sections);
    if state.all {
        let action = show_all(ui, pal, data, session.is_editing(), &sections, state);
        apply_action(cx, &mut state.sort_by_test, action);
        return;
    }
    let SectionsState {
        view,
        status,
        test,
        title,
        text,
        sort_by_test,
        sort,
        cache,
        ..
    } = state;
    let grid = &cache.as_ref().expect("refreshed above").2[&section.id];
    if test
        .as_ref()
        .is_some_and(|t| !grid.columns.iter().any(|(id, _)| id == t))
    {
        *test = None;
    }
    if filters(
        ui,
        pal,
        data,
        &grid.columns,
        status,
        test,
        title,
        text,
        sort,
        view,
    ) {
        *sort_by_test = None;
    }
    ui.add_space(10.0);

    if grid.columns.is_empty() {
        empty_state(
            ui,
            "No test systems",
            &format!("Add test systems to {} in Settings.", section.name),
        );
        return;
    }
    if grid.rows.is_empty() {
        empty_state(
            ui,
            "No staff assigned",
            &format!("Assign staff to {} from Personnel.", section.name),
        );
        return;
    }
    let needle = text.trim().to_lowercase();
    let (cols, rows, sort_col) = visible(
        grid,
        test.as_ref(),
        title.as_ref(),
        &needle,
        *status,
        *sort,
        sort_by_test.as_ref(),
    );
    if rows.is_empty() {
        empty_state(
            ui,
            "No matches",
            "Nobody in this section matches these filters.",
        );
        return;
    }
    let action = draw_grid(
        ui,
        pal,
        session.is_editing(),
        grid,
        &cols,
        &rows,
        sort_col,
        &section.id,
        &[],
        *view,
    );
    apply_action(cx, sort_by_test, action);
}

/// Every section at once, as one table: each section's test systems side by
/// side under a band in the section's colour, in the configured order, and
/// one row per person. It fills the page like a single section's grid.
fn show_all(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    editing: bool,
    sections: &[&Section],
    state: &mut SectionsState,
) -> Option<GridAction> {
    let SectionsState {
        view,
        status,
        test,
        title,
        text,
        sort_by_test,
        sort,
        cache,
        ..
    } = state;
    let Combined { grid, groups } = &cache.as_ref().expect("refreshed above").3;
    // Test systems of every section, labelled "test · section" as on Summary.
    let columns: Vec<(Id, String)> = groups
        .iter()
        .flat_map(|g| {
            grid.columns[g.start..g.start + g.len]
                .iter()
                .map(|(id, name)| (id.clone(), format!("{name} \u{b7} {}", g.name)))
        })
        .collect();
    if test
        .as_ref()
        .is_some_and(|t| !columns.iter().any(|(id, _)| id == t))
    {
        *test = None;
    }
    if filters(
        ui, pal, data, &columns, status, test, title, text, sort, view,
    ) {
        *sort_by_test = None;
    }
    ui.add_space(10.0);
    if grid.columns.is_empty() {
        let body = if sections.is_empty() {
            "Add sections in Settings."
        } else {
            "Add test systems to your sections in Settings."
        };
        empty_state(ui, "No test systems", body);
        return None;
    }
    if grid.rows.is_empty() {
        empty_state(
            ui,
            "No staff assigned",
            "Assign staff to sections from Personnel.",
        );
        return None;
    }
    let needle = text.trim().to_lowercase();
    let (cols, rows, sort_col) = visible(
        grid,
        test.as_ref(),
        title.as_ref(),
        &needle,
        *status,
        *sort,
        sort_by_test.as_ref(),
    );
    if rows.is_empty() {
        empty_state(ui, "No matches", "Nobody matches these filters.");
        return None;
    }
    draw_grid(
        ui,
        pal,
        editing,
        grid,
        &cols,
        &rows,
        sort_col,
        "all_sections",
        groups,
        *view,
    )
}

/// A segmented row: "All sections", then each section. Returns the newly
/// picked one: `Some(None)` for all, `Some(Some(id))` for a section.
fn section_pills(
    ui: &mut egui::Ui,
    pal: &Palette,
    sections: &[&Section],
    current: Option<&str>,
) -> Option<Option<Id>> {
    let mut labels = vec!["All sections"];
    labels.extend(sections.iter().map(|s| s.name.as_str()));
    let at = current
        .and_then(|id| sections.iter().position(|s| s.id == id))
        .map_or(0, |i| i + 1);
    match segmented(ui, pal, at, &labels) {
        Some(i) if i == at => None,
        Some(0) => Some(None),
        Some(i) => Some(Some(sections[i - 1].id.clone())),
        None => None,
    }
}

/// "Next due" / "Completed", whole on the next line when this one is full.
fn view_switch(ui: &mut egui::Ui, pal: &Palette, view: &mut View) {
    const VIEWS: [&str; 2] = ["Next due", "Completed"];
    let font = egui::TextStyle::Button.resolve(ui.style());
    let words: f32 = VIEWS
        .iter()
        .map(|v| {
            let p = ui.painter();
            p.layout_no_wrap((*v).to_owned(), font.clone(), pal.text)
                .size()
                .x
        })
        .sum();
    // Each choice's padding, and the track around them.
    let left = ui.available_size_before_wrap().x;
    if left < words + 4.0 * ui.spacing().button_padding.x + 12.0 {
        ui.end_row();
    }
    let at = usize::from(*view == View::Completed);
    if let Some(i) = segmented(ui, pal, at, &VIEWS) {
        *view = if i == 0 { View::Due } else { View::Completed };
    }
}

/// One row: the name search, the status, test system and title chips, the
/// sort, then which view. Returns true when a sort was picked.
#[allow(clippy::too_many_arguments)]
fn filters(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    columns: &[(Id, String)],
    status: &mut StatusFilter,
    test: &mut Option<Id>,
    title: &mut Option<String>,
    text: &mut String,
    sort: &mut RowSort,
    view: &mut View,
) -> bool {
    let mut sorted = false;
    let statuses = [
        (StatusFilter::Overdue, "Overdue".to_owned()),
        (StatusFilter::DueSoon, "Due soon".to_owned()),
        (StatusFilter::OnTrack, "On track".to_owned()),
        (StatusFilter::NotStarted, "Not started".to_owned()),
        (StatusFilter::Delayed, "Delayed".to_owned()),
    ];
    let titles: Vec<(String, String)> = distinct_titles(data)
        .into_iter()
        .map(|t| (t.clone(), t))
        .collect();
    ui.horizontal_wrapped(|ui| {
        // As tall as the view switch, so everything on the row centres on it.
        ui.set_row_height(ui.spacing().interact_size.y + 8.0);
        search_box(ui, text, "Search by name", SEARCH_W);
        let mut chosen = (*status != StatusFilter::All).then_some(*status);
        if chip_select(ui, "Status", &mut chosen, &statuses) {
            *status = chosen.unwrap_or_default();
        }
        chip_select(ui, "Test system", test, columns);
        chip_select(ui, "Title", title, &titles);
        sorted = sort_select(ui, sort);
        view_switch(ui, pal, view);
        let any = *status != StatusFilter::All
            || test.is_some()
            || title.is_some()
            || !text.trim().is_empty();
        if any
            && ui
                .add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false))
                .clicked()
        {
            *status = StatusFilter::All;
            *test = None;
            *title = None;
            text.clear();
        }
    });
    sorted
}

/// The sort dropdown. Returns true when one was picked (even the current one,
/// which then undoes sorting by a column).
fn sort_select(ui: &mut egui::Ui, sort: &mut RowSort) -> bool {
    let button = Button::new(format!(
        "{}  Sort: {}  {}",
        icon::SORT_ASCENDING,
        sort.label(),
        icon::CARET_DOWN
    ))
    .corner_radius(255)
    .selected(*sort != RowSort::Default);
    let resp = ui.add(button);
    let mut picked = false;
    Popup::menu(&resp).show(|ui| {
        for s in RowSort::ALL {
            if ui.selectable_label(*sort == s, s.label()).clicked() {
                *sort = s;
                picked = true;
            }
        }
    });
    picked
}

/// Where the grid's parts sit this frame.
struct Layout2 {
    /// Each visible row's top, and then the end of the last one. A row is a
    /// block of stage lines when Completed, so rows differ in height.
    tops: Vec<f32>,
    /// Stage lines per visible row (Completed view); empty for Next due.
    lines: Vec<Vec<Line>>,
    /// Name column, plus the stage column when Completed.
    frozen_w: f32,
}

impl Layout2 {
    fn new(view: View, grid: &Grid, cols: &[usize], rows: &[usize]) -> Layout2 {
        let lines: Vec<Vec<Line>> = match view {
            View::Due => Vec::new(),
            View::Completed => rows
                .iter()
                .map(|&r| stage_lines(grid, cols, &grid.rows[r]))
                .collect(),
        };
        let mut tops = vec![0.0];
        for i in 0..rows.len() {
            let h = match lines.get(i) {
                Some(l) => l.len() as f32 * LINE_H + 2.0 * BLOCK_PAD,
                None => ROW_H,
            };
            tops.push(tops[i] + h);
        }
        Layout2 {
            tops,
            lines,
            frozen_w: match view {
                View::Due => NAME_W,
                View::Completed => NAME_W + STAGE_W,
            },
        }
    }

    fn height(&self) -> f32 {
        self.tops.last().copied().unwrap_or(0.0)
    }

    fn row_h(&self, i: usize) -> f32 {
        self.tops[i + 1] - self.tops[i]
    }

    /// The row at `y` (from the top of the first row).
    fn row_at(&self, y: f32) -> Option<usize> {
        (y >= 0.0 && y < self.height()).then(|| self.tops.partition_point(|t| *t <= y) - 1)
    }
}

/// Draws the grid in the remaining space: a fixed corner, a header strip that
/// follows the body's horizontal scroll, a name column that follows its
/// vertical scroll, and the scrolling cells. The frame hugs the content, so
/// the cells scroll only when they don't fit.
#[allow(clippy::too_many_arguments)]
fn draw_grid(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    grid: &Grid,
    cols: &[usize],
    rows: &[usize],
    sort_col: Option<usize>,
    section_id: &str,
    // Section column groups (the combined grid); empty for one section.
    groups: &[Group],
    view: View,
) -> Option<GridAction> {
    let mut action = None;
    let lay = Layout2::new(view, grid, cols, rows);
    let band = if groups.is_empty() { 0.0 } else { BAND_H };
    let head_h = HEAD_H + band;
    // Runs of visible columns per group: (group, first visible j, end j).
    let mut runs: Vec<(&Group, usize, usize)> = Vec::new();
    for (j, &c) in cols.iter().enumerate() {
        let Some(g) = groups
            .iter()
            .find(|g| (g.start..g.start + g.len).contains(&c))
        else {
            continue;
        };
        match runs.last_mut() {
            Some((last, _, end)) if std::ptr::eq(*last, g) && *end == j => *end = j + 1,
            _ => runs.push((g, j, j + 1)),
        }
    }

    // Size the frame to the content (1 px border each side), capped by the
    // space left on the page.
    let avail = ui.available_rect_before_wrap();
    let mut content = Vec2::new(cols.len() as f32 * COL_W, lay.height());
    if lay.frozen_w + content.x + 2.0 > avail.width() {
        content.y += BAR_ROOM;
    }
    let width = (lay.frozen_w + content.x + 2.0).min(avail.width());
    let two_rows = lay.tops[rows.len().min(2)];
    let least = (head_h + two_rows + 2.0).min(avail.height());
    let height = (head_h + content.y + 2.0).min(avail.height()).max(least);
    let outer = Rect::from_min_size(avail.min, Vec2::new(width, height));
    ui.allocate_rect(outer, Sense::hover());
    ui.painter().rect(
        outer,
        CornerRadius::same(10),
        pal.card,
        Stroke::new(1.0, pal.border),
        StrokeKind::Inside,
    );
    let inner = outer.shrink(1.0);
    let header = Rect::from_min_max(
        inner.min + Vec2::new(lay.frozen_w, 0.0),
        egui::pos2(inner.right(), inner.top() + head_h),
    );
    let names = Rect::from_min_max(
        inner.min + Vec2::new(0.0, head_h),
        egui::pos2(inner.left() + lay.frozen_w, inner.bottom()),
    );
    let body = Rect::from_min_max(inner.min + Vec2::new(lay.frozen_w, head_h), inner.max);

    // The row under the pointer, lit across the names and the cells.
    let offset_key = ui.id().with(("grid_offset", section_id));
    let prev: Vec2 = ui.data(|d| d.get_temp(offset_key)).unwrap_or_default();
    let hover_row = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|_| ui.rect_contains_pointer(body.union(names)))
        .and_then(|p| lay.row_at(p.y - body.top() + prev.y));

    // The wheel over the header or the name column scrolls the cells too.
    let mut area = ScrollArea::both()
        .id_salt(("grid_scroll", section_id))
        .auto_shrink(false);
    if ui.rect_contains_pointer(header) || ui.rect_contains_pointer(names) {
        let delta = ui.input(|i| i.smooth_scroll_delta);
        if delta != Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            let max = (content - body.size()).max(Vec2::ZERO);
            area = area.scroll_offset((prev - delta).clamp(Vec2::ZERO, max));
        }
    }
    let mut body_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .id_salt(("grid_body", section_id)),
    );
    let line = Stroke::new(1.0, pal.border);
    let out = area.show_viewport(&mut body_ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let y = (origin.y + viewport.top())..=(origin.y + viewport.bottom());
        let p = ui.painter().clone();
        // A faint wash of each section's colour down its columns, and a
        // divider in its colour where one section ends and the next begins.
        for (k, (g, j0, j1)) in runs.iter().enumerate() {
            let x0 = origin.x + *j0 as f32 * COL_W;
            let x1 = origin.x + *j1 as f32 * COL_W;
            let wash = Rect::from_x_y_ranges(x0..=x1, y.clone());
            p.rect_filled(wash, 0, g.color.gamma_multiply(0.04));
            if k > 0 {
                p.vline(
                    x0,
                    y.clone(),
                    Stroke::new(1.5, g.color.gamma_multiply(0.55)),
                );
            }
        }
        let first = lay.row_at(viewport.top().max(0.0)).unwrap_or(0);
        let last = lay.row_at(viewport.bottom()).map_or(rows.len(), |i| i + 1);
        let c0 = (viewport.left() / COL_W).floor().max(0.0) as usize;
        let c1 = ((viewport.right() / COL_W).ceil() as usize).min(cols.len());
        let xs = (origin.x + viewport.left())..=(origin.x + viewport.right());
        for (i, &r) in rows.iter().enumerate().take(last).skip(first) {
            let top = origin.y + lay.tops[i];
            let row_h = lay.row_h(i);
            if hover_row == Some(i) {
                let rect = Rect::from_x_y_ranges(xs.clone(), top..=top + row_h);
                p.rect_filled(rect, 0, pal.sidebar);
            }
            if i + 1 < rows.len() {
                p.hline(xs.clone(), top + row_h, line);
            }
            let row = &grid.rows[r];
            for (j, &c) in cols.iter().enumerate().take(c1).skip(c0) {
                let min = egui::pos2(origin.x + j as f32 * COL_W, top);
                let rect = Rect::from_min_size(min, Vec2::new(COL_W, row_h));
                let test = &grid.columns[c];
                let lines = lay.lines.get(i).map(Vec::as_slice);
                let hit = cell(ui, pal, editing, rect, row, test, &row.cells[c], lines);
                if hit.is_some() {
                    action = hit;
                }
            }
        }
        ui.allocate_rect(Rect::from_min_size(origin, content), Sense::hover());
    });
    let offset = out.state.offset;
    ui.data_mut(|d| d.insert_temp(offset_key, offset));

    // Header strip.
    let hp = ui.painter_at(header);
    // Section bands: a solid edge in the section's colour over a tint, and
    // its name, which slides along to stay in view while the band is.
    for (k, (g, j0, j1)) in runs.iter().enumerate() {
        let x0 = header.left() + *j0 as f32 * COL_W - offset.x;
        let x1 = header.left() + *j1 as f32 * COL_W - offset.x;
        let rect = Rect::from_min_max(
            egui::pos2(x0, header.top()),
            egui::pos2(x1, header.top() + band),
        );
        if !rect.intersects(header) {
            continue;
        }
        hp.rect_filled(
            Rect::from_min_max(rect.min, egui::pos2(rect.right(), header.top() + head_h)),
            0,
            g.color.gamma_multiply(0.08),
        );
        hp.rect_filled(
            Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + 3.0)),
            0,
            g.color,
        );
        if k > 0 {
            hp.vline(
                x0,
                header.top()..=header.top() + head_h,
                Stroke::new(1.5, g.color.gamma_multiply(0.55)),
            );
        }
        let galley =
            ui.painter()
                .layout_no_wrap(g.name.clone(), FontId::new(12.5, medium()), pal.text);
        // The name in a pill of the section's colour.
        let pad = 9.0;
        let need = galley.size().x + 2.0 * pad;
        let left = (rect.left().max(header.left()) + 8.0).min(rect.right() - need - 8.0);
        let left = left.max(rect.left() + 8.0);
        let mid = rect.top() + 3.0 + (band - 3.0) / 2.0;
        let clip = hp.with_clip_rect(rect.intersect(header));
        clip.rect_filled(
            Rect::from_min_size(egui::pos2(left, mid - 9.5), Vec2::new(need, 19.0)),
            255,
            g.color.gamma_multiply(SECTION_TINT),
        );
        clip.galley(
            egui::pos2(left + pad, mid - galley.size().y / 2.0),
            galley,
            pal.text,
        );
        let n = j1 - j0;
        let count = if n == 1 {
            "1 test system".to_owned()
        } else {
            format!("{n} test systems")
        };
        let resp = ui.interact(
            rect.intersect(header),
            ui.id().with(("grid_band", section_id, &g.name)),
            Sense::hover(),
        );
        resp.on_hover_text(format!("{}: {count}", g.name));
    }
    for (j, &c) in cols.iter().enumerate() {
        let rect = Rect::from_min_size(
            egui::pos2(
                header.left() + j as f32 * COL_W - offset.x,
                header.top() + band,
            ),
            Vec2::new(COL_W, HEAD_H),
        );
        let (id, name) = &grid.columns[c];
        let hit = rect.intersect(header);
        if !hit.is_positive() {
            continue;
        }
        let resp = ui.interact(
            hit,
            ui.id().with(("grid_head", section_id, id)),
            Sense::click(),
        );
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
        let resp = resp
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(format!("{name}\nClick to sort by due date"));
        if resp.hovered() {
            hp.rect_filled(rect.shrink(3.0), CornerRadius::same(6), pal.sidebar);
        }
        let sorted = sort_col == Some(c);
        let mut job = LayoutJob::simple(
            name.clone(),
            FontId::new(12.0, medium()),
            if sorted { pal.accent } else { pal.text },
            // Room either side for the sort mark, so centred text clears it.
            COL_W - 40.0,
        );
        job.wrap.max_rows = 2;
        job.halign = Align::Center;
        let galley = ui.painter().layout_job(job);
        // A centred job lays its lines out around x = 0.
        let pos = egui::pos2(rect.center().x, rect.center().y - galley.size().y / 2.0);
        hp.galley(pos, galley, pal.text);
        if sorted {
            hp.text(
                egui::pos2(rect.right() - 12.0, rect.center().y),
                Align2::CENTER_CENTER,
                icon::SORT_ASCENDING,
                FontId::proportional(13.0),
                pal.accent,
            );
        }
        if resp.clicked() {
            action = Some(GridAction::Sort(id.clone()));
        }
    }

    // Name column (and stage labels when Completed).
    let np = ui.painter_at(names);
    for (i, &r) in rows.iter().enumerate() {
        let top = names.top() + lay.tops[i] - offset.y;
        let row_h = lay.row_h(i);
        if top > names.bottom() || top + row_h < names.top() {
            continue;
        }
        let row = &grid.rows[r];
        let band_rect = Rect::from_min_max(
            egui::pos2(names.left(), top),
            egui::pos2(names.right(), top + row_h),
        );
        if hover_row == Some(i) {
            np.rect_filled(band_rect, 0, pal.sidebar);
        }
        let x = names.left() + 14.0;
        let clip = Rect::from_min_max(
            egui::pos2(x, top),
            egui::pos2(names.left() + NAME_W - 8.0, top + row_h),
        )
        .intersect(names);
        let p = np.with_clip_rect(clip);
        let fit = clip.width().max(0.0);
        let has_title = !row.title.is_empty();
        let (name_y, title_y) = match view {
            View::Due if has_title => (top + row_h / 2.0 - 8.0, top + row_h / 2.0 + 8.0),
            View::Due => (top + row_h / 2.0, 0.0),
            View::Completed => (
                top + BLOCK_PAD + LINE_H / 2.0,
                top + BLOCK_PAD + LINE_H * 1.5,
            ),
        };
        let g = one_line(&p, &row.name, FontId::new(13.5, medium()), pal.text, fit);
        p.galley(egui::pos2(x, name_y - g.size().y / 2.0), g, pal.text);
        if has_title {
            let g = one_line(&p, &row.title, FontId::proportional(11.5), pal.muted, fit);
            p.galley(egui::pos2(x, title_y - g.size().y / 2.0), g, pal.muted);
        }
        for (k, l) in lay.lines.get(i).into_iter().flatten().enumerate() {
            let y = top + BLOCK_PAD + (k as f32 + 0.5) * LINE_H;
            np.text(
                egui::pos2(names.left() + NAME_W, y),
                Align2::LEFT_CENTER,
                &l.label,
                FontId::proportional(11.5),
                pal.muted,
            );
        }
        if i + 1 < rows.len() {
            np.hline(names.x_range(), top + row_h, line);
        }
    }

    // Corner and dividers.
    let corner = Rect::from_min_max(
        inner.min + Vec2::new(0.0, band),
        egui::pos2(inner.left() + lay.frozen_w, inner.top() + head_h),
    );
    let p = ui.painter();
    if band > 0.0 {
        p.hline(corner.x_range(), inner.top() + band, line);
    }
    let people = if rows.len() == 1 {
        "1 person".to_owned()
    } else {
        format!("{} people", rows.len())
    };
    p.text(
        egui::pos2(corner.left() + 14.0, corner.center().y),
        Align2::LEFT_CENTER,
        people,
        FontId::new(12.0, medium()),
        pal.muted,
    );
    if view == View::Completed {
        p.text(
            egui::pos2(corner.left() + NAME_W, corner.center().y),
            Align2::LEFT_CENTER,
            "Stage",
            FontId::new(12.0, medium()),
            pal.muted,
        );
    }
    p.hline(inner.x_range(), header.bottom(), line);
    p.vline(names.right(), outer.y_range(), line);
    action
}

/// `text` on one line, cut with "…" to fit `max_w`.
fn one_line(
    p: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, max_w);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    p.layout_job(job)
}

/// A tinted pill with the due date (or `none` when there isn't one),
/// centred on (`cx`, `y`), `h` tall.
#[allow(clippy::too_many_arguments)]
fn pill(
    p: &egui::Painter,
    pal: &Palette,
    cx: f32,
    y: f32,
    h: f32,
    max_w: f32,
    info: &DueInfo,
    status: DueStatus,
    delayed: bool,
    none: &str,
) {
    // Just the date: where it came from is in the tooltip.
    let mut text = info.due_on.map_or_else(|| none.to_owned(), fmt_date);
    if delayed {
        text = format!("{} {text}", icon::PAUSE);
    }
    let (fill, fg) = status_colors(pal, status);
    let galley = p.layout_no_wrap(text, FontId::proportional(11.5), fg);
    let w = (galley.size().x + 16.0).min(max_w);
    let rect = Rect::from_center_size(egui::pos2(cx, y), Vec2::new(w, h));
    p.rect_filled(rect, CornerRadius::same(255), fill);
    let x = (cx - galley.size().x / 2.0).max(rect.left() + 8.0);
    p.with_clip_rect(rect.shrink2(Vec2::new(6.0, 0.0))).galley(
        egui::pos2(x, y - galley.size().y / 2.0),
        galley,
        fg,
    );
}

/// One grid cell, painted in `rect`. Next due: pill over the stage it's for.
/// Completed: a date (or what's next) on each stage line. N/A, or a faint "—"
/// that offers "Assign" in edit mode.
#[allow(clippy::too_many_arguments)]
fn cell(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    rect: Rect,
    row: &Row,
    test: &(Id, String),
    view: &CellView,
    // The person's stage lines, in the Completed view.
    lines: Option<&[Line]>,
) -> Option<GridAction> {
    let clickable = !matches!(view, CellView::Unassigned) || editing;
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(
        rect.shrink(2.0),
        ui.id().with(("cell", &row.id, &test.0)),
        sense,
    );
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if clickable && resp.hovered() {
        p.rect_filled(rect.shrink(3.0), CornerRadius::same(6), pal.border);
    }
    // Everything in a cell is centred across the column.
    let cx = rect.center().x;
    let max_w = COL_W - 20.0;
    let completed = lines.is_some();
    // The first line of the cell: the middle of the row, or the first stage line.
    let first_y = if completed {
        rect.top() + BLOCK_PAD + LINE_H / 2.0
    } else {
        rect.center().y
    };
    let quiet = |p: &egui::Painter, text: &str, color: Color32| {
        p.text(
            egui::pos2(cx, first_y),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(12.0),
            color,
        );
    };
    let mut tip = None;
    match view {
        CellView::Due {
            info,
            status,
            delayed,
            ..
        } if !completed => {
            pill(
                &p,
                pal,
                cx,
                rect.center().y - 7.0,
                20.0,
                max_w,
                info,
                *status,
                *delayed,
                "Initial pending",
            );
            p.text(
                egui::pos2(cx, rect.center().y + 11.0),
                Align2::CENTER_CENTER,
                &info.next_stage_label,
                FontId::proportional(11.0),
                pal.muted,
            );
            tip = Some(format!(
                "{}: {}\nNext: {}, {} ({})",
                row.name,
                test.1,
                info.next_stage_label,
                due_pill_text(info),
                status_label(*status)
            ));
        }
        CellView::Due {
            info,
            status,
            delayed,
            done,
            na,
        } => {
            let lines = lines.unwrap_or_default();
            let line_y = |k: usize| rect.top() + BLOCK_PAD + (k as f32 + 0.5) * LINE_H;
            let due_line = lines.len().saturating_sub(1);
            for (k, l) in lines.iter().enumerate() {
                let Some(stage) = l.stage else { continue };
                let at = egui::pos2(cx, line_y(k));
                let this = done.iter().filter(|d| d.stage == stage).nth(l.nth);
                if let Some(d) = this {
                    // An unconfirmed date (imported, not ticked) reads as such.
                    let color = if d.unconfirmed {
                        pal.yellow_fg
                    } else {
                        pal.text
                    };
                    p.text(
                        at,
                        Align2::CENTER_CENTER,
                        fmt_date(d.on),
                        FontId::proportional(12.5),
                        color,
                    );
                } else if l.nth == 0 && na.contains(&stage) {
                    p.text(
                        at,
                        Align2::CENTER_CENTER,
                        "N/A",
                        FontId::proportional(11.5),
                        pal.muted.gamma_multiply(0.8),
                    );
                }
            }
            // The next due date in a status pill, as in the Next due view,
            // a little shorter to sit inside its stage line.
            pill(
                &p,
                pal,
                cx,
                line_y(due_line),
                LINE_H - 2.0,
                max_w,
                info,
                *status,
                *delayed,
                "Not scheduled",
            );
            let mut text = Vec::new();
            for l in lines.iter().filter(|l| l.nth == 0) {
                let Some(stage) = l.stage else { continue };
                let mut any = false;
                for d in done.iter().filter(|d| d.stage == stage) {
                    any = true;
                    let mark = if d.unconfirmed { " (unconfirmed)" } else { "" };
                    text.push(format!("{}: {}{mark}", l.label, fmt_date(d.on)));
                }
                if !any && na.contains(&stage) {
                    text.push(format!("{}: N/A", l.label));
                }
            }
            text.push(format!(
                "Next: {}, {} ({})",
                info.next_stage_label,
                due_pill_text(info),
                status_label(*status)
            ));
            tip = Some(format!("{}: {}\n{}", row.name, test.1, text.join("\n")));
        }
        CellView::Na => quiet(&p, "N/A", pal.muted),
        CellView::Unassigned => quiet(&p, "\u{2014}", pal.muted.gamma_multiply(0.45)),
    }
    let name = format!("{} \u{b7} {}", row.name, test.1);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, clickable, &name));
    if !clickable {
        return None;
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let resp = match tip {
        Some(t) => resp.on_hover_text(t),
        None => resp,
    };
    if let CellView::Unassigned = view {
        let mut assign = false;
        Popup::menu(&resp).show(|ui| {
            ui.label(
                RichText::new(format!("{} isn't assigned {}", row.name, test.1)).color(pal.muted),
            );
            assign = ui.button(format!("{}  Assign", icon::PLUS)).clicked();
        });
        return assign.then(|| GridAction::Assign(row.id.clone(), test.0.clone()));
    }
    resp.clicked()
        .then(|| GridAction::Open(row.id.clone(), test.0.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::now_ts;
    use crate::model::{Assignment, Skipped, Staff, StaffStatus, TestSystem};

    /// Two Chemistry tests, B on a rule without Semi-annual, done by one person
    /// (Initial, 1st Annual; `label` is what B's annual was recorded as).
    fn cells(annual_index: u32, skip: bool) -> Vec<CellView> {
        let now = now_ts();
        let today = now.date_naive();
        let mut d = Data::new_empty("t", now);
        let mut short = Rule::default_global();
        short.id = "r-short".into();
        short.stages.remove(1);
        d.rules.push(short);
        for (i, id) in ["a", "b"].into_iter().enumerate() {
            d.sections[0].tests.push(TestSystem {
                id: id.into(),
                name: id.into(),
                order: i as u32,
                retired: false,
                rule_id: (i == 1).then(|| "r-short".into()),
            });
            for (stage, label) in [(0, "Initial"), (annual_index, "1st Annual")] {
                d.records.push(Record {
                    id: format!("{id}{stage}"),
                    staff_id: "p".into(),
                    test_id: id.into(),
                    stage_index: stage,
                    stage_label: label.into(),
                    completed_on: today,
                    comment: String::new(),
                    by: "t".into(),
                    at: now,
                    imported: false,
                    unconfirmed: false,
                });
            }
        }
        if skip {
            d.skipped.push(Skipped {
                staff_id: "p".into(),
                test_id: "a".into(),
                stage_index: 1,
                by: "t".into(),
                at: now,
            });
        }
        let assigned = Assignment {
            state: AssignState::Assigned,
            since: today,
        };
        d.staff.push(Staff {
            id: "p".into(),
            name: "P".into(),
            title: String::new(),
            joined: None,
            status: StaffStatus::Active,
            assignments: ["a", "b"].map(|t| (t.to_string(), assigned.clone())).into(),
        });
        let grid = build_grid(&d, &Index::build(&d), &d.sections[0], today);
        assert_eq!(grid.rule.stages.len(), 3, "the lines are the fuller rule's");
        grid.rows[0].cells.clone()
    }

    fn view(c: &CellView) -> (Vec<u32>, Vec<u32>) {
        let CellView::Due { done, na, .. } = c else {
            panic!("assigned")
        };
        (done.iter().map(|d| d.stage).collect(), na.clone())
    }

    #[test]
    fn a_rule_without_semi_annual_shows_it_as_na() {
        // Recorded under its own rule, or while it still had a Semi-annual.
        for annual_index in [1, 2] {
            let c = cells(annual_index, false);
            assert_eq!(view(&c[1]), (vec![0, 2], vec![1]), "{annual_index}");
        }
    }

    #[test]
    fn a_stage_left_out_for_a_person_is_na_too() {
        assert_eq!(view(&cells(2, false)[0]).1, Vec::<u32>::new());
        assert_eq!(view(&cells(2, true)[0]), (vec![0, 2], vec![1]));
    }
}
