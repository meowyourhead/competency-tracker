//! Personnel: the master staff list (filters, status, row menu), the per-person
//! profile and the permanent-delete confirmation. The add / edit dialog is in
//! `staff_editor.rs`.

use std::collections::HashMap;

use chrono::NaiveDate;
use egui::{Align, Button, Frame, Layout, Margin, Modal, Popup, RichText, Stroke, TextEdit};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Staff, StaffStatus};
use crate::ops::{self, OpError};
use crate::query::{distinct_titles, filter_staff, Index, StaffFilter};
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::staff_editor::{self, StaffForm};
use crate::ui::theme::{heading, medium, serif, Palette};
use crate::ui::widgets::{chip_select, due_pill, empty_state, icon_button, search_box};

#[derive(Default)]
pub(crate) struct PersonnelState {
    pub filter: StaffFilter,
    /// The add / edit dialog, while open.
    pub editing: Option<StaffForm>,
    /// Person whose profile is open (also set by search navigation).
    pub profile: Option<Id>,
    delete: Option<DeleteConfirm>,
    /// Per-person summaries for (revision, date).
    cache: Option<(u64, NaiveDate, HashMap<Id, Summary>)>,
}

/// What the list shows for one person besides their details.
struct Summary {
    /// Sections with at least one assigned test.
    sections: Vec<String>,
    /// The earliest due pill across their assigned tests.
    next: Option<(DueInfo, DueStatus)>,
}

struct DeleteConfirm {
    id: Id,
    name: String,
    text: String,
    error: Option<String>,
    fresh: bool,
}

/// What the user chose from a row menu.
enum RowAction {
    Edit(Id),
    Status(Id, StaffStatus),
    AskDelete(Id),
}

impl PersonnelState {
    /// Rebuilds the summaries only when the data revision or the date changed.
    fn refresh(&mut self, session: &Session, today: NaiveDate) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, d, _)) if *r == rev && *d == today) {
            return;
        }
        self.cache = Some((rev, today, summaries(session.data(), today)));
    }
}

fn is_assigned(s: &Staff, test_id: &str) -> bool {
    s.assignments.get(test_id).is_some_and(|a| a.state == AssignState::Assigned)
}

fn summaries(data: &Data, today: NaiveDate) -> HashMap<Id, Summary> {
    let idx = Index::build(data);
    let sections = data.sections_sorted();
    data.staff
        .iter()
        .map(|s| {
            let mut names = Vec::new();
            let mut next: Option<(DueInfo, DueStatus)> = None;
            for sec in sections.iter().filter(|x| !x.retired) {
                let mut any = false;
                for t in sec.tests.iter().filter(|t| !t.retired && is_assigned(s, &t.id)) {
                    any = true;
                    let info = next_due(data.rule_for_test(&t.id), &idx.pair(&s.id, &t.id), &data.settings);
                    if info.due_on.is_some() && next.as_ref().is_none_or(|(n, _)| info.due_on < n.due_on) {
                        let status = classify(info.due_on, today, data.settings.due_soon_days);
                        next = Some((info, status));
                    }
                }
                if any {
                    names.push(sec.name.clone());
                }
            }
            (s.id.clone(), Summary { sections: names, next })
        })
        .collect()
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    if state.profile.as_deref().is_some_and(|id| cx.session.data().staff(id).is_none()) {
        state.profile = None;
    }
    if state.profile.is_some() {
        profile(ui, cx, state);
    } else {
        list(ui, cx, state);
    }
    if let Some(form) = &mut state.editing {
        if staff_editor::show(ui.ctx(), cx, form) {
            state.editing = None;
        }
    }
    delete_modal(ui.ctx(), cx, state);
}

