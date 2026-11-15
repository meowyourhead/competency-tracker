//! Personnel: the master staff list (filters, status, row menu), the per-person
//! profile and the permanent-delete confirmation. The add / edit dialog is in
//! `staff_editor.rs`.

use std::collections::{BTreeSet, HashMap};

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
use crate::ui::widgets::{
    chip_select, due_pill, empty_state, icon_button, search_box, segmented, SEARCH_W,
};

#[derive(Default)]
pub(crate) struct PersonnelState {
    pub filter: StaffFilter,
    /// The add / edit dialog, while open.
    pub editing: Option<StaffForm>,
    /// Person whose profile is open (also set by search navigation).
    pub profile: Option<Id>,
    delete: Option<DeleteConfirm>,
    /// People ticked in the list, for batch actions.
    selected: BTreeSet<Id>,
    batch_delete: Option<BatchDelete>,
    merge: Option<MergeConfirm>,
    /// Per-person summaries for (revision, date).
    cache: Option<(u64, NaiveDate, HashMap<Id, Summary>)>,
}

/// What the list shows for one person besides their details.
struct Summary {
    /// Sections with at least one assigned test.
    sections: Vec<String>,
    /// The earliest due pill across their assigned tests.
    next: Option<(DueInfo, DueStatus)>,
    /// Every assigned test's pill: (test id, due, status, delayed).
    tests: Vec<(Id, DueInfo, DueStatus, bool)>,
}

struct DeleteConfirm {
    id: Id,
    name: String,
    text: String,
    error: Option<String>,
    fresh: bool,
}

/// "Type delete to confirm" for several removed people at once.
struct BatchDelete {
    ids: Vec<Id>,
    names: Vec<String>,
    text: String,
    error: Option<String>,
    fresh: bool,
}

/// Merging two records of the same person: which one's name and details
/// to keep (`keep` indexes `ids`).
struct MergeConfirm {
    ids: [Id; 2],
    keep: usize,
    error: Option<String>,
}

/// What the user chose from the batch bar.
enum Batch {
    Status(StaffStatus),
    /// Assign (true) or unassign every test of this section.
    Section(Id, bool),
    AskDelete,
    AskMerge,
    Clear,
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
    s.assignments
        .get(test_id)
        .is_some_and(|a| a.state == AssignState::Assigned)
}

