//! Settings → Import & export: launching the Excel import wizard, the review
//! list of records imported as "unconfirmed – from Excel" (confirm / delete),
//! and the status / history exports to `.xlsx`.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::NaiveDate;
use egui::{Align, Button, Label, Layout, RichText, ScrollArea, Vec2};
use egui_phosphor::regular as icon;

use super::{apply, card};
use crate::dates::fmt_date;
use crate::export::{export_history, export_status};
use crate::model::{Data, Id};
use crate::ops;
use crate::session::Session;
use crate::ui::app::{Ctx, Toast};
use crate::ui::theme::Palette;
use crate::ui::widgets::{chip_select, empty_state};

const ROW_H: f32 = 30.0;
/// Column widths: checkbox, staff, section, test, stage, date (comment takes the rest).
const COLS: [f32; 6] = [28.0, 170.0, 120.0, 170.0, 110.0, 100.0];

#[derive(Default)]
pub(super) struct ImportTab {
    section: Option<Id>,
    selected: HashSet<Id>,
    confirm: Option<Pending>,
    /// Unconfirmed records for a data revision.
    cache: Option<(u64, Vec<Unconfirmed>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    DeleteSelected,
    ConfirmAll,
}

struct Unconfirmed {
    id: Id,
    section_id: Id,
    staff: String,
    section: String,
    test: String,
    stage: String,
    date: NaiveDate,
    comment: String,
}

enum Act {
    ConfirmSelected,
    Ask(Pending),
    Cancel,
    DeleteSelected,
    ConfirmAll,
}

#[derive(Clone, Copy)]
enum Export {
    Status,
    History,
}

impl ImportTab {
    fn refresh(&mut self, session: &Session) {
        let rev = session.revision();
        if matches!(&self.cache, Some((r, _)) if *r == rev) {
            return;
        }
        let rows = unconfirmed(session.data());
        let ids: HashSet<&Id> = rows.iter().map(|r| &r.id).collect();
        self.selected.retain(|id| ids.contains(id));
        self.cache = Some((rev, rows));
    }
}

/// Sorted by section order, staff name, then completion date.
fn unconfirmed(data: &Data) -> Vec<Unconfirmed> {
    let mut rows: Vec<(u32, u32, Unconfirmed)> = data
        .records
        .iter()
        .filter(|r| r.unconfirmed)
        .map(|r| {
            let (sec, test) = data
                .test(&r.test_id)
                .map_or((None, None), |(s, t)| (Some(s), Some(t)));
            let row = Unconfirmed {
                id: r.id.clone(),
                section_id: sec.map(|s| s.id.clone()).unwrap_or_default(),
                staff: data
                    .staff(&r.staff_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
                section: sec.map(|s| s.name.clone()).unwrap_or_default(),
                test: test.map(|t| t.name.clone()).unwrap_or_default(),
                stage: r.stage_label.clone(),
                date: r.completed_on,
                comment: r.comment.clone(),
            };
            (
                sec.map_or(u32::MAX, |s| s.order),
                test.map_or(u32::MAX, |t| t.order),
                row,
            )
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.0, a.2.staff.to_lowercase(), a.1, a.2.date).cmp(&(
            b.0,
            b.2.staff.to_lowercase(),
            b.1,
            b.2.date,
        ))
    });
    rows.into_iter().map(|(_, _, r)| r).collect()
}

/// Returns true when the user asked to open the import wizard.
pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut ImportTab) -> bool {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    if !editing {
        st.confirm = None;
    }
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
    st.refresh(cx.session);
    let data = cx.session.data();
    let mut act = None;
    card(ui, pal, "Imported records to review", |ui| {
        act = review(ui, pal, editing, data, st)
    });
    if let Some(a) = act {
        run(cx, st, a);
    }
    let mut export = None;
    card(ui, pal, "Export", |ui| {
        ui.label(RichText::new("Save an Excel copy for audits and inspections.").color(pal.muted));
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

fn shown(st: &ImportTab) -> impl Iterator<Item = &Unconfirmed> {
    let rows = st.cache.as_ref().map_or(&[][..], |(_, r)| r.as_slice());
    rows.iter()
        .filter(|r| st.section.as_ref().is_none_or(|s| *s == r.section_id))
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn review(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    data: &Data,
    st: &mut ImportTab,
) -> Option<Act> {
    let total = st.cache.as_ref().map_or(0, |(_, r)| r.len());
    if total == 0 {
        empty_state(
            ui,
            "Nothing to review",
            "Records imported as \u{201c}unconfirmed \u{2013} from Excel\u{201d} appear here.",
        );
        return None;
    }
    let mut act = None;
    ui.label(
        RichText::new(format!(
            "{total} record{} marked \u{201c}unconfirmed \u{2013} from Excel\u{201d}. Check them against the workbook, then confirm or delete.",
            plural(total)
        ))
        .color(pal.muted),
    );
    ui.add_space(4.0);
    let sections: Vec<(Id, String)> = data
        .sections_sorted()
        .into_iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    let ids: Vec<Id> = shown(st).map(|r| r.id.clone()).collect();
    let picked = ids.iter().filter(|id| st.selected.contains(*id)).count();
    ui.horizontal(|ui| {
        chip_select(ui, "Section", &mut st.section, &sections);
        ui.label(
            RichText::new(format!("{} shown \u{b7} {picked} selected", ids.len())).color(pal.muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(editing && !ids.is_empty(), Button::new("Confirm all"))
                .clicked()
            {
                act = Some(Act::Ask(Pending::ConfirmAll));
            }
            if ui
                .add_enabled(editing && picked > 0, Button::new("Delete selected"))
                .clicked()
            {
                act = Some(Act::Ask(Pending::DeleteSelected));
            }
            if ui
                .add_enabled(editing && picked > 0, Button::new("Confirm selected"))
                .clicked()
            {
                act = Some(Act::ConfirmSelected);
            }
        });
    });
    if let Some(pending) = st.confirm {
        ui.horizontal(|ui| {
            let (question, button, ready, then) = match pending {
                Pending::ConfirmAll => (
                    RichText::new(format!(
                        "Confirm all {} record{}?",
                        ids.len(),
                        plural(ids.len())
                    )),
                    RichText::new("Confirm"),
                    !ids.is_empty(),
                    Act::ConfirmAll,
                ),
                Pending::DeleteSelected => (
                    RichText::new(format!(
                        "Delete {picked} selected record{}? This can't be undone.",
                        plural(picked)
                    ))
                    .color(pal.red_fg),
                    RichText::new("Delete").color(pal.red_fg),
                    picked > 0,
                    Act::DeleteSelected,
                ),
            };
            ui.label(question);
            if ui
                .add_enabled(editing && ready, Button::new(button))
                .clicked()
            {
                act = Some(then);
            }
            if ui.button("Cancel").clicked() {
                act = Some(Act::Cancel);
            }
        });
    }
    ui.add_space(4.0);
    table(ui, pal, editing, st, &ids, picked);
    act
}

/// A fixed-width, single-line table cell.
fn cell(ui: &mut egui::Ui, width: f32, text: RichText) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(Label::new(text).truncate());
        },
    );
}

fn table(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    st: &mut ImportTab,
    ids: &[Id],
    picked: usize,
) {
    ui.horizontal(|ui| {
        let mut all = !ids.is_empty() && picked == ids.len();
        ui.allocate_ui_with_layout(
            Vec2::new(COLS[0], ROW_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                if ui
                    .add_enabled(editing, egui::Checkbox::without_text(&mut all))
                    .on_hover_text("Select all shown")
                    .changed()
                {
                    for id in ids {
                        if all {
                            st.selected.insert(id.clone());
                        } else {
                            st.selected.remove(id);
                        }
                    }
                }
            },
        );
        for (head, w) in ["Staff", "Section", "Test system", "Stage", "Completed"]
            .iter()
            .zip(&COLS[1..])
        {
            cell(ui, *w, RichText::new(*head).size(12.5).color(pal.muted));
        }
        cell(
            ui,
            ui.available_width(),
            RichText::new("Comment").size(12.5).color(pal.muted),
        );
    });
    let rows: Vec<&Unconfirmed> = shown(st).collect();
    let mut toggles = Vec::new();
    ScrollArea::vertical()
        .id_salt("unconfirmed_rows")
        .max_height(420.0)
        .auto_shrink([false, true])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for r in &rows[range] {
                ui.horizontal(|ui| {
                    let mut on = st.selected.contains(&r.id);
                    ui.allocate_ui_with_layout(
                        Vec2::new(COLS[0], ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            if ui
                                .add_enabled(editing, egui::Checkbox::without_text(&mut on))
                                .changed()
                            {
                                toggles.push((r.id.clone(), on));
                            }
                        },
                    );
                    cell(ui, COLS[1], RichText::new(&r.staff).color(pal.text));
                    cell(ui, COLS[2], RichText::new(&r.section).color(pal.muted));
                    cell(ui, COLS[3], RichText::new(&r.test).color(pal.text));
                    cell(ui, COLS[4], RichText::new(&r.stage).color(pal.text));
                    cell(ui, COLS[5], RichText::new(fmt_date(r.date)).color(pal.text));
                    cell(
                        ui,
                        ui.available_width(),
                        RichText::new(&r.comment).color(pal.muted),
                    );
                });
            }
        });
    for (id, on) in toggles {
        if on {
            st.selected.insert(id);
        } else {
            st.selected.remove(&id);
        }
    }
}