/// Runs a status change; a toast reports the outcome.
fn set_status(cx: &mut Ctx, id: &str, status: StaffStatus) {
    let name = cx.session.data().staff(id).map(|s| s.name.clone()).unwrap_or_default();
    let res = cx.session.mutate(cx.now, |d, a| ops::set_staff_status(d, a, id, status));
    let msg = match (res, status) {
        (Some(Ok(())), StaffStatus::Active) => format!("{name} is active again"),
        (Some(Ok(())), StaffStatus::Hidden) => format!("{name} hidden"),
        (Some(Ok(())), StaffStatus::Removed) => format!("{name} removed"),
        (Some(Err(e)), _) => format!("Couldn't update {name}: {e}"),
        (None, _) => "Start editing to make changes".to_owned(),
    };
    cx.toasts.push(Toast::new(msg));
}

fn list(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    let editing = cx.session.is_editing();
    let pal = cx.pal;
    ui.horizontal(|ui| {
        heading(ui, "Personnel");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let add = ui.add_enabled(editing, Button::new(format!("{}  Add staff", icon::PLUS)));
            if add.on_disabled_hover_text("Start editing to make changes").clicked() {
                state.editing = Some(StaffForm::new());
            }
        });
    });
    ui.add_space(12.0);
    state.refresh(cx.session, cx.today);
    let PersonnelState { filter, cache, .. } = state;
    let sums = &cache.as_ref().expect("refreshed above").2;
    let data = cx.session.data();
    filters(ui, pal, data, filter);
    ui.add_space(12.0);
    let staff = filter_staff(data, filter);
    if staff.is_empty() {
        let narrowed = filter.section.is_some() || filter.title.is_some() || !filter.text.trim().is_empty();
        let (title, body) = match (data.staff.is_empty(), narrowed) {
            (true, _) => ("No staff yet", "Add staff here, or import your Excel tracker from Settings."),
            (_, true) => ("No matches", "Nobody matches these filters."),
            _ => ("Nobody here", "No staff have this status."),
        };
        empty_state(ui, title, body);
        return;
    }
    let mut open = None;
    let mut action = None;
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("staff_table").num_columns(6).spacing([24.0, 10.0]).show(ui, |ui| {
                for head in ["Name", "Title", "Joined", "Sections", "Next due", ""] {
                    ui.label(RichText::new(head).color(pal.muted).size(12.5));
                }
                ui.end_row();
                for s in &staff {
                    let name = Button::new(RichText::new(&s.name).family(medium()).color(pal.text)).frame(false);
                    if ui.add(name).clicked() {
                        open = Some(s.id.clone());
                    }
                    ui.label(RichText::new(&s.title).color(pal.muted));
                    ui.label(RichText::new(s.joined.map_or("\u{2014}".to_owned(), fmt_date)).color(pal.muted));
                    let sum = sums.get(&s.id);
                    ui.horizontal(|ui| section_chips(ui, pal, sum.map_or(&[], |m| m.sections.as_slice())));
                    match sum.and_then(|m| m.next.as_ref()) {
                        Some((info, status)) => {
                            due_pill(ui, pal, info, *status, false);
                        }
                        None => {
                            ui.label(RichText::new("\u{2014}").color(pal.muted));
                        }
                    }
                    if let Some(a) = row_menu(ui, pal, editing, s) {
                        action = Some(a);
                    }
                    ui.end_row();
                }
            });
        });
    if let Some(id) = open {
        state.profile = Some(id);
    }
    match action {
        Some(RowAction::Edit(id)) => state.editing = data.staff(&id).map(StaffForm::edit),
        Some(RowAction::Status(id, st)) => set_status(cx, &id, st),
        Some(RowAction::AskDelete(id)) => {
            let name = data.staff(&id).map(|s| s.name.clone()).unwrap_or_default();
            state.delete = Some(DeleteConfirm { id, name, text: String::new(), error: None, fresh: true });
        }
        None => {}
    }
}

