//! Sections page: one grid per lab section, staff (rows) × test systems
//! (columns). Two views of the same grid:
//!
//! - **Next due**: each cell shows the next-due pill and the stage it's for.
//! - **Completed**: like the master tracking workbook, each person gets one
//!   line per stage (Initial, Semi-annual, Annual) with the date it was
//!   completed, and the stage still to do shows when it's due.
//!
//! The header row and the name column stay put while the cells scroll; the
//! grid is exactly as tall as its rows, so it only scrolls when they don't fit.

use chrono::NaiveDate;
use egui::text::LayoutJob;
use std::collections::HashMap;

use egui::{
    Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Layout, Popup, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Section};
use crate::ops;
use crate::query::{distinct_titles, Index, StatusFilter};
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::{
    chip_select, due_pill_text, empty_state, rgb, search_box, segmented, status_colors,
    status_label, SEARCH_W,
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

/// What a person has done at one stage of one test.
#[derive(Clone)]
enum StageCell {
    /// Completed on this date; `nth` counts repeats of the last stage
    /// (the 3rd Annual is `Some(3)`).
    Done {
        on: NaiveDate,
        nth: Option<u32>,
    },
    Skipped,
    Open,
}

#[derive(Clone)]
enum CellView {
    Due {
        info: DueInfo,
        status: DueStatus,
        delayed: bool,
        /// One entry per stage of the test's rule; the last one holds the
        /// latest of its repeats.
        stages: Vec<StageCell>,
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
    /// Each column's stage labels (from its schedule rule).
    stages: Vec<Vec<String>>,
    rows: Vec<Row>,
}

/// What the user did in the grid this frame.
enum GridAction {
    Open(Id, Id),
    Assign(Id, Id),
    Sort(Id),
}

impl SectionsState {
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
    let mut stages = Vec::new();
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
        stages.extend(g.stages.iter().cloned());
    }
    // Everyone in any section, by name.
    let mut people: Vec<(&str, &str, &str)> = Vec::new();
    for s in sections {
        for r in &grids[&s.id].rows {
            if !people.iter().any(|(id, ..)| *id == r.id) {
                people.push((&r.id, &r.name, &r.title));
            }
        }
    }
    people.sort_by_key(|(_, name, _)| name.to_lowercase());
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
            stages,
            rows,
        },
        groups,
    }
}

