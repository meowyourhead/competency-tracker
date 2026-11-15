//! Competency detail panel: one person and one test system. Header with the
//! next-due pill, the actions (edit mode only), and the history, newest first.

use chrono::NaiveDate;
use egui::{Align, Button, ComboBox, Frame, Key, Layout, Margin, RichText, ScrollArea, TextEdit};
use egui_extras::DatePickerButton;
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Rule, Ts};
use crate::ops::{self, Actor, OpError};
use crate::query::{history, split_history, HistoryEntry, HISTORY_DEFAULT};
use crate::schedule::{classify, completion_variance, next_due, stage_label, variance_tag, DueInfo, PairState};
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{heading, medium, Palette};
use crate::ui::widgets::{due_pill, icon_button};

#[derive(Default)]
pub(crate) enum DetailForm {
    #[default]
    None,
    Complete { date: NaiveDate, stage: u32, comment: String },
    Delay { note: String, expected: Option<NaiveDate> },
    EditRecord { id: Id, date: NaiveDate, stage: u32, comment: String },
}

/// An action waiting for an inline "are you sure?".
#[derive(Clone, PartialEq)]
enum Confirm {
    Unassign,
    DeleteRecord(Id),
    DeleteDelay(Id),
}

#[derive(Default)]
pub(crate) struct DetailState {
    /// Show every history entry, not just the latest four.
    pub expanded: bool,
    pub form: DetailForm,
    /// The (staff, test) this state belongs to; a new target resets it.
    target: Option<(Id, Id)>,
    /// Date being picked for "Set due date".
    due_pick: Option<NaiveDate>,
    confirm: Option<Confirm>,
    error: Option<String>,
}

/// A change requested this frame, applied once drawing is done.
enum Op {
    Complete { date: NaiveDate, stage: u32, comment: String },
    Edit { id: Id, date: NaiveDate, stage: u32, comment: String },
    DeleteRecord(Id),
    Delay { note: String, expected: Option<NaiveDate> },
    DeleteDelay(Id),
    Skip(u32),
    Due(Option<NaiveDate>),
    Assign(Option<AssignState>),
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut DetailState) {
    let Some(target) = cx.open_detail.clone() else { return };
    if state.target.as_ref() != Some(&target) {
        *state = DetailState { target: Some(target.clone()), ..DetailState::default() };
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) && ui.memory(|m| m.focused().is_none()) {
        *cx.open_detail = None;
        return;
    }
    let (staff_id, test_id) = target;
    let mut close = false;
    let mut op = None;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let data = cx.session.data();
        let (Some(staff), Some((sec, test))) = (data.staff(&staff_id), data.test(&test_id)) else {
            close = header(ui, cx.pal, "Not found", "This person or test no longer exists.");
            return;
        };
        close = header(ui, cx.pal, &staff.name, &format!("{} \u{b7} {}", test.name, sec.name));
        let rule = data.rule_for_test(&test_id);
        let pair = pair_state(data, &staff_id, &test_id);
        let assign = staff.assignments.get(&test_id).map(|a| a.state);
        let info = next_due(rule, &pair, &data.settings);
        let editing = cx.session.is_editing();
        let view = View { pal: cx.pal, data, rule, editing, today: cx.today };
        ui.add_space(6.0);
        match assign {
            Some(AssignState::Assigned) => {
                ui.horizontal(|ui| {
                    let status = classify(info.due_on, cx.today, data.settings.due_soon_days);
                    let delayed = data.delay_notes.iter().any(|n| {
                        n.staff_id == staff_id && n.test_id == test_id && n.for_stage_index == info.next_stage_index
                    });
                    due_pill(ui, cx.pal, &info, status, delayed);
                    ui.label(RichText::new(format!("for {}", info.next_stage_label)).color(cx.pal.muted));
                });
            }
            Some(AssignState::Na) => {
                ui.label(RichText::new("N/A \u{2014} not applicable").color(cx.pal.muted));
            }
            None => {
                ui.label(RichText::new("Not assigned").color(cx.pal.muted));
            }
        }
        ui.add_space(10.0);
        op = actions(ui, &view, state, assign, &info);
        if let Some(e) = &state.error {
            ui.label(RichText::new(e).color(cx.pal.red_fg));
        }
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("History").family(medium()).size(15.0));
        let entries = history(data, &staff_id, &test_id);
        if let Some(o) = history_list(ui, &view, state, &entries, &pair) {
            op = Some(o);
        }
    });
    if close {
        *cx.open_detail = None;
        return;
    }
    if let Some(op) = op {
        apply(cx, state, &staff_id, &test_id, op);
    }
}

