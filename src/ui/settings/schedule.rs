//! Settings → Schedule: one editor card per rule (the global rule first),
//! with its stages, a live preview line, and Save / Revert. Named rules can
//! be renamed and deleted; "New rule" copies the global rule.

use std::collections::HashMap;

use egui::{Button, DragValue, RichText, TextEdit};
use egui_phosphor::regular as icon;

use super::{apply, card};
use crate::model::{Data, Id, Rule, Stage};
use crate::ops::{self, OpError};
use crate::schedule::stage_label;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::Palette;
use crate::ui::widgets::icon_button;

#[derive(Default)]
pub(super) struct ScheduleTab {
    /// Unsaved edits per rule id.
    drafts: HashMap<Id, Draft>,
    confirm_delete: Option<Id>,
}

struct Draft {
    /// The rule as last seen in the data; a change there resets the draft.
    base: Rule,
    name: String,
    stages: Vec<Stage>,
}

impl Draft {
    fn new(r: &Rule) -> Draft {
        Draft {
            base: r.clone(),
            name: r.name.clone(),
            stages: r.stages.clone(),
        }
    }

    fn dirty(&self) -> bool {
        self.name != self.base.name || self.stages != self.base.stages
    }

    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.stages.is_empty()
            && self.stages.iter().all(|s| !s.label.trim().is_empty())
    }

    /// The first stage has no interval; only the last may repeat or count ordinally.
    fn normalized(&self) -> Vec<Stage> {
        let last = self.stages.len().saturating_sub(1);
        self.stages
            .iter()
            .enumerate()
            .map(|(i, s)| Stage {
                label: s.label.trim().to_owned(),
                interval_months: if i == 0 {
                    None
                } else {
                    Some(s.interval_months.unwrap_or(12).max(1))
                },
                repeats: i > 0 && i == last && s.repeats,
                ordinal: i > 0 && i == last && s.ordinal,
            })
            .collect()
    }
}

enum Act {
    Save(Id),
    Revert(Id),
    AskDelete(Id),
    CancelDelete,
    Delete(Id),
    New,
}

/// "Initial → +6 mo Semi-annual → +12 mo 1st Annual → every 12 mo".
pub(super) fn preview(stages: &[Stage]) -> String {
    let rule = Rule {
        id: String::new(),
        name: String::new(),
        stages: stages.to_vec(),
    };
    let mut parts: Vec<String> = Vec::new();
    for (i, s) in stages.iter().enumerate() {
        let label = stage_label(&rule, i as u32);
        if i == 0 {
            parts.push(label);
        } else {
            parts.push(format!("+{} mo {label}", s.interval_months.unwrap_or(0)));
        }
    }
    if let Some(last) = stages.last().filter(|s| s.repeats && stages.len() > 1) {
        parts.push(format!("every {} mo", last.interval_months.unwrap_or(0)));
    }
    parts.join(" \u{2192} ")
}

pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut ScheduleTab) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let data = cx.session.data();
    // Drop drafts of deleted rules; reset drafts whose rule changed underneath.
    st.drafts
        .retain(|id, _| data.rules.iter().any(|r| &r.id == id));
    for r in &data.rules {
        if st.drafts.get(&r.id).is_none_or(|d| d.base != *r) {
            st.drafts.insert(r.id.clone(), Draft::new(r));
        }
    }
    ui.label(
        RichText::new("A rule is an ordered list of stages. Each interval counts from the previous real completion; the last stage can repeat.")
            .color(pal.muted),
    );
    ui.add_space(10.0);
    let mut act = None;
    let mut order: Vec<&Rule> = data.rules.iter().collect();
    order.sort_by_key(|r| r.id != data.default_rule_id);
    for r in order {
        let is_default = r.id == data.default_rule_id;
        let used = data
            .sections
            .iter()
            .flat_map(|s| &s.tests)
            .filter(|t| t.rule_id.as_deref() == Some(r.id.as_str()))
            .count();
        let draft = st.drafts.get_mut(&r.id).expect("synced above");
        let confirming = st.confirm_delete.as_deref() == Some(r.id.as_str());
        let title = if is_default {
            "Global rule".to_owned()
        } else {
            r.name.clone()
        };
        card(ui, pal, &title, |ui| {
            if let Some(a) = rule_card(ui, pal, editing, &r.id, is_default, used, draft, confirming)
            {
                act = Some(a);
            }
        });
    }
    ui.horizontal(|ui| {
        let new = ui.add_enabled(editing, Button::new(format!("{}  New rule", icon::PLUS)));
        if new
            .on_disabled_hover_text("Start editing to make changes")
            .clicked()
        {
            act = Some(Act::New);
        }
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Assign a rule to specific tests in Sections & tests. Tests without one use Global.",
        )
        .color(pal.muted),
    );
    if let Some(a) = act {
        run(cx, st, a);
    }
}

