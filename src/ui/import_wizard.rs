//! Excel import wizard: choose the workbook, review which names are the same
//! person (`import_review.rs`), preview the counts, import, done. The wizard
//! replaces all data, so the import button needs edit mode.

use std::path::PathBuf;

use egui::{Button, Frame, Margin, RichText, Stroke};

use crate::import::excel::{read_workbook, RawSheet};
use crate::import::mapping::{build_import, carry_over, propose_merges, ImportOptions, ImportPlan, MergeGroup};
use crate::ui::app::{Ctx, Toast};
use crate::ui::import_review;
use crate::ui::theme::{heading, medium, Palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WizardStep {
    #[default]
    PickFile,
    ReviewMerges,
    Preview,
    Done,
}

impl WizardStep {
    const ALL: [(WizardStep, &'static str); 4] = [
        (WizardStep::PickFile, "1 Choose file"),
        (WizardStep::ReviewMerges, "2 Review names"),
        (WizardStep::Preview, "3 Preview"),
        (WizardStep::Done, "4 Done"),
    ];
}

#[derive(Default)]
pub(crate) struct ImportWizard {
    pub(crate) step: WizardStep,
    sheets: Vec<RawSheet>,
    groups: Vec<MergeGroup>,
    plan: Option<ImportPlan>,
    error: Option<String>,
    /// The chosen workbook.
    path: Option<PathBuf>,
    /// A workbook waiting to be read, and whether "Reading…" has been drawn yet.
    reading: Option<(PathBuf, bool)>,
    /// What the import did: `(staff, records, unconfirmed)`.
    done: Option<(usize, usize, usize)>,
    /// Set once `import-report.txt` was written.
    report_path: Option<PathBuf>,
    /// The report's warnings, shown on the Done step.
    warnings: Vec<String>,
}

impl ImportWizard {
    /// Test hook that skips the file dialog: uses these sheets and goes to the name review.
    #[cfg(test)]
    pub(crate) fn load_sheets(&mut self, sheets: Vec<RawSheet>) {
        self.set_sheets(sheets);
        self.step = WizardStep::ReviewMerges;
    }

    fn set_sheets(&mut self, sheets: Vec<RawSheet>) {
        self.groups = propose_merges(&sheets);
        self.sheets = sheets;
        self.error = None;
    }

    fn read(&mut self, path: PathBuf) {
        match read_workbook(&path) {
            Ok(sheets) => self.set_sheets(sheets),
            Err(e) => {
                self.sheets.clear();
                self.groups.clear();
                self.error = Some(format!("Couldn't read this workbook: {e}"));
            }
        }
        self.path = Some(path);
    }

    fn build_preview(&mut self, cx: &Ctx) {
        let opts = ImportOptions {
            user: cx.session.user().to_owned(),
            now: cx.now,
            tolerance_days: cx.session.data().settings.variance_tolerance_days,
        };
        self.plan = Some(build_import(&self.sheets, &self.groups, &opts));
        self.error = None;
        self.step = WizardStep::Preview;
    }

    fn run_import(&mut self, cx: &mut Ctx) {
        let Some(plan) = &self.plan else { return };
        // Settings, the default rule and the audit history survive a re-import.
        let mut data = plan.data.clone();
        let mut report = plan.report.clone();
        carry_over(&mut data, &mut report, cx.session.data());
        // `Ok` means replaced: a failed save is retried by the session and
        // shown on the Done step, so the import is never run twice.
        if let Err(e) = cx.session.replace_data(cx.now, data) {
            self.error = Some(if cx.session.is_editing() { e } else { "Editing ended, so nothing was imported.".to_owned() });
            return;
        }
        let path = cx.session.data_dir().import_report();
        match std::fs::write(&path, report.to_text()) {
            Ok(()) => self.report_path = Some(path),
            Err(e) => cx.toasts.push(Toast::new(format!("Couldn't write the import report: {e}"))),
        }
        self.done = Some((report.staff, report.records, report.unconfirmed));
        self.warnings = report.warnings;
        self.error = None;
        self.step = WizardStep::Done;
    }
}

/// Draws the wizard. Returns true when the user asked to review the unconfirmed records.
pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, w: &mut ImportWizard) -> bool {
    let pal = cx.pal;
    heading(ui, "Import from Excel");
    ui.add_space(6.0);
    steps(ui, pal, w.step);
    ui.add_space(14.0);
    let mut review = false;
    match w.step {
        WizardStep::PickFile => pick_file(ui, pal, w),
        WizardStep::ReviewMerges => review_names(ui, cx, w),
        WizardStep::Preview => preview(ui, cx, w),
        WizardStep::Done => review = done(ui, cx, w),
    }
    review
}

fn steps(ui: &mut egui::Ui, pal: &Palette, current: WizardStep) {
    ui.horizontal(|ui| {
        for (i, (step, label)) in WizardStep::ALL.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("\u{b7}").color(pal.muted));
            }
            let text = if *step == current {
                RichText::new(*label).family(medium()).color(pal.accent)
            } else {
                RichText::new(*label).color(pal.muted)
            };
            ui.label(text);
        }
    });
}

