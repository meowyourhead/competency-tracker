//! Summary (home) page: at a glance, who is due for retraining and on which
//! tests. Stat cards, filters, and the due list as cards grouped by person or
//! by test system.

use chrono::NaiveDate;
use egui::{
    Align, Button, Color32, CursorIcon, Frame, Label, Layout, Margin, Rect, RichText, Sense, Stroke, UiBuilder,
    WidgetInfo, WidgetType,
};

use crate::dates::fmt_date;
use crate::model::{Data, Id};
use crate::query::{
    counts, distinct_titles, due_items, filter_items, group_by_person, group_by_test, DueFilter, DueItem, Index,
    StatusFilter,
};
use crate::schedule::DueStatus;
use crate::session::Session;
use crate::ui::app::Ctx;
use crate::ui::theme::{heading, Palette};
use crate::ui::widgets::{chip_select, due_pill, empty_state, search_box, stat_card};

/// The page content is a calm centred column at most this wide.
const MAX_WIDTH: f32 = 980.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Grouping {
    /// One card per person, listing their tests.
    #[default]
    Person,
    /// One card per test system, listing the staff.
    Test,
}

#[derive(Default)]
pub(crate) struct SummaryState {
    pub filter: DueFilter,
    pub group: Grouping,
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

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SummaryState) {
    centred(ui, |ui| page(ui, cx, state));
}

fn centred(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let avail = ui.available_rect_before_wrap();
    let width = avail.width().min(MAX_WIDTH);
    let left = avail.left() + (avail.width() - width) / 2.0;
    let rect = Rect::from_min_size(egui::pos2(left, avail.top()), egui::vec2(width, avail.height()));
    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
}

fn page(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SummaryState) {
    let pal = cx.pal;
    ui.horizontal(|ui| {
        heading(ui, "Summary");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(fmt_date(cx.today)).color(pal.muted));
        });
    });
    ui.add_space(16.0);

    let session: &Session = cx.session;
    let data = session.data();
    if data.staff.is_empty() {
        empty_state(ui, "No data yet", "Import your Excel tracker from Settings.");
        return;
    }
    state.refresh(session, cx.today);
    let SummaryState { filter, group, cache } = state;
    let all: &[DueItem] = cache.as_ref().map_or(&[], |(_, _, items)| items.as_slice());
    let soon_days = data.settings.due_soon_days;

    stat_cards(ui, pal, all, filter, soon_days);
    ui.add_space(20.0);
    filters(ui, pal, data, filter, group);
    ui.add_space(16.0);

    let shown = filter_items(all, filter);
    let any_due = shown.iter().any(|i| matches!(i.status, DueStatus::Overdue | DueStatus::DueSoon));
    let narrowed = filter.section.is_some() || filter.test.is_some() || filter.title.is_some() || !filter.text.trim().is_empty();
    if filter.status == StatusFilter::All && !any_due && (!shown.is_empty() || !narrowed) {
        empty_state(ui, "Everyone is up to date", &format!("Nothing is overdue or due within {soon_days} days."));
        return;
    }
    if shown.is_empty() {
        empty_state(ui, "No matches", "Nothing matches these filters.");
        return;
    }
    let picked = match group {
        Grouping::Person => cards(ui, pal, group_by_person(&shown), Grouping::Person),
        Grouping::Test => cards(ui, pal, group_by_test(&shown), Grouping::Test),
    };
    if picked.is_some() {
        *cx.open_detail = picked;
    }
}

/// Overdue, due soon, on track, delayed. Counts follow every filter except
/// status; clicking a card filters by it, clicking it again shows all.
fn stat_cards(ui: &mut egui::Ui, pal: &Palette, all: &[DueItem], filter: &mut DueFilter, soon_days: i64) {
    let base = DueFilter { status: StatusFilter::All, ..filter.clone() };
    let c = counts(filter_items(all, &base));
    let cards = [
        (StatusFilter::Overdue, "Overdue".to_owned(), c.overdue),
        (StatusFilter::DueSoon, format!("Due within {soon_days} days"), c.due_soon),
        (StatusFilter::OnTrack, "On track".to_owned(), c.on_track),
        (StatusFilter::Delayed, "Delayed".to_owned(), c.delayed),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        for (status, label, n) in cards {
            let selected = filter.status == status;
            if stat_card(ui, pal, &label, n, selected).clicked() {
                filter.status = if selected { StatusFilter::All } else { status };
            }
        }
    });
}

/// Search box, then chips (section, test system, title) and the grouping toggle.
fn filters(ui: &mut egui::Ui, pal: &Palette, data: &Data, filter: &mut DueFilter, group: &mut Grouping) {
    search_box(ui, &mut filter.text, "Search by name, test or section");
    ui.add_space(4.0);
    let sections: Vec<(Id, String)> =
        data.sections_sorted().into_iter().filter(|s| !s.retired).map(|s| (s.id.clone(), s.name.clone())).collect();
    let titles: Vec<(String, String)> = distinct_titles(data).into_iter().map(|t| (t.clone(), t)).collect();
    ui.horizontal(|ui| {
        chip_select(ui, "Section", &mut filter.section, &sections);
        let tests = test_options(data, filter.section.as_deref());
        if filter.test.as_ref().is_some_and(|t| !tests.iter().any(|(id, _)| id == t)) {
            filter.test = None;
        }
        chip_select(ui, "Test system", &mut filter.test, &tests);
        chip_select(ui, "Title", &mut filter.title, &titles);
        let any = filter.section.is_some()
            || filter.test.is_some()
            || filter.title.is_some()
            || !filter.text.is_empty()
            || filter.status != StatusFilter::All;
        if any && ui.add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false)).clicked() {
            *filter = DueFilter::default();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| segmented(ui, pal, group));
    });
}

