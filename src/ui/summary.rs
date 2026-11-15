//! Summary (home) page: the whole tracker at a glance. A strip of headline
//! figures, each section's health as a bar, the assessments that need action,
//! and a register of every person. One set of filters (section, staff member,
//! assessment status) drives all of it.
//!
//! An assessment is one assigned test system for one active person. It is
//! *current* when it has been completed and is not overdue.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use egui::{
    Align, Button, Color32, CornerRadius, CursorIcon, FontId, Frame, Label, Layout, Margin, Rect,
    RichText, Sense, Stroke, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::model::{Data, Id};
use crate::query::{due_items, DueItem, Index};
use crate::schedule::DueStatus;
use crate::session::Session;
use crate::ui::app::Ctx;
use crate::ui::theme::{medium, serif, Palette};
use crate::ui::widgets::{
    chip_select, due_pill, empty_state, search_box, section_color, segmented, SEARCH_W,
};

/// Where an assessment stands. Every assessment is exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Health {
    Overdue,
    DueSoon,
    NotDone,
    UpToDate,
}

impl Health {
    const ALL: [Health; 4] = [
        Health::UpToDate,
        Health::DueSoon,
        Health::Overdue,
        Health::NotDone,
    ];

    fn of(i: &DueItem) -> Health {
        match i.status {
            DueStatus::Overdue => Health::Overdue,
            DueStatus::DueSoon => Health::DueSoon,
            _ if i.info.last_completed_on.is_none() => Health::NotDone,
            _ => Health::UpToDate,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Health::Overdue => "Overdue",
            Health::DueSoon => "Due soon",
            Health::NotDone => "Not done yet",
            Health::UpToDate => "Up to date",
        }
    }

    /// (fill, text): the same colours as the due pills.
    fn colors(self, pal: &Palette) -> (Color32, Color32) {
        match self {
            Health::Overdue => (pal.red_bg, pal.red_fg),
            Health::DueSoon => (pal.yellow_bg, pal.yellow_fg),
            Health::NotDone => (pal.grey_bg, pal.grey_fg),
            Health::UpToDate => (pal.green_bg, pal.green_fg),
        }
    }

    /// The solid colour of its part of a section bar.
    fn bar(self, pal: &Palette) -> Color32 {
        match self {
            Health::NotDone => pal.border,
            h => h.colors(pal).1.gamma_multiply(0.85),
        }
    }
}

/// Completed and not overdue.
fn is_current(i: &DueItem) -> bool {
    i.info.last_completed_on.is_some() && i.status != DueStatus::Overdue
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SummaryFilter {
    pub section: Option<Id>,
    pub staff: Option<Id>,
    pub health: Option<Health>,
}

impl SummaryFilter {
    fn any(&self) -> bool {
        self.section.is_some() || self.staff.is_some() || self.health.is_some()
    }

    fn keeps(&self, i: &DueItem) -> bool {
        self.section.as_ref().is_none_or(|s| *s == i.section_id)
            && self.staff.as_ref().is_none_or(|s| *s == i.staff_id)
            && self.health.is_none_or(|h| h == Health::of(i))
    }
}

/// Which list the "Needs action" card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ActionTab {
    #[default]
    Overdue,
    DueSoon,
}

/// The register's sort column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortBy {
    #[default]
    Status,
    Name,
    Current,
    NotDone,
    Overdue,
}

#[derive(Default)]
pub(crate) struct SummaryState {
    pub filter: SummaryFilter,
    /// The register's own name search.
    search: String,
    tab: ActionTab,
    /// Show every row of the action list, not just the first few.
    all_actions: bool,
    sort: SortBy,
    /// Due items for (revision, date).
    cache: Option<(u64, NaiveDate, Vec<DueItem>)>,
}

impl SummaryState {
    /// Recomputes the due items only when the data revision or the date changed.
    fn refresh(&mut self, session: &Session, today: NaiveDate) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, d, _)) if *r == rev && *d == today) {
            return;
        }
        let data = session.data();
        self.cache = Some((rev, today, due_items(data, &Index::build(data), today)));
    }
}

/// Rows the action list shows before "Show all".
const ACTION_ROWS: usize = 8;
/// Below this width the two middle cards stack.
const TWO_COLUMNS_W: f32 = 900.0;
const GAP: f32 = 16.0;

