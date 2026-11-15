//! One person's profile: header with the status strip and next-up line,
//! then their tests as section cards or as one table.

use chrono::NaiveDate;
use egui::{
    Align, Button, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense, Stroke,
    Vec2,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Staff, StaffStatus};
use crate::prefs::ProfileView;
use crate::query::Index;
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::ui::app::Ctx;
use crate::ui::theme::{heading, medium, Palette};
use crate::ui::widgets::{
    due_pill, empty_state, paint_section_chip, rgb, section_chip_width, section_dot, segmented,
    status_colors,
};

/// What the profile keeps between frames.
#[derive(Default)]
pub(crate) struct ProfileUi {
    /// Only tests with this status (from the strip); not remembered.
    pub filter: Option<DueStatus>,
    /// Table view: every row by soonest due, instead of grouped by section.
    pub sort_by_due: bool,
    /// (person, data revision, date) the rows were built for.
    cache: Option<(Id, u64, NaiveDate, Vec<ProfileRow>)>,
}

/// What the user asked of the page this frame.
#[derive(Default)]
pub(crate) struct ProfileOut {
    pub back: bool,
    pub edit: bool,
}

/// The strip's label for a status.
fn strip_label(s: DueStatus) -> &'static str {
    match s {
        DueStatus::Overdue => "Overdue",
        DueStatus::DueSoon => "Due soon",
        DueStatus::OnTrack => "Up to date",
        DueStatus::NotStarted => "Not done",
    }
}

/// The narrowest a section card gets, and the most cards side by side.
const CARD_MIN_W: f32 = 320.0;
const MAX_COLS: usize = 3;
const GAP: f32 = 12.0;

pub(crate) fn show(
    ui: &mut egui::Ui,
    cx: &mut Ctx,
    staff_id: &str,
    st: &mut ProfileUi,
) -> ProfileOut {
    let mut out = ProfileOut::default();
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let data = cx.session.data();
    let Some(staff) = data.staff(staff_id) else {
        out.back = true;
        return out;
    };
    let rev = cx.session.revision();
    let fresh =
        matches!(&st.cache, Some((id, r, d, _)) if id == staff_id && *r == rev && *d == cx.today);
    if !fresh {
        if st.cache.as_ref().is_some_and(|(id, ..)| id != staff_id) {
            // Another person: start from an unfiltered page.
            st.filter = None;
            st.sort_by_due = false;
        }
        st.cache = Some((staff.id.clone(), rev, cx.today, rows(data, staff, cx.today)));
    }
    let rows = &st.cache.as_ref().expect("built above").3;

    if ui
        .add(Button::new(RichText::new("\u{2190} All staff").color(pal.muted)).frame(false))
        .clicked()
    {
        out.back = true;
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        heading(ui, &staff.name);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let edit = ui.add_enabled(
                editing,
                Button::new(format!("{}  Edit", icon::PENCIL_SIMPLE)),
            );
            if edit
                .on_disabled_hover_text("Start editing to make changes")
                .clicked()
            {
                out.edit = true;
            }
        });
    });
    let mut bits = Vec::new();
    if !staff.title.is_empty() {
        bits.push(staff.title.clone());
    }
    if let Some(j) = staff.joined {
        bits.push(format!("Joined {}", fmt_date(j)));
    }
    match staff.status {
        StaffStatus::Active => {}
        StaffStatus::Hidden => bits.push("Hidden".to_owned()),
        StaffStatus::Removed => bits.push("Removed".to_owned()),
    }
    if !bits.is_empty() {
        ui.label(RichText::new(bits.join(" \u{b7} ")).color(pal.muted));
    }
    if rows.is_empty() {
        ui.add_space(12.0);
        empty_state(
            ui,
            "No assignments",
            "Edit this person to assign test systems.",
        );
        return out;
    }
    ui.add_space(12.0);

    // Status strip, with the view toggle on the right.
    let n = counts(rows);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, s) in STATUSES.into_iter().enumerate() {
            let on = st.filter == Some(s);
            if status_chip(ui, pal, s, n[i], on).clicked() {
                st.filter = if on { None } else { Some(s) };
            }
        }
        let mut toggle = |ui: &mut egui::Ui| {
            let current = usize::from(*cx.profile_view == ProfileView::Table);
            if let Some(i) = segmented(ui, pal, current, &["Cards", "Table"]) {
                *cx.profile_view = if i == 0 {
                    ProfileView::Cards
                } else {
                    ProfileView::Table
                };
            }
        };
        if ui.available_width() >= 150.0 {
            ui.with_layout(Layout::right_to_left(Align::Center), toggle);
        } else {
            toggle(ui);
        }
    });

    let mut open = None;
    if let Some(r) = next_up(rows) {
        ui.add_space(6.0);
        if next_up_line(ui, pal, r, cx.today).clicked() {
            open = Some(r.test_id.clone());
        }
    }
    ui.add_space(14.0);

    let shown = filtered(rows, st.filter);
    if shown.is_empty() {
        empty_state(ui, "Nothing here", "No tests have this status.");
    } else {
        let picked = match *cx.profile_view {
            ProfileView::Cards | ProfileView::Table => cards(ui, pal, &shown),
        };
        if picked.is_some() {
            open = picked;
        }
    }
    if let Some(t) = open {
        *cx.open_detail = Some((staff.id.clone(), t));
    }
    out
}

