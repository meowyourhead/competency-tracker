//! Settings → General: thresholds and timeouts (saved together with
//! "Save settings"), snapshot archiving, and the data folder location.

use egui::{Button, DragValue, Grid, RichText};

use super::{apply, card};
use crate::model::Settings;
use crate::ops;
use crate::snapshot::archive_old;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::Palette;

/// Defaults when an optional setting is switched on.
const INITIAL_DUE_DEFAULT: i64 = 30;
const ARCHIVE_DEFAULT: i64 = 90;

#[derive(Default)]
pub(super) struct GeneralTab {
    /// (settings as last seen in the data, unsaved edits).
    draft: Option<(Settings, Settings)>,
}

pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut GeneralTab) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let saved = cx.session.data().settings.clone();
    if st.draft.as_ref().is_none_or(|(base, _)| *base != saved) {
        st.draft = Some((saved.clone(), saved.clone()));
    }
    let (_, draft) = st.draft.as_mut().expect("synced above");
    let mut save = false;
    card(ui, pal, "Due dates and editing", |ui| {
        ui.add_enabled_ui(editing, |ui| fields(ui, pal, draft));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let dirty = *draft != saved;
            save = ui
                .add_enabled(editing && dirty, Button::new("Save settings"))
                .clicked();
            if ui.add_enabled(dirty, Button::new("Revert")).clicked() {
                *draft = saved.clone();
            }
            let defaults = Settings::default();
            let reset = ui
                .add_enabled(
                    editing && *draft != defaults,
                    Button::new("Reset to defaults"),
                )
                .on_hover_text("Fills in the default values; nothing changes until you save.");
            if reset.clicked() {
                *draft = defaults;
            }
        });
    });
    if save {
        let s = draft.clone();
        apply(cx, "Settings saved", "Couldn't save settings", |d, a| {
            ops::update_settings(d, a, s)
        });
    }
    archive(ui, cx, &saved);
    data_folder(ui, cx);
}

fn fields(ui: &mut egui::Ui, pal: &Palette, s: &mut Settings) {
    let hint = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).color(pal.muted));
    };
    Grid::new("general_settings")
        .num_columns(3)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Due soon");
            ui.add(
                DragValue::new(&mut s.due_soon_days)
                    .range(1..=365)
                    .suffix(" days"),
            );
            hint(ui, "Items due within this many days show yellow.");
            ui.end_row();

            ui.label("Date variance tolerance");
            ui.add(
                DragValue::new(&mut s.variance_tolerance_days)
                    .range(0..=365)
                    .suffix(" days"),
            );
            hint(
                ui,
                "Completions this close to their due date get no early / late tag.",
            );
            ui.end_row();

            ui.label("Inactivity prompt after");
            ui.add(
                DragValue::new(&mut s.inactivity_minutes)
                    .range(1..=240)
                    .suffix(" min"),
            );
            hint(
                ui,
                "Asks \u{201c}Are you still editing?\u{201d} after this long without activity.",
            );
            ui.end_row();

            ui.label("Prompt countdown");
            ui.add(
                DragValue::new(&mut s.inactivity_grace_minutes)
                    .range(1..=120)
                    .suffix(" min"),
            );
            hint(
                ui,
                "Editing stops if nobody answers the prompt within this time.",
            );
            ui.end_row();

            ui.label("Initial due");
            optional_days(
                ui,
                &mut s.initial_due_days,
                "After assignment",
                INITIAL_DUE_DEFAULT,
                0..=365,
            );
            hint(
                ui,
                "When off, a missing Initial shows \u{201c}Initial pending\u{201d}.",
            );
            ui.end_row();

            ui.label("Archive snapshots");
            optional_days(
                ui,
                &mut s.archive_after_days,
                "Older than",
                ARCHIVE_DEFAULT,
                7..=3650,
            );
            hint(ui, "Moves old daily snapshots to the archive folder.");
            ui.end_row();
        });
}

/// A checkbox that switches the setting on (with `default`) and the days field.
fn optional_days(
    ui: &mut egui::Ui,
    v: &mut Option<i64>,
    label: &str,
    default: i64,
    range: std::ops::RangeInclusive<i64>,
) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, label).changed() {
            *v = on.then_some(default);
        }
        if let Some(days) = v {
            ui.add(DragValue::new(days).range(range).suffix(" days"));
        }
    });
}

fn archive(ui: &mut egui::Ui, cx: &mut Ctx, saved: &Settings) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let mut run = false;
    card(ui, pal, "Archive", |ui| {
        let text = match saved.archive_after_days {
            Some(d) => format!("Snapshots older than {d} days can be moved to the archive folder. The newest is always kept."),
            None => "Turn on \u{201c}Archive snapshots\u{201d} above (and save) to archive old snapshots.".to_owned(),
        };
        ui.label(RichText::new(text).color(pal.muted));
        ui.add_space(4.0);
        let why = if saved.archive_after_days.is_none() {
            "Archiving is off"
        } else {
            "Start editing to make changes"
        };
        let button = ui.add_enabled(
            editing && saved.archive_after_days.is_some(),
            Button::new("Archive now"),
        );
        run = button.on_disabled_hover_text(why).clicked();
    });
    let Some(days) = saved.archive_after_days.filter(|_| run) else {
        return;
    };
    let msg = match archive_old(cx.session.data_dir(), days, cx.today) {
        Ok(0) => "No snapshots old enough to archive".to_owned(),
        Ok(1) => "Archived 1 snapshot".to_owned(),
        Ok(n) => format!("Archived {n} snapshots"),
        Err(e) => format!("Couldn't archive: {e}"),
    };
    cx.toasts.push(Toast::new(msg));
}

fn data_folder(ui: &mut egui::Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    let root = cx.session.data_dir().root.clone();
    let mut open = false;
    card(ui, pal, "Data folder", |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(root.display().to_string())
                        .monospace()
                        .color(pal.text),
                )
                .selectable(true),
            );
            open = ui.button("Open folder").clicked();
        });
        ui.label(
            RichText::new("To move the data folder, edit tracker.config.json next to the app.")
                .color(pal.muted),
        );
    });
    if open {
        if let Err(e) = std::process::Command::new("explorer.exe")
            .arg(&root)
            .spawn()
        {
            cx.toasts
                .push(Toast::new(format!("Couldn't open the folder: {e}")));
        }
    }
}