/// What a click on the page asks for, handled once drawing is done.
enum Pick {
    Detail(Id, Id),
    Profile(Id),
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SummaryState) {
    let pal = cx.pal;
    let session: &Session = cx.session;
    let data = session.data();
    if data.staff.is_empty() {
        empty_state(
            ui,
            "No data yet \u{2014} import your Excel tracker from Settings",
            "",
        );
        return;
    }
    state.refresh(session, cx.today);
    let all: Vec<&DueItem> = state
        .cache
        .as_ref()
        .map_or(&[][..], |(_, _, v)| v.as_slice())
        .iter()
        .collect();
    let soon_days = data.settings.due_soon_days;
    let SummaryState {
        filter,
        search,
        tab,
        all_actions,
        sort,
        ..
    } = state;
    filters(ui, pal, data, &all, filter);
    ui.add_space(14.0);
    let shown: Vec<&DueItem> = all.iter().copied().filter(|i| filter.keeps(i)).collect();
    let mut pick = None;
    kpis(ui, pal, data, &shown, filter, soon_days);
    ui.add_space(GAP);
    let two = ui.available_width() >= TWO_COLUMNS_W;
    if two {
        ui.columns(2, |cols| {
            cols[0].spacing_mut().item_spacing.x = GAP;
            sections_card(&mut cols[0], pal, data, &shown, filter);
            if let Some(p) = actions_card(&mut cols[1], pal, &shown, tab, all_actions, soon_days) {
                pick = Some(p);
            }
        });
    } else {
        sections_card(ui, pal, data, &shown, filter);
        ui.add_space(GAP);
        if let Some(p) = actions_card(ui, pal, &shown, tab, all_actions, soon_days) {
            pick = Some(p);
        }
    }
    ui.add_space(GAP);
    if let Some(p) = register(ui, pal, data, &shown, search, sort) {
        pick = Some(p);
    }
    match pick {
        Some(Pick::Detail(s, t)) => *cx.open_detail = Some((s, t)),
        Some(Pick::Profile(s)) => *cx.open_profile = Some(s),
        None => {}
    }
}

/// A titled card. `side` draws on the right of the title row.
fn card<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    side: impl FnOnce(&mut egui::Ui),
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .family(medium())
                        .size(15.5)
                        .color(pal.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), side);
            });
            ui.add_space(8.0);
            add(ui)
        })
        .inner
}

/// Section, staff member and assessment status; Clear filters when any is set.
fn filters(ui: &mut egui::Ui, pal: &Palette, data: &Data, all: &[&DueItem], f: &mut SummaryFilter) {
    let sections: Vec<(Id, String)> = data
        .sections_sorted()
        .into_iter()
        .filter(|s| !s.retired)
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    // People with at least one assessment (in the chosen section).
    let mut people: BTreeMap<String, Id> = BTreeMap::new();
    for i in all
        .iter()
        .filter(|i| f.section.as_ref().is_none_or(|s| *s == i.section_id))
    {
        people.insert(i.staff_name.to_lowercase(), i.staff_id.clone());
    }
    let names: HashMap<&Id, &str> = all
        .iter()
        .map(|i| (&i.staff_id, i.staff_name.as_str()))
        .collect();
    let staff: Vec<(Id, String)> = people
        .into_values()
        .map(|id| {
            let n = names.get(&id).copied().unwrap_or_default().to_owned();
            (id, n)
        })
        .collect();
    if f.staff
        .as_ref()
        .is_some_and(|s| !staff.iter().any(|(id, _)| id == s))
    {
        f.staff = None;
    }
    let statuses: Vec<(Health, String)> = [
        Health::Overdue,
        Health::DueSoon,
        Health::NotDone,
        Health::UpToDate,
    ]
    .into_iter()
    .map(|h| (h, h.label().to_owned()))
    .collect();
    ui.horizontal_wrapped(|ui| {
        chip_select(ui, "Section", &mut f.section, &sections);
        chip_select(ui, "Staff member", &mut f.staff, &staff);
        chip_select(ui, "Status", &mut f.health, &statuses);
        if f.any()
            && ui
                .add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false))
                .clicked()
        {
            *f = SummaryFilter::default();
        }
    });
}