/// Everything the drawing helpers read.
struct View<'a> {
    pal: &'a Palette,
    data: &'a Data,
    rule: &'a Rule,
    editing: bool,
    today: NaiveDate,
}

/// Name and subtitle with the close button; returns true when closed.
fn header(ui: &mut egui::Ui, pal: &Palette, title: &str, subtitle: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        heading(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = icon_button(ui, pal, icon::X, "Close panel").clicked();
        });
    });
    ui.label(RichText::new(subtitle).color(pal.muted));
    close
}

fn pair_state<'a>(data: &'a Data, staff_id: &str, test_id: &str) -> PairState<'a> {
    let is = |s: &str, t: &str| s == staff_id && t == test_id;
    PairState {
        records: data.records.iter().filter(|r| is(&r.staff_id, &r.test_id)).collect(),
        skipped: data.skipped.iter().filter(|s| is(&s.staff_id, &s.test_id)).map(|s| s.stage_index).collect(),
        override_: data.due_overrides.iter().filter(|o| is(&o.staff_id, &o.test_id)).max_by_key(|o| o.at),
        assigned_on: data.staff(staff_id).and_then(|s| s.assignments.get(test_id)).map(|a| a.since),
    }
}

/// The action buttons and the open form. Disabled unless editing.
fn actions(
    ui: &mut egui::Ui,
    v: &View,
    state: &mut DetailState,
    assign: Option<AssignState>,
    info: &DueInfo,
) -> Option<Op> {
    let mut op = None;
    let on = v.editing;
    ui.horizontal_wrapped(|ui| match assign {
        Some(AssignState::Assigned) => {
            if ui.add_enabled(on, Button::new("Mark complete")).clicked() {
                state.form = DetailForm::Complete { date: v.today, stage: info.next_stage_index, comment: String::new() };
                state.error = None;
            }
            if ui.add_enabled(on, Button::new("Add delay note")).clicked() {
                state.form = DetailForm::Delay { note: String::new(), expected: None };
                state.error = None;
            }
            if ui.add_enabled(on, Button::new("Skip stage")).on_hover_text(format!("Skip {}", info.next_stage_label)).clicked() {
                op = Some(Op::Skip(info.next_stage_index));
            }
            if ui.add_enabled(on, Button::new("Set due date")).clicked() {
                state.due_pick = Some(info.due_on.unwrap_or(v.today));
            }
            ui.add_enabled_ui(on, |ui| {
                ui.menu_button(format!("Mark N/A / Unassign  {}", icon::CARET_DOWN), |ui| {
                    if ui.button("Mark N/A").clicked() {
                        op = Some(Op::Assign(Some(AssignState::Na)));
                    }
                    if ui.button("Unassign").clicked() {
                        state.confirm = Some(Confirm::Unassign);
                    }
                });
            });
        }
        Some(AssignState::Na) | None => {
            if ui.add_enabled(on, Button::new(format!("{}  Assign", icon::PLUS))).clicked() {
                op = Some(Op::Assign(Some(AssignState::Assigned)));
            }
            if assign.is_some() && ui.add_enabled(on, Button::new("Unassign")).clicked() {
                state.confirm = Some(Confirm::Unassign);
            }
        }
    });
    if !on {
        ui.label(RichText::new("Start editing to make changes.").color(v.pal.muted).size(12.5));
    }
    if state.confirm == Some(Confirm::Unassign) {
        if let Some(yes) = confirm_row(ui, v.pal, "Remove this assignment? Its history is kept.", "Unassign") {
            state.confirm = None;
            if yes {
                op = Some(Op::Assign(None));
            }
        }
    }
    if let Some(date) = &mut state.due_pick {
        let mut done = None;
        card(ui, v.pal, |ui| {
            ui.label(RichText::new("Set due date").family(medium()));
            ui.horizontal(|ui| {
                date_picker(ui, date, "due_date");
                if ui.add_enabled(on, Button::new("Set")).clicked() {
                    done = Some(Some(Op::Due(Some(*date))));
                }
                if ui.add_enabled(on && info.source != crate::schedule::DueSource::Rule, Button::new("Clear")).clicked() {
                    done = Some(Some(Op::Due(None)));
                }
                if ui.button("Cancel").clicked() {
                    done = Some(None);
                }
            });
        });
        if let Some(o) = done {
            state.due_pick = None;
            op = o.or(op);
        }
    }
    if let Some(o) = form(ui, v, state, info) {
        op = Some(o);
    }
    op
}

