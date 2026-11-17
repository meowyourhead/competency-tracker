//! Reusable widgets: due pill, stat card, chip dropdown, search box, empty
//! state, plus the icon button and nav row used by the shell.

use egui::{
    Align2, Color32, CornerRadius, FontId, Popup, Rect, RichText, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::Data;
use crate::schedule::{DueInfo, DueSource, DueStatus};
use crate::ui::theme::{serif, Palette};

const PILL: CornerRadius = CornerRadius::same(255);

pub fn status_label(status: DueStatus) -> &'static str {
    match status {
        DueStatus::Overdue => "Overdue",
        DueStatus::DueSoon => "Due soon",
        DueStatus::OnTrack => "On track",
        DueStatus::NotStarted => "Not started",
    }
}

/// (fill, text) colours for a due status.
pub fn status_colors(pal: &Palette, status: DueStatus) -> (Color32, Color32) {
    match status {
        DueStatus::Overdue => (pal.red_bg, pal.red_fg),
        DueStatus::DueSoon => (pal.yellow_bg, pal.yellow_fg),
        DueStatus::OnTrack => (pal.green_bg, pal.green_fg),
        DueStatus::NotStarted => (pal.grey_bg, pal.grey_fg),
    }
}

/// The text shown inside a due pill (without the delay marker).
pub fn due_pill_text(info: &DueInfo) -> String {
    let mut text = info
        .due_on
        .map(fmt_date)
        .unwrap_or_else(|| "Initial pending".to_owned());
    match info.source {
        DueSource::Excel => text.push_str(" \u{b7} from Excel"),
        DueSource::Manual => text.push_str(" \u{b7} manual"),
        DueSource::Rule | DueSource::None => {}
    }
    text
}

/// A fully rounded, tinted pill with the due date (or "Initial pending").
pub fn due_pill(
    ui: &mut Ui,
    pal: &Palette,
    info: &DueInfo,
    status: DueStatus,
    delayed: bool,
) -> egui::Response {
    let mut text = due_pill_text(info);
    if delayed {
        text = format!("{} {text}", icon::PAUSE);
    }
    let (fill, fg) = status_colors(pal, status);
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), FontId::proportional(12.5), fg);
    let size = galley.size() + Vec2::new(20.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, PILL, fill);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, fg);
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
    resp.on_hover_text(format!(
        "Next: {} \u{b7} {}",
        info.next_stage_label,
        status_label(status)
    ))
}

/// `0xRRGGBB` as a colour.
pub fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// A section's colour: its own, else the default for its place among the
/// sorted sections. `None` when there is no such section.
pub fn section_color(data: &Data, section_id: &str) -> Option<Color32> {
    data.sections_sorted()
        .iter()
        .enumerate()
        .find(|(_, s)| s.id == section_id)
        .map(|(i, s)| rgb(s.color_rgb(i)))
}

/// Size of a section chip's text.
pub const SECTION_CHIP_SIZE: f32 = 11.5;
/// Space between a section chip's edge and its text.
const SECTION_CHIP_PAD: f32 = 10.0;
/// How strongly a section's colour tints its chip.
pub const SECTION_TINT: f32 = 0.24;

/// Width of a section chip for `name` (text and padding).
pub fn section_chip_width(ui: &Ui, name: &str) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(
            name.to_owned(),
            FontId::proportional(SECTION_CHIP_SIZE),
            Color32::WHITE,
        )
        .size()
        .x;
    text + 2.0 * SECTION_CHIP_PAD
}

