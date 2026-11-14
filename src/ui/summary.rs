//! Summary (home) page. Placeholder until Task 12.

use crate::dates::fmt_date;
use crate::ui::app::Ctx;
use crate::ui::theme::heading;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub(crate) struct SummaryState {}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, _state: &mut SummaryState) {
    heading(ui, "Summary");
    ui.label(egui::RichText::new(fmt_date(cx.today)).color(cx.pal.muted));
    empty_state(ui, "Nothing here yet", "The due list arrives in a later step.");
}
