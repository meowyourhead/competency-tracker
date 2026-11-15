//! Sections page: one grid per lab section, staff (rows) × test systems
//! (columns), each cell showing the next-due pill and stage. The header row
//! and the name column stay put while the cells scroll both ways.

use chrono::NaiveDate;
use egui::text::LayoutJob;
use egui::{
    Align, Button, CornerRadius, CursorIcon, FontId, Frame, Label, Layout, Margin, Popup, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::model::{AssignState, Data, Id, Section};
use crate::ops;
use crate::query::{distinct_titles, Index, StatusFilter};
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{heading, medium, Palette};
use crate::ui::widgets::{chip_select, due_pill, empty_state, search_box};

const NAME_W: f32 = 210.0;
const COL_W: f32 = 150.0;
const HEAD_H: f32 = 46.0;
const ROW_H: f32 = 54.0;

#[derive(Default)]
pub(crate) struct SectionsState {
    /// Section shown; `None` (or a retired one) falls back to the first.
    pub section: Option<Id>,
    pub status: StatusFilter,
    /// Show only this test system's column.
    pub test: Option<Id>,
    pub title: Option<String>,
    /// Name search.
    pub text: String,
    /// Rows sorted by this test's due date (earliest first, no date last).
    pub sort_by_test: Option<Id>,
    /// The grid for (revision, date, section).
    cache: Option<(u64, NaiveDate, Id, Grid)>,
}

enum CellView {
    Due {
        info: DueInfo,
        status: DueStatus,
        delayed: bool,
    },
    Na,
    Unassigned,
}

struct Row {
    id: Id,
    name: String,
    title: String,
    cells: Vec<CellView>,
}

struct Grid {
    /// (test id, test name), in configured order.
    columns: Vec<(Id, String)>,
    rows: Vec<Row>,
}

/// What the user did in the grid this frame.
enum GridAction {
    Open(Id, Id),
    Assign(Id, Id),
    Sort(Id),
}

impl SectionsState {
    /// Rebuilds the grid only when the data revision, the date or the section changed.
    fn refresh(&mut self, session: &Session, today: NaiveDate, section: &Section) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, d, s, _)) if *r == rev && *d == today && *s == section.id)
        {
            return;
        }
        self.cache = Some((
            rev,
            today,
            section.id.clone(),
            build_grid(session.data(), section, today),
        ));
    }
}

fn build_grid(data: &Data, section: &Section, today: NaiveDate) -> Grid {
    let idx = Index::build(data);
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
                        let info = next_due(
                            data.rule_for_test(&t.id),
                            &idx.pair(&s.id, &t.id),
                            &data.settings,
                        );
                        let status = classify(info.due_on, today, data.settings.due_soon_days);
                        let delayed = idx
                            .delay_notes(&s.id, &t.id)
                            .iter()
                            .any(|n| n.for_stage_index == info.next_stage_index);
                        CellView::Due {
                            info,
                            status,
                            delayed,
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

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SectionsState) {
    heading(ui, "Sections");
    ui.add_space(12.0);
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
    if let Some(picked) = section_pills(ui, pal, &sections, &section.id) {
        section = find(Some(&picked)).unwrap_or(section);
        state.test = None;
        state.sort_by_test = None;
    }
    if state.section.as_deref() != Some(section.id.as_str()) {
        state.section = Some(section.id.clone());
    }
    ui.add_space(12.0);
    state.refresh(session, cx.today, section);
    let SectionsState {
        status,
        test,
        title,
        text,
        sort_by_test,
        cache,
        ..
    } = state;
    let grid = &cache.as_ref().expect("refreshed above").3;
    if test
        .as_ref()
        .is_some_and(|t| !grid.columns.iter().any(|(id, _)| id == t))
    {
        *test = None;
    }
    filters(ui, pal, data, grid, status, test, title, text);
    ui.add_space(12.0);

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
    let cols: Vec<usize> = (0..grid.columns.len())
        .filter(|&c| test.as_ref().is_none_or(|t| *t == grid.columns[c].0))
        .collect();
    let needle = text.trim().to_lowercase();
    let mut rows: Vec<usize> = (0..grid.rows.len())
        .filter(|&r| {
            let row = &grid.rows[r];
            title
                .as_ref()
                .is_none_or(|t| t.eq_ignore_ascii_case(&row.title))
                && (needle.is_empty() || row.name.to_lowercase().contains(&needle))
                && (*status == StatusFilter::All
                    || cols.iter().any(|&c| matches_status(&row.cells[c], *status)))
        })
        .collect();
    if rows.is_empty() {
        empty_state(
            ui,
            "No matches",
            "Nobody in this section matches these filters.",
        );
        return;
    }
    let sort_col = sort_by_test
        .as_ref()
        .and_then(|t| cols.iter().copied().find(|&c| grid.columns[c].0 == *t));
    if let Some(c) = sort_col {
        let due = |r: usize| match &grid.rows[r].cells[c] {
            CellView::Due { info, .. } => info.due_on,
            _ => None,
        };
        rows.sort_by_key(|&r| (due(r).is_none(), due(r)));
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
    );
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

/// A segmented pill row of the sections; returns the newly picked one.
fn section_pills(
    ui: &mut egui::Ui,
    pal: &Palette,
    sections: &[&Section],
    current: &str,
) -> Option<Id> {
    let mut picked = None;
    Frame::new()
        .fill(pal.sidebar)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for s in sections {
                    let selected = s.id == current;
                    if ui
                        .add(Button::selectable(selected, s.name.as_str()).corner_radius(8))
                        .clicked()
                        && !selected
                    {
                        picked = Some(s.id.clone());
                    }
                }
            });
        });
    picked
}