/// Paints a section chip (a pill tinted in the section's colour, holding its
/// name) into `rect`. A name too long for `rect` is cut off at its edge.
pub fn paint_section_chip(ui: &Ui, pal: &Palette, rect: Rect, name: &str, color: Color32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    p.rect_filled(rect, PILL, color.gamma_multiply(SECTION_TINT));
    p.with_clip_rect(rect.shrink2(Vec2::new(SECTION_CHIP_PAD - 2.0, 0.0)))
        .text(
            egui::pos2(rect.left() + SECTION_CHIP_PAD, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(SECTION_CHIP_SIZE),
            pal.text,
        );
}

/// Section chips on one line, `width` wide. Those that don't fit are
/// counted in a "+N" at the end; the tooltip names every section.
pub fn section_chip_row(
    ui: &mut Ui,
    pal: &Palette,
    secs: &[(&str, Color32)],
    width: f32,
) -> egui::Response {
    const H: f32 = 20.0;
    const GAP: f32 = 6.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, H), Sense::hover());
    let more_w = 30.0;
    let mut x = rect.left();
    for (k, (name, color)) in secs.iter().enumerate() {
        let w = section_chip_width(ui, name);
        let last = k + 1 == secs.len();
        // Keep room for the "+N" unless this is the last chip.
        let limit = rect.right() - if last { 0.0 } else { more_w };
        if x + w > limit && k > 0 {
            ui.painter().text(
                egui::pos2(x, rect.center().y),
                Align2::LEFT_CENTER,
                format!("+{}", secs.len() - k),
                FontId::proportional(SECTION_CHIP_SIZE + 0.5),
                pal.muted,
            );
            break;
        }
        let w = w.min(rect.right() - x);
        let chip = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(w, H));
        paint_section_chip(ui, pal, chip, name, *color);
        x += w + GAP;
    }
    let names: Vec<&str> = secs.iter().map(|(n, _)| *n).collect();
    let label = names.join(", ");
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &label));
    resp.on_hover_text(names.join("\n"))
}

/// A section's name as a chip in its colour.
pub fn section_chip(ui: &mut Ui, pal: &Palette, name: &str, color: Color32) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Body).min(18.0) + 2.0;
    let size = Vec2::new(section_chip_width(ui, name), h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_section_chip(ui, pal, rect, name, color);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, name));
    resp
}

/// A clickable summary card: big serif number over a muted label.
pub fn stat_card(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    value: usize,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(176.0, 84.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let stroke = if selected {
            Stroke::new(1.5, pal.accent)
        } else if resp.hovered() {
            Stroke::new(1.0, pal.muted)
        } else {
            Stroke::new(1.0, pal.border)
        };
        let p = ui.painter();
        p.rect(
            rect,
            CornerRadius::same(12),
            pal.card,
            stroke,
            StrokeKind::Inside,
        );
        let left = rect.left() + 16.0;
        p.text(
            egui::pos2(left, rect.top() + 12.0),
            Align2::LEFT_TOP,
            value,
            FontId::new(30.0, serif()),
            pal.text,
        );
        p.text(
            egui::pos2(left, rect.bottom() - 12.0),
            Align2::LEFT_BOTTOM,
            label,
            FontId::proportional(13.0),
            pal.muted,
        );
    }
    resp.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Button,
            true,
            selected,
            format!("{label} {value}"),
        )
    });
    resp
}

/// A pill-styled dropdown. `None` means "All". Returns true when changed.
pub fn chip_select<T: PartialEq + Clone>(
    ui: &mut Ui,
    label: &str,
    current: &mut Option<T>,
    options: &[(T, String)],
) -> bool {
    let shown = current
        .as_ref()
        .and_then(|c| options.iter().find(|(v, _)| v == c))
        .map_or_else(|| label.to_owned(), |(_, name)| format!("{label}: {name}"));
    let button = egui::Button::new(format!("{shown}  {}", icon::CARET_DOWN))
        .corner_radius(PILL)
        .selected(current.is_some());
    let resp = ui.add(button);
    let mut changed = false;
    Popup::menu(&resp).show(|ui| {
        if ui.selectable_label(current.is_none(), "All").clicked() {
            changed |= current.is_some();
            *current = None;
        }
        for (value, name) in options {
            let selected = current.as_ref() == Some(value);
            if ui.selectable_label(selected, name).clicked() {
                changed |= !selected;
                *current = Some(value.clone());
            }
        }
    });
    changed
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
/// One day in the picker's calendar; seven make its width.
const DAY_CELL: f32 = 36.0;

/// Which part of the date the picker's body is choosing.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum PickView {
    #[default]
    Day,
    Month,
    Year,
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    chrono::NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| chrono::Datelike::day(&d))
}