/// One clickable count in the status strip.
fn status_chip(
    ui: &mut egui::Ui,
    pal: &Palette,
    s: DueStatus,
    count: usize,
    on: bool,
) -> egui::Response {
    let (bg, fg) = status_colors(pal, s);
    let label = strip_label(s);
    let text_color = if count == 0 && !on {
        pal.muted
    } else {
        pal.text
    };
    let p = ui.painter();
    let name = p.layout_no_wrap(label.to_owned(), FontId::proportional(13.0), text_color);
    let num = p.layout_no_wrap(count.to_string(), FontId::new(13.0, medium()), text_color);
    let size = Vec2::new(
        12.0 + 8.0 + 6.0 + name.size().x + 6.0 + num.size().x + 12.0,
        28.0,
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let (fill, stroke) = if on {
            (bg, Stroke::new(1.5, fg))
        } else if resp.hovered() {
            (pal.sidebar, Stroke::new(1.0, pal.muted))
        } else {
            (pal.card, Stroke::new(1.0, pal.border))
        };
        p.rect(
            rect,
            CornerRadius::same(255),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        let mid = rect.center().y;
        let dot = if count == 0 && !on { pal.border } else { fg };
        p.circle_filled(egui::pos2(rect.left() + 16.0, mid), 4.0, dot);
        let x = rect.left() + 26.0;
        let name_w = name.size().x;
        p.galley(egui::pos2(x, mid - name.size().y / 2.0), name, text_color);
        p.galley(
            egui::pos2(x + name_w + 6.0, mid - num.size().y / 2.0),
            num,
            text_color,
        );
    }
    if count > 0 || on {
        resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    let tip = if on {
        "Show all tests"
    } else {
        "Show only these"
    };
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            true,
            on,
            format!("{label} {count}"),
        )
    });
    resp.on_hover_text(tip)
}

/// "In 3 days", "due today", "overdue by 2 days".
fn when(due: NaiveDate, today: NaiveDate) -> String {
    let days = (due - today).num_days();
    let plural = |n: i64| if n == 1 { "day" } else { "days" };
    match days {
        d if d < 0 => format!("overdue by {} {}", -d, plural(-d)),
        0 => "due today".to_owned(),
        d => format!("due {} \u{b7} in {d} {}", fmt_date(due), plural(d)),
    }
}