/// The headline figures in one strip. Overdue and Due soon also filter the
/// page by that status (a second click shows everything again).
fn kpis(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    items: &[&DueItem],
    f: &mut SummaryFilter,
    soon_days: i64,
) {
    let total = items.len();
    let current = items.iter().filter(|i| is_current(i)).count();
    let count = |h: Health| items.iter().filter(|i| Health::of(i) == h).count();
    let (overdue, soon) = (count(Health::Overdue), count(Health::DueSoon));
    let staff = if f.any() {
        let mut ids: Vec<&Id> = items.iter().map(|i| &i.staff_id).collect();
        ids.sort();
        ids.dedup();
        ids.len()
    } else {
        data.staff
            .iter()
            .filter(|s| s.status == crate::model::StaffStatus::Active)
            .count()
    };
    let pct = if total == 0 {
        0
    } else {
        ((current as f32 / total as f32) * 100.0).round() as usize
    };
    let cells: [(String, &str, String, Option<Health>); 5] = [
        (
            staff.to_string(),
            "Staff",
            if f.any() {
                "With assessments shown".to_owned()
            } else {
                "Active staff".to_owned()
            },
            None,
        ),
        (
            format!("{pct}%"),
            "Assessments current",
            format!("{current} of {total} done and in date"),
            None,
        ),
        (
            (total - current).to_string(),
            "Outstanding",
            "Never done or overdue".to_owned(),
            None,
        ),
        (
            overdue.to_string(),
            "Overdue",
            "Past their due date".to_owned(),
            Some(Health::Overdue),
        ),
        (
            soon.to_string(),
            "Due soon",
            format!("Within {soon_days} days"),
            Some(Health::DueSoon),
        ),
    ];
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(4, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let w = ui.available_width();
            // Five across when there's room, otherwise rows of three.
            let per_row = if w >= 760.0 { 5 } else { 3 };
            let cell_w = w / per_row as f32;
            for row in cells.chunks(per_row) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (k, (value, label, note, health)) in row.iter().enumerate() {
                        let selected = health.is_some() && f.health == *health;
                        let resp = kpi_cell(
                            ui,
                            pal,
                            cell_w,
                            value,
                            label,
                            note,
                            *health,
                            selected,
                            k > 0,
                            (label == &"Assessments current").then_some((current, total)),
                        );
                        if let (Some(h), true) = (health, resp.clicked()) {
                            f.health = if selected { None } else { Some(*h) };
                        }
                    }
                });
            }
        });
}