/// `d` moved to `year` / `month`, its day pulled back when the month is shorter.
fn with_month(d: chrono::NaiveDate, year: i32, month: u32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let day = d.day().min(days_in_month(year, month));
    chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap_or(d)
}

/// `d` moved by `months` (negative goes back).
fn shift_months(d: chrono::NaiveDate, months: i32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let n = d.year() * 12 + d.month0() as i32 + months;
    with_month(d, n.div_euclid(12), n.rem_euclid(12) as u32 + 1)
}

/// A date field: a button showing the date ("15 Nov 2026") that opens a
/// picker. The picker's heading reads day, month, year; each part is a tab
/// for choosing it (a calendar of days, the twelve months, a page of years).
pub fn date_picker(ui: &mut Ui, date: &mut chrono::NaiveDate, salt: &str) {
    let text = format!("{}  {}", icon::CALENDAR_BLANK, fmt_date(*date));
    let resp = ui
        .push_id(("date_picker", salt), |ui| ui.button(text))
        .inner;
    let popup_id = Popup::default_response_id(&resp);
    if resp.clicked() {
        // Every opening starts on the calendar, at the date's own years.
        ui.data_mut(|d| {
            d.remove_temp::<PickView>(popup_id);
            d.remove_temp::<i32>(popup_id);
        });
    }
    let frame = egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(14));
    Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .frame(frame)
        .gap(6.0)
        .show(|ui| date_picker_body(ui, date, popup_id));
}