fn error_line(ui: &mut egui::Ui, pal: &Palette, error: &Option<String>) {
    if let Some(e) = error {
        ui.add_space(6.0);
        ui.label(RichText::new(e).color(pal.red_fg));
    }
}

fn card<R>(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let inner = Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(12.0);
    inner
}

fn pick_file(ui: &mut egui::Ui, pal: &Palette, w: &mut ImportWizard) {
    if let Some((path, drawn)) = &mut w.reading {
        if *drawn {
            let path = path.clone();
            w.reading = None;
            w.read(path);
        } else {
            *drawn = true;
            ui.ctx().request_repaint();
        }
    }
    card(ui, pal, |ui| {
        ui.label(RichText::new("Choose the lab's Excel tracker (.xlsm or .xlsx). The file is only read, never changed.").color(pal.muted));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(w.reading.is_none(), Button::new("Choose Excel file\u{2026}")).clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Excel", &["xlsm", "xlsx"]).pick_file() {
                    w.reading = Some((p, false));
                    w.error = None;
                    ui.ctx().request_repaint();
                }
            }
            if let Some((p, _)) = &w.reading {
                ui.label(RichText::new(format!("Reading\u{2026} {}", p.display())).color(pal.muted));
            } else if let Some(p) = &w.path {
                ui.label(RichText::new(p.display().to_string()).color(pal.text));
            }
        });
        error_line(ui, pal, &w.error);
    });
    let ready = !w.sheets.is_empty() && w.reading.is_none();
    if ui.add_enabled(ready, Button::new("Next")).clicked() {
        w.step = WizardStep::ReviewMerges;
    }
}

fn review_names(ui: &mut egui::Ui, cx: &mut Ctx, w: &mut ImportWizard) {
    let pal = cx.pal;
    ui.label(
        RichText::new("The same person can appear on several tabs. Check the names that were joined together; split any that are different people.")
            .color(pal.muted),
    );
    ui.add_space(8.0);
    let names_ok = import_review::show(ui, pal, cx.today, &mut w.groups);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            w.step = WizardStep::PickFile;
        }
        if ui.add_enabled(names_ok, Button::new("Next")).clicked() {
            w.build_preview(cx);
        }
    });
}

fn count_row(ui: &mut egui::Ui, pal: &Palette, label: &str, n: usize) {
    ui.label(RichText::new(label).color(pal.muted));
    ui.label(RichText::new(n.to_string()).color(pal.text));
    ui.end_row();
}

