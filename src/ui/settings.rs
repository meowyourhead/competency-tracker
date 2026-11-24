//! Settings: a segmented control over four tabs: sections and test systems,
//! schedule rules, general settings, and import / export. Each tab lives in
//! its own file under `settings/`. Every change needs edit mode; exports and
//! "Open folder" work while viewing.

mod general;
mod import_export;
mod review;
mod schedule;
mod sections_tests;

use egui::{Button, Frame, Margin, RichText, Stroke};

use crate::model::Data;
use crate::ops::{Actor, OpError};
use crate::ui::app::{Ctx, Toast};
use crate::ui::import_wizard::{self, ImportWizard};
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::segmented;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsTab {
    #[default]
    Sections,
    Schedule,
    General,
    Import,
}

impl SettingsTab {
    const ALL: [SettingsTab; 4] = [
        SettingsTab::Sections,
        SettingsTab::Schedule,
        SettingsTab::General,
        SettingsTab::Import,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::Sections => "Sections & tests",
            SettingsTab::Schedule => "Schedule",
            SettingsTab::General => "General",
            SettingsTab::Import => "Import & export",
        }
    }
}

#[derive(Default)]
pub(crate) struct SettingsState {
    pub tab: SettingsTab,
    /// The Excel import wizard, while open.
    pub wizard: Option<ImportWizard>,
    sections: sections_tests::SectionsTab,
    schedule: schedule::ScheduleTab,
    general: general::GeneralTab,
    import: import_export::ImportTab,
}

pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut SettingsState) {
    if let Some(wizard) = &mut state.wizard {
        if ui
            .add(Button::new(RichText::new("\u{2190} Settings").color(cx.pal.muted)).frame(false))
            .clicked()
        {
            state.wizard = None;
            return;
        }
        ui.add_space(4.0);
        if import_wizard::show(ui, cx, wizard) {
            state.wizard = None;
            state.tab = SettingsTab::Import;
        }
        return;
    }
    tabs(ui, cx.pal, &mut state.tab);
    if !cx.session.is_editing() {
        ui.add_space(4.0);
        ui.label(
            RichText::new("You're viewing. Start editing to change settings.").color(cx.pal.muted),
        );
    }
    ui.add_space(14.0);
    match state.tab {
        SettingsTab::Sections => sections_tests::show(ui, cx, &mut state.sections),
        SettingsTab::Schedule => schedule::show(ui, cx, &mut state.schedule),
        SettingsTab::General => general::show(ui, cx, &mut state.general),
        SettingsTab::Import => {
            if import_export::show(ui, cx, &mut state.import) {
                state.wizard = Some(ImportWizard::default());
            }
        }
    }
}

fn tabs(ui: &mut egui::Ui, pal: &Palette, tab: &mut SettingsTab) {
    let labels = SettingsTab::ALL.map(SettingsTab::label);
    let current = SettingsTab::ALL.iter().position(|t| t == tab).unwrap_or(0);
    if let Some(i) = segmented(ui, pal, current, &labels) {
        *tab = SettingsTab::ALL[i];
    }
}

/// A titled card spanning the page width.
pub(super) fn card<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let inner = Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(title)
                    .family(medium())
                    .size(15.5)
                    .color(pal.text),
            );
            ui.add_space(6.0);
            add(ui)
        })
        .inner;
    ui.add_space(12.0);
    inner
}

/// Runs an audited operation; a toast reports failure (prefixed by `fail`),
/// success (`done`, unless empty) or that edit mode is needed.
pub(super) fn apply<R>(
    cx: &mut Ctx,
    done: &str,
    fail: &str,
    f: impl FnOnce(&mut Data, &Actor) -> Result<R, OpError>,
) -> Option<R> {
    let msg = match cx.session.mutate(cx.now, f) {
        Some(Ok(r)) => {
            if !done.is_empty() {
                cx.toasts.push(Toast::new(done));
            }
            return Some(r);
        }
        Some(Err(e)) => format!("{fail}: {e}"),
        None => "Start editing to make changes".to_owned(),
    };
    cx.toasts.push(Toast::new(msg));
    None
}