/// Status segmented control, then section and title chips and the search box.
fn filters(ui: &mut egui::Ui, pal: &Palette, data: &Data, f: &mut StaffFilter) {
    search_box(ui, &mut f.text, "Search by name");
    ui.add_space(4.0);
    let sections: Vec<(Id, String)> =
        data.sections_sorted().into_iter().filter(|s| !s.retired).map(|s| (s.id.clone(), s.name.clone())).collect();
    let titles: Vec<(String, String)> = distinct_titles(data).into_iter().map(|t| (t.clone(), t)).collect();
    ui.horizontal(|ui| {
        Frame::new()
            .fill(pal.sidebar)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(10)
            .inner_margin(Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let current = f.status.unwrap_or(StaffStatus::Active);
                    for (st, label) in
                        [(StaffStatus::Active, "Active"), (StaffStatus::Hidden, "Hidden"), (StaffStatus::Removed, "Removed")]
                    {
                        if ui.add(Button::selectable(current == st, label).corner_radius(8)).clicked() {
                            f.status = Some(st);
                        }
                    }
                });
            });
        chip_select(ui, "Section", &mut f.section, &sections);
        chip_select(ui, "Title", &mut f.title, &titles);
        let any = f.section.is_some() || f.title.is_some() || !f.text.trim().is_empty();
        if any && ui.add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false)).clicked() {
            f.section = None;
            f.title = None;
            f.text.clear();
        }
    });
}

/// Up to three section chips, then "+N".
fn section_chips(ui: &mut egui::Ui, pal: &Palette, names: &[String]) {
    if names.is_empty() {
        ui.label(RichText::new("\u{2014}").color(pal.muted));
    }
    let chip = |ui: &mut egui::Ui, text: &str| {
        Frame::new().fill(pal.grey_bg).corner_radius(255).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(pal.grey_fg));
        });
    };
    for n in names.iter().take(3) {
        chip(ui, n);
    }
    if names.len() > 3 {
        chip(ui, &format!("+{}", names.len() - 3));
    }
}

/// The "..." menu of a row. Every item needs edit mode.
fn row_menu(ui: &mut egui::Ui, pal: &Palette, editing: bool, s: &Staff) -> Option<RowAction> {
    let resp = icon_button(ui, pal, icon::DOTS_THREE, &format!("Actions for {}", s.name));
    let mut action = None;
    Popup::menu(&resp).show(|ui| {
        let item =|ui: &mut egui::Ui, label: &str| ui.add_enabled(editing, Button::new(label)).clicked();
        if item(ui, "Edit") {
            action = Some(RowAction::Edit(s.id.clone()));
        }
        let status = |st| Some(RowAction::Status(s.id.clone(), st));
        match s.status {
            StaffStatus::Active => {
                if item(ui, "Hide") {
                    action = status(StaffStatus::Hidden);
                }
                if item(ui, "Remove") {
                    action = status(StaffStatus::Removed);
                }
            }
            StaffStatus::Hidden => {
                if item(ui, "Unhide") {
                    action = status(StaffStatus::Active);
                }
                if item(ui, "Remove") {
                    action = status(StaffStatus::Removed);
                }
            }
            StaffStatus::Removed => {
                if item(ui, "Restore") {
                    action = status(StaffStatus::Active);
                }
                if item(ui, "Delete permanently") {
                    action = Some(RowAction::AskDelete(s.id.clone()));
                }
            }
        }
        if !editing {
            ui.label(RichText::new("Start editing to make changes.").color(pal.muted).size(12.5));
        }
    });
    action
}

