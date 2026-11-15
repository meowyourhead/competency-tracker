//! The add / edit staff dialog: name, title, joined date and the assignments
//! checklist (sections → test systems). Saving applies the audited operations;
//! Cancel discards everything.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use egui::collapsing_header::CollapsingState;
use egui::{Button, Frame, Margin, Modal, RichText, ScrollArea, TextEdit};

use crate::model::{AssignState, Data, Id, Staff};
use crate::ops::{self, OpError};
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{medium, serif};
use crate::ui::widgets::{date_picker, section_color, section_dot};

/// A dialog in progress. `id` is `None` when adding a person.
pub(crate) struct StaffForm {
    id: Option<Id>,
    name: String,
    title: String,
    joined: Option<NaiveDate>,
    /// The wanted state per test; a missing test means "not assigned".
    assign: BTreeMap<Id, AssignState>,
    error: Option<String>,
    /// Focus the name field on the first frame.
    fresh: bool,
}

impl StaffForm {
    pub fn new() -> StaffForm {
        StaffForm {
            id: None,
            name: String::new(),
            title: String::new(),
            joined: None,
            assign: BTreeMap::new(),
            error: None,
            fresh: true,
        }
    }

    pub fn edit(s: &Staff) -> StaffForm {
        StaffForm {
            id: Some(s.id.clone()),
            name: s.name.clone(),
            title: s.title.clone(),
            joined: s.joined,
            assign: s
                .assignments
                .iter()
                .map(|(t, a)| (t.clone(), a.state))
                .collect(),
            ..StaffForm::new()
        }
    }
}

enum Act {
    None,
    Save,
    Cancel,
}

/// Draws the dialog; returns true once it should close (saved or cancelled).
pub(crate) fn show(ctx: &egui::Context, cx: &mut Ctx, form: &mut StaffForm) -> bool {
    let editing = cx.session.is_editing();
    let data = cx.session.data();
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let mut act = Act::None;
    let resp = Modal::new(egui::Id::new("staff_editor"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(520.0);
            let heading = if form.id.is_some() {
                "Edit staff"
            } else {
                "Add staff"
            };
            ui.label(RichText::new(heading).family(serif()).size(20.0));
            ui.add_space(10.0);
            fields(ui, form, cx.today);
            ui.add_space(10.0);
            ui.label(RichText::new("Assignments").family(medium()));
            ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| assignments(ui, data, form));
            if let Some(e) = &form.error {
                ui.colored_label(cx.pal.red_fg, e);
            }
            if !editing {
                ui.label(RichText::new("Start editing to make changes.").color(cx.pal.muted));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = editing && !form.name.trim().is_empty();
                if ui.add_enabled(ready, Button::new("Save")).clicked() {
                    act = Act::Save;
                }
                if ui.button("Cancel").clicked() {
                    act = Act::Cancel;
                }
            });
        });
    if matches!(act, Act::None) && resp.should_close() {
        act = Act::Cancel;
    }
    match act {
        Act::None => false,
        Act::Cancel => true,
        Act::Save => save(cx, form),
    }
}

fn fields(ui: &mut egui::Ui, form: &mut StaffForm, today: NaiveDate) {
    ui.label("Name");
    let name = ui.add(
        TextEdit::singleline(&mut form.name)
            .hint_text("Full name")
            .desired_width(f32::INFINITY),
    );
    if form.fresh {
        name.request_focus();
        form.fresh = false;
    }
    ui.add_space(4.0);
    ui.label("Title");
    ui.add(
        TextEdit::singleline(&mut form.title)
            .hint_text("e.g. Senior MLT")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Joined");
        match &mut form.joined {
            Some(date) => {
                date_picker(ui, date, "staff_joined");
                if ui.button("No date").clicked() {
                    form.joined = None;
                }
            }
            None => {
                if ui.button("Set date").clicked() {
                    form.joined = Some(today);
                }
            }
        }
    });
}

