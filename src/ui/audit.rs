//! Audit log. Placeholder until Task 15.

use crate::ui::app::Ctx;
use crate::ui::theme::heading;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct AuditState {}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, _state: &mut AuditState) {
    heading(ui, "Audit log");
    let n = cx.session.data().audit.len();
    empty_state(ui, "Nothing here yet", &format!("{n} recorded changes. The log view arrives in a later step."));
}
