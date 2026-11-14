//! Settings. Placeholder until Task 15.

use crate::ui::app::Ctx;
use crate::ui::import_wizard::{self, ImportWizard};
use crate::ui::theme::heading;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct SettingsState {
    /// The Excel import wizard, while open.
    pub wizard: Option<ImportWizard>,
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SettingsState) {
    heading(ui, "Settings");
    if let Some(wizard) = &mut state.wizard {
        import_wizard::show(ui, cx, wizard);
        return;
    }
    empty_state(ui, "Nothing here yet", "Settings arrive in a later step.");
}