/// One figure: a big serif number, its label and a one-line note. A
/// clickable cell (a status) shows a pointer and, when chosen, an accent rule.
#[allow(clippy::too_many_arguments)]
fn kpi_cell(
    ui: &mut egui::Ui,
    pal: &Palette,
    w: f32,
    value: &str,
    label: &str,
    note: &str,
    health: Option<Health>,
    selected: bool,
    divider: bool,
    progress: Option<(usize, usize)>,
) -> egui::Response {
    let sense = if health.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 104.0), sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if health.is_some() && (resp.hovered() || selected) {
            p.rect_filled(rect.shrink(2.0), 9, pal.sidebar);
        }
        if divider {
            p.vline(
                rect.left(),
                rect.y_range().shrink(18.0),
                Stroke::new(1.0, pal.border),
            );
        }
        let x = rect.left() + 20.0;
        let color = match health {
            Some(h) if value != "0" => h.colors(pal).1,
            _ => pal.text,
        };
        p.text(
            egui::pos2(x, rect.top() + 14.0),
            egui::Align2::LEFT_TOP,
            value,
            FontId::new(32.0, serif()),
            color,
        );
        p.text(
            egui::pos2(x, rect.top() + 58.0),
            egui::Align2::LEFT_TOP,
            label,
            FontId::new(13.5, medium()),
            pal.text,
        );
        if let Some((done, total)) = progress {
            // A thin bar in place of the note: how much is current.
            let bar = Rect::from_min_size(
                egui::pos2(x, rect.top() + 82.0),
                Vec2::new((w - 44.0).max(40.0), 6.0),
            );
            p.rect_filled(bar, 3, pal.border);
            if total > 0 {
                let mut fill = bar;
                fill.set_width(bar.width() * done as f32 / total as f32);
                p.rect_filled(fill, 3, pal.green_fg.gamma_multiply(0.85));
            }
        } else {
            p.text(
                egui::pos2(x, rect.top() + 78.0),
                egui::Align2::LEFT_TOP,
                note,
                FontId::proportional(12.5),
                pal.muted,
            );
        }
        if selected {
            p.hline(
                (x)..=(rect.right() - 20.0),
                rect.bottom() - 4.0,
                Stroke::new(2.0, pal.accent),
            );
        }
    }
    let name = format!("{label} {value}");
    resp.widget_info(|| match health {
        Some(_) => WidgetInfo::selected(WidgetType::Button, true, selected, &name),
        None => WidgetInfo::labeled(WidgetType::Label, true, &name),
    });
    let resp = resp.on_hover_text(note);
    if health.is_some() {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Each section: its share of current assessments as a bar split by status,
/// the percentage, and how many are overdue. Clicking a row filters by it.
fn sections_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    items: &[&DueItem],
    f: &mut SummaryFilter,
) {
    let mut by: HashMap<&str, [usize; 4]> = HashMap::new();
    let mut current: HashMap<&str, usize> = HashMap::new();
    for i in items {
        let e = by.entry(i.section_id.as_str()).or_default();
        let k = Health::ALL
            .iter()
            .position(|h| *h == Health::of(i))
            .expect("every health is listed");
        e[k] += 1;
        if is_current(i) {
            *current.entry(i.section_id.as_str()).or_default() += 1;
        }
    }
    let mut clicked = None;
    card(
        ui,
        pal,
        "By section",
        |_| {},
        |ui| {
            let sections: Vec<_> = data
                .sections_sorted()
                .into_iter()
                .filter(|s| !s.retired && by.contains_key(s.id.as_str()))
                .collect();
            if sections.is_empty() {
                ui.label(RichText::new("No assessments match these filters.").color(pal.muted));
                return;
            }
            let name_w = sections
                .iter()
                .map(|s| {
                    ui.painter()
                        .layout_no_wrap(s.name.clone(), FontId::new(14.0, medium()), pal.text)
                        .size()
                        .x
                })
                .fold(0.0, f32::max)
                .min(ui.available_width() * 0.4);
            for s in sections {
                let counts = by[s.id.as_str()];
                let total: usize = counts.iter().sum();
                let cur = current.get(s.id.as_str()).copied().unwrap_or(0);
                let pct = (cur as f32 / total.max(1) as f32 * 100.0).round() as usize;
                let selected = f.section.as_deref() == Some(s.id.as_str());
                let color = section_color(data, &s.id).unwrap_or(pal.muted);
                if section_row(ui, pal, &s.name, color, name_w, counts, pct, selected).clicked() {
                    clicked = Some((s.id.clone(), selected));
                }
            }
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                for h in Health::ALL {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(r, 2, h.bar(pal));
                        ui.label(RichText::new(h.label()).size(12.5).color(pal.muted));
                    });
                }
            });
        },
    );
    if let Some((id, was)) = clicked {
        f.section = if was { None } else { Some(id) };
    }
}

/// Room for the colour dot before a section's name.
const DOT_W: f32 = 16.0;