/// The non-retired tests of `section`, or of every non-retired section
/// (labelled "test · section") when none is chosen.
fn test_options(data: &Data, section: Option<&str>) -> Vec<(Id, String)> {
    let mut out = Vec::new();
    for sec in data.sections_sorted().into_iter().filter(|s| !s.retired) {
        if section.is_some_and(|id| id != sec.id) {
            continue;
        }
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired).collect();
        tests.sort_by_key(|t| t.order);
        for t in tests {
            let label = if section.is_some() { t.name.clone() } else { format!("{} \u{b7} {}", t.name, sec.name) };
            out.push((t.id.clone(), label));
        }
    }
    out
}

/// A two-option segmented control: "By person" / "By test".
fn segmented(ui: &mut egui::Ui, pal: &Palette, group: &mut Grouping) {
    Frame::new()
        .fill(pal.sidebar)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (g, label) in [(Grouping::Person, "By person"), (Grouping::Test, "By test")] {
                    let selected = *group == g;
                    let button = Button::selectable(selected, label).corner_radius(8);
                    if ui.add(button).clicked() {
                        *group = g;
                    }
                }
            });
        });
}

/// One card per group; returns the (staff, test) of a clicked row.
fn cards(ui: &mut egui::Ui, pal: &Palette, groups: Vec<(Id, Vec<&DueItem>)>, by: Grouping) -> Option<(Id, Id)> {
    let mut picked = None;
    for (_, rows) in groups {
        let first = rows[0];
        let (title, subtitle) = match by {
            Grouping::Person => (&first.staff_name, &first.title),
            Grouping::Test => (&first.test_name, &first.section_name),
        };
        Frame::new()
            .fill(pal.card)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).strong().size(15.5).color(pal.text));
                    if !subtitle.is_empty() {
                        ui.label(RichText::new(subtitle).color(pal.muted));
                    }
                });
                ui.add_space(2.0);
                for item in rows {
                    let (primary, secondary) = match by {
                        Grouping::Person => {
                            (item.test_name.clone(), format!("{} \u{b7} {}", item.section_name, item.info.next_stage_label))
                        }
                        Grouping::Test => {
                            let mut s = item.info.next_stage_label.clone();
                            if !item.title.is_empty() {
                                s = format!("{} \u{b7} {s}", item.title);
                            }
                            (item.staff_name.clone(), s)
                        }
                    };
                    if due_row(ui, pal, item, &primary, &secondary) {
                        picked = Some((item.staff_id.clone(), item.test_id.clone()));
                    }
                }
            });
        ui.add_space(12.0);
    }
    picked
}

/// A clickable row: primary text, muted details, and the due pill on the
/// right. Its accessible name is "staff · test". Returns true when clicked.
fn due_row(ui: &mut egui::Ui, pal: &Palette, item: &DueItem, primary: &str, secondary: &str) -> bool {
    let mut pill_clicked = false;
    let builder = UiBuilder::new().id_salt((&item.staff_id, &item.test_id)).sense(Sense::click());
    let resp = ui
        .scope_builder(builder, |ui| {
            let fill = if ui.response().contains_pointer() { pal.sidebar } else { Color32::TRANSPARENT };
            Frame::new().fill(fill).corner_radius(8).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(Label::new(RichText::new(primary).color(pal.text)).selectable(false));
                    ui.add(Label::new(RichText::new(secondary).color(pal.muted)).selectable(false));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let delayed = item.delay_note.is_some();
                        pill_clicked = due_pill(ui, pal, &item.info, item.status, delayed).clicked();
                    });
                });
            });
        })
        .response;
    let name = format!("{} \u{b7} {}", item.staff_name, item.test_name);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &name));
    let mut resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if let Some(note) = &item.delay_note {
        resp = resp.on_hover_text(format!("Delay note: {}", note.note));
    }
    resp.clicked() || pill_clicked
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
        let data_dir = DataDir { root: dir.path().join("CompetencyData") };
        let mut s = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        let today = now_ts().date_naive();
        s.mutate(now_ts(), |d, _| {
            d.sections[0].tests.push(TestSystem { id: "t-1".into(), name: "Cobas".into(), order: 0, retired: false, rule_id: None });
            d.staff.push(Staff {
                id: "p-1".into(),
                name: "Agnes".into(),
                title: "MLT".into(),
                joined: None,
                status: StaffStatus::Active,
                assignments: [("t-1".to_string(), Assignment { state: AssignState::Assigned, since: today })].into(),
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

        s.mutate(now_ts(), |d, _| d.staff[0].name = "Agnes Tan".into()).unwrap();
        st.refresh(&s, today.succ_opt().unwrap());
        assert_eq!(st.cache.as_ref().unwrap().2[0].staff_name, "Agnes Tan", "new revision: recomputed");
    }
}