fn date_picker_body(ui: &mut Ui, date: &mut chrono::NaiveDate, popup_id: egui::Id) {
    use chrono::Datelike;
    let width = 7.0 * DAY_CELL;
    ui.set_width(width);
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    let accent = ui.visuals().hyperlink_color;
    let muted = ui.visuals().weak_text_color();
    let border = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let today = crate::dates::now_ts().date_naive();
    let mut view: PickView = ui.data(|d| d.get_temp(popup_id)).unwrap_or_default();
    let page_of = |year: i32| year.div_euclid(12) * 12;
    let mut page: i32 = ui
        .data(|d| d.get_temp(popup_id))
        .unwrap_or_else(|| page_of(date.year()));

    // The date as a heading, in the order it's chosen: day, month, year.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 9.0;
        let parts = [
            (PickView::Day, date.day().to_string(), "Day"),
            (
                PickView::Month,
                MONTHS[date.month0() as usize].to_owned(),
                "Month",
            ),
            (PickView::Year, date.year().to_string(), "Year"),
        ];
        for (v, text, what) in parts {
            if date_part(ui, &text, what, view == v, accent, muted).clicked() {
                view = v;
                page = page_of(date.year());
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (unit, step) = match view {
                PickView::Day => ("month", 1),
                PickView::Month => ("year", 12),
                PickView::Year => ("years", 0),
            };
            let mut go = 0;
            if step_button(ui, icon::CARET_RIGHT, &format!("Next {unit}")).clicked() {
                go = 1;
            }
            if step_button(ui, icon::CARET_LEFT, &format!("Previous {unit}")).clicked() {
                go = -1;
            }
            if view == PickView::Year {
                page += 12 * go;
            } else if go != 0 {
                *date = shift_months(*date, step * go);
            }
        });
    });
    ui.add_space(10.0);

    match view {
        PickView::Day => {
            ui.horizontal(|ui| {
                for name in WEEKDAYS {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(DAY_CELL, 22.0), Sense::hover());
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        name,
                        FontId::proportional(11.5),
                        muted,
                    );
                }
            });
            let first = date.with_day(1).unwrap_or(*date);
            let lead = first.weekday().num_days_from_monday();
            let days = days_in_month(date.year(), date.month());
            let mut picked = None;
            for week in 0..(lead + days).div_ceil(7) {
                ui.horizontal(|ui| {
                    for slot in week * 7..week * 7 + 7 {
                        let day = (slot + 1).saturating_sub(lead);
                        if slot < lead || day > days {
                            ui.allocate_exact_size(Vec2::splat(DAY_CELL), Sense::hover());
                            continue;
                        }
                        let Some(d) = date.with_day(day) else {
                            continue;
                        };
                        let cell = pick_cell(
                            ui,
                            Vec2::splat(DAY_CELL),
                            &day.to_string(),
                            &fmt_date(d),
                            d == *date,
                            d == today,
                            true,
                        );
                        if cell.clicked() {
                            picked = Some(d);
                        }
                    }
                });
            }
            if let Some(d) = picked {
                *date = d;
            }
        }
        PickView::Month => {
            let size = Vec2::new(width / 3.0, 40.0);
            for row in 0..4 {
                ui.horizontal(|ui| {
                    for m in row * 3..row * 3 + 3u32 {
                        let name = MONTHS[m as usize];
                        let this = date.year() == today.year() && m == today.month0();
                        let on = m == date.month0();
                        if pick_cell(ui, size, &name[..3], name, on, this, false).clicked() {
                            *date = with_month(*date, date.year(), m + 1);
                        }
                    }
                });
            }
        }
        PickView::Year => {
            let size = Vec2::new(width / 3.0, 40.0);
            for row in 0..4 {
                ui.horizontal(|ui| {
                    for y in page + row * 3..page + row * 3 + 3 {
                        let text = y.to_string();
                        let (on, this) = (y == date.year(), y == today.year());
                        if pick_cell(ui, size, &text, &text, on, this, false).clicked() {
                            *date = with_month(*date, y, date.month());
                        }
                    }
                });
            }
        }
    }

    ui.add_space(10.0);
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, border));
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let jump = egui::Button::new(RichText::new("Today").color(accent)).frame(false);
        if ui.add(jump).on_hover_text(fmt_date(today)).clicked() {
            *date = today;
            view = PickView::Day;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Done").clicked() {
                Popup::close_id(ui.ctx(), popup_id);
            }
        });
    });
    ui.data_mut(|d| {
        d.insert_temp(popup_id, view);
        d.insert_temp(popup_id, page);
    });
}

/// One part of the picker's heading (the day, the month or the year). The
/// part being chosen is in full ink over an accent rule; the others are quiet.
fn date_part(
    ui: &mut Ui,
    text: &str,
    what: &str,
    active: bool,
    accent: Color32,
    muted: Color32,
) -> egui::Response {
    let font = FontId::new(19.0, serif());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if active || resp.hovered() || resp.has_focus() {
            ui.visuals().text_color()
        } else {
            muted
        };
        let p = ui.painter();
        p.galley_with_override_text_color(
            egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0 - 1.0),
            galley,
            color,
        );
        if active {
            p.hline(
                rect.x_range(),
                rect.bottom() - 1.0,
                Stroke::new(2.0, accent),
            );
        }
    }
    let label = format!("{what}: {text}");
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small chevron that steps the picker back or forward.
fn step_button(ui: &mut Ui, glyph: &str, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() || resp.has_focus() {
            let fill = ui.visuals().widgets.hovered.bg_fill;
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(15.0),
            ui.visuals().text_color(),
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    resp.on_hover_text(label)
}

/// One choice in the picker: a day (`round`), a month or a year. The chosen
/// one is filled with the accent; `current` (today, this month, this year)
/// wears an accent ring.
fn pick_cell(
    ui: &mut Ui,
    size: Vec2,
    text: &str,
    label: &str,
    selected: bool,
    current: bool,
    round: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let accent = ui.visuals().hyperlink_color;
        let shape = if round {
            Rect::from_center_size(rect.center(), Vec2::splat(size.min_elem() - 4.0))
        } else {
            rect.shrink2(Vec2::new(3.0, 3.0))
        };
        let radius = if round { PILL } else { CornerRadius::same(9) };
        let lit = resp.hovered() || resp.has_focus();
        let (fill, fg) = if selected {
            (accent, Color32::WHITE)
        } else if lit {
            (
                ui.visuals().widgets.hovered.bg_fill,
                ui.visuals().text_color(),
            )
        } else {
            (Color32::TRANSPARENT, ui.visuals().text_color())
        };
        let p = ui.painter();
        p.rect_filled(shape, radius, fill);
        if current && !selected {
            p.rect_stroke(shape, radius, Stroke::new(1.0, accent), StrokeKind::Inside);
        }
        let font = if selected {
            FontId::new(13.5, crate::ui::theme::medium())
        } else {
            FontId::proportional(13.5)
        };
        p.text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    resp
}

/// The width of a search field that sits in a filter row: room for a long
/// name, not the whole page.
pub const SEARCH_W: f32 = 220.0;

/// A single-line search field with a magnifying-glass hint, `width` wide
/// (`f32::INFINITY` fills the row).
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> egui::Response {
    ui.add(
        TextEdit::singleline(text)
            .hint_text(format!("{}  {hint}", icon::MAGNIFYING_GLASS))
            .margin(egui::vec2(10.0, 6.0))
            .desired_width(width),
    )
}

/// A centred, quiet message for empty pages and lists.
pub fn empty_state(ui: &mut Ui, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(RichText::new(title).family(serif()).size(20.0));
        ui.add_space(4.0);
        ui.label(RichText::new(body).color(ui.visuals().weak_text_color()));
    });
}