#[allow(clippy::too_many_arguments)]
fn section_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    name: &str,
    color: Color32,
    name_w: f32,
    counts: [usize; 4],
    pct: usize,
    selected: bool,
) -> egui::Response {
    let h = 34.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if selected {
            p.rect_filled(
                rect.expand2(Vec2::new(8.0, 0.0)),
                8,
                pal.accent.gamma_multiply(0.10),
            );
        } else if resp.hovered() {
            p.rect_filled(rect.expand2(Vec2::new(8.0, 0.0)), 8, pal.sidebar);
        }
        let mid = rect.center().y;
        // The section's colour as a dot before its name.
        p.circle_filled(egui::pos2(rect.left() + 4.0, mid), 4.0, color);
        let name_g = p.layout_no_wrap(name.to_owned(), FontId::new(14.0, medium()), pal.text);
        p.with_clip_rect(Rect::from_min_size(rect.min, Vec2::new(DOT_W + name_w, h)))
            .galley(
                egui::pos2(rect.left() + DOT_W, mid - name_g.size().y / 2.0),
                name_g,
                pal.text,
            );
        let overdue = counts[2];
        let tail_w = 150.0;
        let pct_g = p.layout_no_wrap(format!("{pct}%"), FontId::new(14.0, medium()), pal.text);
        let note = if overdue > 0 {
            format!("{overdue} overdue")
        } else {
            String::new()
        };
        let note_g = p.layout_no_wrap(note, FontId::proportional(12.5), pal.red_fg);
        let bar = Rect::from_min_max(
            egui::pos2(rect.left() + DOT_W + name_w + 16.0, mid - 5.0),
            egui::pos2(rect.right() - tail_w, mid + 5.0),
        );
        stacked_bar(p, pal, bar, counts);
        let tx = bar.right() + 12.0;
        p.galley(egui::pos2(tx, mid - pct_g.size().y / 2.0), pct_g, pal.text);
        p.galley(
            egui::pos2(tx + 46.0, mid - note_g.size().y / 2.0),
            note_g,
            pal.red_fg,
        );
    }
    let total: usize = counts.iter().sum();
    let label = format!("{name} {pct}% current");
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, &label));
    resp.on_hover_text(format!(
        "{name}: {total} assessments\nUp to date {}, due soon {}, overdue {}, not done yet {}\nClick to show only this section",
        counts[0], counts[1], counts[2], counts[3]
    ))
    .on_hover_cursor(CursorIcon::PointingHand)
}

/// A bar split into the statuses in [`Health::ALL`] order, rounded at its ends.
fn stacked_bar(p: &egui::Painter, pal: &Palette, bar: Rect, counts: [usize; 4]) {
    let total: usize = counts.iter().sum();
    p.rect_filled(bar, 5, pal.border.gamma_multiply(0.5));
    if total == 0 || bar.width() <= 0.0 {
        return;
    }
    let parts: Vec<(usize, Health)> = counts
        .iter()
        .zip(Health::ALL)
        .filter(|(n, _)| **n > 0)
        .map(|(n, h)| (*n, h))
        .collect();
    let mut x = bar.left();
    for (k, (n, h)) in parts.iter().enumerate() {
        let w = bar.width() * *n as f32 / total as f32;
        let r = Rect::from_min_max(egui::pos2(x, bar.top()), egui::pos2(x + w, bar.bottom()));
        let first = k == 0;
        let last = k + 1 == parts.len();
        let radius = CornerRadius {
            nw: if first { 5 } else { 0 },
            sw: if first { 5 } else { 0 },
            ne: if last { 5 } else { 0 },
            se: if last { 5 } else { 0 },
        };
        p.rect_filled(r, radius, h.bar(pal));
        x += w;
    }
}

/// Overdue (most overdue first) or due soon (soonest first), a few rows at
/// a time. Clicking a row opens that assessment.
fn actions_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    items: &[&DueItem],
    tab: &mut ActionTab,
    all: &mut bool,
    soon_days: i64,
) -> Option<Pick> {
    let mut overdue: Vec<&DueItem> = items
        .iter()
        .copied()
        .filter(|i| i.status == DueStatus::Overdue)
        .collect();
    let mut soon: Vec<&DueItem> = items
        .iter()
        .copied()
        .filter(|i| i.status == DueStatus::DueSoon)
        .collect();
    overdue.sort_by_key(|i| (i.info.due_on, i.staff_name.to_lowercase()));
    soon.sort_by_key(|i| (i.info.due_on, i.staff_name.to_lowercase()));
    let mut picked = None;
    let labels = [
        format!("Overdue ({})", overdue.len()),
        format!("Due soon ({})", soon.len()),
    ];
    let mut switch = None;
    let shown_tab = *tab;
    card(
        ui,
        pal,
        "Needs action",
        |ui| {
            let current = usize::from(shown_tab == ActionTab::DueSoon);
            let refs = [labels[0].as_str(), labels[1].as_str()];
            if let Some(i) = segmented(ui, pal, current, &refs) {
                switch = Some(if i == 0 {
                    ActionTab::Overdue
                } else {
                    ActionTab::DueSoon
                });
            }
        },
        |ui| {
            let list = match tab {
                ActionTab::Overdue => &overdue,
                ActionTab::DueSoon => &soon,
            };
            if list.is_empty() {
                let text = match tab {
                    ActionTab::Overdue => "Nothing is overdue.".to_owned(),
                    ActionTab::DueSoon => format!("Nothing is due in the next {soon_days} days."),
                };
                ui.add_space(8.0);
                ui.label(RichText::new(text).color(pal.muted));
                return;
            }
            let n = if *all {
                list.len()
            } else {
                list.len().min(ACTION_ROWS)
            };
            ui.spacing_mut().item_spacing.y = 2.0;
            for item in &list[..n] {
                if action_row(ui, pal, item) {
                    picked = Some(Pick::Detail(item.staff_id.clone(), item.test_id.clone()));
                }
            }
            if list.len() > ACTION_ROWS {
                ui.add_space(4.0);
                let text = if *all {
                    "Show fewer".to_owned()
                } else {
                    format!("Show all {}", list.len())
                };
                if ui.link(text).clicked() {
                    *all = !*all;
                }
            }
        },
    );
    if let Some(next) = switch {
        if next != *tab {
            *all = false;
        }
        *tab = next;
    }
    picked
}

