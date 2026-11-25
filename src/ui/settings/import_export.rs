//! Settings → Import & export: launching the Excel import wizard, the review
//! of unconfirmed imported records (`review.rs`), and the status / history
//! exports to `.xlsx`.

use std::path::PathBuf;

use egui::{Button, RichText};
use egui_phosphor::regular as icon;

use super::{card, review};
use crate::dates::fmt_date;
use crate::export::{export_history, export_status};
use crate::ui::app::{Ctx, Toast};

#[derive(Default)]
pub(super) struct ImportTab {
    review: review::Review,
}

#[derive(Clone, Copy)]
enum Export {
    Status,
    History,
}

/// Returns true when the user asked to open the import wizard.
pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut ImportTab) -> bool {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    let mut wizard = false;
    card(ui, pal, "Import from Excel", |ui| {
        ui.label(
            RichText::new(
                "Read your existing Excel tracker. You review everything before it is saved.",
            )
            .color(pal.muted),
        );
        ui.add_space(4.0);
        let button = ui.add_enabled(
            editing,
            Button::new(format!("{}  Import from Excel\u{2026}", icon::FILE_XLS)),
        );
        wizard = button
            .on_disabled_hover_text("Start editing to make changes")
            .clicked();
    });
    card(ui, pal, "Imported records to review", |ui| {
        review::show(ui, cx, &mut st.review)
    });
    let mut export = None;
    card(ui, pal, "Export", |ui| {
        ui.label(
            RichText::new(
                "Save a simple Excel copy. For audits and inspections, use the Audit report tab.",
            )
            .color(pal.muted),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button(format!("{}  Export status\u{2026}", icon::EXPORT))
                .clicked()
            {
                export = Some(Export::Status);
            }
            if ui
                .button(format!("{}  Export history\u{2026}", icon::EXPORT))
                .clicked()
            {
                export = Some(Export::History);
            }
        });
    });
    if let Some(kind) = export {
        run_export(cx, kind);
    }
    wizard
}

/// Asks where to save (a native dialog), then writes the workbook.
fn run_export(cx: &mut Ctx, kind: Export) {
    let word = match kind {
        Export::Status => "status",
        Export::History => "history",
    };
    let default_name = format!("Competency {word} {}.xlsx", fmt_date(cx.today));
    let Some(path) = save_dialog(&default_name, "Excel workbook", "xlsx") else {
        return;
    };
    let data = cx.session.data();
    let res = match kind {
        Export::Status => export_status(data, cx.today, &path),
        Export::History => export_history(data, &path),
    };
    let msg = match res {
        Ok(()) => format!("Exported to {}", path.display()),
        Err(e) => format!("Couldn't export: {e}"),
    };
    cx.toasts.push(Toast::new(msg));
}

/// Asks where to save a file of this kind; the path always ends in `ext`.
pub(super) fn save_dialog(default_name: &str, kind: &str, ext: &str) -> Option<PathBuf> {
    let mut path = rfd::FileDialog::new()
        .add_filter(kind, &[ext])
        .set_file_name(default_name)
        .save_file()?;
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    {
        // Not `set_extension`: a name may have dots of its own.
        let mut name = path.into_os_string();
        name.push(".");
        name.push(ext);
        path = PathBuf::from(name);
    }
    Some(path)
}