fn summaries(data: &Data, today: NaiveDate) -> HashMap<Id, Summary> {
    let idx = Index::build(data);
    let sections = data.sections_sorted();
    data.staff
        .iter()
        .map(|s| {
            let mut names = Vec::new();
            let mut next: Option<(DueInfo, DueStatus)> = None;
            let mut tests = Vec::new();
            for sec in sections.iter().filter(|x| !x.retired) {
                let mut any = false;
                for t in sec
                    .tests
                    .iter()
                    .filter(|t| !t.retired && is_assigned(s, &t.id))
                {
                    any = true;
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
                    tests.push((t.id.clone(), info.clone(), status, delayed));
                    if info.due_on.is_some()
                        && next.as_ref().is_none_or(|(n, _)| info.due_on < n.due_on)
                    {
                        next = Some((info, status));
                    }
                }
                if any {
                    names.push(sec.name.clone());
                }
            }
            (
                s.id.clone(),
                Summary {
                    sections: names,
                    next,
                    tests,
                },
            )
        })
        .collect()
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    if state
        .profile
        .as_deref()
        .is_some_and(|id| cx.session.data().staff(id).is_none())
    {
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
    batch_delete_modal(ui.ctx(), cx, state);
    merge_modal(ui.ctx(), cx, state);
}

/// "Merge 2 people": pick whose name and details to keep, see what each
/// brings, then merge. Everything recorded for either ends up on the kept one.
fn merge_modal(ctx: &egui::Context, cx: &mut Ctx, state: &mut PersonnelState) {
    let Some(m) = &mut state.merge else { return };
    let data = cx.session.data();
    let (Some(a), Some(b)) = (data.staff(&m.ids[0]), data.staff(&m.ids[1])) else {
        state.merge = None;
        return;
    };
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let people = [a, b];
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let (mut merge, mut cancel) = (false, false);
    let resp = Modal::new(egui::Id::new("staff_merge"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(480.0);
            ui.label(RichText::new("Merge 2 people").family(serif()).size(20.0));
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Use this when the same person was added twice. Their sections, \
                     assignments, completions, notes and history all move to the one you keep; \
                     the other is deleted.",
                )
                .color(pal.muted),
            );
            ui.add_space(12.0);
            ui.label(RichText::new("Keep the name and details of").family(medium()));
            ui.add_space(4.0);
            for (i, s) in people.iter().enumerate() {
                let records = data.records.iter().filter(|r| r.staff_id == s.id).count();
                let tests = s.assignments.len();
                let selected = m.keep == i;
                let stroke = if selected {
                    Stroke::new(1.5, pal.accent)
                } else {
                    Stroke::new(1.0, pal.border)
                };
                let fill = if selected {
                    pal.accent.gamma_multiply(0.08)
                } else {
                    pal.card
                };
                let card = Frame::new()
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            // Clicks land on the whole card (below).
                            let _ = ui.radio(selected, "");
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&s.name).family(medium()).color(pal.text));
                                let mut bits = Vec::new();
                                if !s.title.is_empty() {
                                    bits.push(s.title.clone());
                                }
                                if let Some(j) = s.joined {
                                    bits.push(format!("Joined {}", fmt_date(j)));
                                }
                                let plural = |n: usize, one: &str, many: &str| {
                                    if n == 1 {
                                        format!("1 {one}")
                                    } else {
                                        format!("{n} {many}")
                                    }
                                };
                                bits.push(plural(tests, "test system", "test systems"));
                                bits.push(plural(records, "completion", "completions"));
                                ui.label(RichText::new(bits.join(", ")).color(pal.muted));
                            });
                        });
                    });
                let click = ui.interact(
                    card.response.rect,
                    egui::Id::new(("merge_keep", i)),
                    egui::Sense::click(),
                );
                click.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::RadioButton,
                        true,
                        selected,
                        format!("Keep {}", s.name),
                    )
                });
                if click.clicked() {
                    m.keep = i;
                }
                ui.add_space(6.0);
            }
            if let Some(e) = &m.error {
                ui.colored_label(pal.red_fg, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let into = format!("Merge into {}", people[m.keep].name);
                let button =
                    Button::new(RichText::new(into).color(egui::Color32::WHITE)).fill(pal.accent);
                merge = ui
                    .add_enabled(editing, button)
                    .on_disabled_hover_text("Start editing to make changes")
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if cancel || resp.should_close() {
        state.merge = None;
        return;
    }
    if !merge {
        return;
    }
    let keep = m.ids[m.keep].clone();
    let gone = m.ids[1 - m.keep].clone();
    let (keep_name, gone_name) = (people[m.keep].name.clone(), people[1 - m.keep].name.clone());
    match cx
        .session
        .mutate(cx.now, |d, a| ops::merge_staff(d, a, &keep, &gone))
    {
        Some(Ok(_)) => {
            state.merge = None;
            state.selected.clear();
            if state.profile.as_deref() == Some(gone.as_str()) {
                state.profile = Some(keep);
            }
            cx.toasts
                .push(Toast::new(format!("Merged {gone_name} into {keep_name}")));
        }
        Some(Err(e)) => m.error = Some(format!("Couldn't merge: {e}")),
        None => m.error = Some("Start editing to make changes".to_owned()),
    }
}

/// The word typed to confirm a batch delete.
const DELETE_WORD: &str = "delete";

/// Permanently deletes several removed people after "delete" is typed.
fn batch_delete_modal(ctx: &egui::Context, cx: &mut Ctx, state: &mut PersonnelState) {
    let Some(d) = &mut state.batch_delete else {
        return;
    };
    let editing = cx.session.is_editing();
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let (mut delete, mut cancel) = (false, false);
    let n = people(d.ids.len());
    let resp = Modal::new(egui::Id::new("staff_batch_delete"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(
                RichText::new(format!("Delete {n} permanently"))
                    .family(serif())
                    .size(20.0),
            );
            ui.add_space(6.0);
            let mut shown: Vec<&str> = d.names.iter().take(5).map(String::as_str).collect();
            let more = d.names.len().saturating_sub(5);
            let extra = format!("{more} more");
            if more > 0 {
                shown.push(&extra);
            }
            ui.label(RichText::new(shown.join(", ")).color(cx.pal.muted));
            ui.add_space(6.0);
            ui.label("This deletes them and all their records. It can't be undone.");
            ui.add_space(8.0);
            ui.label(format!("Type {DELETE_WORD} to confirm"));
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
                let matches = d.text.trim().eq_ignore_ascii_case(DELETE_WORD);
                let button = Button::new(RichText::new(format!("Delete {n}")).color(cx.pal.red_fg));
                delete = ui.add_enabled(editing && matches, button).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if cancel || resp.should_close() {
        state.batch_delete = None;
        return;
    }
    if !delete {
        return;
    }
    let ids = d.ids.clone();
    let res = cx.session.mutate(cx.now, |data, a| {
        ids.iter().try_for_each(|id| {
            let name = data.staff(id).map(|s| s.name.clone()).unwrap_or_default();
            ops::delete_staff_permanently(data, a, id, &name)
        })
    });
    match res {
        Some(Ok(())) => {
            state.batch_delete = None;
            state.selected.clear();
            cx.toasts.push(Toast::new(format!("Deleted {n}")));
        }
        Some(Err(OpError::NotRemoved)) => {
            d.error = Some("Only removed staff can be deleted permanently.".to_owned())
        }
        Some(Err(e)) => d.error = Some(format!("Couldn't delete: {e}")),
        None => d.error = Some("Start editing to make changes".to_owned()),
    }
}

/// Runs a status change; a toast reports the outcome.
fn set_status(cx: &mut Ctx, id: &str, status: StaffStatus) {
    let name = cx
        .session
        .data()
        .staff(id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let res = cx
        .session
        .mutate(cx.now, |d, a| ops::set_staff_status(d, a, id, status));
    let msg = match (res, status) {
        (Some(Ok(())), StaffStatus::Active) => format!("{name} is active again"),
        (Some(Ok(())), StaffStatus::Hidden) => format!("{name} hidden"),
        (Some(Ok(())), StaffStatus::Removed) => format!("{name} removed"),
        (Some(Err(e)), _) => format!("Couldn't update {name}: {e}"),
        (None, _) => "Start editing to make changes".to_owned(),
    };
    cx.toasts.push(Toast::new(msg));
}

/// Runs one status change for every selected person; one toast sums it up.
fn batch_status(cx: &mut Ctx, ids: &[Id], status: StaffStatus) {
    let res = cx.session.mutate(cx.now, |d, a| {
        ids.iter()
            .try_for_each(|id| ops::set_staff_status(d, a, id, status))
    });
    let n = people(ids.len());
    let msg = match (res, status) {
        (Some(Ok(())), StaffStatus::Removed) => format!("Removed {n}"),
        (Some(Ok(())), _) => format!("Restored {n}"),
        (Some(Err(e)), _) => format!("Couldn't update {n}: {e}"),
        (None, _) => "Start editing to make changes".to_owned(),
    };
    cx.toasts.push(Toast::new(msg));
}

/// Assigns (or unassigns) every test of a section for every selected person.
fn batch_section(cx: &mut Ctx, ids: &[Id], section: &str, assigned: bool) {
    let sec_name = cx
        .session
        .data()
        .section(section)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let res = cx.session.mutate(cx.now, |d, a| {
        ids.iter()
            .try_for_each(|id| ops::assign_section(d, a, id, section, assigned))
    });
    let n = people(ids.len());
    let msg = match res {
        Some(Ok(())) if assigned => format!("Assigned {sec_name} to {n}"),
        Some(Ok(())) => format!("Unassigned {sec_name} from {n}"),
        Some(Err(e)) => format!("Couldn't update {n}: {e}"),
        None => "Start editing to make changes".to_owned(),
    };
    cx.toasts.push(Toast::new(msg));
}

/// "1 person" / "3 people".
fn people(n: usize) -> String {
    if n == 1 {
        "1 person".to_owned()
    } else {
        format!("{n} people")
    }
}

fn list(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    let editing = cx.session.is_editing();
    let pal = cx.pal;
    state.refresh(cx.session, cx.today);
    let PersonnelState {
        filter,
        cache,
        selected,
        ..
    } = state;
    let sums = &cache.as_ref().expect("refreshed above").2;
    let data = cx.session.data();
    let mut add_staff = false;
    filters(ui, pal, data, filter, editing, &mut add_staff);
    if add_staff {
        state.editing = Some(StaffForm::new());
        return;
    }
    ui.add_space(12.0);
    let staff = filter_staff(data, filter);
    // Batch actions only ever touch people the list is showing.
    selected.retain(|id| staff.iter().any(|s| s.id == *id));
    let tab = filter.status.unwrap_or(StaffStatus::Active);
    if staff.is_empty() {
        let narrowed =
            filter.section.is_some() || filter.title.is_some() || !filter.text.trim().is_empty();
        let (title, body) = match (data.staff.is_empty(), narrowed) {
            (true, _) => (
                "No staff yet",
                "Add staff here, or import your Excel tracker from Settings.",
            ),
            (_, true) => ("No matches", "Nobody matches these filters."),
            _ => ("Nobody here", "No staff have this status."),
        };
        empty_state(ui, title, body);
        return;
    }
    let mut batch = None;
    if !selected.is_empty() {
        batch = batch_bar(ui, pal, data, editing, tab, selected.len());
        ui.add_space(10.0);
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
            egui::Grid::new("staff_table")
                .num_columns(7)
                .spacing([20.0, 10.0])
                .show(ui, |ui| {
                    let all = staff.iter().all(|s| selected.contains(&s.id));
                    let some = !all && !selected.is_empty();
                    let mut all_box = all;
                    let head = ui
                        .add(egui::Checkbox::without_text(&mut all_box).indeterminate(some))
                        .on_hover_text(if all { "Select none" } else { "Select all" });
                    if head.changed() {
                        if all {
                            selected.clear();
                        } else {
                            selected.extend(staff.iter().map(|s| s.id.clone()));
                        }
                    }
                    for head in ["Name", "Title", "Joined", "Sections", "Next due", ""] {
                        ui.label(RichText::new(head).color(pal.muted).size(12.5));
                    }
                    ui.end_row();
                    for s in &staff {
                        let mut on = selected.contains(&s.id);
                        let pick = ui
                            .add(egui::Checkbox::without_text(&mut on))
                            .on_hover_text(format!("Select {}", s.name));
                        if pick.changed() {
                            if on {
                                selected.insert(s.id.clone());
                            } else {
                                selected.remove(&s.id);
                            }
                        }
                        let name =
                            Button::new(RichText::new(&s.name).family(medium()).color(pal.text))
                                .frame(false);
                        if ui.add(name).clicked() {
                            open = Some(s.id.clone());
                        }
                        ui.label(RichText::new(&s.title).color(pal.muted));
                        ui.label(
                            RichText::new(s.joined.map_or("\u{2014}".to_owned(), fmt_date))
                                .color(pal.muted),
                        );
                        let sum = sums.get(&s.id);
                        ui.horizontal(|ui| {
                            section_chips(ui, pal, sum.map_or(&[], |m| m.sections.as_slice()))
                        });
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
    let ids: Vec<Id> = state.selected.iter().cloned().collect();
    match batch {
        Some(Batch::Status(st)) => {
            batch_status(cx, &ids, st);
            state.selected.clear();
        }
        Some(Batch::Section(sec, assigned)) => batch_section(cx, &ids, &sec, assigned),
        Some(Batch::AskDelete) => {
            // Only removed staff can be deleted (not those hidden earlier).
            let (ids, names): (Vec<Id>, Vec<String>) = ids
                .iter()
                .filter_map(|id| cx.session.data().staff(id))
                .filter(|s| s.status == StaffStatus::Removed)
                .map(|s| (s.id.clone(), s.name.clone()))
                .unzip();
            if ids.is_empty() {
                cx.toasts.push(Toast::new(
                    "Remove these people first; then they can be deleted",
                ));
            } else {
                state.batch_delete = Some(BatchDelete {
                    ids,
                    names,
                    text: String::new(),
                    error: None,
                    fresh: true,
                });
            }
        }
        Some(Batch::AskMerge) => {
            if let [a, b] = ids.as_slice() {
                state.merge = Some(MergeConfirm {
                    ids: [a.clone(), b.clone()],
                    keep: 0,
                    error: None,
                });
            }
        }
        Some(Batch::Clear) => state.selected.clear(),
        None => {}
    }
    let data = cx.session.data();
    match action {
        Some(RowAction::Edit(id)) => state.editing = data.staff(&id).map(StaffForm::edit),
        Some(RowAction::Status(id, st)) => set_status(cx, &id, st),
        Some(RowAction::AskDelete(id)) => {
            let name = data.staff(&id).map(|s| s.name.clone()).unwrap_or_default();
            state.delete = Some(DeleteConfirm {
                id,
                name,
                text: String::new(),
                error: None,
                fresh: true,
            });
        }
        None => {}
    }
}

/// The strip over the table while people are selected: how many, then the
/// actions that apply to everyone selected on this tab.
fn batch_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    editing: bool,
    tab: StaffStatus,
    count: usize,
) -> Option<Batch> {
    let mut out = None;
    Frame::new()
        .fill(pal.accent.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, pal.accent.gamma_multiply(0.35)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} selected", people(count)))
                        .family(medium())
                        .color(pal.text),
                );
                ui.add_space(8.0);
                let sections: Vec<_> = data
                    .sections_sorted()
                    .into_iter()
                    .filter(|s| !s.retired && s.tests.iter().any(|t| !t.retired))
                    .collect();
                let disabled = "Start editing to make changes";
                let mut section_menu = |ui: &mut egui::Ui, label: &str, assign: bool| {
                    let resp = ui
                        .add_enabled(
                            editing,
                            Button::new(format!("{label}  {}", icon::CARET_DOWN)),
                        )
                        .on_disabled_hover_text(disabled);
                    Popup::menu(&resp).show(|ui| {
                        for s in &sections {
                            if ui.button(&s.name).clicked() {
                                out = Some(Batch::Section(s.id.clone(), assign));
                            }
                        }
                    });
                };
                let merge = ui
                    .add_enabled(editing && count == 2, Button::new("Merge"))
                    .on_hover_text("Combine two records of the same person into one")
                    .on_disabled_hover_text(if editing {
                        "Select exactly two people to merge"
                    } else {
                        disabled
                    });
                let ask_merge = merge.clicked();
                if tab == StaffStatus::Active {
                    section_menu(ui, "Assign section", true);
                    section_menu(ui, "Unassign section", false);
                    if ui
                        .add_enabled(editing, Button::new("Remove"))
                        .on_disabled_hover_text(disabled)
                        .clicked()
                    {
                        out = Some(Batch::Status(StaffStatus::Removed));
                    }
                } else {
                    if ui
                        .add_enabled(editing, Button::new("Restore"))
                        .on_disabled_hover_text(disabled)
                        .clicked()
                    {
                        out = Some(Batch::Status(StaffStatus::Active));
                    }
                    let delete = Button::new(RichText::new("Delete permanently").color(pal.red_fg));
                    if ui
                        .add_enabled(editing, delete)
                        .on_disabled_hover_text(disabled)
                        .clicked()
                    {
                        out = Some(Batch::AskDelete);
                    }
                }
                if ask_merge {
                    out = Some(Batch::AskMerge);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let clear =
                        Button::new(RichText::new("Clear selection").color(pal.muted)).frame(false);
                    if ui.add(clear).clicked() {
                        out = Some(Batch::Clear);
                    }
                });
            });
        });
    out
}

/// Search box, then the Active / Removed control, section and title chips,
/// and Add staff on the right.
fn filters(
    ui: &mut egui::Ui,
    pal: &Palette,
    data: &Data,
    f: &mut StaffFilter,
    editing: bool,
    add_staff: &mut bool,
) {
    let sections: Vec<(Id, String)> = data
        .sections_sorted()
        .into_iter()
        .filter(|s| !s.retired)
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    let titles: Vec<(String, String)> = distinct_titles(data)
        .into_iter()
        .map(|t| (t.clone(), t))
        .collect();
    ui.horizontal(|ui| {
        search_box(ui, &mut f.text, "Search by name", SEARCH_W);
        let tabs = [StaffStatus::Active, StaffStatus::Removed];
        let current = usize::from(f.status.unwrap_or(StaffStatus::Active) != StaffStatus::Active);
        if let Some(i) = segmented(ui, pal, current, &["Active", "Removed"]) {
            f.status = Some(tabs[i]);
        }
        chip_select(ui, "Section", &mut f.section, &sections);
        chip_select(ui, "Title", &mut f.title, &titles);
        let any = f.section.is_some() || f.title.is_some() || !f.text.trim().is_empty();
        if any
            && ui
                .add(Button::new(RichText::new("Clear filters").color(pal.muted)).frame(false))
                .clicked()
        {
            f.section = None;
            f.title = None;
            f.text.clear();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let add = ui.add_enabled(editing, Button::new(format!("{}  Add staff", icon::PLUS)));
            if add
                .on_disabled_hover_text("Start editing to make changes")
                .clicked()
            {
                *add_staff = true;
            }
        });
    });
}