/// Who and what on the left; how late (or how soon) and the due pill on the
/// right. Its accessible name is "staff · test". Returns true when clicked.
fn action_row(ui: &mut egui::Ui, pal: &Palette, item: &DueItem) -> bool {
    let mut pill_clicked = false;
    let builder = UiBuilder::new()
        .id_salt(("action", &item.staff_id, &item.test_id))
        .sense(Sense::click());
    let resp = ui
        .scope_builder(builder, |ui| {
            let fill = if ui.response().contains_pointer() {
                pal.sidebar
            } else {
                Color32::TRANSPARENT
            };
            Frame::new()
                .fill(fill)
                .corner_radius(8)
                .inner_margin(Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().interact_size.y = 26.0;
                    // A row first, so the right-to-left part is one line tall.
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let delayed = item.delay_note.is_some();
                            pill_clicked =
                                due_pill(ui, pal, &item.info, item.status, delayed).clicked();
                            ui.add_space(4.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(
                                    Label::new(
                                        RichText::new(&item.staff_name)
                                            .family(medium())
                                            .color(pal.text),
                                    )
                                    .selectable(false),
                                );
                                ui.add(
                                    Label::new(
                                        RichText::new(format!(
                                            "{}, {}",
                                            item.test_name, item.info.next_stage_label
                                        ))
                                        .color(pal.muted),
                                    )
                                    .selectable(false)
                                    .truncate(),
                                );
                            });
                        })
                    });
                });
        })
        .response;
    let name = format!("{} \u{b7} {}", item.staff_name, item.test_name);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &name));
    let mut tip = format!("{} \u{b7} {}", item.test_name, item.section_name);
    if let Some(note) = &item.delay_note {
        tip.push_str(&format!("\nDelay note: {}", note.note));
    }
    let resp = resp
        .on_hover_text(tip)
        .on_hover_cursor(CursorIcon::PointingHand);
    resp.clicked() || pill_clicked
}

/// One person's figures in the register.
struct Person<'a> {
    id: &'a Id,
    name: &'a str,
    title: &'a str,
    /// (name, id) of each section, in section order.
    sections: Vec<(&'a str, &'a str)>,
    current: usize,
    not_done: usize,
    overdue: usize,
    worst: Health,
}

fn people<'a>(data: &Data, items: &[&'a DueItem]) -> Vec<Person<'a>> {
    let order: HashMap<&str, usize> = data
        .sections_sorted()
        .iter()
        .enumerate()
        .map(|(k, s)| (s.id.as_str(), k))
        .collect();
    let mut map: HashMap<&Id, Person<'a>> = HashMap::new();
    for i in items {
        let p = map.entry(&i.staff_id).or_insert_with(|| Person {
            id: &i.staff_id,
            name: &i.staff_name,
            title: &i.title,
            sections: Vec::new(),
            current: 0,
            not_done: 0,
            overdue: 0,
            worst: Health::UpToDate,
        });
        if !p.sections.iter().any(|(_, id)| *id == i.section_id) {
            p.sections.push((&i.section_name, &i.section_id));
        }
        let h = Health::of(i);
        p.worst = p.worst.min(h);
        match h {
            Health::Overdue => p.overdue += 1,
            Health::NotDone => p.not_done += 1,
            _ => {}
        }
        if is_current(i) {
            p.current += 1;
        }
    }
    let mut out: Vec<Person> = map.into_values().collect();
    for p in &mut out {
        p.sections
            .sort_by_key(|(_, id)| order.get(id).copied().unwrap_or(usize::MAX));
    }
    out
}