/// The open Mark complete / delay note / edit record form.
fn form(ui: &mut egui::Ui, v: &View, state: &mut DetailState, info: &DueInfo) -> Option<Op> {
    let mut op = None;
    let mut cancel = false;
    let on = v.editing;
    let max_stage = info.next_stage_index.max(v.rule.stages.len().saturating_sub(1) as u32) + 1;
    match &mut state.form {
        DetailForm::None => return None,
        DetailForm::Complete { date, stage, comment } => card(ui, v.pal, |ui| {
            ui.label(RichText::new("Mark complete").family(medium()));
            date_stage_comment(ui, v.rule, date, stage, comment, max_stage, "complete");
            ui.horizontal(|ui| {
                if ui.add_enabled(on, Button::new("Save")).clicked() {
                    op = Some(Op::Complete { date: *date, stage: *stage, comment: comment.trim().to_owned() });
                }
                cancel = ui.button("Cancel").clicked();
            });
        }),
        DetailForm::EditRecord { id, date, stage, comment } => card(ui, v.pal, |ui| {
            ui.label(RichText::new("Edit entry").family(medium()));
            let max = max_stage.max(*stage + 1);
            date_stage_comment(ui, v.rule, date, stage, comment, max, "edit");
            ui.horizontal(|ui| {
                if ui.add_enabled(on, Button::new("Save")).clicked() {
                    op = Some(Op::Edit { id: id.clone(), date: *date, stage: *stage, comment: comment.trim().to_owned() });
                }
                cancel = ui.button("Cancel").clicked();
            });
        }),
        DetailForm::Delay { note, expected } => card(ui, v.pal, |ui| {
            ui.label(RichText::new(format!("Delay note for {}", info.next_stage_label)).family(medium()));
            ui.add(TextEdit::multiline(note).hint_text("Reason").desired_rows(2).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                let mut has = expected.is_some();
                if ui.checkbox(&mut has, "Expected completion").changed() {
                    *expected = has.then_some(v.today);
                }
                if let Some(d) = expected {
                    date_picker(ui, d, "delay_expected");
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(on && !note.trim().is_empty(), Button::new("Save")).clicked() {
                    op = Some(Op::Delay { note: note.trim().to_owned(), expected: *expected });
                }
                cancel = ui.button("Cancel").clicked();
            });
        }),
    }
    if cancel {
        state.form = DetailForm::None;
        state.error = None;
    }
    op
}

fn date_stage_comment(
    ui: &mut egui::Ui,
    rule: &Rule,
    date: &mut NaiveDate,
    stage: &mut u32,
    comment: &mut String,
    max_stage: u32,
    salt: &str,
) {
    ui.horizontal(|ui| {
        date_picker(ui, date, salt);
        ComboBox::from_id_salt(("stage", salt)).selected_text(stage_label(rule, *stage)).show_ui(ui, |ui| {
            for i in 0..=max_stage {
                ui.selectable_value(stage, i, stage_label(rule, i));
            }
        });
    });
    ui.add(TextEdit::multiline(comment).hint_text("Comment (optional)").desired_rows(2).desired_width(f32::INFINITY));
}

/// egui_extras' date picker (which works on `jiff` dates) for a chrono date.
fn date_picker(ui: &mut egui::Ui, date: &mut NaiveDate, salt: &str) {
    use chrono::Datelike;
    let Ok(mut d) = jiff::civil::Date::new(date.year() as i16, date.month() as i8, date.day() as i8) else { return };
    ui.add(DatePickerButton::new(&mut d).id_salt(salt).format("%-d %b %Y").calendar_week(false));
    if let Some(back) = NaiveDate::from_ymd_opt(d.year().into(), d.month() as u32, d.day() as u32) {
        *date = back;
    }
}

