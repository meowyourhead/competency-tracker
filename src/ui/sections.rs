//! Sections grid. Placeholder until Task 13.

use crate::model::Id;
use crate::ui::app::Ctx;
use crate::ui::theme::heading;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct SectionsState {
    /// Section to show (set by search navigation).
    pub section: Option<Id>,
    /// Test system to focus within the section.
    pub test: Option<Id>,
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SectionsState) {
    heading(ui, "Sections");
    let data = cx.session.data();
    if let Some(sec) = state.section.as_deref().and_then(|id| data.section(id)) {
        let test = state.test.as_deref().and_then(|t| data.test(t)).map(|(_, t)| t.name.as_str());
        let line = match test {
            Some(t) => format!("{} \u{b7} {t}", sec.name),
            None => sec.name.clone(),
        };
        ui.label(egui::RichText::new(line).color(cx.pal.muted));
    }
    empty_state(ui, "Nothing here yet", "The section grid arrives in a later step.");
}