/// A selectable option (segmented controls, result lists). Unlike egui's
/// selectable button, its size is the same in every state, so hovering never
/// nudges it or its neighbours. `full_width` left-aligns it across the row.
pub fn choice(
    ui: &mut Ui,
    pal: &Palette,
    selected: bool,
    text: &str,
    full_width: bool,
) -> egui::Response {
    let pad = ui.spacing().button_padding;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let fg = if selected { pal.accent } else { pal.text };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, fg);
    let mut size = galley.size() + 2.0 * pad;
    size.y = size.y.max(ui.spacing().interact_size.y);
    if full_width {
        size.x = size.x.max(ui.available_width());
    }
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            pal.accent.gamma_multiply(0.18)
        } else if resp.hovered() || resp.has_focus() {
            pal.border
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        let pos = if full_width {
            egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y / 2.0)
        } else {
            rect.center() - galley.size() / 2.0
        };
        ui.painter().galley(pos, galley, fg);
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, text));
    resp
}

/// A segmented control: a tinted track holding one [`choice`] per label.
/// Returns the index clicked this frame (also when already selected).
pub fn segmented(ui: &mut Ui, pal: &Palette, current: usize, labels: &[&str]) -> Option<usize> {
    let mut picked = None;
    // In a right-to-left row the labels would come out backwards.
    let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
    let order: Vec<usize> = if rtl {
        (0..labels.len()).rev().collect()
    } else {
        (0..labels.len()).collect()
    };
    egui::Frame::new()
        .fill(pal.sidebar)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                for &i in &order {
                    if choice(ui, pal, i == current, labels[i], false).clicked() {
                        picked = Some(i);
                    }
                }
            });
        });
    picked
}

/// A title-bar button (minimise, maximise, close), flush with the window
/// edge like the system ones. `danger` turns the hover red (close).
pub fn window_button(
    ui: &mut Ui,
    pal: &Palette,
    glyph: &str,
    label: &str,
    danger: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(46.0, ui.available_height()), Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, fg) = match (resp.hovered(), danger) {
            (true, true) => (hex_close(resp.is_pointer_button_down_on()), Color32::WHITE),
            (true, false) => (pal.border, pal.text),
            _ => (Color32::TRANSPARENT, pal.text),
        };
        ui.painter().rect_filled(rect, 0, fill);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(15.0),
            fg,
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    resp.on_hover_text(label)
}