/// Everyone matching the filters (and the register's own search): their
/// sections, how many assessments are current, not done yet and overdue,
/// and their overall status. Clicking a name opens their profile.
fn register(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    items: &[&DueItem],
    search: &mut String,
    sort: &mut SortBy,
) -> Option<Pick> {
    let q = search.trim().to_lowercase();
    let mut rows: Vec<Person> = people(data, items)
        .into_iter()
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
        .collect();
    let by_name = |a: &Person, b: &Person| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    rows.sort_by(|a, b| match sort {
        SortBy::Name => by_name(a, b),
        SortBy::Status => a
            .worst
            .cmp(&b.worst)
            .then(b.overdue.cmp(&a.overdue))
            .then_with(|| by_name(a, b)),
        SortBy::Current => b.current.cmp(&a.current).then_with(|| by_name(a, b)),
        SortBy::NotDone => b.not_done.cmp(&a.not_done).then_with(|| by_name(a, b)),
        SortBy::Overdue => b.overdue.cmp(&a.overdue).then_with(|| by_name(a, b)),
    });
    let mut picked = None;
    let count = rows.len();
    card(
        ui,
        pal,
        &format!("Staff register  ({count})"),
        |ui| {
            search_box(ui, search, "Search by name", SEARCH_W);
        },
        |ui| {
            if rows.is_empty() {
                empty_state(ui, "No matches", "Nobody matches these filters.");
                return;
            }
            let w = ui.available_width();
            // Fixed columns for the figures and status; names and sections share the rest.
            let fig_w = 96.0;
            let status_w = 120.0;
            let rest = (w - 3.0 * fig_w - status_w).max(240.0);
            let name_w = (rest * 0.36).clamp(150.0, 280.0);
            let sec_w = rest - name_w;
            let cols: [(&str, f32, Option<SortBy>); 6] = [
                ("Name", name_w, Some(SortBy::Name)),
                ("Sections", sec_w, None),
                ("Current", fig_w, Some(SortBy::Current)),
                ("Not done yet", fig_w, Some(SortBy::NotDone)),
                ("Overdue", fig_w, Some(SortBy::Overdue)),
                ("Status", status_w, Some(SortBy::Status)),
            ];
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (head, cw, key) in cols {
                    ui.allocate_ui_with_layout(
                        Vec2::new(cw, 24.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_width(cw);
                            let on = key == Some(*sort);
                            let text = if on {
                                format!("{head} {}", icon::CARET_DOWN)
                            } else {
                                head.to_owned()
                            };
                            let color = if on { pal.text } else { pal.muted };
                            let rt = RichText::new(text).size(12.5).color(color);
                            match key {
                                Some(k) => {
                                    if ui
                                        .add(Button::new(rt).frame(false))
                                        .on_hover_text(format!("Sort by {}", head.to_lowercase()))
                                        .clicked()
                                    {
                                        *sort = k;
                                    }
                                }
                                None => {
                                    ui.add_space(12.0);
                                    ui.label(rt);
                                }
                            }
                        },
                    );
                }
            });
            ui.separator();
            for (k, p) in rows.iter().enumerate() {
                if k > 0 {
                    let y = ui.cursor().top() - 1.0;
                    ui.painter().hline(
                        ui.max_rect().x_range(),
                        y,
                        Stroke::new(1.0, pal.border.gamma_multiply(0.6)),
                    );
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.set_min_height(38.0);
                    let cell = |ui: &mut egui::Ui, cw: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(cw, 38.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_width(cw);
                                add(ui);
                            },
                        );
                    };
                    cell(ui, name_w, &mut |ui| {
                        let name =
                            Button::new(RichText::new(p.name).family(medium()).color(pal.text))
                                .frame(false)
                                .wrap_mode(egui::TextWrapMode::Truncate);
                        let resp = ui.add(name).on_hover_text(format!(
                            "{}{}\nOpen their competency record",
                            p.name,
                            if p.title.is_empty() {
                                String::new()
                            } else {
                                format!(", {}", p.title)
                            }
                        ));
                        if resp.clicked() {
                            picked = Some(Pick::Profile(p.id.clone()));
                        }
                        if !p.title.is_empty() {
                            ui.add_space(2.0);
                            ui.add(
                                Label::new(RichText::new(p.title).size(12.5).color(pal.muted))
                                    .truncate(),
                            );
                        }
                    });
                    cell(ui, sec_w, &mut |ui| {
                        ui.add_space(12.0);
                        // Each name after a dot in its section's colour.
                        let mut job = egui::text::LayoutJob::default();
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        let fmt = |color| egui::TextFormat::simple(font.clone(), color);
                        for (k, (name, id)) in p.sections.iter().enumerate() {
                            if k > 0 {
                                job.append("   ", 0.0, fmt(pal.muted));
                            }
                            let dot = section_color(data, id).unwrap_or(pal.muted);
                            job.append("\u{25cf} ", 0.0, fmt(dot));
                            job.append(name, 0.0, fmt(pal.muted));
                        }
                        let names: Vec<&str> = p.sections.iter().map(|(n, _)| *n).collect();
                        ui.add(Label::new(job).truncate())
                            .on_hover_text(names.join("\n"));
                    });
                    for (n, warn) in [
                        (p.current, None),
                        (p.not_done, None),
                        (p.overdue, Some(pal.red_fg)),
                    ] {
                        cell(ui, fig_w, &mut |ui| {
                            ui.add_space(12.0);
                            let color = match warn {
                                Some(c) if n > 0 => c,
                                _ if n == 0 => pal.muted,
                                _ => pal.text,
                            };
                            ui.label(RichText::new(n.to_string()).family(medium()).color(color));
                        });
                    }
                    cell(ui, status_w, &mut |ui| {
                        ui.add_space(12.0);
                        status_pill(ui, pal, p.worst);
                    });
                });
            }
        },
    );
    picked
}