/// "Next up: test · [section] · stage — when", clickable as a whole.
fn next_up_line(
    ui: &mut egui::Ui,
    pal: &Palette,
    r: &ProfileRow,
    today: NaiveDate,
) -> egui::Response {
    let Some((info, status, _)) = &r.due else {
        unreachable!("next_up only picks assigned rows")
    };
    let due = info.due_on.expect("next_up only picks dated rows");
    let (_, fg) = status_colors(pal, *status);
    let tail = when(due, today);
    // Painted as one widget, so it is one thing to click (and to find).
    let size = 13.5;
    let p = ui.painter();
    let lead = p.layout_no_wrap("Next up".into(), FontId::proportional(size), pal.muted);
    let name = p.layout_no_wrap(r.test_name.clone(), FontId::new(size, medium()), pal.text);
    let stage = p.layout_no_wrap(
        info.next_stage_label.clone(),
        FontId::proportional(size),
        pal.muted,
    );
    let tail_g = p.layout_no_wrap(format!("\u{2014} {tail}"), FontId::proportional(size), fg);
    let arrow = p.layout_no_wrap(
        icon::ARROW_RIGHT.into(),
        FontId::proportional(size),
        pal.muted,
    );
    let chip_w = section_chip_width(ui, &r.section_name);
    let gap = 7.0;
    let widths = [
        lead.size().x,
        name.size().x,
        chip_w,
        stage.size().x,
        tail_g.size().x,
        arrow.size().x,
    ];
    let total = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total, 22.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let mid = rect.center().y;
        let mut x = rect.left();
        let mut put = |g: std::sync::Arc<egui::Galley>, c: Color32| {
            let w = g.size().x;
            ui.painter()
                .galley(egui::pos2(x, mid - g.size().y / 2.0), g, c);
            x += w + gap;
        };
        put(lead, pal.muted);
        put(name, pal.text);
        let chip = egui::Rect::from_min_size(egui::pos2(x, mid - 9.0), Vec2::new(chip_w, 18.0));
        paint_section_chip(ui, pal, chip, &r.section_name, r.color);
        x += chip_w + gap;
        let mut put = |g: std::sync::Arc<egui::Galley>, c: Color32| {
            let w = g.size().x;
            ui.painter()
                .galley(egui::pos2(x, mid - g.size().y / 2.0), g, c);
            x += w + gap;
        };
        put(stage, pal.muted);
        put(tail_g, fg);
        put(arrow, pal.muted);
    }
    if resp.hovered() {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() + 1.0,
            Stroke::new(1.0, pal.border),
        );
    }
    let label = format!(
        "Next up: {} \u{b7} {} \u{b7} {} \u{2014} {tail}",
        r.test_name, r.section_name, info.next_stage_label
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Consecutive rows of one section.
fn groups<'a>(rows: &[&'a ProfileRow]) -> Vec<Vec<&'a ProfileRow>> {
    let mut out: Vec<Vec<&ProfileRow>> = Vec::new();
    for r in rows {
        match out.last_mut() {
            Some(g) if g[0].section_id == r.section_id => g.push(r),
            _ => out.push(vec![r]),
        }
    }
    out
}

/// A test's name as a frameless button, shortened to fit (in full on hover).
fn test_name(ui: &mut egui::Ui, pal: &Palette, name: &str) -> egui::Response {
    let b = Button::new(RichText::new(name).color(pal.text))
        .frame(false)
        .wrap_mode(egui::TextWrapMode::Truncate);
    ui.add(b).on_hover_text(name)
}

/// The tests as one card per section, as many side by side as fit.
/// Returns the test clicked.
fn cards(ui: &mut egui::Ui, pal: &Palette, rows: &[&ProfileRow]) -> Option<Id> {
    let mut picked = None;
    let avail = ui.available_width();
    let cols = (((avail + GAP) / (CARD_MIN_W + GAP)).floor() as usize).clamp(1, MAX_COLS);
    let w = (avail - GAP * (cols - 1) as f32) / cols as f32;
    for line in groups(rows).chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for g in line {
                ui.vertical(|ui| {
                    ui.set_width(w);
                    if let Some(t) = card(ui, pal, g) {
                        picked = Some(t);
                    }
                });
            }
        });
        ui.add_space(GAP);
    }
    picked
}