/// Windows' own close-button red, a little darker while pressed.
fn hex_close(pressed: bool) -> Color32 {
    if pressed {
        Color32::from_rgb(0x94, 0x1F, 0x14)
    } else {
        Color32::from_rgb(0xC4, 0x2B, 0x1C)
    }
}

/// The title bar's presence capsule: a status dot, the user's name and
/// "Editing" / "Viewing", joined to the button that switches between them.
/// While editing, the capsule carries an accent tint and the button is quiet
/// ("Stop editing"); while viewing, the capsule is neutral and the button is
/// the accent call to action ("Start editing"). Returns the button's response.
pub fn presence(
    ui: &mut Ui,
    pal: &Palette,
    user: &str,
    editing: bool,
    can_toggle: bool,
) -> egui::Response {
    const H: f32 = 30.0;
    const PAD: f32 = 12.0;
    let status = if editing { "Editing" } else { "Viewing" };
    let action = if editing {
        "Stop editing"
    } else {
        "Start editing"
    };
    let name_g = ui.painter().layout_no_wrap(
        user.to_owned(),
        FontId::new(13.5, crate::ui::theme::medium()),
        pal.text,
    );
    let status_color = if editing { pal.accent } else { pal.muted };
    let status_g =
        ui.painter()
            .layout_no_wrap(status.to_owned(), FontId::proportional(13.0), status_color);
    let action_g = ui.painter().layout_no_wrap(
        action.to_owned(),
        FontId::new(13.0, crate::ui::theme::medium()),
        pal.text,
    );
    let info_w = PAD + 8.0 + 8.0 + name_g.size().x + 8.0 + status_g.size().x + PAD;
    let button_w = PAD + action_g.size().x + PAD;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(info_w + button_w, H), Sense::hover());
    let info = Rect::from_min_size(rect.min, Vec2::new(info_w, H));
    let button = Rect::from_min_max(egui::pos2(info.right(), rect.top()), rect.max);
    let enabled = can_toggle;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(button, ui.id().with("presence_toggle"), sense);
    let who = ui.interact(info, ui.id().with("presence_info"), Sense::hover());
    who.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Label, true, format!("{user} \u{b7} {status}"))
    });
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let pill = CornerRadius::same((H / 2.0) as u8);
        let (fill, stroke) = if editing {
            (
                pal.accent.gamma_multiply(0.10),
                Stroke::new(1.0, pal.accent.gamma_multiply(0.45)),
            )
        } else {
            (pal.card, Stroke::new(1.0, pal.border))
        };
        p.rect(rect, pill, fill, stroke, StrokeKind::Inside);
        // The button half: accent when it invites, quiet when it stops.
        let right = CornerRadius {
            nw: 0,
            sw: 0,
            ne: pill.ne,
            se: pill.se,
        };
        let hovered = enabled && resp.hovered();
        let (bfill, bfg) = match (editing, enabled) {
            (_, false) => (Color32::TRANSPARENT, pal.muted),
            (false, true) if hovered => (pal.accent.gamma_multiply(0.85), Color32::WHITE),
            (false, true) => (pal.accent, Color32::WHITE),
            (true, true) if hovered => (pal.accent.gamma_multiply(0.18), pal.text),
            (true, true) => (Color32::TRANSPARENT, pal.text),
        };
        p.rect_filled(button.shrink(1.0), right, bfill);
        if editing || !enabled {
            p.vline(
                button.left(),
                button.y_range().shrink(7.0),
                Stroke::new(1.0, stroke.color),
            );
        }
        let mid = rect.center().y;
        let dot = egui::pos2(info.left() + PAD + 4.0, mid);
        if editing {
            p.circle_filled(dot, 6.0, pal.accent.gamma_multiply(0.22));
        }
        p.circle_filled(dot, 3.5, if editing { pal.accent } else { pal.muted });
        let mut x = dot.x + 4.0 + 8.0;
        let name_w = name_g.size().x;
        p.galley(egui::pos2(x, mid - name_g.size().y / 2.0), name_g, pal.text);
        x += name_w + 8.0;
        p.galley(
            egui::pos2(x, mid - status_g.size().y / 2.0),
            status_g,
            status_color,
        );
        p.galley_with_override_text_color(
            egui::pos2(
                button.center().x - action_g.size().x / 2.0,
                mid - action_g.size().y / 2.0,
            ),
            action_g,
            bfg,
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, action));
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp.on_hover_text("Editing isn't available right now")
    }
}