fn card<R>(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(6.0);
    Frame::new()
        .fill(pal.bg)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// "Are you sure?" with a confirm and a Cancel button; Some(true) confirms.
fn confirm_row(ui: &mut egui::Ui, pal: &Palette, text: &str, yes: &str) -> Option<bool> {
    let mut answer = None;
    card(ui, pal, |ui| {
        ui.label(text);
        ui.horizontal(|ui| {
            if ui.button(RichText::new(yes).color(pal.red_fg)).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    answer
}

fn tag(ui: &mut egui::Ui, text: &str, fill: egui::Color32, fg: egui::Color32) {
    Frame::new().fill(fill).corner_radius(255).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(text).size(11.5).color(fg));
    });
}

fn by_line(ui: &mut egui::Ui, pal: &Palette, by: &str, at: Ts) {
    let text = format!("by {by} \u{b7} {}", at.format("%-d %b %Y %H:%M"));
    ui.label(RichText::new(text).size(12.0).color(pal.muted));
}

/// The visible entries and the "Show N older entries" link.
fn history_list(
    ui: &mut egui::Ui,
    v: &View,
    state: &mut DetailState,
    entries: &[HistoryEntry],
    pair: &PairState,
) -> Option<Op> {
    if entries.is_empty() {
        ui.label(RichText::new("No history yet.").color(v.pal.muted));
        return None;
    }
    let mut op = None;
    let (shown, hidden) = split_history(entries, state.expanded);
    for e in shown {
        ui.add_space(6.0);
        if let Some(o) = entry(ui, v, state, e, pair) {
            op = Some(o);
        }
    }
    ui.add_space(6.0);
    if hidden > 0 {
        let s = if hidden == 1 { "entry" } else { "entries" };
        if ui.link(format!("Show {hidden} older {s}")).clicked() {
            state.expanded = true;
        }
    } else if state.expanded && entries.len() > HISTORY_DEFAULT && ui.link("Show fewer").clicked() {
        state.expanded = false;
    }
    op
}

fn entry(ui: &mut egui::Ui, v: &View, state: &mut DetailState, e: &HistoryEntry, pair: &PairState) -> Option<Op> {
    let pal = v.pal;
    let mut op = None;
    let line = |text: String| RichText::new(text).family(medium()).color(pal.text);
    ui.spacing_mut().item_spacing.y = 4.0;
    match *e {
        HistoryEntry::Completion(r) => {
            let label = if r.stage_label.is_empty() { stage_label(v.rule, r.stage_index) } else { r.stage_label.clone() };
            ui.horizontal(|ui| {
                ui.label(line(format!("{label} \u{b7} {}", fmt_date(r.completed_on))));
                if v.editing {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, icon::TRASH, "Delete entry").clicked() {
                            state.confirm = Some(Confirm::DeleteRecord(r.id.clone()));
                        }
                        if icon_button(ui, pal, icon::PENCIL_SIMPLE, "Edit entry").clicked() {
                            state.form = DetailForm::EditRecord {
                                id: r.id.clone(),
                                date: r.completed_on,
                                stage: r.stage_index,
                                comment: r.comment.clone(),
                            };
                            state.error = None;
                        }
                    });
                }
            });
            if !r.comment.is_empty() {
                ui.label(RichText::new(&r.comment).color(pal.text));
            }
            by_line(ui, pal, &r.by, r.at);
            let tolerance = v.data.settings.variance_tolerance_days;
            let variance = completion_variance(v.rule, pair, &r.id).and_then(|d| variance_tag(d, tolerance));
            if r.unconfirmed || variance.is_some() {
                ui.horizontal(|ui| {
                    if r.unconfirmed {
                        tag(ui, "unconfirmed \u{2013} from Excel", pal.yellow_bg, pal.yellow_fg);
                    }
                    if let Some(t) = &variance {
                        tag(ui, t, pal.grey_bg, pal.grey_fg);
                    }
                });
            }
            if state.confirm == Some(Confirm::DeleteRecord(r.id.clone())) {
                if let Some(yes) = confirm_row(ui, pal, "Delete this entry? This can't be undone.", "Delete") {
                    state.confirm = None;
                    if yes {
                        op = Some(Op::DeleteRecord(r.id.clone()));
                    }
                }
            }
        }
        HistoryEntry::Delay(n) => {
            ui.horizontal(|ui| {
                let stage = stage_label(v.rule, n.for_stage_index);
                ui.label(line(format!("{}  Delay note \u{b7} {stage}", icon::PAUSE)));
                if v.editing {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, icon::TRASH, "Delete delay note").clicked() {
                            state.confirm = Some(Confirm::DeleteDelay(n.id.clone()));
                        }
                    });
                }
            });
            ui.label(RichText::new(&n.note).color(pal.text));
            if let Some(d) = n.expected_on {
                ui.label(RichText::new(format!("Expected by {}", fmt_date(d))).color(pal.muted));
            }
            by_line(ui, pal, &n.by, n.at);
            if state.confirm == Some(Confirm::DeleteDelay(n.id.clone())) {
                if let Some(yes) = confirm_row(ui, pal, "Delete this delay note?", "Delete") {
                    state.confirm = None;
                    if yes {
                        op = Some(Op::DeleteDelay(n.id.clone()));
                    }
                }
            }
        }
        HistoryEntry::Note(n) => {
            ui.label(line(format!("{}  Note", icon::CHAT_CIRCLE)));
            ui.label(RichText::new(&n.text).color(pal.text));
            by_line(ui, pal, &n.by, n.at);
        }
    }
    op
}