fn preview(ui: &mut egui::Ui, cx: &mut Ctx, w: &mut ImportWizard) {
    let pal = cx.pal;
    let Some(plan) = &w.plan else {
        w.step = WizardStep::ReviewMerges;
        return;
    };
    let r = &plan.report;
    let data = cx.session.data();
    if !data.staff.is_empty() || !data.records.is_empty() {
        Frame::new().fill(pal.yellow_bg).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.label(
                RichText::new("This replaces the current data. A backup of it is saved first to the snapshots\\archive folder.")
                    .color(pal.yellow_fg),
            );
        });
        ui.add_space(10.0);
    }
    card(ui, pal, |ui| {
        ui.label(RichText::new("What will be imported").family(medium()).size(15.5).color(pal.text));
        ui.add_space(6.0);
        egui::Grid::new("import_counts").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
            count_row(ui, pal, "Staff", r.staff);
            count_row(ui, pal, "Records", r.records);
            count_row(ui, pal, "Unconfirmed records", r.unconfirmed);
            count_row(ui, pal, "Planned dates", r.planned);
            count_row(ui, pal, "Notes", r.notes);
            count_row(ui, pal, "Excel due dates used", r.excel_overrides.len());
            count_row(ui, pal, "Excel due dates ignored", r.excel_ignored_planned.len() + r.excel_ignored_years.len());
            count_row(ui, pal, "Annual numbering to check", r.flagged_stage.len());
        });
        ui.add_space(6.0);
        ui.label(RichText::new("The full list is saved as import-report.txt in the data folder.").color(pal.muted));
        error_line(ui, pal, &w.error);
    });
    let editing = cx.session.is_editing();
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            w.plan = None;
            w.step = WizardStep::ReviewMerges;
        }
        if ui.add_enabled(editing, Button::new("Import")).clicked() {
            w.run_import(cx);
        }
        if !editing {
            ui.label(RichText::new("Start editing to import").color(pal.muted));
        }
    });
}