/// A square, borderless button showing a Phosphor icon; `label` is its
/// tooltip and accessible name.
pub fn icon_button(ui: &mut Ui, pal: &Palette, glyph: &str, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(8), pal.border);
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(18.0),
            pal.text,
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    resp.on_hover_text(label)
}

/// A sidebar navigation row; the active row gets an accent-tinted background.
/// When `compact`, only the icon is drawn (the label stays the accessible name).
pub fn nav_item(
    ui: &mut Ui,
    pal: &Palette,
    glyph: &str,
    label: &str,
    active: bool,
    compact: bool,
) -> egui::Response {
    let width = if compact { 36.0 } else { ui.available_width() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if active {
            p.rect_filled(rect, CornerRadius::same(8), pal.accent.gamma_multiply(0.16));
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(8), pal.border);
        }
        let color = if active { pal.accent } else { pal.text };
        let icon_rect = Rect::from_min_size(rect.min, Vec2::new(36.0, rect.height()));
        p.text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(18.0),
            color,
        );
        if !compact {
            let pos = egui::pos2(icon_rect.right() + 2.0, rect.center().y);
            p.text(
                pos,
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(14.5),
                pal.text,
            );
        }
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, label));
    if compact {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Data, SECTION_COLORS};

    fn data() -> Data {
        Data::new_empty("t", crate::dates::now_ts())
    }

    #[test]
    fn date_picker_chooses_day_month_and_year() {
        use egui_kittest::kittest::Queryable;
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(500.0, 500.0))
            .build_ui_state(
                |ui, (styled, d): &mut (bool, chrono::NaiveDate)| {
                    if !*styled {
                        // The serif face only exists from the next pass.
                        crate::ui::theme::install_fonts(ui.ctx());
                        *styled = true;
                        return;
                    }
                    date_picker(ui, d, "t");
                },
                (false, start),
            );
        h.run_steps(3);
        h.get_by_label_contains("31 Jan 2026").click();
        h.run_steps(3);
        h.get_by_label("15 Jan 2026").click();
        h.run_steps(3);
        assert_eq!(h.state().1.to_string(), "2026-01-15");
        h.get_by_label("Month: January").click();
        h.run_steps(3);
        h.get_by_label("February").click();
        h.run_steps(3);
        h.get_by_label("Year: 2026").click();
        h.run_steps(3);
        h.get_by_label("2024").click();
        h.run_steps(3);
        assert_eq!(h.state().1.to_string(), "2024-02-15");
        h.get_by_label("Done").click();
        h.run_steps(3);
        assert!(h.query_by_label("Done").is_none());
    }

    #[test]
    fn a_shorter_month_pulls_the_day_back() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(shift_months(d, 1).to_string(), "2026-02-28");
        assert_eq!(shift_months(d, -1).to_string(), "2025-12-31");
        assert_eq!(with_month(d, 2024, 2).to_string(), "2024-02-29");
    }

    #[test]
    fn section_color_by_position() {
        let d = data();
        let id = d.sections_sorted()[1].id.clone();
        assert_eq!(section_color(&d, &id), Some(rgb(SECTION_COLORS[1])));
    }

    #[test]
    fn section_color_custom_and_unknown() {
        let mut d = data();
        d.sections[0].color = Some("#112233".into());
        let id = d.sections[0].id.clone();
        assert_eq!(section_color(&d, &id), Some(rgb(0x112233)));
        assert_eq!(section_color(&d, "nope"), None);
    }
}