/// Runs `op` through the audited operations; reports failures inline.
fn apply(cx: &mut Ctx, state: &mut DetailState, staff_id: &str, test_id: &str, op: Op) {
    let (s, t) = (staff_id, test_id);
    let (done, closes_form): (&str, bool) = match &op {
        Op::Complete { .. } => ("Marked complete", true),
        Op::Edit { .. } => ("Entry updated", true),
        Op::DeleteRecord(_) => ("Entry deleted", false),
        Op::Delay { .. } => ("Delay note added", true),
        Op::DeleteDelay(_) => ("Delay note deleted", false),
        Op::Skip(_) => ("Stage skipped", false),
        Op::Due(Some(_)) => ("Due date set", false),
        Op::Due(None) => ("Due date cleared", false),
        Op::Assign(Some(AssignState::Assigned)) => ("Assigned", false),
        Op::Assign(Some(AssignState::Na)) => ("Marked N/A", false),
        Op::Assign(None) => ("Unassigned", false),
    };
    let run = |d: &mut Data, a: &Actor| -> Result<(), OpError> {
        match op {
            Op::Complete { date, stage, comment } => ops::add_completion(d, a, s, t, stage, date, &comment).map(drop),
            Op::Edit { id, date, stage, comment } => ops::edit_record(d, a, &id, date, stage, &comment),
            Op::DeleteRecord(id) => ops::delete_record(d, a, &id),
            Op::Delay { note, expected } => ops::add_delay_note(d, a, s, t, stage_for_delay(d, s, t), &note, expected).map(drop),
            Op::DeleteDelay(id) => ops::delete_delay_note(d, a, &id),
            Op::Skip(stage) => ops::set_skipped(d, a, s, t, stage, true),
            Op::Due(due) => ops::set_due_override(d, a, s, t, due),
            Op::Assign(st) => ops::set_assignment(d, a, s, t, st),
        }
    };
    match cx.session.mutate(cx.now, run) {
        Some(Ok(())) => {
            state.error = None;
            if closes_form {
                state.form = DetailForm::None;
            }
            cx.toasts.push(Toast::new(done));
        }
        Some(Err(OpError::FutureDate)) => state.error = Some("Completion date can't be in the future".to_owned()),
        Some(Err(e)) => state.error = Some(format!("Couldn't save: {e}")),
        None => state.error = Some("Start editing to make changes".to_owned()),
    }
}

/// The stage a new delay note attaches to: the current next-due stage.
fn stage_for_delay(d: &Data, staff_id: &str, test_id: &str) -> u32 {
    next_due(d.rule_for_test(test_id), &pair_state(d, staff_id, test_id), &d.settings).next_stage_index
}
