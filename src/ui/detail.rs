//! Competency detail panel: one person and one test system. Header with the
//! next-due pill, the actions (edit mode only), and the history, newest first.

use std::collections::HashSet;

use chrono::NaiveDate;
use egui::{
    Align, Button, Color32, ComboBox, Frame, Key, Layout, Margin, Pos2, RichText, ScrollArea,
    Stroke, TextEdit,
};
use egui_phosphor::regular as icon;

use crate::dates::fmt_date;
use crate::model::{AssignState, Data, Id, Rule, Ts};
use crate::ops::{self, Actor, OpError};
use crate::query::{history, split_history, HistoryEntry, HISTORY_DEFAULT};
use crate::schedule::{
    classify, completion_variance, next_due, stage_label, variance_tag, DueInfo, DueSource,
    PairState,
};
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::{heading, medium, Palette};
use crate::ui::timeline::{self, stage_chip, stage_color, stage_kind};
use crate::ui::widgets::{date_picker, due_pill, icon_button, section_chip, section_color};

#[derive(Default)]
pub(crate) enum DetailForm {
    #[default]
    None,
    Complete {
        date: NaiveDate,
        stage: u32,
        comment: String,
    },
    Delay {
        note: String,
        expected: Option<NaiveDate>,
    },
    EditRecord {
        id: Id,
        date: NaiveDate,
        stage: u32,
        comment: String,
    },
}

/// An action waiting for an inline "are you sure?".
#[derive(Clone, PartialEq)]
enum Confirm {
    Unassign,
    /// Skip this stage index.
    Skip(u32),
    DeleteRecord(Id),
    DeleteDelay(Id),
}

