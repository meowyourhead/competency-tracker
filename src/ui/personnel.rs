//! Personnel: the master staff list (filters, status, row menu), the per-person
//! profile and the permanent-delete confirmation. The add / edit dialog is in
//! `staff_editor.rs`.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use chrono::NaiveDate;
use egui::{
    Align, Button, Color32, CornerRadius, FontId, Frame, Layout, Margin, Modal, Popup, RichText,
    Sense, Stroke, StrokeKind, TextEdit, Vec2, WidgetInfo, WidgetType,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Staff, StaffStatus};
use crate::ops::{self, OpError};
use crate::query::{distinct_titles, filter_staff, Index, StaffFilter};
use crate::schedule::{classify, next_due, DueInfo, DueStatus};
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::profile;
use crate::ui::staff_editor::{self, StaffForm};
use crate::ui::theme::{medium, serif, Palette};
use crate::ui::widgets::{
    chip_select, due_pill, due_pill_text, empty_state, icon_button, paint_section_chip, rgb,
    search_box, section_chip_width, segmented, SEARCH_W,
};

/// What the list is sorted by: one of its columns.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum SortBy {
    Name,
    Title,
    #[default]
    Joined,
    Sections,
    Due,
}

#[derive(Default)]
pub(crate) struct PersonnelState {
    pub filter: StaffFilter,
    /// Earliest join date first, until another column is picked.
    sort: SortBy,
    descending: bool,
    /// The add / edit dialog, while open.
    pub editing: Option<StaffForm>,
    /// Person whose profile is open (also set by search navigation).
    pub profile: Option<Id>,
    profile_ui: profile::ProfileUi,
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
    sections: Vec<(String, Color32)>,
    /// Those sections' places in the section order, for sorting.
    section_order: Vec<usize>,
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
            let mut section_order = Vec::new();
            let mut next: Option<(DueInfo, DueStatus)> = None;
            for (pos, sec) in sections.iter().enumerate().filter(|(_, x)| !x.retired) {
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
                    if info.due_on.is_some()
                        && next.as_ref().is_none_or(|(n, _)| info.due_on < n.due_on)
                    {
                        next = Some((info, status));
                    }
                }
                if any {
                    names.push((sec.name.clone(), rgb(sec.color_rgb(pos))));
                    section_order.push(pos);
                }
            }
            (
                s.id.clone(),
                Summary {
                    sections: names,
                    section_order,
                    next,
                },
            )
        })
        .collect()
}