fn run(cx: &mut Ctx, st: &mut ImportTab, act: Act) {
    let shown_ids: Vec<Id> = shown(st).map(|r| r.id.clone()).collect();
    let selected: Vec<Id> = shown_ids
        .iter()
        .filter(|id| st.selected.contains(*id))
        .cloned()
        .collect();
    match act {
        Act::Ask(p) => st.confirm = Some(p),
        Act::Cancel => st.confirm = None,
        Act::ConfirmSelected => confirm(cx, st, &selected),
        Act::ConfirmAll => confirm(cx, st, &shown_ids),
        Act::DeleteSelected => {
            st.confirm = None;
            let n = selected.len();
            let done = format!("Deleted {n} record{}", plural(n));
            let res = apply(cx, &done, "Couldn't delete", |d, a| {
                selected
                    .iter()
                    .try_for_each(|id| ops::delete_record(d, a, id))
            });
            if res.is_some() {
                st.selected.clear();
            }
        }
    }
}

fn confirm(cx: &mut Ctx, st: &mut ImportTab, ids: &[Id]) {
    st.confirm = None;
    if let Some(n) = apply(cx, "", "Couldn't confirm", |d, a| {
        ops::confirm_records(d, a, ids)
    }) {
        cx.toasts
            .push(Toast::new(format!("Confirmed {n} record{}", plural(n))));
        for id in ids {
            st.selected.remove(id);
        }
    }
}

/// Asks where to save (a native dialog), then writes the workbook.
fn run_export(cx: &mut Ctx, kind: Export) {
    let word = match kind {
        Export::Status => "status",
        Export::History => "history",
    };
    let default_name = format!("Competency {word} {}.xlsx", fmt_date(cx.today));
    let Some(path) = save_dialog(&default_name) else {
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

fn save_dialog(default_name: &str) -> Option<PathBuf> {
    let mut path = rfd::FileDialog::new()
        .add_filter("Excel workbook", &["xlsx"])
        .set_file_name(default_name)
        .save_file()?;
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
    {
        path.set_extension("xlsx");
    }
    Some(path)
}
