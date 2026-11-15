//! Import wizard, step 2: review which names are the same person. Each group
//! shows its members, an editable name / joined date / title, any conflicts
//! between tabs, and a Split button.

use chrono::NaiveDate;
use egui::collapsing_header::CollapsingState;
use egui::{Frame, Margin, RichText, Stroke, TextEdit};

use crate::import::mapping::{split_group, MergeGroup};
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::date_picker;

fn needs_look(g: &MergeGroup) -> bool {
    g.members.len() > 1 || !g.conflicts.is_empty()
}

pub(super) fn show(ui: &mut egui::Ui, pal: &Palette, today: NaiveDate, groups: &mut Vec<MergeGroup>) {
    let (look, single): (Vec<usize>, Vec<usize>) = (0..groups.len()).partition(|&i| needs_look(&groups[i]));
    let mut split = None;
    ui.label(RichText::new(format!("Needs a look ({})", look.len())).family(medium()).size(15.5).color(pal.text));
    ui.add_space(4.0);
    if look.is_empty() {
        ui.label(RichText::new("No name appears on more than one tab.").color(pal.muted));
    }
    for &i in &look {
        group_card(ui, pal, today, groups, i, &mut split);
    }
    ui.add_space(6.0);
    let id = ui.make_persistent_id("import_single_names");
    CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            ui.label(RichText::new(format!("Single names ({})", single.len())).family(medium()).size(15.5).color(pal.text));
        })
        .body(|ui| {
            for &i in &single {
                group_card(ui, pal, today, groups, i, &mut split);
            }
        });
    if let Some(i) = split {
        split_group(groups, i);
        // The new single names inherit the old group's joined date and title: show them for editing.
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
        state.set_open(true);
        state.store(ui.ctx());
    }
}

fn group_card(ui: &mut egui::Ui, pal: &Palette, today: NaiveDate, groups: &mut [MergeGroup], idx: usize, split: &mut Option<usize>) {
    let g = &mut groups[idx];
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                for (sheet, _, raw) in &g.members {
                    Frame::new().fill(pal.sidebar).corner_radius(255).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
                        ui.label(RichText::new(format!("{sheet} \u{b7} {raw}")).size(12.0).color(pal.muted));
                    });
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(TextEdit::singleline(&mut g.name).desired_width(200.0));
                ui.label("Title");
                ui.add(TextEdit::singleline(&mut g.title).desired_width(140.0));
                ui.label("Joined");
                match &mut g.joined {
                    Some(d) => {
                        date_picker(ui, d, &format!("import_joined_{idx}"));
                        if ui.button("No date").clicked() {
                            g.joined = None;
                        }
                    }
                    None => {
                        ui.label(RichText::new("No date").color(pal.muted));
                        if ui.button("Set date").clicked() {
                            g.joined = Some(today);
                        }
                    }
                }
            });
            for c in &g.conflicts {
                ui.add_space(4.0);
                Frame::new().fill(pal.yellow_bg).corner_radius(6).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                    ui.label(RichText::new(c).color(pal.yellow_fg));
                });
            }
            ui.add_space(4.0);
            let one = g.members.len() < 2;
            let btn = ui.add_enabled(!one, egui::Button::new("Split"));
            if btn.on_disabled_hover_text("Only one name here").on_hover_text("Treat each name as a different person").clicked() {
                *split = Some(idx);
            }
        });
    ui.add_space(8.0);
}