/// Sorts people already in name order (which settles ties). Those with
/// nothing to sort by (no join date, say) come last in either direction.
fn sort_staff(staff: &mut [&Staff], sums: &HashMap<Id, Summary>, sort: SortBy, descending: bool) {
    fn order<K: Ord>(a: Option<K>, b: Option<K>, descending: bool) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) if descending => b.cmp(&a),
            (Some(a), Some(b)) => a.cmp(&b),
            (a, b) => a.is_none().cmp(&b.is_none()),
        }
    }
    let title = |s: &Staff| (!s.title.is_empty()).then(|| s.title.to_lowercase());
    let sections = |s: &Staff| {
        let places = &sums.get(&s.id)?.section_order;
        (!places.is_empty()).then_some(places)
    };
    let due = |s: &Staff| sums.get(&s.id)?.next.as_ref()?.0.due_on;
    staff.sort_by(|a, b| match sort {
        SortBy::Name => order(
            Some(a.name.to_lowercase()),
            Some(b.name.to_lowercase()),
            descending,
        ),
        SortBy::Title => order(title(a), title(b), descending),
        SortBy::Joined => order(a.joined, b.joined, descending),
        SortBy::Sections => order(sections(a), sections(b), descending),
        SortBy::Due => order(due(a), due(b), descending),
    });
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    if state
        .profile
        .as_deref()
        .is_some_and(|id| cx.session.data().staff(id).is_none())
    {
        state.profile = None;
    }
    if let Some(id) = state.profile.clone() {
        let out = profile::show(ui, cx, &id, &mut state.profile_ui);
        if out.back {
            state.profile = None;
            state.profile_ui.reset_view();
        } else if out.edit {
            state.editing = cx.session.data().staff(&id).map(StaffForm::edit);
        }
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
        sort,
        descending,
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
    let mut staff = filter_staff(data, filter);
    sort_staff(&mut staff, sums, *sort, *descending);
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
            let cols = columns(ui, pal, &staff, sums);
            // Its own id per shape, so column widths never carry across.
            egui::Grid::new(("staff_table", cols.title, cols.joined))
                .num_columns(5 + usize::from(cols.title) + usize::from(cols.joined))
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
                    for (head, key) in HEADS {
                        if (key == SortBy::Title && !cols.title)
                            || (key == SortBy::Joined && !cols.joined)
                        {
                            continue;
                        }
                        let on = *sort == key;
                        if sort_head(ui, pal, head, on, *descending).clicked() {
                            *descending = on && !*descending;
                            *sort = key;
                        }
                    }
                    ui.label("");
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
                        if name_button(ui, pal, &s.name, cols.name_w).clicked() {
                            open = Some(s.id.clone());
                        }
                        if cols.title {
                            ui.label(RichText::new(&s.title).color(pal.muted));
                        }
                        if cols.joined {
                            ui.label(
                                RichText::new(s.joined.map_or("\u{2014}".to_owned(), fmt_date))
                                    .color(pal.muted),
                            );
                        }
                        let sum = sums.get(&s.id);
                        section_chips(
                            ui,
                            pal,
                            sum.map_or(&[], |m| m.sections.as_slice()),
                            cols.sections_w,
                        );
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
    ui.horizontal_wrapped(|ui| {
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
        let add = |ui: &mut egui::Ui| {
            let add = ui.add_enabled(editing, Button::new(format!("{}  Add staff", icon::PLUS)));
            add.on_disabled_hover_text("Start editing to make changes")
                .clicked()
        };
        // Right-aligned when it fits on this line; otherwise it wraps below.
        let clicked = if ui.available_width() >= 130.0 {
            ui.with_layout(Layout::right_to_left(Align::Center), add)
                .inner
        } else {
            add(ui)
        };
        if clicked {
            *add_staff = true;
        }
    });
}

const CHIP_GAP: f32 = 6.0;
const CHIP_H: f32 = 18.0;

/// The sortable columns, in order.
const HEADS: [(&str, SortBy); 5] = [
    ("Name", SortBy::Name),
    ("Title", SortBy::Title),
    ("Joined", SortBy::Joined),
    ("Sections", SortBy::Sections),
    ("Next due", SortBy::Due),
];
const HEAD_SIZE: f32 = 12.5;

/// A heading's text with its sort mark: which way the list is sorted, or
/// that it can be.
fn head_text(head: &str, on: bool, descending: bool) -> String {
    let mark = match (on, descending) {
        (false, _) => icon::CARET_UP_DOWN,
        (true, false) => icon::CARET_UP,
        (true, true) => icon::CARET_DOWN,
    };
    format!("{head} {mark}")
}

/// A column heading that sorts the list; a second click reverses it.
fn sort_head(
    ui: &mut egui::Ui,
    pal: &Palette,
    head: &str,
    on: bool,
    descending: bool,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        head_text(head, on, descending),
        FontId::proportional(HEAD_SIZE),
        Color32::PLACEHOLDER,
    );
    let size = Vec2::new(galley.size().x, ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let lit = on || resp.hovered() || resp.has_focus();
        let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
        let color = if lit { pal.text } else { pal.muted };
        ui.painter()
            .galley_with_override_text_color(pos, galley, color);
    }
    let label = format!("Sort by {head}");
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if on { "Reverse the order" } else { &label })
}

const NAME_H: f32 = 30.0;
const NAME_PAD: f32 = 12.0;
/// Room for the arrow at the end of a name button.
const NAME_ARROW_W: f32 = 22.0;