fn status_pill(ui: &mut egui::Ui, pal: &Palette, h: Health) {
    let (fill, fg) = h.colors(pal);
    let g = ui
        .painter()
        .layout_no_wrap(h.label().to_owned(), FontId::proportional(12.5), fg);
    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(20.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 255, fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, h.label()));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::dates::now_ts;
    use crate::model::{AssignState, Assignment, Staff, StaffStatus, TestSystem};
    use crate::snapshot::DataDir;

    #[test]
    fn due_items_recomputed_only_on_revision_or_date_change() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        let mut s = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        let today = now_ts().date_naive();
        s.mutate(now_ts(), |d, _| {
            d.sections[0].tests.push(TestSystem {
                id: "t-1".into(),
                name: "Cobas".into(),
                order: 0,
                retired: false,
                rule_id: None,
            });
            d.staff.push(Staff {
                id: "p-1".into(),
                name: "Agnes".into(),
                title: "MLT".into(),
                joined: None,
                status: StaffStatus::Active,
                assignments: [(
                    "t-1".to_string(),
                    Assignment {
                        state: AssignState::Assigned,
                        since: today,
                    },
                )]
                .into(),
            });
        })
        .unwrap();
        let ptr = |st: &SummaryState| st.cache.as_ref().unwrap().2.as_ptr();
        let mut st = SummaryState::default();
        st.refresh(&s, today);
        let first = ptr(&st);
        st.refresh(&s, today);
        assert_eq!(ptr(&st), first, "same revision and date: cached");

        st.refresh(&s, today.succ_opt().unwrap());
        let second = ptr(&st);
        assert_ne!(second, first, "new date: recomputed");

        s.mutate(now_ts(), |d, _| d.staff[0].name = "Agnes Tan".into())
            .unwrap();
        st.refresh(&s, today.succ_opt().unwrap());
        assert_eq!(
            st.cache.as_ref().unwrap().2[0].staff_name,
            "Agnes Tan",
            "new revision: recomputed"
        );
    }
}
