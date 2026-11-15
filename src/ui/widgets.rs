//! Reusable widgets: due pill, stat card, chip dropdown, search box, empty
//! state, plus the icon button and nav row used by the shell.

use egui::{
    Align2, Color32, CornerRadius, FontId, Popup, Rect, RichText, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
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

/// egui_extras' date picker (which works on `jiff` dates) for a chrono date.
pub fn date_picker(ui: &mut Ui, date: &mut chrono::NaiveDate, salt: &str) {
    use chrono::Datelike;
    let Ok(mut d) =
        jiff::civil::Date::new(date.year() as i16, date.month() as i8, date.day() as i8)
    else {
        return;
    };
    ui.add(
        egui_extras::DatePickerButton::new(&mut d)
            .id_salt(salt)
            .format("%-d %b %Y")
            .calendar_week(false),
    );
    if let Some(back) =
        chrono::NaiveDate::from_ymd_opt(d.year().into(), d.month() as u32, d.day() as u32)
    {
        *date = back;
    }
}

/// A single-line search field with a magnifying-glass hint.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str) -> egui::Response {
    ui.add(
        TextEdit::singleline(text)
            .hint_text(format!("{}  {hint}", icon::MAGNIFYING_GLASS))
            .margin(egui::vec2(10.0, 6.0))
            .desired_width(f32::INFINITY),
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
    egui::Frame::new()
        .fill(pal.sidebar)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                for (i, label) in labels.iter().enumerate() {
                    if choice(ui, pal, i == current, label, false).clicked() {
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