/// A person's name as a button that opens their profile, `width` wide so the
/// names line up as one column of buttons.
fn name_button(ui: &mut egui::Ui, pal: &Palette, name: &str, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, NAME_H), Sense::click());
    if ui.is_rect_visible(rect) {
        let lit = resp.hovered() || resp.has_focus();
        let (stroke, arrow) = if lit {
            (Stroke::new(1.0, pal.accent), pal.accent)
        } else {
            (Stroke::new(1.0, pal.muted.gamma_multiply(0.45)), pal.muted)
        };
        let p = ui.painter();
        let radius = CornerRadius::same(8);
        p.rect(rect, radius, pal.sidebar, stroke, StrokeKind::Inside);
        if lit {
            let strength = if resp.is_pointer_button_down_on() {
                0.24
            } else {
                0.12
            };
            p.rect_filled(rect, radius, pal.accent.gamma_multiply(strength));
        }
        let mid = rect.center().y;
        p.text(
            egui::pos2(rect.left() + NAME_PAD, mid),
            egui::Align2::LEFT_CENTER,
            name,
            FontId::new(body_size(ui), medium()),
            pal.text,
        );
        p.text(
            egui::pos2(rect.right() - NAME_PAD + 2.0, mid),
            egui::Align2::RIGHT_CENTER,
            icon::CARET_RIGHT,
            FontId::proportional(13.0),
            arrow,
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Open profile")
}

fn body_size(ui: &egui::Ui) -> f32 {
    egui::TextStyle::Body.resolve(ui.style()).size
}

/// How the table's columns share the page.
struct Columns {
    name_w: f32,
    /// Wide enough for everyone's chips on one line, unless the other columns
    /// need the room (then the chips wrap).
    sections_w: f32,
    /// Whether there is room for Title and Joined. Title gives way first;
    /// each person's profile still shows both.
    title: bool,
    joined: bool,
}

fn columns(ui: &egui::Ui, pal: &Palette, staff: &[&Staff], sums: &HashMap<Id, Summary>) -> Columns {
    let measure = |t: String, font: FontId| ui.painter().layout_no_wrap(t, font, pal.text).size().x;
    let text = |t: &str, family: egui::FontFamily| {
        measure(t.to_owned(), FontId::new(body_size(ui), family))
    };
    let head = |t: &str| measure(head_text(t, false, false), FontId::proportional(HEAD_SIZE));
    let widest = |f: &dyn Fn(&Staff) -> f32| staff.iter().map(|s| f(s)).fold(0.0, f32::max);
    let name = (widest(&|s| text(&s.name, medium())) + 2.0 * NAME_PAD + NAME_ARROW_W).ceil();
    let title = widest(&|s| text(&s.title, egui::FontFamily::Proportional)).max(head("Title"));
    let joined = text("28 Dec 2026", egui::FontFamily::Proportional).max(head("Joined"));
    let pill = widest(&|s| {
        sums.get(&s.id)
            .and_then(|m| m.next.as_ref())
            .map_or(20.0, |(info, _)| {
                ui.painter()
                    .layout_no_wrap(
                        format!("{} {}", icon::PAUSE, due_pill_text(info)),
                        egui::FontId::proportional(12.5),
                        pal.text,
                    )
                    .size()
                    .x
                    + 20.0
            })
    })
    .max(head("Next due"));
    // Checkbox, the row menu, the gaps between seven columns, and some slack.
    let others = 20.0 + name + title + joined + pill + 32.0 + 6.0 * 20.0 + 12.0;
    let chips = widest(&|s| {
        let secs = sums.get(&s.id).map_or(&[][..], |m| m.sections.as_slice());
        // A little slack so rounding never pushes the last chip onto a new line.
        secs.iter()
            .map(|(n, _)| section_chip_width(ui, n) + CHIP_GAP)
            .sum::<f32>()
            + 4.0
    });
    let mut room = ui.available_width() - others;
    let show_title = room >= MIN_SECTIONS_W;
    if !show_title {
        room += title + 20.0;
    }
    let show_joined = room >= MIN_SECTIONS_W;
    if !show_joined {
        room += joined + 20.0;
    }
    Columns {
        name_w: name,
        sections_w: chips.min(room).max(MIN_SECTIONS_W),
        title: show_title,
        joined: show_joined,
    }
}

/// The Sections column never gets narrower than this.
const MIN_SECTIONS_W: f32 = 160.0;

/// Every section the person has tests in, as chips in their colours,
/// `width` wide: on one line when they fit, otherwise on as many lines as
/// they need (each chip whole). The lines are worked out here, so the
/// row's height is known.
fn section_chips(ui: &mut egui::Ui, pal: &Palette, secs: &[(String, Color32)], width: f32) {
    if secs.is_empty() {
        ui.allocate_ui(Vec2::new(width, 0.0), |ui| {
            ui.set_width(width);
            ui.label(RichText::new("\u{2014}").color(pal.muted));
        });
        return;
    }
    let widths: Vec<f32> = secs
        .iter()
        .map(|(n, _)| section_chip_width(ui, n))
        .collect();
    let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
    let mut x = 0.0;
    for (i, &w) in widths.iter().enumerate() {
        let line = lines.last_mut().expect("never empty");
        if !line.is_empty() && x + w > width {
            lines.push(vec![i]);
            x = w + CHIP_GAP;
        } else {
            line.push(i);
            x += w + CHIP_GAP;
        }
    }
    let line_gap = 5.0;
    let height = lines.len() as f32 * (CHIP_H + line_gap) - line_gap;
    let (block, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    for (row, line) in lines.iter().enumerate() {
        let mut x = block.left();
        let y = block.top() + row as f32 * (CHIP_H + line_gap);
        for &i in line {
            let rect = egui::Rect::from_min_size(egui::pos2(x, y), Vec2::new(widths[i], CHIP_H));
            x = rect.right() + CHIP_GAP;
            let (name, color) = &secs[i];
            paint_section_chip(ui, pal, rect, name, *color);
            // Named for screen readers; each chip's place keeps its id unique.
            let id = egui::Id::new(("section_chip", rect.min.x as i32, rect.min.y as i32));
            let resp = ui.interact(rect, id, egui::Sense::hover());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, name));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::now_ts;
    use crate::model::{Assignment, DueOverride, OverrideSource, TestSystem};

    /// Name, title, join year, and tests assigned.
    fn person(name: &str, title: &str, joined: Option<i32>, tests: &[&str]) -> Staff {
        let since = now_ts().date_naive();
        Staff {
            id: name.into(),
            name: name.into(),
            title: title.into(),
            joined: joined.and_then(|y| NaiveDate::from_ymd_opt(y, 3, 1)),
            status: StaffStatus::Active,
            assignments: tests
                .iter()
                .map(|t| {
                    let state = AssignState::Assigned;
                    ((*t).to_string(), Assignment { state, since })
                })
                .collect(),
        }
    }

    fn names(sort: SortBy, descending: bool) -> Vec<String> {
        let mut d = Data::new_empty("t", now_ts());
        for (i, id) in ["t-chem", "t-haem"].into_iter().enumerate() {
            d.sections[i].tests = vec![TestSystem {
                id: id.into(),
                name: id.into(),
                order: 0,
                retired: false,
                rule_id: None,
            }];
        }
        d.staff = vec![
            person("Agnes", "", Some(2020), &["t-haem"]),
            person("Bram", "Senior", None, &[]),
            person("cara", "MLT", Some(2015), &["t-chem"]),
            person("Dana", "MLT", Some(2020), &["t-chem", "t-haem"]),
        ];
        d.settings.initial_due_days = None;
        // Only Dana has a due date.
        d.due_overrides.push(DueOverride {
            staff_id: "Dana".into(),
            test_id: "t-chem".into(),
            due_on: now_ts().date_naive(),
            source: OverrideSource::Manual,
            by: "t".into(),
            at: now_ts(),
        });
        let sums = summaries(&d, now_ts().date_naive());
        let mut staff = filter_staff(&d, &StaffFilter::default());
        sort_staff(&mut staff, &sums, sort, descending);
        staff.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn default_sort_is_earliest_join_date_first() {
        let st = PersonnelState::default();
        assert_eq!((st.sort, st.descending), (SortBy::Joined, false));
        assert_eq!(
            names(st.sort, st.descending),
            ["cara", "Agnes", "Dana", "Bram"]
        );
    }

    #[test]
    fn no_join_date_stays_last_when_reversed() {
        assert_eq!(
            names(SortBy::Joined, true),
            ["Agnes", "Dana", "cara", "Bram"]
        );
    }

    #[test]
    fn sorts_by_the_other_columns() {
        let by = |sort| names(sort, false);
        assert_eq!(by(SortBy::Name), ["Agnes", "Bram", "cara", "Dana"]);
        assert_eq!(names(SortBy::Name, true), ["Dana", "cara", "Bram", "Agnes"]);
        assert_eq!(by(SortBy::Title), ["cara", "Dana", "Bram", "Agnes"]);
        assert_eq!(by(SortBy::Sections), ["cara", "Dana", "Agnes", "Bram"]);
        assert_eq!(by(SortBy::Due), ["Dana", "Agnes", "Bram", "cara"]);
    }
}