/// One collapsible group per section: the whole-section checkbox in the
/// header, a 3-way selector per test inside.
fn assignments(ui: &mut egui::Ui, data: &Data, form: &mut StaffForm) {
    for sec in data.sections_sorted().into_iter().filter(|s| !s.retired) {
        let mut tests: Vec<_> = sec.tests.iter().filter(|t| !t.retired).collect();
        if tests.is_empty() {
            continue;
        }
        tests.sort_by_key(|t| t.order);
        let id = ui.make_persistent_id(("staff_editor_section", &sec.id));
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| {
                if let Some(c) = section_color(data, &sec.id) {
                    section_dot(ui, c);
                }
                ui.label(RichText::new(&sec.name).family(medium()));
                let mut whole = tests
                    .iter()
                    .all(|t| form.assign.get(&t.id) == Some(&AssignState::Assigned));
                if ui.checkbox(&mut whole, "Assign whole section").changed() {
                    for t in &tests {
                        if whole {
                            form.assign.insert(t.id.clone(), AssignState::Assigned);
                        } else {
                            form.assign.remove(&t.id);
                        }
                    }
                }
            })
            .body(|ui| {
                for t in &tests {
                    ui.horizontal(|ui| {
                        ui.label(&t.name);
                        let mut cur = form.assign.get(&t.id).copied();
                        ui.selectable_value(&mut cur, Some(AssignState::Assigned), "Assigned");
                        ui.selectable_value(&mut cur, Some(AssignState::Na), "N/A");
                        ui.selectable_value(&mut cur, None, "Not assigned");
                        match cur {
                            Some(state) => form.assign.insert(t.id.clone(), state),
                            None => form.assign.remove(&t.id),
                        };
                    });
                }
            });
    }
}

/// Applies the details, then the assignment differences. Returns true when saved.
fn save(cx: &mut Ctx, form: &mut StaffForm) -> bool {
    let was_new = form.id.is_none();
    let done = if was_new {
        "Staff added"
    } else {
        "Changes saved"
    };
    // Set as soon as the person exists, so a failure later never leads to a second add.
    let mut created: Option<Id> = None;
    let res = cx.session.mutate(cx.now, |d, a| {
        let id = match &form.id {
            Some(id) => {
                ops::update_staff(d, a, id, &form.name, &form.title, form.joined)?;
                id.clone()
            }
            None => {
                let id = ops::add_staff(d, a, &form.name, &form.title, form.joined)?;
                created = Some(id.clone());
                id
            }
        };
        let tests: Vec<Id> = d
            .sections
            .iter()
            .flat_map(|s| &s.tests)
            .filter(|t| !t.retired)
            .map(|t| t.id.clone())
            .collect();
        for t in tests {
            let want = form.assign.get(&t).copied();
            let have = d
                .staff(&id)
                .and_then(|s| s.assignments.get(&t))
                .map(|a| a.state);
            if want != have {
                ops::set_assignment(d, a, &id, &t, want)?;
            }
        }
        Ok::<(), OpError>(())
    });
    match res {
        Some(Ok(())) => {
            cx.toasts.push(Toast::new(done));
            true
        }
        Some(Err(OpError::EmptyName)) => {
            form.error = Some("Enter a name".to_owned());
            false
        }
        Some(Err(e)) => {
            let name = form.name.trim().to_owned();
            if let Some(id) = created {
                // The person exists now: a retry must update, not add again.
                form.id = Some(id);
                form.error = Some(format!(
                    "Saved {name}, but couldn't apply all assignments: {e}"
                ));
            } else if form.id.is_some() && !matches!(e, OpError::EmptyName | OpError::NotFound(_)) {
                form.error = Some(format!(
                    "Saved details, but couldn't apply all assignments: {e}"
                ));
            } else {
                form.error = Some(format!("Couldn't save: {e}"));
            }
            false
        }
        None => {
            form.error = Some("Start editing to make changes".to_owned());
            false
        }
    }
}
