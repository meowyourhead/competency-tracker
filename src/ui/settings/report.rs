//! Settings → Audit report: tick what to include, then export to Excel or PDF
//! (see `crate::report`). Works while viewing.

use std::path::Path;

use chrono::NaiveDate;
use egui::{Button, Color32, Label, RichText, Sense};
use egui_phosphor::regular as icon;

use super::card;
use super::import_export::save_dialog;
use crate::dates::fmt_date;
use crate::model::Id;
use crate::report::{export_audit_report, preview, FileType, MatrixDates, Options, Part, Preview};
use crate::ui::app::{Ctx, Toast};
use crate::ui::audit::date_bound;
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::{chip_select, segmented};

#[derive(Default)]
pub(super) struct ReportTab {
    opts: Options,
    /// What the report would hold, for (revision, date, options).
    cache: Option<(u64, NaiveDate, Options, Preview)>,
}

pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut ReportTab) {
    let pal = cx.pal;
    let data = cx.session.data();
    let opts = &mut st.opts;
    // A section deleted since it was picked.
    if opts
        .section
        .as_ref()
        .is_some_and(|s| data.section(s).is_none())
    {
        opts.section = None;
    }
    let mut export = None;
    card(ui, pal, "Audit report", |ui| {
        ui.label(RichText::new("Tick what to include, then export.").color(pal.muted));
        ui.add_space(8.0);
        egui::Grid::new("report_parts")
            .num_columns(2)
            .spacing([28.0, 4.0])
            .min_row_height(28.0)
            .show(ui, |ui| {
                for part in Part::ALL {
                    let mut on = opts.has(part);
                    ui.checkbox(&mut on, part.label());
                    if part == Part::Matrix && on {
                        // Which dates its cells show.
                        let at = MatrixDates::ALL.iter().position(|d| *d == opts.dates);
                        let labels = MatrixDates::ALL.map(MatrixDates::label);
                        if let Some(i) = segmented(ui, pal, at.unwrap_or(0), &labels) {
                            opts.dates = MatrixDates::ALL[i];
                        }
                    } else if part == Part::ChangeLog && on {
                        ui.horizontal(|ui| {
                            date_bound(ui, pal, "From", &mut opts.from, cx.today, "report_from");
                            date_bound(ui, pal, "To", &mut opts.to, cx.today, "report_to");
                        });
                    } else {
                        about(ui, pal, part.about(), &mut on);
                    }
                    ui.end_row();
                    if on {
                        opts.parts.insert(part);
                    } else {
                        opts.parts.remove(&part);
                    }
                }
            });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let sections: Vec<(Id, String)> = data
                .sections_sorted()
                .into_iter()
                .filter(|s| !s.retired)
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect();
            chip_select(ui, "Section", &mut opts.section, &sections);
            ui.add_space(8.0);
            ui.checkbox(&mut opts.removed, "Include removed staff");
        });
        ui.add_space(8.0);
        let key = (cx.session.revision(), cx.today);
        let found = match &st.cache {
            Some((r, d, o, p)) if (*r, *d) == key && o == opts => *p,
            _ => {
                let p = preview(data, opts, cx.today);
                st.cache = Some((key.0, key.1, opts.clone(), p));
                p
            }
        };
        let problem = opts.problem();
        ui.horizontal_wrapped(|ui| {
            for (glyph, words, file) in [
                (icon::FILE_XLS, "Export to Excel\u{2026}", FileType::Excel),
                (icon::FILE_PDF, "Export to PDF\u{2026}", FileType::Pdf),
            ] {
                let words = RichText::new(format!("{glyph}  {words}")).color(Color32::WHITE);
                if ui
                    .add_enabled(problem.is_none(), Button::new(words).fill(pal.accent))
                    .clicked()
                {
                    export = Some(file);
                }
            }
            ui.add_space(6.0);
            match problem {
                Some(p) => ui.label(RichText::new(p).color(pal.red_fg)),
                None => ui.label(
                    RichText::new(format!(
                        "{} staff \u{b7} {} assessments",
                        found.staff, found.assessments
                    ))
                    .family(medium())
                    .color(pal.muted),
                ),
            };
        });
    });
    if let Some(file) = export {
        let ext = file.extension();
        let kind = match file {
            FileType::Excel => "Excel workbook",
            FileType::Pdf => "PDF document",
        };
        let name = format!("Competency audit report {}.{ext}", fmt_date(cx.today));
        if let Some(path) = save_dialog(&name, kind, ext) {
            run(cx, &st.opts, &path, file);
        }
    }
}

/// What a part holds, beside its tick box; clicking it ticks too.
fn about(ui: &mut egui::Ui, pal: &Palette, words: &str, on: &mut bool) {
    let label = Label::new(RichText::new(words).color(pal.muted))
        .selectable(false)
        .sense(Sense::click());
    if ui.add(label).clicked() {
        *on = !*on;
    }
}

/// Writes the report; a toast says how it went.
fn run(cx: &mut Ctx, opts: &Options, path: &Path, file: FileType) {
    let user = cx.session.user().to_owned();
    let msg = match export_audit_report(cx.session.data(), opts, &user, cx.now, path, file) {
        Ok(()) => format!("Audit report saved to {}", path.display()),
        Err(e) => format!("Couldn't save the report (is it open in another program?): {e}"),
    };
    cx.toasts.push(Toast::new(msg));
}