fn card(ui: &mut egui::Ui, pal: &Palette, g: &[&ProfileRow]) -> Option<Id> {
    let mut picked = None;
    let first = g[0];
    let frame = Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 13,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                section_dot(ui, first.color);
                ui.label(
                    RichText::new(&first.section_name)
                        .family(medium())
                        .size(14.0)
                        .color(pal.text),
                );
            });
            ui.add_space(4.0);
            for (k, r) in g.iter().enumerate() {
                if k > 0 {
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        match &r.due {
                            None => {
                                ui.label(RichText::new("N/A").color(pal.muted));
                            }
                            Some((info, status, delayed)) => {
                                if due_pill(ui, pal, info, *status, *delayed).clicked() {
                                    picked = Some(r.test_id.clone());
                                }
                            }
                        }
                        ui.add_space(6.0);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if test_name(ui, pal, &r.test_name).clicked() {
                                picked = Some(r.test_id.clone());
                            }
                        });
                    });
                });
                if r.due.is_some() {
                    let text = r.last_done.map_or("Never done".to_owned(), |d| {
                        format!("Last done {}", fmt_date(d))
                    });
                    ui.label(RichText::new(text).size(12.0).color(pal.muted));
                }
            }
        });
    // The section's colour along the top edge.
    let rect = frame.response.rect;
    let strip = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0));
    ui.painter().rect_filled(
        strip,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        first.color,
    );
    picked
}

/// One of the person's tests, ready to draw.
pub(crate) struct ProfileRow {
    pub section_id: Id,
    pub section_name: String,
    pub color: Color32,
    pub test_id: Id,
    pub test_name: String,
    /// `None` when the test is N/A for this person.
    pub due: Option<(DueInfo, DueStatus, bool)>,
    /// The last completion; `None` for N/A and never-done tests.
    pub last_done: Option<NaiveDate>,
}

impl ProfileRow {
    pub fn status(&self) -> Option<DueStatus> {
        self.due.as_ref().map(|d| d.1)
    }
}

/// The strip's statuses, in order.
pub(crate) const STATUSES: [DueStatus; 4] = [
    DueStatus::Overdue,
    DueStatus::DueSoon,
    DueStatus::OnTrack,
    DueStatus::NotStarted,
];

/// Every live test the person has an assignment for (assigned or N/A), by
/// section order then test order.
pub(crate) fn rows(data: &Data, staff: &Staff, today: NaiveDate) -> Vec<ProfileRow> {
    let idx = Index::build(data);
    let mut out = Vec::new();
    for (pos, sec) in data.sections_sorted().into_iter().enumerate() {
        if sec.retired {
            continue;
        }
        let color = rgb(sec.color_rgb(pos));
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired).collect();
        tests.sort_by_key(|t| t.order);
        for t in tests {
            let Some(a) = staff.assignments.get(&t.id) else {
                continue;
            };
            let due = (a.state == AssignState::Assigned).then(|| {
                let info = next_due(
                    data.rule_for_test(&t.id),
                    &idx.pair(&staff.id, &t.id),
                    &data.settings,
                );
                let status = classify(info.due_on, today, data.settings.due_soon_days);
                let delayed = idx
                    .delay_notes(&staff.id, &t.id)
                    .iter()
                    .any(|n| n.for_stage_index == info.next_stage_index);
                (info, status, delayed)
            });
            out.push(ProfileRow {
                section_id: sec.id.clone(),
                section_name: sec.name.clone(),
                color,
                test_id: t.id.clone(),
                test_name: t.name.clone(),
                last_done: due.as_ref().and_then(|d| d.0.last_completed_on),
                due,
            });
        }
    }
    out
}

/// How many assigned tests have each status, in [`STATUSES`] order.
pub(crate) fn counts(rows: &[ProfileRow]) -> [usize; 4] {
    let mut out = [0; 4];
    for s in rows.iter().filter_map(ProfileRow::status) {
        if let Some(i) = STATUSES.iter().position(|x| *x == s) {
            out[i] += 1;
        }
    }
    out
}

/// The assigned test with the earliest due date.
pub(crate) fn next_up(rows: &[ProfileRow]) -> Option<&ProfileRow> {
    rows.iter()
        .filter_map(|r| Some((r, r.due.as_ref()?.0.due_on?)))
        .min_by_key(|(_, d)| *d)
        .map(|(r, _)| r)
}