#[allow(clippy::too_many_arguments)]
fn rule_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    id: &str,
    is_default: bool,
    used: usize,
    d: &mut Draft,
    confirming: bool,
) -> Option<Act> {
    let mut act = None;
    if is_default {
        ui.label(
            RichText::new("Global \u{b7} used by every test without its own rule").color(pal.muted),
        );
    } else {
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add_enabled(
                editing,
                TextEdit::singleline(&mut d.name).desired_width(220.0),
            );
            let s = if used == 1 { "" } else { "s" };
            ui.label(RichText::new(format!("Used by {used} test{s}")).color(pal.muted));
        });
    }
    ui.add_space(4.0);
    let last = d.stages.len().saturating_sub(1);
    let mut remove = None;
    ui.add_enabled_ui(editing, |ui| {
        for (i, s) in d.stages.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut s.label)
                        .hint_text("Stage name")
                        .desired_width(160.0),
                );
                if i == 0 {
                    ui.label(RichText::new("First stage \u{b7} no interval").color(pal.muted));
                    return;
                }
                let mut months = s.interval_months.unwrap_or(12);
                ui.add(
                    DragValue::new(&mut months)
                        .range(1..=120)
                        .prefix("+")
                        .suffix(" mo"),
                );
                s.interval_months = Some(months);
                ui.label(RichText::new("after the previous stage").color(pal.muted));
                if i == last {
                    ui.checkbox(&mut s.repeats, "Repeats");
                    ui.checkbox(&mut s.ordinal, "Ordinal (1st, 2nd\u{2026})");
                }
                if icon_button(ui, pal, icon::TRASH, &format!("Remove stage {}", s.label)).clicked()
                {
                    remove = Some(i);
                }
            });
        }
        if ui.button(format!("{}  Add stage", icon::PLUS)).clicked() {
            add_stage(&mut d.stages);
        }
    });
    if let Some(i) = remove {
        remove_stage(&mut d.stages, i);
    }
    ui.add_space(4.0);
    ui.label(RichText::new(preview(&d.normalized())).color(pal.text));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(editing && d.dirty() && d.valid(), Button::new("Save"))
            .clicked()
        {
            act = Some(Act::Save(id.to_owned()));
        }
        if ui.add_enabled(d.dirty(), Button::new("Revert")).clicked() {
            act = Some(Act::Revert(id.to_owned()));
        }
        if !is_default
            && !confirming
            && ui
                .add_enabled(editing, Button::new("Delete rule"))
                .clicked()
        {
            act = Some(Act::AskDelete(id.to_owned()));
        }
    });
    if confirming {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "Delete {}? Tests using it go back to Global.",
                    d.base.name
                ))
                .color(pal.red_fg),
            );
            if ui
                .add_enabled(
                    editing,
                    Button::new(RichText::new("Delete").color(pal.red_fg)),
                )
                .clicked()
            {
                act = Some(Act::Delete(id.to_owned()));
            }
            if ui.button("Cancel").clicked() {
                act = Some(Act::CancelDelete);
            }
        });
    }
    act
}

/// Appends a stage; the repeat / ordinal flags move to the new last stage.
fn add_stage(stages: &mut Vec<Stage>) {
    let multi = stages.len() > 1;
    let (repeats, ordinal) = match stages.last_mut() {
        Some(s) if multi => (
            std::mem::take(&mut s.repeats),
            std::mem::take(&mut s.ordinal),
        ),
        _ => (false, false),
    };
    let n = stages.len() + 1;
    stages.push(Stage {
        label: format!("Stage {n}"),
        interval_months: Some(12),
        repeats,
        ordinal,
    });
}

/// Removes a stage (never the first); removing the last hands its flags on.
fn remove_stage(stages: &mut Vec<Stage>, i: usize) {
    if i == 0 || i >= stages.len() {
        return;
    }
    let gone = stages.remove(i);
    if i == stages.len() && stages.len() > 1 {
        let last = stages.last_mut().expect("non-empty");
        last.repeats = gone.repeats;
        last.ordinal = gone.ordinal;
    }
}

/// "Custom rule N" with the first free N.
fn new_rule_name(data: &Data) -> String {
    (1..)
        .map(|n| format!("Custom rule {n}"))
        .find(|n| !data.rules.iter().any(|r| &r.name == n))
        .expect("a free name")
}

fn run(cx: &mut Ctx, st: &mut ScheduleTab, act: Act) {
    match act {
        Act::Save(id) => {
            let Some(d) = st.drafts.get(&id) else { return };
            let (name, stages) = (d.name.clone(), d.normalized());
            apply(cx, "Rule saved", "Couldn't save the rule", |data, a| {
                ops::update_rule(data, a, &id, &name, stages)
            });
        }
        Act::Revert(id) => {
            st.drafts.remove(&id);
        }
        Act::AskDelete(id) => st.confirm_delete = Some(id),
        Act::CancelDelete => st.confirm_delete = None,
        Act::Delete(id) => {
            st.confirm_delete = None;
            let msg = match cx
                .session
                .mutate(cx.now, |d, a| ops::delete_rule(d, a, &id))
            {
                Some(Ok(())) => "Rule deleted".to_owned(),
                Some(Err(OpError::IsDefaultRule)) => "The global rule can't be deleted".to_owned(),
                Some(Err(e)) => format!("Couldn't delete the rule: {e}"),
                None => "Start editing to make changes".to_owned(),
            };
            cx.toasts.push(Toast::new(msg));
        }
        Act::New => {
            let data = cx.session.data();
            let name = new_rule_name(data);
            let stages = data
                .rules
                .iter()
                .find(|r| r.id == data.default_rule_id)
                .map(|r| r.stages.clone())
                .unwrap_or_default();
            let done = format!("Added {name}");
            apply(cx, &done, "Couldn't add the rule", |d, a| {
                ops::add_rule(d, a, &name, stages)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_of_default_rule() {
        assert_eq!(
            preview(&Rule::default_global().stages),
            "Initial \u{2192} +6 mo Semi-annual \u{2192} +12 mo 1st Annual \u{2192} every 12 mo"
        );
    }

    #[test]
    fn adding_and_removing_stages_moves_repeat_flags() {
        let mut stages = Rule::default_global().stages;
        add_stage(&mut stages);
        assert!(!stages[2].repeats && stages[3].repeats && stages[3].ordinal);
        remove_stage(&mut stages, 3);
        assert!(stages[2].repeats && stages[2].ordinal);
        remove_stage(&mut stages, 0);
        assert_eq!(stages.len(), 3, "the first stage stays");
    }
}