#[allow(clippy::too_many_arguments)]
fn filters(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    grid: &Grid,
    status: &mut StatusFilter,
    test: &mut Option<Id>,
    title: &mut Option<String>,
    text: &mut String,
) {
    search_box(ui, text, "Search by name");
    ui.add_space(4.0);
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
    ui.horizontal(|ui| {
        let mut chosen = (*status != StatusFilter::All).then_some(*status);
        if chip_select(ui, "Status", &mut chosen, &statuses) {
            *status = chosen.unwrap_or_default();
        }
        chip_select(ui, "Test system", test, &grid.columns);
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

/// Draws the grid in the remaining space: a fixed corner, a header strip that
/// follows the body's horizontal scroll, a name column that follows its
/// vertical scroll, and the scrolling cells.
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
) -> Option<GridAction> {
    let mut action = None;
    let avail = ui.available_rect_before_wrap();
    let content = Vec2::new(cols.len() as f32 * COL_W, rows.len() as f32 * ROW_H);
    let bar = ui.spacing().scroll.allocated_width();
    let size = Vec2::new(
        (NAME_W + content.x + bar).min(avail.width()),
        (HEAD_H + content.y + bar)
            .min(avail.height())
            .max(HEAD_H + 2.0 * ROW_H),
    );
    let outer = Rect::from_min_size(avail.min, size);
    ui.allocate_rect(outer, Sense::hover());
    ui.painter().rect(
        outer,
        CornerRadius::same(10),
        pal.card,
        Stroke::new(1.0, pal.border),
        StrokeKind::Inside,
    );
    let header = Rect::from_min_max(
        outer.min + Vec2::new(NAME_W, 0.0),
        egui::pos2(outer.right(), outer.top() + HEAD_H),
    );
    let names = Rect::from_min_max(
        outer.min + Vec2::new(0.0, HEAD_H),
        egui::pos2(outer.left() + NAME_W, outer.bottom()),
    );
    let body = Rect::from_min_max(outer.min + Vec2::new(NAME_W, HEAD_H), outer.max);

    // The wheel over the header or the name column scrolls the cells too.
    let offset_key = ui.id().with(("grid_offset", section_id));
    let mut area = ScrollArea::both()
        .id_salt(("grid_scroll", section_id))
        .auto_shrink(false);
    if ui.rect_contains_pointer(header) || ui.rect_contains_pointer(names) {
        let delta = ui.input(|i| i.smooth_scroll_delta);
        if delta != Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            let prev: Vec2 = ui.data(|d| d.get_temp(offset_key)).unwrap_or_default();
            let max = (content - body.size()).max(Vec2::ZERO) + Vec2::splat(bar);
            area = area.scroll_offset((prev - delta).clamp(Vec2::ZERO, max));
        }
    }
    let mut body_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(body.shrink(1.0))
            .id_salt(("grid_body", section_id)),
    );
    let out = area.show_viewport(&mut body_ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let first = (viewport.top() / ROW_H).floor().max(0.0) as usize;
        let last = ((viewport.bottom() / ROW_H).ceil() as usize).min(rows.len());
        for (i, &r) in rows.iter().enumerate().take(last).skip(first) {
            let row = &grid.rows[r];
            for (j, &c) in cols.iter().enumerate() {
                let min = origin + Vec2::new(j as f32 * COL_W, i as f32 * ROW_H);
                let rect = Rect::from_min_size(min, Vec2::new(COL_W, ROW_H));
                let test = &grid.columns[c];
                if let Some(a) = cell(ui, pal, editing, rect, row, test, &row.cells[c]) {
                    action = Some(a);
                }
            }
        }
        ui.allocate_rect(Rect::from_min_size(origin, content), Sense::hover());
    });
    let offset = out.state.offset;
    ui.data_mut(|d| d.insert_temp(offset_key, offset));
    let line = Stroke::new(1.0, pal.border);

    // Header strip.
    let hp = ui.painter_at(header);
    for (j, &c) in cols.iter().enumerate() {
        let rect = Rect::from_min_size(
            egui::pos2(
                header.left() + 1.0 + j as f32 * COL_W - offset.x,
                header.top(),
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
            .on_hover_text(format!("Sort by {name} due date"));
        if resp.hovered() {
            hp.rect_filled(rect.shrink(3.0), CornerRadius::same(6), pal.sidebar);
        }
        let sorted = sort_col == Some(c);
        let mut job = LayoutJob::simple(
            name.clone(),
            FontId::new(13.0, medium()),
            pal.text,
            COL_W - 34.0,
        );
        job.wrap.max_rows = 2;
        let galley = ui.painter().layout_job(job);
        let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0);
        hp.galley(pos, galley, pal.text);
        if sorted {
            let glyph = egui::pos2(rect.right() - 14.0, rect.center().y);
            hp.text(
                glyph,
                egui::Align2::CENTER_CENTER,
                icon::SORT_ASCENDING,
                FontId::proportional(14.0),
                pal.accent,
            );
        }
        if resp.clicked() {
            action = Some(GridAction::Sort(id.clone()));
        }
    }

    // Name column.
    let np = ui.painter_at(names);
    for (i, &r) in rows.iter().enumerate() {
        let top = names.top() + 1.0 + i as f32 * ROW_H - offset.y;
        if top > names.bottom() || top + ROW_H < names.top() {
            continue;
        }
        let row = &grid.rows[r];
        let x = names.left() + 16.0;
        let has_title = !row.title.is_empty();
        let name_y = if has_title {
            top + ROW_H / 2.0 - 9.0
        } else {
            top + ROW_H / 2.0
        };
        let font = FontId::new(14.0, medium());
        let galley = ui
            .painter()
            .layout_no_wrap(row.name.clone(), font, pal.text);
        let clip = Rect::from_min_max(
            egui::pos2(x, top),
            egui::pos2(names.right() - 8.0, top + ROW_H),
        )
        .intersect(names);
        let p = np.with_clip_rect(clip);
        p.galley(
            egui::pos2(x, name_y - galley.size().y / 2.0),
            galley,
            pal.text,
        );
        if has_title {
            let pos = egui::pos2(x, top + ROW_H / 2.0 + 9.0);
            p.text(
                pos,
                egui::Align2::LEFT_CENTER,
                &row.title,
                FontId::proportional(12.0),
                pal.muted,
            );
        }
        np.hline(names.x_range(), top + ROW_H, line);
    }

    // Corner and dividers.
    let corner = Rect::from_min_max(
        outer.min,
        egui::pos2(outer.left() + NAME_W, outer.top() + HEAD_H),
    );
    let p = ui.painter();
    p.text(
        egui::pos2(corner.left() + 16.0, corner.center().y),
        egui::Align2::LEFT_CENTER,
        format!("Name \u{b7} {}", rows.len()),
        FontId::new(13.0, medium()),
        pal.muted,
    );
    p.hline(outer.x_range(), header.bottom(), line);
    p.vline(names.right(), outer.y_range(), line);
    action
}