/// All rows, or only the assigned ones with status `f`.
pub(crate) fn filtered(rows: &[ProfileRow], f: Option<DueStatus>) -> Vec<&ProfileRow> {
    rows.iter()
        .filter(|r| f.is_none() || r.status() == f)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::now_ts;
    use crate::model::{
        AssignState, Assignment, Data, DueOverride, OverrideSource, Record, Staff, StaffStatus,
        TestSystem,
    };

    fn today() -> NaiveDate {
        now_ts().date_naive()
    }

    fn test_sys(id: &str, name: &str, order: u32) -> TestSystem {
        TestSystem {
            id: id.into(),
            name: name.into(),
            order,
            retired: false,
            rule_id: None,
        }
    }

    /// Chemistry: B (order 1), A (order 0). Haematology: H.
    fn data() -> Data {
        let mut d = Data::new_empty("t", now_ts());
        d.settings.initial_due_days = None;
        d.sections[0].tests = vec![test_sys("t-b", "B", 1), test_sys("t-a", "A", 0)];
        d.sections[1].tests = vec![test_sys("t-h", "H", 0)];
        d
    }

    fn person(tests: &[(&str, AssignState)]) -> Staff {
        Staff {
            id: "p".into(),
            name: "P".into(),
            title: String::new(),
            joined: None,
            status: StaffStatus::Active,
            assignments: tests
                .iter()
                .map(|(t, state)| {
                    (
                        (*t).to_string(),
                        Assignment {
                            state: *state,
                            since: today() - chrono::Days::new(800),
                        },
                    )
                })
                .collect(),
        }
    }

    fn done(d: &mut Data, test: &str, days_ago: u64) {
        d.records.push(Record {
            id: format!("r-{test}"),
            staff_id: "p".into(),
            test_id: test.into(),
            stage_index: 0,
            stage_label: "Initial".into(),
            completed_on: today() - chrono::Days::new(days_ago),
            comment: String::new(),
            by: "t".into(),
            at: now_ts(),
            imported: false,
            unconfirmed: false,
        });
    }

    fn due_in(d: &mut Data, test: &str, days: u64) {
        d.due_overrides.push(DueOverride {
            staff_id: "p".into(),
            test_id: test.into(),
            due_on: today() + chrono::Days::new(days),
            source: OverrideSource::Manual,
            by: "t".into(),
            at: now_ts(),
        });
    }

    use AssignState::{Assigned, Na};

    #[test]
    fn rows_follow_section_and_test_order() {
        let d = data();
        let p = person(&[("t-h", Assigned), ("t-b", Assigned), ("t-a", Assigned)]);
        let names: Vec<_> = rows(&d, &p, today())
            .into_iter()
            .map(|r| r.test_name)
            .collect();
        assert_eq!(names, ["A", "B", "H"]);
    }

    #[test]
    fn counts_only_assigned() {
        let mut d = data();
        done(&mut d, "t-a", 400); // overdue
        let p = person(&[("t-a", Assigned), ("t-b", Assigned), ("t-h", Na)]);
        assert_eq!(counts(&rows(&d, &p, today())), [1, 0, 0, 1]);
    }

    #[test]
    fn next_up_skips_undated_and_na() {
        let mut d = data();
        due_in(&mut d, "t-h", 10);
        let p = person(&[("t-a", Assigned), ("t-b", Na), ("t-h", Assigned)]);
        let r = rows(&d, &p, today());
        assert_eq!(next_up(&r).map(|r| r.test_name.as_str()), Some("H"));
        let only_na = person(&[("t-b", Na)]);
        assert!(next_up(&rows(&d, &only_na, today())).is_none());
    }

    #[test]
    fn never_done_has_no_last_done() {
        let d = data();
        let p = person(&[("t-a", Assigned)]);
        let r = rows(&d, &p, today());
        assert_eq!(r[0].last_done, None);
        assert_eq!(r[0].due.as_ref().map(|d| d.1), Some(DueStatus::NotStarted));
    }

    #[test]
    fn only_na_counts_zero() {
        let d = data();
        let p = person(&[("t-a", Na), ("t-h", Na)]);
        let r = rows(&d, &p, today());
        assert_eq!(r.len(), 2);
        assert_eq!(counts(&r), [0, 0, 0, 0]);
    }

    #[test]
    fn filter_hides_na() {
        let mut d = data();
        done(&mut d, "t-a", 400);
        let p = person(&[("t-a", Assigned), ("t-b", Na)]);
        let r = rows(&d, &p, today());
        let over: Vec<_> = filtered(&r, Some(DueStatus::Overdue))
            .iter()
            .map(|r| r.test_name.as_str())
            .collect();
        assert_eq!(over, ["A"]);
        assert_eq!(filtered(&r, None).len(), 2);
    }
}