fn build_grid(data: &Data, idx: &Index, section: &Section, today: NaiveDate) -> Grid {
    let mut tests: Vec<_> = section.tests.iter().filter(|t| !t.retired).collect();
    tests.sort_by_key(|t| t.order);
    let mut staff: Vec<_> = data
        .staff
        .iter()
        .filter(|s| s.status == crate::model::StaffStatus::Active)
        .filter(|s| tests.iter().any(|t| s.assignments.contains_key(&t.id)))
        .collect();
    staff.sort_by_key(|s| s.name.to_lowercase());
    let rows = staff
        .into_iter()
        .map(|s| {
            let cells = tests
                .iter()
                .map(|t| match s.assignments.get(&t.id).map(|a| a.state) {
                    Some(AssignState::Assigned) => {
                        let rule = data.rule_for_test(&t.id);
                        let pair = idx.pair(&s.id, &t.id);
                        let info = next_due(rule, &pair, &data.settings);
                        let status = classify(info.due_on, today, data.settings.due_soon_days);
                        let delayed = idx
                            .delay_notes(&s.id, &t.id)
                            .iter()
                            .any(|n| n.for_stage_index == info.next_stage_index);
                        let stages = stage_cells(rule, &pair);
                        CellView::Due {
                            info,
                            status,
                            delayed,
                            stages,
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
        stages: tests
            .iter()
            .map(|t| {
                let rule = data.rule_for_test(&t.id);
                rule.stages.iter().map(|s| s.label.clone()).collect()
            })
            .collect(),
        rows,
    }
}

/// One [`StageCell`] per stage of `rule`: the latest completion of each
/// stage, the last one standing for all of its repeats.
fn stage_cells(rule: &crate::model::Rule, pair: &crate::schedule::PairState) -> Vec<StageCell> {
    let n = rule.stages.len();
    let Some(last) = n.checked_sub(1) else {
        return Vec::new();
    };
    let last = last as u32;
    (0..n as u32)
        .map(|k| {
            let latest = pair
                .records
                .iter()
                .filter(|r| {
                    if k == last {
                        r.stage_index >= last
                    } else {
                        r.stage_index == k
                    }
                })
                .max_by_key(|r| (r.completed_on, r.stage_index));
            match latest {
                Some(r) => StageCell::Done {
                    on: r.completed_on,
                    nth: (k == last && rule.stages[k as usize].ordinal)
                        .then(|| r.stage_index - last + 1),
                },
                None if pair.skipped.contains(&k) => StageCell::Skipped,
                None => StageCell::Open,
            }
        })
        .collect()
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
    if let Some(c) = sort_col {
        let due = |r: usize| match &grid.rows[r].cells[c] {
            CellView::Due { info, .. } => info.due_on,
            _ => None,
        };
        rows.sort_by_key(|&r| (due(r).is_none(), due(r)));
    }
    (cols, rows, sort_col)
}

/// The lines the Completed view shows per person for `cols`: the stages of
/// the test with the most (normally every test shares the Global rule), then
/// "Next due", as in the workbook.
fn stage_lines(grid: &Grid, cols: &[usize]) -> Vec<String> {
    let mut lines = cols
        .iter()
        .map(|&c| &grid.stages[c])
        .max_by_key(|s| s.len())
        .cloned()
        .unwrap_or_default();
    lines.push(NEXT_DUE.to_owned());
    lines
}

const NEXT_DUE: &str = "Next due";

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
    // Sections on the left, the view switch on the right. The switch is
    // placed first, so on a narrow page the sections wrap beside it.
    let picked = ui
        .with_layout(Layout::right_to_left(Align::Min), |ui| {
            view_switch(ui, pal, &mut state.view);
            ui.add_space(12.0);
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                section_pills(ui, pal, &sections, current)
            })
            .inner
        })
        .inner;
    if let Some(picked) = picked {
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
    filters(ui, pal, data, &grid.columns, status, test, title, text);
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
    filters(ui, pal, data, &columns, status, test, title, text);
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

/// "Next due" / "Completed".
fn view_switch(ui: &mut egui::Ui, pal: &Palette, view: &mut View) {
    let at = usize::from(*view == View::Completed);
    if let Some(i) = segmented(ui, pal, at, &["Next due", "Completed"]) {
        *view = if i == 0 { View::Due } else { View::Completed };
    }
}

/// One row: the name search, then the status, test system and title chips.
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
) {
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
        search_box(ui, text, "Search by name", SEARCH_W);
        let mut chosen = (*status != StatusFilter::All).then_some(*status);
        if chip_select(ui, "Status", &mut chosen, &statuses) {
            *status = chosen.unwrap_or_default();
        }
        chip_select(ui, "Test system", test, columns);
        chip_select(ui, "Title", title, &titles);
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
}

/// Where the grid's parts sit this frame.
struct Layout2 {
    /// Height of one person's row (a block of stage lines when Completed).
    row_h: f32,
    /// Stage lines per person (Completed view); empty for Next due.
    lines: Vec<String>,
    /// Name column, plus the stage column when Completed.
    frozen_w: f32,
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
    let lay = match view {
        View::Due => Layout2 {
            row_h: ROW_H,
            lines: Vec::new(),
            frozen_w: NAME_W,
        },
        View::Completed => {
            let lines = stage_lines(grid, cols);
            Layout2 {
                row_h: lines.len() as f32 * LINE_H + 2.0 * BLOCK_PAD,
                lines,
                frozen_w: NAME_W + STAGE_W,
            }
        }
    };
    let row_h = lay.row_h;
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
    let mut content = Vec2::new(cols.len() as f32 * COL_W, rows.len() as f32 * row_h);
    if lay.frozen_w + content.x + 2.0 > avail.width() {
        content.y += BAR_ROOM;
    }
    let width = (lay.frozen_w + content.x + 2.0).min(avail.width());
    let least = (head_h + 2.0 * row_h + 2.0).min(avail.height());
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
        .and_then(|p| {
            let i = ((p.y - body.top() + prev.y) / row_h).floor();
            (i >= 0.0 && (i as usize) < rows.len()).then_some(i as usize)
        });

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
        let first = (viewport.top() / row_h).floor().max(0.0) as usize;
        let last = ((viewport.bottom() / row_h).ceil() as usize).min(rows.len());
        let c0 = (viewport.left() / COL_W).floor().max(0.0) as usize;
        let c1 = ((viewport.right() / COL_W).ceil() as usize).min(cols.len());
        let xs = (origin.x + viewport.left())..=(origin.x + viewport.right());
        for (i, &r) in rows.iter().enumerate().take(last).skip(first) {
            let top = origin.y + i as f32 * row_h;
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
                let hit = cell(ui, pal, editing, rect, row, test, &row.cells[c], &lay);
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
        let dot_w = 13.0;
        let need = dot_w + galley.size().x;
        let left = (rect.left().max(header.left()) + 10.0).min(rect.right() - need - 10.0);
        let left = left.max(rect.left() + 10.0);
        let mid = rect.top() + 3.0 + (band - 3.0) / 2.0;
        let clip = hp.with_clip_rect(rect.intersect(header));
        clip.circle_filled(egui::pos2(left + 3.5, mid), 3.5, g.color);
        clip.galley(
            egui::pos2(left + dot_w, mid - galley.size().y / 2.0),
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
            COL_W - 30.0,
        );
        job.wrap.max_rows = 2;
        let galley = ui.painter().layout_job(job);
        let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0);
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
        let top = names.top() + i as f32 * row_h - offset.y;
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
        for (k, label) in lay.lines.iter().enumerate() {
            let y = top + BLOCK_PAD + (k as f32 + 0.5) * LINE_H;
            np.text(
                egui::pos2(names.left() + NAME_W, y),
                Align2::LEFT_CENTER,
                label,
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

/// A tinted pill with the due date, its left edge at `left`, centred on `y`.
#[allow(clippy::too_many_arguments)]
fn pill(
    p: &egui::Painter,
    pal: &Palette,
    left: f32,
    y: f32,
    max_w: f32,
    info: &DueInfo,
    status: DueStatus,
    delayed: bool,
) {
    // Just the date: where it came from is in the tooltip.
    let mut text = info
        .due_on
        .map_or_else(|| "Initial pending".to_owned(), fmt_date);
    if delayed {
        text = format!("{} {text}", icon::PAUSE);
    }
    let (fill, fg) = status_colors(pal, status);
    let galley = p.layout_no_wrap(text, FontId::proportional(11.5), fg);
    let w = (galley.size().x + 16.0).min(max_w);
    let rect = Rect::from_min_size(egui::pos2(left, y - 10.0), Vec2::new(w, 20.0));
    p.rect_filled(rect, CornerRadius::same(255), fill);
    p.with_clip_rect(rect.shrink2(Vec2::new(6.0, 0.0))).galley(
        egui::pos2(left + 8.0, y - galley.size().y / 2.0),
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
    lay: &Layout2,
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
    let left = rect.left() + 10.0;
    let max_w = COL_W - 20.0;
    let completed = !lay.lines.is_empty();
    // The first line of the cell: the middle of the row, or the first stage line.
    let first_y = if completed {
        rect.top() + BLOCK_PAD + LINE_H / 2.0
    } else {
        rect.center().y
    };
    let quiet = |p: &egui::Painter, text: &str, color: Color32| {
        p.text(
            egui::pos2(left, first_y),
            Align2::LEFT_CENTER,
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
                left,
                rect.center().y - 7.0,
                max_w,
                info,
                *status,
                *delayed,
            );
            p.text(
                egui::pos2(left + 2.0, rect.center().y + 11.0),
                Align2::LEFT_CENTER,
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
            stages,
        } => {
            let line_y = |k: usize| rect.top() + BLOCK_PAD + (k as f32 + 0.5) * LINE_H;
            let due_line = lay.lines.len() - 1;
            for (k, st) in stages.iter().enumerate().take(due_line) {
                let y = line_y(k);
                let at = egui::pos2(left, y);
                match st {
                    StageCell::Done { on, nth } => {
                        let g =
                            p.layout_no_wrap(fmt_date(*on), FontId::proportional(12.5), pal.text);
                        let w = g.size().x;
                        p.galley(egui::pos2(left, y - g.size().y / 2.0), g, pal.text);
                        if let Some(n) = nth.filter(|n| *n > 1) {
                            p.text(
                                egui::pos2(left + w + 6.0, y),
                                Align2::LEFT_CENTER,
                                crate::schedule::ordinal(n),
                                FontId::proportional(10.5),
                                pal.muted,
                            );
                        }
                    }
                    StageCell::Skipped => {
                        p.text(
                            at,
                            Align2::LEFT_CENTER,
                            "Skipped",
                            FontId::proportional(11.5),
                            pal.muted.gamma_multiply(0.8),
                        );
                    }
                    StageCell::Open => {}
                }
            }
            // The next due date: a dot in its status colour, and the date in
            // that colour too when it needs attention.
            let y = line_y(due_line);
            let (_, fg) = status_colors(pal, *status);
            let color = match status {
                DueStatus::Overdue | DueStatus::DueSoon => fg,
                _ => pal.muted,
            };
            let mut text = info
                .due_on
                .map_or_else(|| "Not scheduled".to_owned(), fmt_date);
            if *delayed {
                text = format!("{} {text}", icon::PAUSE);
            }
            p.circle_filled(egui::pos2(left + 3.0, y), 3.0, fg);
            p.text(
                egui::pos2(left + 11.0, y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(12.0),
                color,
            );
            let mut lines = Vec::new();
            for (k, st) in stages.iter().enumerate() {
                let label = lay.lines.get(k).map_or("", String::as_str);
                match st {
                    StageCell::Done { on, nth } => {
                        let label = match nth {
                            Some(n) => format!("{} {label}", crate::schedule::ordinal(*n)),
                            None => label.to_owned(),
                        };
                        lines.push(format!("{label}: {}", fmt_date(*on)));
                    }
                    StageCell::Skipped => lines.push(format!("{label}: skipped")),
                    StageCell::Open => {}
                }
            }
            lines.push(format!(
                "Next: {}, {} ({})",
                info.next_stage_label,
                due_pill_text(info),
                status_label(*status)
            ));
            tip = Some(format!("{}: {}\n{}", row.name, test.1, lines.join("\n")));
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