/// One grid cell. Assigned: pill + stage label; N/A; or a faint "—" that
/// offers "Assign" in edit mode.
fn cell(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    rect: Rect,
    row: &Row,
    test: &(Id, String),
    view: &CellView,
) -> Option<GridAction> {
    let clickable = !matches!(view, CellView::Unassigned) || editing;
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let builder = UiBuilder::new()
        .max_rect(rect.shrink2(Vec2::new(8.0, 6.0)))
        .id_salt(("cell", &row.id, &test.0))
        .layout(Layout::top_down(Align::Min))
        .sense(sense);
    let mut pill_clicked = false;
    let resp = ui
        .scope_builder(builder, |ui| {
            if clickable && ui.response().contains_pointer() {
                ui.painter().rect_filled(
                    ui.max_rect().expand(3.0),
                    CornerRadius::same(8),
                    pal.sidebar,
                );
            }
            ui.spacing_mut().item_spacing.y = 3.0;
            match view {
                CellView::Due {
                    info,
                    status,
                    delayed,
                } => {
                    pill_clicked = due_pill(ui, pal, info, *status, *delayed).clicked();
                    let stage = RichText::new(&info.next_stage_label)
                        .size(11.5)
                        .color(pal.muted);
                    ui.add(Label::new(stage).selectable(false).truncate());
                }
                CellView::Na => {
                    ui.add(Label::new(RichText::new("N/A").color(pal.muted)).selectable(false));
                }
                CellView::Unassigned => {
                    let faint = pal.muted.gamma_multiply(0.5);
                    ui.add(Label::new(RichText::new("\u{2014}").color(faint)).selectable(false));
                }
            }
        })
        .response;
    let name = format!("{} \u{b7} {}", row.name, test.1);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, clickable, &name));
    if !clickable {
        return None;
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
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
    (resp.clicked() || pill_clicked).then(|| GridAction::Open(row.id.clone(), test.0.clone()))
}