/// "Type {name} to confirm"; Delete unlocks only on an exact (trimmed) match.
fn delete_modal(ctx: &egui::Context, cx: &mut Ctx, state: &mut PersonnelState) {
    let Some(d) = &mut state.delete else { return };
    let editing = cx.session.is_editing();
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let (mut delete, mut cancel) = (false, false);
    let resp = Modal::new(egui::Id::new("staff_delete")).frame(frame).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new("Delete permanently").family(serif()).size(20.0));
        ui.add_space(6.0);
        ui.label(format!("This deletes {} and all their records. It can't be undone.", d.name));
        ui.add_space(8.0);
        ui.label(format!("Type {} to confirm", d.name));
        let field = ui.add(TextEdit::singleline(&mut d.text).desired_width(f32::INFINITY));
        if d.fresh {
            field.request_focus();
            d.fresh = false;
        }
        if let Some(e) = &d.error {
            ui.colored_label(cx.pal.red_fg, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let matches = d.text.trim() == d.name;
            let button = Button::new(RichText::new("Delete").color(cx.pal.red_fg));
            delete = ui.add_enabled(editing && matches, button).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if cancel || resp.should_close() {
        state.delete = None;
    } else if delete {
        let (id, text, name) = (d.id.clone(), d.text.clone(), d.name.clone());
        match cx.session.mutate(cx.now, |data, a| ops::delete_staff_permanently(data, a, &id, &text)) {
            Some(Ok(())) => {
                state.delete = None;
                cx.toasts.push(Toast::new(format!("{name} deleted")));
            }
            Some(Err(OpError::ConfirmMismatch)) => d.error = Some("That doesn't match the name.".to_owned()),
            Some(Err(OpError::NotRemoved)) => d.error = Some("Only removed staff can be deleted permanently.".to_owned()),
            Some(Err(e)) => d.error = Some(format!("Couldn't delete: {e}")),
            None => d.error = Some("Start editing to make changes".to_owned()),
        }
    }
}

/// One person: header, then a card per section listing their tests with pills.
fn profile(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let data = cx.session.data();
    let Some(staff) = state.profile.as_deref().and_then(|id| data.staff(id)) else { return };
    if ui.add(Button::new(RichText::new("\u{2190} All staff").color(pal.muted)).frame(false)).clicked() {
        state.profile = None;
        return;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        heading(ui, &staff.name);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let edit = ui.add_enabled(editing, Button::new(format!("{}  Edit", icon::PENCIL_SIMPLE)));
            if edit.on_disabled_hover_text("Start editing to make changes").clicked() {
                state.editing = Some(StaffForm::edit(staff));
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
    ui.label(RichText::new(bits.join(" \u{b7} ")).color(pal.muted));
    ui.add_space(14.0);

    let idx = Index::build(data);
    let mut picked = None;
    let mut any = false;
    for sec in data.sections_sorted().into_iter().filter(|s| !s.retired) {
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired && staff.assignments.contains_key(&t.id)).collect();
        if tests.is_empty() {
            continue;
        }
        any = true;
        tests.sort_by_key(|t| t.order);
        Frame::new()
            .fill(pal.card)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&sec.name).strong().size(15.5).color(pal.text));
                for t in tests {
                    ui.horizontal(|ui| {
                        let name = Button::new(RichText::new(&t.name).color(pal.text)).frame(false);
                        if ui.add(name).clicked() {
                            picked = Some((staff.id.clone(), t.id.clone()));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match staff.assignments[&t.id].state {
                            AssignState::Na => {
                                ui.label(RichText::new("N/A").color(pal.muted));
                            }
                            AssignState::Assigned => {
                                let info = next_due(data.rule_for_test(&t.id), &idx.pair(&staff.id, &t.id), &data.settings);
                                let status = classify(info.due_on, cx.today, data.settings.due_soon_days);
                                let delayed =
                                    idx.delay_notes(&staff.id, &t.id).iter().any(|n| n.for_stage_index == info.next_stage_index);
                                if due_pill(ui, pal, &info, status, delayed).clicked() {
                                    picked = Some((staff.id.clone(), t.id.clone()));
                                }
                            }
                        });
                    });
                }
            });
        ui.add_space(12.0);
    }
    if !any {
        empty_state(ui, "No assignments", "Edit this person to assign test systems.");
    }
    if picked.is_some() {
        *cx.open_detail = picked;
    }
}
