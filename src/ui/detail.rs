//! Competency detail panel (slides in from the right). Placeholder until Task 13.

use egui_phosphor::regular as icon;

use crate::ui::app::Ctx;
use crate::ui::theme::heading;
use crate::ui::widgets::icon_button;

#[derive(Default)]
pub(crate) struct DetailState {}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, _state: &mut DetailState) {
    let Some((staff_id, test_id)) = cx.open_detail.clone() else { return };
    ui.horizontal(|ui| {
        let data = cx.session.data();
        let name = data.staff(&staff_id).map_or("Unknown", |s| s.name.as_str());
        heading(ui, name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, cx.pal, icon::X, "Close panel").clicked() {
                *cx.open_detail = None;
            }
        });
    });
    if let Some((sec, test)) = cx.session.data().test(&test_id) {
        ui.label(egui::RichText::new(format!("{} \u{b7} {}", test.name, sec.name)).color(cx.pal.muted));
    }
}