/// Up to three section chips, then "+N".
fn section_chips(ui: &mut egui::Ui, pal: &Palette, names: &[String]) {
    if names.is_empty() {
        ui.label(RichText::new("\u{2014}").color(pal.muted));
    }
    let chip = |ui: &mut egui::Ui, text: &str| {
        Frame::new()
            .fill(pal.grey_bg)
            .corner_radius(255)
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
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
    let resp = icon_button(
        ui,
        pal,
        icon::DOTS_THREE,
        &format!("Actions for {}", s.name),
    );
    let mut action = None;
    Popup::menu(&resp).show(|ui| {
        let item =
            |ui: &mut egui::Ui, label: &str| ui.add_enabled(editing, Button::new(label)).clicked();
        if item(ui, "Edit") {
            action = Some(RowAction::Edit(s.id.clone()));
        }
        let status = |st| Some(RowAction::Status(s.id.clone(), st));
        match s.status {
            StaffStatus::Active => {
                if item(ui, "Remove") {
                    action = status(StaffStatus::Removed);
                }
            }
            // Hidden earlier (no longer offered): restore, or remove properly.
            StaffStatus::Hidden => {
                if item(ui, "Restore") {
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
            ui.label(
                RichText::new("Start editing to make changes.")
                    .color(pal.muted)
                    .size(12.5),
            );
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
    let resp = Modal::new(egui::Id::new("staff_delete"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(
                RichText::new("Delete permanently")
                    .family(serif())
                    .size(20.0),
            );
            ui.add_space(6.0);
            ui.label(format!(
                "This deletes {} and all their records. It can't be undone.",
                d.name
            ));
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
        match cx.session.mutate(cx.now, |data, a| {
            ops::delete_staff_permanently(data, a, &id, &text)
        }) {
            Some(Ok(())) => {
                state.delete = None;
                cx.toasts.push(Toast::new(format!("{name} deleted")));
            }
            Some(Err(OpError::ConfirmMismatch)) => {
                d.error = Some("That doesn't match the name.".to_owned())
            }
            Some(Err(OpError::NotRemoved)) => {
                d.error = Some("Only removed staff can be deleted permanently.".to_owned())
            }
            Some(Err(e)) => d.error = Some(format!("Couldn't delete: {e}")),
            None => d.error = Some("Start editing to make changes".to_owned()),
        }
    }
}

/// One person: header, then a card per section listing their tests with pills.
fn profile(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    state.refresh(cx.session, cx.today);
    let data = cx.session.data();
    let Some(staff) = state.profile.as_deref().and_then(|id| data.staff(id)) else {
        return;
    };
    let tests_due = state
        .cache
        .as_ref()
        .and_then(|(_, _, m)| m.get(&staff.id))
        .map_or(&[][..], |m| m.tests.as_slice());
    if ui
        .add(Button::new(RichText::new("\u{2190} All staff").color(pal.muted)).frame(false))
        .clicked()
    {
        state.profile = None;
        return;
    }
    ui.add_space(4.0);
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

    let mut picked = None;
    let mut any = false;
    for sec in data.sections_sorted().into_iter().filter(|s| !s.retired) {
        let mut tests: Vec<_> = sec
            .tests
            .iter()
            .filter(|t| !t.retired && staff.assignments.contains_key(&t.id))
            .collect();
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
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            match staff.assignments[&t.id].state {
                                AssignState::Na => {
                                    ui.label(RichText::new("N/A").color(pal.muted));
                                }
                                AssignState::Assigned => {
                                    if let Some((_, info, status, delayed)) =
                                        tests_due.iter().find(|(id, ..)| *id == t.id)
                                    {
                                        if due_pill(ui, pal, info, *status, *delayed).clicked() {
                                            picked = Some((staff.id.clone(), t.id.clone()));
                                        }
                                    }
                                }
                            }
                        });
                    });
                }
            });
        ui.add_space(12.0);
    }
    if !any {
        empty_state(
            ui,
            "No assignments",
            "Edit this person to assign test systems.",
        );
    }
    if picked.is_some() {
        *cx.open_detail = picked;
    }
}