#[derive(Default)]
pub(crate) struct DetailState {
    /// Show every history entry, not just the latest four.
    pub expanded: bool,
    /// History entries opened to show who recorded them, comments and actions.
    open_entries: HashSet<Id>,
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
    Complete {
        date: NaiveDate,
        stage: u32,
        comment: String,
    },
    Edit {
        id: Id,
        date: NaiveDate,
        stage: u32,
        comment: String,
    },
    DeleteRecord(Id),
    Delay {
        note: String,
        expected: Option<NaiveDate>,
    },
    DeleteDelay(Id),
    Skip(u32),
    Due(Option<NaiveDate>),
    Assign(Option<AssignState>),
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut DetailState) {
    let Some(target) = cx.open_detail.clone() else {
        return;
    };
    if state.target.as_ref() != Some(&target) {
        *state = DetailState {
            target: Some(target.clone()),
            ..DetailState::default()
        };
    }
    // Esc closes the panel, unless it is closing a popup, a modal or a text field.
    let busy = egui::Popup::is_any_open(ui.ctx())
        || ui.memory(|m| m.focused().is_some() || m.top_modal_layer().is_some());
    if ui.input(|i| i.key_pressed(Key::Escape)) && !busy {
        *cx.open_detail = None;
        return;
    }
    if !cx.session.is_editing() {
        // Forms and confirmations belong to edit mode only.
        state.form = DetailForm::None;
        state.confirm = None;
        state.due_pick = None;
    }
    let (staff_id, test_id) = target;
    let mut close = false;
    let mut op = None;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let data = cx.session.data();
        let (Some(staff), Some((sec, test))) = (data.staff(&staff_id), data.test(&test_id)) else {
            close = header(
                ui,
                cx.pal,
                "Not found",
                "This person or test no longer exists.",
                None,
            );
            return;
        };
        let color = section_color(data, &sec.id).unwrap_or(cx.pal.muted);
        close = header(
            ui,
            cx.pal,
            &staff.name,
            &test.name,
            Some((&sec.name, color)),
        );
        let rule = data.rule_for_test(&test_id);
        let pair = pair_state(data, &staff_id, &test_id);
        let assign = staff.assignments.get(&test_id).map(|a| a.state);
        let info = next_due(rule, &pair, &data.settings);
        let editing = cx.session.is_editing();
        let view = View {
            pal: cx.pal,
            data,
            rule,
            editing,
            today: cx.today,
            info: &info,
        };
        ui.add_space(6.0);
        match assign {
            Some(AssignState::Assigned) => {
                ui.horizontal(|ui| {
                    let status = classify(info.due_on, cx.today, data.settings.due_soon_days);
                    let delayed = data.delay_notes.iter().any(|n| {
                        n.staff_id == staff_id
                            && n.test_id == test_id
                            && n.for_stage_index == info.next_stage_index
                    });
                    due_pill(ui, cx.pal, &info, status, delayed);
                    ui.label(
                        RichText::new(format!("for {}", info.next_stage_label)).color(cx.pal.muted),
                    );
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
        // An entry being edited shows its own error, beside its form.
        if !editing_entry(state) {
            if let Some(e) = &state.error {
                ui.label(RichText::new(e).color(cx.pal.red_fg));
            }
        }
        ui.add_space(14.0);
        section_title(ui, cx.pal, "Timeline");
        timeline::show(
            ui,
            cx.pal,
            rule,
            &pair,
            &info,
            cx.today,
            data.settings.due_soon_days,
            assign == Some(AssignState::Assigned),
        );
        ui.add_space(18.0);
        section_title(ui, cx.pal, "History");
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
    info: &'a DueInfo,
}

/// A history entry's Edit form is open (it shows inside that entry).
fn editing_entry(state: &DetailState) -> bool {
    matches!(state.form, DetailForm::EditRecord { .. })
}

/// Name and subtitle (with the section's chip after it, when given) and the
/// close button; returns true when closed.
fn header(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    subtitle: &str,
    section: Option<(&str, Color32)>,
) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        heading(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = icon_button(ui, pal, icon::X, "Close panel").clicked();
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(subtitle).color(pal.muted));
        if let Some((name, color)) = section {
            section_chip(ui, pal, name, color);
        }
    });
    close
}

fn pair_state<'a>(data: &'a Data, staff_id: &str, test_id: &str) -> PairState<'a> {
    let is = |s: &str, t: &str| s == staff_id && t == test_id;
    PairState {
        records: data
            .records
            .iter()
            .filter(|r| is(&r.staff_id, &r.test_id))
            .collect(),
        skipped: data
            .skipped
            .iter()
            .filter(|s| is(&s.staff_id, &s.test_id))
            .map(|s| s.stage_index)
            .collect(),
        override_: data
            .due_overrides
            .iter()
            .filter(|o| is(&o.staff_id, &o.test_id))
            .max_by_key(|o| o.at),
        assigned_on: data
            .staff(staff_id)
            .and_then(|s| s.assignments.get(test_id))
            .map(|a| a.since),
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
    ui.horizontal_wrapped(|ui| {
        // A button that doesn't fit moves to the next line whole.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        match assign {
            Some(AssignState::Assigned) => {
                if ui.add_enabled(on, Button::new("Mark complete")).clicked() {
                    state.form = DetailForm::Complete {
                        date: v.today,
                        stage: info.next_stage_index,
                        comment: String::new(),
                    };
                    state.error = None;
                }
                if ui.add_enabled(on, Button::new("Add delay note")).clicked() {
                    state.form = DetailForm::Delay {
                        note: String::new(),
                        expected: None,
                    };
                    state.error = None;
                }
                if ui
                    .add_enabled(on, Button::new("Skip stage"))
                    .on_hover_text(format!("Skip {}", info.next_stage_label))
                    .clicked()
                {
                    state.confirm = Some(Confirm::Skip(info.next_stage_index));
                }
                if ui.add_enabled(on, Button::new("Set due date")).clicked() {
                    state.due_pick = Some(info.due_on.unwrap_or(v.today));
                }
                // A plain button (not a nested ui), so the row can wrap it.
                let menu = ui.add_enabled(
                    on,
                    Button::new(format!("Mark N/A / Unassign  {}", icon::CARET_DOWN)),
                );
                egui::Popup::menu(&menu).show(|ui| {
                    if ui.button("Mark N/A").clicked() {
                        op = Some(Op::Assign(Some(AssignState::Na)));
                    }
                    if ui.button("Unassign").clicked() {
                        state.confirm = Some(Confirm::Unassign);
                    }
                });
            }
            Some(AssignState::Na) | None => {
                if ui
                    .add_enabled(on, Button::new(format!("{}  Assign", icon::PLUS)))
                    .clicked()
                {
                    op = Some(Op::Assign(Some(AssignState::Assigned)));
                }
                if assign.is_some() && ui.add_enabled(on, Button::new("Unassign")).clicked() {
                    state.confirm = Some(Confirm::Unassign);
                }
            }
        }
    });
    if !on {
        ui.label(
            RichText::new("Start editing to make changes.")
                .color(v.pal.muted)
                .size(12.5),
        );
    }
    if assign != Some(AssignState::Assigned) {
        // Only an assigned test can be completed, delayed, skipped or rescheduled.
        if !matches!(state.form, DetailForm::EditRecord { .. }) {
            state.form = DetailForm::None;
        }
        state.due_pick = None;
        if matches!(state.confirm, Some(Confirm::Skip(_))) {
            state.confirm = None;
        }
    }
    match state.confirm.clone() {
        Some(Confirm::Unassign) => {
            if let Some(yes) = confirm_row(
                ui,
                v.pal,
                "Remove this assignment? Its history is kept.",
                "Unassign",
            ) {
                state.confirm = None;
                if yes {
                    op = Some(Op::Assign(None));
                }
            }
        }
        Some(Confirm::Skip(stage)) => {
            let text = format!("Skip {}?", stage_label(v.rule, stage));
            if let Some(yes) = confirm_row(ui, v.pal, &text, "Skip") {
                state.confirm = None;
                if yes {
                    op = Some(Op::Skip(stage));
                }
            }
        }
        _ => {}
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
                let has_override = matches!(info.source, DueSource::Manual | DueSource::Excel);
                if ui
                    .add_enabled(on && has_override, Button::new("Clear"))
                    .clicked()
                {
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
    // Editing an entry happens in the entry itself, down in the history.
    if !editing_entry(state) {
        if let Some(o) = form(ui, v, state, info) {
            op = Some(o);
        }
    }
    op
}

/// The open Mark complete / delay note / edit record form.
fn form(ui: &mut egui::Ui, v: &View, state: &mut DetailState, info: &DueInfo) -> Option<Op> {
    let mut op = None;
    let mut cancel = false;
    let on = v.editing;
    let max_stage = info
        .next_stage_index
        .max(v.rule.stages.len().saturating_sub(1) as u32)
        + 1;
    match &mut state.form {
        DetailForm::None => return None,
        DetailForm::Complete {
            date,
            stage,
            comment,
        } => card(ui, v.pal, |ui| {
            ui.label(RichText::new("Mark complete").family(medium()));
            date_stage_comment(ui, v.rule, date, stage, comment, max_stage, "complete");
            ui.horizontal(|ui| {
                if ui.add_enabled(on, Button::new("Save")).clicked() {
                    op = Some(Op::Complete {
                        date: *date,
                        stage: *stage,
                        comment: comment.trim().to_owned(),
                    });
                }
                cancel = ui.button("Cancel").clicked();
            });
        }),
        DetailForm::EditRecord {
            id,
            date,
            stage,
            comment,
        } => card(ui, v.pal, |ui| {
            ui.label(RichText::new("Edit entry").family(medium()));
            let max = max_stage.max(*stage + 1);
            date_stage_comment(ui, v.rule, date, stage, comment, max, "edit");
            ui.horizontal(|ui| {
                if ui.add_enabled(on, Button::new("Save")).clicked() {
                    op = Some(Op::Edit {
                        id: id.clone(),
                        date: *date,
                        stage: *stage,
                        comment: comment.trim().to_owned(),
                    });
                }
                cancel = ui.button("Cancel").clicked();
            });
        }),
        DetailForm::Delay { note, expected } => card(ui, v.pal, |ui| {
            ui.label(
                RichText::new(format!("Delay note for {}", info.next_stage_label)).family(medium()),
            );
            ui.add(
                TextEdit::multiline(note)
                    .hint_text("Reason")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
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
                if ui
                    .add_enabled(on && !note.trim().is_empty(), Button::new("Save"))
                    .clicked()
                {
                    op = Some(Op::Delay {
                        note: note.trim().to_owned(),
                        expected: *expected,
                    });
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
        ComboBox::from_id_salt(("stage", salt))
            .selected_text(stage_label(rule, *stage))
            .show_ui(ui, |ui| {
                for i in 0..=max_stage {
                    ui.selectable_value(stage, i, stage_label(rule, i));
                }
            });
    });
    ui.add(
        TextEdit::multiline(comment)
            .hint_text("Comment (optional)")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
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

fn section_title(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .family(medium())
            .size(15.0)
            .color(pal.text),
    );
    ui.add_space(2.0);
}

/// Who recorded an entry and when; only shown once the entry is opened.
fn by_line(ui: &mut egui::Ui, pal: &Palette, by: &str, at: Ts) {
    let text = format!(
        "Recorded by {by} on {} at {}",
        fmt_date(at.date_naive()),
        at.format("%H:%M")
    );
    ui.label(RichText::new(text).size(12.5).color(pal.muted));
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
    // The rail joining the entries' dots, drawn behind them once they're placed.
    let rail = ui.painter().add(egui::Shape::Noop);
    let mut dots = Vec::with_capacity(shown.len());
    for e in shown {
        ui.add_space(4.0);
        let (dot, o) = entry(ui, v, state, e, pair);
        dots.push(dot);
        if let Some(o) = o {
            op = Some(o);
        }
    }
    if let (Some(first), Some(last)) = (dots.first(), dots.last()) {
        // Older entries continue below: the rail runs on, fading out.
        let end = if hidden > 0 { last.y + 22.0 } else { last.y };
        ui.painter().set(
            rail,
            egui::Shape::line_segment(
                [*first, Pos2::new(first.x, end)],
                Stroke::new(2.0, v.pal.border),
            ),
        );
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

/// Width of the column holding the rail and the entries' dots.
const RAIL_W: f32 = 22.0;

/// One history entry beside the rail: what matters (the completion date
/// and stage) on its first line, and everything else (who recorded it and
/// when, the comment, edit and delete) behind its "details" toggle.
/// Returns the centre of its dot on the rail.
fn entry(
    ui: &mut egui::Ui,
    v: &View,
    state: &mut DetailState,
    e: &HistoryEntry,
    pair: &PairState,
) -> (Pos2, Option<Op>) {
    let pal = v.pal;
    let mut op = None;
    let id = match *e {
        HistoryEntry::Completion(r) => r.id.clone(),
        HistoryEntry::Delay(n) => n.id.clone(),
        HistoryEntry::Note(n) => n.id.clone(),
    };
    let open = state.open_entries.contains(&id);
    let mut toggle = false;
    let lead = |text: String| {
        RichText::new(text)
            .family(medium())
            .size(15.0)
            .color(pal.text)
    };
    let row_top = ui.cursor().top();
    let (dot_color, hollow) = match *e {
        HistoryEntry::Completion(r) => (stage_color(pal, stage_kind(v.rule, r.stage_index)), false),
        HistoryEntry::Delay(_) => (pal.yellow_fg, true),
        HistoryEntry::Note(_) => (pal.muted, true),
    };
    ui.horizontal_top(|ui| {
        ui.add_space(RAIL_W);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let summary = match *e {
                    HistoryEntry::Completion(r) => {
                        let label = if r.stage_label.is_empty() {
                            stage_label(v.rule, r.stage_index)
                        } else {
                            r.stage_label.clone()
                        };
                        let date = fmt_date(r.completed_on);
                        ui.label(lead(date.clone()));
                        stage_chip(ui, pal, v.rule, r.stage_index, &label);
                        let tolerance = v.data.settings.variance_tolerance_days;
                        if let Some(t) = completion_variance(v.rule, pair, &r.id)
                            .and_then(|d| variance_tag(d, tolerance))
                        {
                            ui.label(RichText::new(t).size(12.5).color(pal.muted));
                        }
                        if r.unconfirmed {
                            ui.label(RichText::new("unconfirmed").size(12.5).color(pal.yellow_fg))
                                .on_hover_text("Imported from Excel and not yet confirmed");
                        }
                        format!("{label}, {date}")
                    }
                    HistoryEntry::Delay(n) => {
                        ui.label(lead("Delay note".to_owned()));
                        let stage = stage_label(v.rule, n.for_stage_index);
                        stage_chip(ui, pal, v.rule, n.for_stage_index, &stage);
                        format!("delay note for {stage}")
                    }
                    HistoryEntry::Note(n) => {
                        ui.label(lead("Note".to_owned()));
                        ui.label(
                            RichText::new(fmt_date(n.at.date_naive()))
                                .size(12.5)
                                .color(pal.muted),
                        );
                        format!("note from {}", fmt_date(n.at.date_naive()))
                    }
                };
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (glyph, verb) = if open {
                        (icon::CARET_UP, "Hide")
                    } else {
                        (icon::CARET_DOWN, "Show")
                    };
                    toggle = icon_button(ui, pal, glyph, &format!("{verb} details of {summary}"))
                        .clicked();
                });
            });
            // A delay's reason and a note's text are the point of them, so
            // they stay in view.
            match *e {
                HistoryEntry::Delay(n) => {
                    ui.label(RichText::new(&n.note).color(pal.text));
                    if let Some(d) = n.expected_on {
                        ui.label(
                            RichText::new(format!("Expected by {}", fmt_date(d)))
                                .size(12.5)
                                .color(pal.muted),
                        );
                    }
                }
                HistoryEntry::Note(n) => {
                    ui.label(RichText::new(&n.text).color(pal.text));
                }
                HistoryEntry::Completion(_) => {}
            }
            if open {
                details(ui, v, state, e, &mut op);
            }
        });
    });
    if toggle && !state.open_entries.remove(&id) {
        state.open_entries.insert(id);
    }
    let dot = Pos2::new(ui.max_rect().left() + 8.0, row_top + 16.0);
    let p = ui.painter();
    p.circle_filled(dot, 7.5, pal.card);
    if hollow {
        p.circle_stroke(dot, 4.5, Stroke::new(2.0, dot_color));
    } else {
        p.circle_filled(dot, 5.5, dot_color);
    }
    (dot, op)
}

/// An opened entry: comment or note text, who recorded it, and (while
/// editing) its actions with their confirmation.
fn details(
    ui: &mut egui::Ui,
    v: &View,
    state: &mut DetailState,
    e: &HistoryEntry,
    op: &mut Option<Op>,
) {
    let pal = v.pal;
    Frame::new()
        .fill(pal.bg)
        .corner_radius(8)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            match *e {
                HistoryEntry::Completion(r) => {
                    if !r.comment.is_empty() {
                        ui.label(RichText::new(&r.comment).color(pal.text));
                    }
                    by_line(ui, pal, &r.by, r.at);
                    if r.unconfirmed {
                        ui.label(
                            RichText::new("Imported from Excel and not yet confirmed.")
                                .size(12.5)
                                .color(pal.yellow_fg),
                        );
                    }
                    let editing_this = matches!(
                        &state.form,
                        DetailForm::EditRecord { id, .. } if *id == r.id
                    );
                    if editing_this {
                        // The form opens right here, where Edit was clicked.
                        if let Some(o) = form(ui, v, state, v.info) {
                            *op = Some(o);
                        }
                        if let Some(e) = &state.error {
                            ui.label(RichText::new(e).color(pal.red_fg));
                        }
                    } else if v.editing {
                        ui.horizontal(|ui| {
                            let edit = Button::new(format!("{}  Edit", icon::PENCIL_SIMPLE));
                            if ui.add(edit).clicked() {
                                state.form = DetailForm::EditRecord {
                                    id: r.id.clone(),
                                    date: r.completed_on,
                                    stage: r.stage_index,
                                    comment: r.comment.clone(),
                                };
                                state.error = None;
                            }
                            let delete = Button::new(
                                RichText::new(format!("{}  Delete", icon::TRASH)).color(pal.red_fg),
                            );
                            if ui.add(delete).clicked() {
                                state.confirm = Some(Confirm::DeleteRecord(r.id.clone()));
                            }
                        });
                    }
                    if state.confirm == Some(Confirm::DeleteRecord(r.id.clone())) {
                        if let Some(yes) = confirm_row(
                            ui,
                            pal,
                            "Delete this entry? This can't be undone.",
                            "Delete",
                        ) {
                            state.confirm = None;
                            if yes {
                                *op = Some(Op::DeleteRecord(r.id.clone()));
                            }
                        }
                    }
                }
                HistoryEntry::Delay(n) => {
                    by_line(ui, pal, &n.by, n.at);
                    if v.editing {
                        let delete = Button::new(
                            RichText::new(format!("{}  Delete", icon::TRASH)).color(pal.red_fg),
                        );
                        if ui.add(delete).clicked() {
                            state.confirm = Some(Confirm::DeleteDelay(n.id.clone()));
                        }
                    }
                    if state.confirm == Some(Confirm::DeleteDelay(n.id.clone())) {
                        if let Some(yes) = confirm_row(ui, pal, "Delete this delay note?", "Delete")
                        {
                            state.confirm = None;
                            if yes {
                                *op = Some(Op::DeleteDelay(n.id.clone()));
                            }
                        }
                    }
                }
                HistoryEntry::Note(n) => by_line(ui, pal, &n.by, n.at),
            }
        });
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
        // A changed assignment makes any open form stale.
        Op::Assign(Some(AssignState::Assigned)) => ("Assigned", true),
        Op::Assign(Some(AssignState::Na)) => ("Marked N/A", true),
        Op::Assign(None) => ("Unassigned", true),
    };
    let run = |d: &mut Data, a: &Actor| -> Result<(), OpError> {
        match op {
            Op::Complete {
                date,
                stage,
                comment,
            } => ops::add_completion(d, a, s, t, stage, date, &comment).map(drop),
            Op::Edit {
                id,
                date,
                stage,
                comment,
            } => ops::edit_record(d, a, &id, date, stage, &comment),
            Op::DeleteRecord(id) => ops::delete_record(d, a, &id),
            Op::Delay { note, expected } => {
                ops::add_delay_note(d, a, s, t, stage_for_delay(d, s, t), &note, expected).map(drop)
            }
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
                state.due_pick = None;
                state.confirm = None;
            }
            cx.toasts.push(Toast::new(done));
        }
        Some(Err(OpError::FutureDate)) => {
            state.error = Some("Completion date can't be in the future".to_owned())
        }
        Some(Err(e)) => state.error = Some(format!("Couldn't save: {e}")),
        None => state.error = Some("Start editing to make changes".to_owned()),
    }
}

/// The stage a new delay note attaches to: the current next-due stage.
fn stage_for_delay(d: &Data, staff_id: &str, test_id: &str) -> u32 {
    next_due(
        d.rule_for_test(test_id),
        &pair_state(d, staff_id, test_id),
        &d.settings,
    )
    .next_stage_index
}
