//! Audit log: every change, newest first, as a virtualised list of
//! "timestamp · user" over the description. Filters: user, date range,
//! section, staff, action type, and a search over the descriptions.

use chrono::NaiveDate;
use egui::{Button, Frame, Label, Margin, RichText, ScrollArea, Stroke, Vec2};
use egui_phosphor::regular as icon;

use crate::model::{AuditEntry, Data, Id};
use crate::query::{distinct_audit_users, filter_audit, AuditFilter};
use crate::session::Session;
use crate::ui::app::Ctx;
use crate::ui::theme::{heading, Palette};
use crate::ui::widgets::{chip_select, date_picker, empty_state, search_box};

/// Height of one list row (two lines of text).
const ROW_H: f32 = 44.0;

/// (chip label, action prefix) for the action-type filter.
const ACTIONS: [(&str, &str); 12] = [
    ("Record", "record."),
    ("Delay", "delay."),
    ("Skip", "stage."),
    ("Due date", "due."),
    ("Staff", "staff."),
    ("Assignment", "assignment."),
    ("Section", "section."),
    ("Test", "test."),
    ("Schedule rule", "rule."),
    ("Settings", "settings."),
    ("Import", "import."),
    ("Lock", "lock."),
];

#[derive(Default)]
pub(crate) struct AuditState {
    pub filter: AuditFilter,
    /// Matching entries for (revision, filter).
    cache: Option<(u64, FilterKey, Vec<AuditEntry>)>,
}

type FilterKey = (Option<String>, Option<NaiveDate>, Option<NaiveDate>, Option<Id>, Option<Id>, Option<String>, String);

fn key(f: &AuditFilter) -> FilterKey {
    (f.user.clone(), f.from, f.to, f.section.clone(), f.staff.clone(), f.action_prefix.clone(), f.text.trim().to_owned())
}

impl AuditState {
    /// Re-filters only when the data revision or the filter changed.
    fn refresh(&mut self, session: &Session) {
        let (rev, k) = (session.revision(), key(&self.filter));
        if matches!(&self.cache, Some((r, fk, _)) if *r == rev && *fk == k) {
            return;
        }
        let entries = filter_audit(session.data(), &self.filter).into_iter().cloned().collect();
        self.cache = Some((rev, k, entries));
    }
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut AuditState) {
    let pal = cx.pal;
    let data = cx.session.data();
    heading(ui, "Audit log");
    ui.label(RichText::new(format!("{} recorded changes", data.audit.len())).color(pal.muted));
    ui.add_space(12.0);
    filters(ui, pal, data, &mut state.filter, cx.today);
    ui.add_space(12.0);
    state.refresh(cx.session);
    let entries = &state.cache.as_ref().expect("refreshed above").2;
    if entries.is_empty() {
        let (title, body) = if data.audit.is_empty() {
            ("Nothing here yet", "Changes appear here as soon as anyone edits the data.")
        } else {
            ("No matches", "No changes match these filters.")
        };
        empty_state(ui, title, body);
        return;
    }
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let height = (ui.available_height() - 24.0).max(240.0);
            ScrollArea::vertical().id_salt("audit_rows").max_height(height).auto_shrink([false, true]).show_rows(
                ui,
                ROW_H,
                entries.len(),
                |ui, range| {
                    for e in &entries[range] {
                        row(ui, pal, e);
                    }
                },
            );
        });
}

fn row(ui: &mut egui::Ui, pal: &Palette, e: &AuditEntry) {
    let width = ui.available_width();
    ui.allocate_ui(Vec2::new(width, ROW_H), |ui| {
        ui.set_min_size(Vec2::new(width, ROW_H));
        ui.spacing_mut().item_spacing.y = 2.0;
        let when = e.at.format("%-d %b %Y %H:%M");
        ui.add(Label::new(RichText::new(format!("{when} \u{b7} {}", e.by)).size(12.0).color(pal.muted)).truncate());
        ui.add(Label::new(RichText::new(&e.detail).color(pal.text)).truncate());
    });
}

fn filters(ui: &mut egui::Ui, pal: &Palette, data: &Data, f: &mut AuditFilter, today: NaiveDate) {
    search_box(ui, &mut f.text, "Search descriptions");
    ui.add_space(4.0);
    let users: Vec<(String, String)> = distinct_audit_users(data).into_iter().map(|u| (u.clone(), u)).collect();
    let sections: Vec<(Id, String)> = data.sections_sorted().into_iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    let mut staff: Vec<(Id, String)> = data.staff.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    staff.sort_by_key(|(_, n)| n.to_lowercase());
    let actions: Vec<(String, String)> = ACTIONS.iter().map(|(l, p)| ((*p).to_owned(), (*l).to_owned())).collect();
    ui.horizontal_wrapped(|ui| {
        chip_select(ui, "User", &mut f.user, &users);
        chip_select(ui, "Section", &mut f.section, &sections);
        chip_select(ui, "Staff", &mut f.staff, &staff);
        chip_select(ui, "Action", &mut f.action_prefix, &actions);
        date_bound(ui, pal, "From", &mut f.from, today, "audit_from");
        date_bound(ui, pal, "To", &mut f.to, today, "audit_to");
        let any = f.user.is_some()
            || f.section.is_some()
            || f.staff.is_some()
            || f.action_prefix.is_some()
            || f.from.is_some()
            || f.to.is_some()
            || !f.text.trim().is_empty();
        if any && ui.add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false)).clicked() {
            *f = AuditFilter::default();
        }
    });
}

/// An optional date bound: a chip that turns into a date picker plus a clear button.
fn date_bound(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &mut Option<NaiveDate>, today: NaiveDate, salt: &str) {
    match value {
        Some(d) => {
            ui.label(RichText::new(label).color(pal.muted));
            date_picker(ui, d, salt);
            if ui.add(Button::new(icon::X).frame(false)).on_hover_text(format!("Clear {} date", label.to_lowercase())).clicked() {
                *value = None;
            }
        }
        None => {
            if ui.add(Button::new(format!("{label} date")).corner_radius(255)).clicked() {
                *value = Some(today);
            }
        }
    }
}
