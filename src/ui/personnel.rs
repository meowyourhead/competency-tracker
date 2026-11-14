//! Personnel list, editor and profile. Placeholder until Task 14.

use crate::model::Id;
use crate::ui::app::Ctx;
use crate::ui::theme::heading;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct PersonnelState {
    /// Person whose profile is open (set by search navigation).
    pub profile: Option<Id>,
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut PersonnelState) {
    heading(ui, "Personnel");
    if let Some(staff) = state.profile.as_deref().and_then(|id| cx.session.data().staff(id)) {
        ui.label(egui::RichText::new(&staff.name).color(cx.pal.muted));
    }
    empty_state(ui, "Nothing here yet", "The staff list arrives in a later step.");
}