/// Returns true for "Review unconfirmed".
fn done(ui: &mut egui::Ui, cx: &mut Ctx, w: &mut ImportWizard) -> bool {
    let pal = cx.pal;
    let (staff, records, unconfirmed) = w.done.unwrap_or_default();
    let mut review = false;
    card(ui, pal, |ui| {
        ui.label(RichText::new(format!("Imported {staff} staff and {records} records.")).family(medium()).size(15.5).color(pal.text));
        ui.add_space(6.0);
        if cx.session.save_error().is_some() {
            ui.label(RichText::new("Imported, but not saved yet \u{2014} the app keeps retrying").color(pal.yellow_fg));
        }
        for warning in &w.warnings {
            ui.label(RichText::new(warning).color(pal.yellow_fg));
        }
        if w.report_path.is_none() {
            ui.label(RichText::new("The import report could not be saved.").color(pal.yellow_fg));
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(w.report_path.is_some(), Button::new("Open report")).clicked() {
                if let Some(p) = &w.report_path {
                    if let Err(e) = std::process::Command::new("notepad.exe").arg(p).spawn() {
                        cx.toasts.push(Toast::new(format!("Couldn't open the report: {e}")));
                    }
                }
            }
            review = ui.button(format!("Review unconfirmed ({unconfirmed})")).clicked();
        });
    });
    review
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::now_ts;
    use crate::import::excel::{Cell, RawBlock, RawTest, StageCell};
    use crate::model::Id;
    use crate::session::Session;
    use crate::snapshot::DataDir;
    use crate::ui::app::Toast;
    use crate::ui::theme::{self, Palette};
    use chrono::NaiveDate;
    use egui_kittest::kittest::{NodeT, Queryable};
    use egui_kittest::Harness;
    use std::time::Instant;

    struct St {
        session: Session,
        wizard: ImportWizard,
        toasts: Vec<Toast>,
        open: Option<(Id, Id)>,
        pal: Palette,
        review: bool,
        styled: bool,
    }

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn sheet(name: &str, person: &str, title: &str) -> RawSheet {
        let empty = || vec![StageCell { cell: Cell::Empty, ticked: false }];
        RawSheet {
            name: name.into(),
            tests: vec![RawTest { col: 5, header: "Test A".into(), tick_col: Some(6), section_hint: name.into() }],
            blocks: vec![RawBlock {
                row: 4,
                name: person.into(),
                title: title.into(),
                joined: Some(ymd(2020, 1, 1)),
                stages: [vec![StageCell { cell: Cell::Date(ymd(2025, 1, 10)), ticked: true }], empty(), empty(), empty()],
                remarks: vec![],
            }],
        }
    }

    fn harness(dir: &tempfile::TempDir) -> Harness<'static, St> {
        let data_dir = DataDir { root: dir.path().join("CompetencyData") };
        let session = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        let st = St {
            session,
            wizard: ImportWizard::default(),
            toasts: Vec::new(),
            open: None,
            pal: theme::palette(crate::prefs::Theme::Light),
            review: false,
            styled: false,
        };
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, st: &mut St| {
                if !st.styled {
                    // New fonts only take effect from the next pass.
                    theme::install_fonts(ui.ctx());
                    st.styled = true;
                    ui.ctx().request_repaint();
                    return;
                }
                let now = now_ts();
                let mut cx = Ctx {
                    session: &mut st.session,
                    pal: &st.pal,
                    today: now.date_naive(),
                    now,
                    toasts: &mut st.toasts,
                    open_detail: &mut st.open,
                };
                if show(ui, &mut cx, &mut st.wizard) {
                    st.review = true;
                }
            },
            st,
        );
        h.run();
        h
    }

    fn button<'a>(h: &'a Harness<'static, St>, label: &'a str) -> egui_kittest::Node<'a> {
        h.get_by_role_and_label(egui::accesskit::Role::Button, label)
    }

    #[test]
    fn wizard_preview_and_import_from_raw() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT"), sheet("Serology", "xin  yi", "Senior MT")]);
        h.run();
        assert_eq!(h.state().wizard.step, WizardStep::ReviewMerges);
        h.get_by_label("Needs a look (1)");
        h.get_by_label("Hematology \u{b7} Xin Yi");
        button(&h, "Next").click();
        h.run();
        assert_eq!(h.state().wizard.step, WizardStep::Preview);
        button(&h, "Import").click();
        h.run();
        assert_eq!(h.state().wizard.step, WizardStep::Done);
        let data = h.state().session.data();
        assert_eq!(data.staff.len(), 1, "both tabs are one person");
        assert_eq!(data.staff[0].name, "Xin Yi");
        assert!(!data.records.is_empty());
        let report = std::fs::read_to_string(dir.path().join("CompetencyData").join("import-report.txt")).unwrap();
        assert!(report.starts_with("Excel import report"));
        h.get_by_label_contains("Imported 1 staff and");
        button(&h, "Review unconfirmed (0)").click();
        h.run();
        assert!(h.state().review);
    }

    #[test]
    fn import_with_a_failed_save_is_done_but_says_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        // Today's snapshot cannot be written: a folder is in its place.
        let name = crate::snapshot::snapshot_file_name(now_ts().date_naive(), "tester");
        let target = dir.path().join("CompetencyData").join("snapshots").join(name);
        std::fs::create_dir_all(&target).unwrap();
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT")]);
        h.run();
        import_now(&mut h);
        assert!(h.state().wizard.error.is_none());
        assert_eq!(h.state().session.data().staff.len(), 1);
        assert!(h.state().session.save_error().is_some());
        h.get_by_label("Imported, but not saved yet \u{2014} the app keeps retrying");
        assert!(dir.path().join("CompetencyData").join("import-report.txt").exists());
        // Once the save goes through, the note goes away.
        std::fs::remove_dir(&target).unwrap();
        h.state_mut().session.save_now(now_ts()).unwrap();
        h.run();
        assert!(h.query_by_label("Imported, but not saved yet \u{2014} the app keeps retrying").is_none());
    }

    #[test]
    fn import_needs_edit_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().session.stop_editing(now_ts()).unwrap();
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT")]);
        h.run();
        button(&h, "Next").click();
        h.run();
        h.get_by_label("Start editing to import");
        assert!(button(&h, "Import").accesskit_node().is_disabled());
        assert!(h.state().session.data().staff.is_empty());
    }

    fn import_now(h: &mut Harness<'static, St>) {
        button(h, "Next").click();
        h.run();
        button(h, "Import").click();
        h.run();
        assert_eq!(h.state().wizard.step, WizardStep::Done);
    }

    #[test]
    fn edited_name_title_and_joined_reach_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT"), sheet("Serology", "Xin Yi", "MT")]);
        h.run();
        let g = &mut h.state_mut().wizard.groups[0];
        g.name = "Xin Yi Lim".into();
        g.title = "SMT".into();
        g.joined = Some(ymd(2019, 5, 6));
        h.run();
        import_now(&mut h);
        let s = &h.state().session.data().staff[0];
        assert_eq!((s.name.as_str(), s.title.as_str(), s.joined), ("Xin Yi Lim", "SMT", Some(ymd(2019, 5, 6))));
    }

    #[test]
    fn split_then_import_yields_two_staff() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT"), sheet("Serology", "Xin Yi", "MT")]);
        h.run();
        button(&h, "Split").click();
        h.run();
        // Both halves are named "Xin Yi": Next waits until one is renamed.
        h.get_by_label("Some names are blank or shared by two people. Fix them (see the yellow notes) before going on.");
        assert!(button(&h, "Next").accesskit_node().is_disabled());
        h.state_mut().wizard.groups[1].name = "Xin Yi B".into();
        h.run();
        import_now(&mut h);
        assert_eq!(h.state().session.data().staff.len(), 2);
    }

    #[test]
    fn split_of_differently_named_members_leaves_next_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Shawn (Weixiang)", "MT"), sheet("Serology", " Weixiang ", "MT")]);
        h.run();
        button(&h, "Split").click();
        h.run();
        let names: Vec<&str> = h.state().wizard.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Shawn (Weixiang)", "Weixiang"]);
        assert!(!button(&h, "Next").accesskit_node().is_disabled());
        import_now(&mut h);
        assert_eq!(h.state().session.data().staff.len(), 2);
    }

    #[test]
    fn blank_name_disables_next() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT")]);
        h.run();
        assert!(!button(&h, "Next").accesskit_node().is_disabled());
        h.state_mut().wizard.groups[0].name = "  ".into();
        h.run();
        assert!(button(&h, "Next").accesskit_node().is_disabled());
    }

    #[test]
    fn reimport_keeps_settings_audit_and_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().session.mutate(now_ts(), |d, _| d.settings.due_soon_days = 45).unwrap();
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT")]);
        h.run();
        import_now(&mut h);
        let first = h.state().session.data().audit.len();
        // A second import over the first, the same day.
        h.state_mut().wizard = ImportWizard::default();
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Bea", "MT")]);
        h.run();
        button(&h, "Next").click();
        h.run();
        h.get_by_label_contains("A backup of it is saved first");
        button(&h, "Import").click();
        h.run();
        assert_eq!(h.state().wizard.step, WizardStep::Done);
        let data = h.state().session.data();
        assert_eq!(data.settings.due_soon_days, 45);
        assert_eq!(data.audit.len(), first + 1, "history kept, one new import entry");
        assert_eq!(data.staff[0].name, "Bea");
        let archive = dir.path().join("CompetencyData").join("snapshots").join("archive");
        assert_eq!(std::fs::read_dir(archive).unwrap().count(), 1, "the first import was backed up");
    }

    #[test]
    fn split_makes_each_member_its_own_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(&dir);
        h.state_mut().wizard.load_sheets(vec![sheet("Hematology", "Xin Yi", "MT"), sheet("Serology", "Xin Yi", "MT")]);
        h.run();
        h.get_by_label("Needs a look (1)");
        button(&h, "Split").click();
        h.run();
        assert_eq!(h.state().wizard.groups.len(), 2);
        assert!(h.query_by_label("Needs a look (1)").is_none());
        h.get_by_label("Single names (2)");
    }
}
