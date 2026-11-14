//! Excel import wizard. Placeholder until Task 16.

use crate::ui::app::Ctx;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct ImportWizard {}

pub(crate) fn show(ui: &mut egui::Ui, _cx: &mut Ctx, _wizard: &mut ImportWizard) {
    empty_state(ui, "Import from Excel", "The import wizard arrives in a later step.");
}
