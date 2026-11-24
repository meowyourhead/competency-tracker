//! Smoke tests for the app shell, driven headlessly with egui_kittest.

use std::time::Instant;

use competency_tracker::dates::{fmt_date, now_ts};
use competency_tracker::model::{AssignState, Assignment, Record, Staff, StaffStatus, TestSystem};
use competency_tracker::session::Session;
use competency_tracker::snapshot::DataDir;
use competency_tracker::ui::app::TrackerApp;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;

fn harness(app: TrackerApp) -> Harness<'static, TrackerApp> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_theme(egui::Theme::Light)
        .build_ui_state(|ui, app: &mut TrackerApp| app.show_root(ui), app)
}

fn session(dir: &tempfile::TempDir, pid: u32) -> Session {
    let data_dir = DataDir {
        root: dir.path().join("CompetencyData"),
    };
    Session::open(data_dir, "tester", "pc", pid, now_ts(), Instant::now()).unwrap()
}

#[test]
fn shows_sidebar_nav() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    for label in ["Summary", "Sections", "Personnel", "History", "Settings"] {
        h.get_by_role_and_label(egui::accesskit::Role::Button, label);
    }
    assert!(h.query_by_label("Competency Tracker").is_some());
}

#[test]
fn viewer_banner_when_lock_held() {
    let dir = tempfile::tempdir().unwrap();
    // Another process (pid 999) takes the edit lock first.
    let other = {
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        Session::open(data_dir, "agnes", "pc2", 999, now_ts(), Instant::now()).unwrap()
    };
    assert!(other.is_editing());
    let h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.get_by_label_contains("agnes is editing");
}

#[test]
fn failed_save_on_close_cancels_and_offers_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    assert!(s.is_editing());
    s.mutate(now_ts(), |d, _| d.settings.due_soon_days = 45)
        .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    // Make saving impossible: replace the snapshots folder with a file.
    let snaps = dir.path().join("CompetencyData").join("snapshots");
    std::fs::remove_dir_all(&snaps).unwrap();
    std::fs::write(&snaps, b"not a folder").unwrap();

    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
    let commands = &h.output().viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(commands
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
    h.run();
    h.get_by_label_contains("Couldn't save your changes:");
    h.get_by_label("Retry");
    h.get_by_label("Quit anyway");
    assert!(
        h.state().session().is_editing(),
        "edits and lock are kept until the user decides"
    );
}

#[test]
fn failed_save_on_close_with_nothing_unsaved_just_closes() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(&dir, 1);
    assert!(s.is_editing());
    let mut h = harness(TrackerApp::new_for_test(s));
    let snaps = dir.path().join("CompetencyData").join("snapshots");
    std::fs::remove_dir_all(&snaps).unwrap();
    std::fs::write(&snaps, b"not a folder").unwrap();

    close_window(&mut h);
    assert!(!root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
    assert!(!h.state().session().is_editing());
    assert!(
        !dir.path().join("CompetencyData").join("edit.lock").exists(),
        "lock released"
    );
}

fn close_window(h: &mut Harness<'static, TrackerApp>) {
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
}

fn root_commands(h: &Harness<'static, TrackerApp>) -> Vec<egui::ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[test]
fn close_after_lock_lost_shows_conflict_then_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| d.settings.due_soon_days = 45)
        .unwrap();
    // Another process takes over the lock while our edits are unsaved.
    let data_dir = DataDir {
        root: dir.path().join("CompetencyData"),
    };
    std::fs::remove_file(data_dir.lock_path()).unwrap();
    let other = Session::open(data_dir, "agnes", "pc2", 999, now_ts(), Instant::now()).unwrap();
    assert!(other.is_editing());

    let mut h = harness(TrackerApp::new_for_test(s));
    close_window(&mut h);
    assert!(root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
    h.run();
    h.get_by_label_contains("Someone else took over editing. Your changes were saved to");
    h.get_by_label("Close").click();
    h.step();
    assert!(root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::Close)));
}

#[test]
fn theme_toggle_switches_dark() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    assert!(!h.ctx.global_style().visuals.dark_mode);
    h.get_by_label("Switch to dark mode").click();
    h.run();
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert!(h.query_by_label("Switch to light mode").is_some());
}

fn staff(id: &str, name: &str, title: &str, test_id: &str) -> Staff {
    let since = now_ts().date_naive() - chrono::Days::new(400);
    Staff {
        id: id.into(),
        name: name.into(),
        title: title.into(),
        joined: None,
        status: StaffStatus::Active,
        assignments: [(
            test_id.to_string(),
            Assignment {
                state: AssignState::Assigned,
                since,
            },
        )]
        .into(),
    }
}

/// Initial stage completed `days_ago`; the Semi-annual stage is due 6 months later.
fn initial(staff_id: &str, test_id: &str, days_ago: u64) -> Record {
    Record {
        id: format!("r-{staff_id}-{test_id}"),
        staff_id: staff_id.into(),
        test_id: test_id.into(),
        stage_index: 0,
        stage_label: "Initial".into(),
        completed_on: now_ts().date_naive() - chrono::Days::new(days_ago),
        comment: String::new(),
        by: "tester".into(),
        at: now_ts(),
        imported: false,
        unconfirmed: false,
    }
}

/// Chemistry: Cobas, Architect. Agnes (Cobas) is overdue; Bram (Architect) is on track.
fn seeded(dir: &tempfile::TempDir, agnes_days_ago: u64) -> Harness<'static, TrackerApp> {
    let mut s = session(dir, 1);
    s.mutate(now_ts(), |d, _| {
        let test = |id: &str, name: &str, order| TestSystem {
            id: id.into(),
            name: name.into(),
            order,
            retired: false,
            rule_id: None,
        };
        d.sections[0].tests = vec![test("t-cobas", "Cobas", 0), test("t-arch", "Architect", 1)];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        d.staff
            .push(staff("p-bram", "Bram", "Senior MLT", "t-arch"));
        d.records
            .push(initial("p-agnes", "t-cobas", agnes_days_ago));
        d.records.push(initial("p-bram", "t-arch", 30));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    h
}

#[test]
fn summary_shows_figures_and_action_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = seeded(&dir, 365);
    h.get_by_label("Staff 2");
    h.get_by_label("Assessments current 50%");
    h.get_by_label("Outstanding 1");
    h.get_by_label("Overdue 1");
    h.get_by_label("Due soon 0");
    h.get_by_label("Overdue (1)");
    h.get_by_label("Agnes \u{b7} Cobas");
    // The register lists both people.
    button(&h, "Agnes");
    button(&h, "Bram");
}

#[test]
fn summary_status_figure_filters_and_toggles_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.get_by_label("Overdue 1").click();
    h.run();
    assert!(
        h.query_by_label("Bram").is_none(),
        "only people with overdue assessments are listed"
    );
    button(&h, "Agnes");
    h.get_by_label("Staff 1");
    h.get_by_label("Overdue 1").click();
    h.run();
    assert!(
        h.query_by_label("Bram").is_some(),
        "a second click shows everyone again"
    );
}

#[test]
fn summary_section_row_filters_by_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    assert!(h.query_by_label("Clear filters").is_none());
    h.get_by_label("Chemistry 50% current").click();
    h.run();
    h.get_by_label("Clear filters").click();
    h.run();
    assert!(h.query_by_label("Clear filters").is_none());
}

#[test]
fn summary_row_click_opens_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    assert!(h.query_by_label("Close panel").is_none());
    h.get_by_label("Agnes \u{b7} Cobas").click();
    h.run();
    h.get_by_label("Close panel");
}

#[test]
fn summary_register_name_opens_profile() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.get_by_label("Bram").click();
    h.run();
    h.get_by_label("\u{2190} All staff");
    h.get_by_label("Architect");
}

#[test]
fn summary_empty_states() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.run();
    h.get_by_label_contains("No data yet");

    let dir = tempfile::tempdir().unwrap();
    let h = seeded(&dir, 30); // both on track
    h.get_by_label("Nothing is overdue.");
    assert!(h.query_by_label("Agnes \u{b7} Cobas").is_none());
}

#[test]
fn fonts_register_serif_and_medium_and_headings_render() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    // Fonts installed on the first pass take effect on the next one.
    h.run();
    h.step();
    let families: Vec<egui::FontFamily> = h
        .ctx
        .fonts(|f| f.definitions().families.keys().cloned().collect());
    for name in ["serif", "serif-regular", "medium"] {
        assert!(
            families.contains(&egui::FontFamily::Name(name.into())),
            "missing family {name}"
        );
    }
    let regular = h
        .ctx
        .fonts(|f| f.definitions().families[&egui::FontFamily::Proportional][0].clone());
    assert_eq!(regular, "Inter-Regular");
    // Pages carry no title of their own: the sidebar names them.
    assert_eq!(h.query_all_by_label("Summary").count(), 1);
}

#[test]
fn window_buttons_replace_the_title_bar() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    button(&h, "Minimize").click();
    h.step();
    assert!(root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::Minimized(true))));
    button(&h, "Maximize").click();
    h.step();
    assert!(root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::Maximized(true))));
    button(&h, "Close window").click();
    h.step();
    assert!(root_commands(&h)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::Close)));
}

#[test]
fn title_bar_shows_presence_and_toggles_editing_in_both_sidebar_widths() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.run();
    h.get_by_label("tester \u{b7} Editing");
    // No idle timer is shown any more.
    assert!(h.query_by_label_contains("Editing stops in").is_none());
    button(&h, "Collapse sidebar").click();
    h.run();
    // The controls live in the title bar, so collapsing changes nothing.
    h.get_by_label("Switch to dark mode");
    button(&h, "Stop editing").click();
    h.run();
    assert!(!h.state().session().is_editing());
    h.get_by_label("tester \u{b7} Viewing");
    button(&h, "Start editing").click();
    h.run();
    assert!(h.state().session().is_editing());
}

#[test]
fn merge_two_people_keeps_both_competencies() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    tick(&mut h, 0); // the header box: both
    h.get_by_label("2 people selected");
    button(&h, "Merge").click();
    h.run();
    h.get_by_label("Merge 2 people");
    h.get_by_label("Keep Bram").click();
    h.run();
    button(&h, "Merge into Bram").click();
    h.run();
    let data = h.state().session().data();
    assert_eq!(data.staff.len(), 1);
    let bram = &data.staff[0];
    assert_eq!(bram.name, "Bram");
    assert!(bram.assignments.contains_key("t-cobas"));
    assert!(bram.assignments.contains_key("t-arch"));
    assert_eq!(
        data.records
            .iter()
            .filter(|r| r.staff_id == "p-bram")
            .count(),
        2
    );
    assert!(h.query_by_label("Merge 2 people").is_none());
}

#[test]
fn all_sections_is_one_table_grouped_by_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        let test = |id: &str, name: &str| TestSystem {
            id: id.into(),
            name: name.into(),
            order: 0,
            retired: false,
            rule_id: None,
        };
        d.sections[0].tests = vec![test("t-cobas", "Cobas")];
        d.sections[1].tests = vec![test("t-sysmex", "Sysmex")];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        d.staff.push(staff("p-bram", "Bram", "MLT", "t-sysmex"));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    button(&h, "All sections").click();
    h.run();
    // One table: both sections' columns, one row per person.
    h.get_by_label("Agnes \u{b7} Cobas");
    h.get_by_label("Bram \u{b7} Sysmex");
    // Each person also has a cell under the other section's test.
    h.get_by_label("Agnes \u{b7} Sysmex");
    h.get_by_label("Bram \u{b7} Cobas");
}

#[test]
fn summary_register_search_narrows_people() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 30); // both on track
    summary_search(&h).click();
    h.run();
    summary_search(&h).type_text("Agnes");
    h.run();
    h.get_by_label("Agnes");
    assert!(h.query_by_label("Bram").is_none());
}

#[test]
fn summary_search_with_no_match_shows_no_matches() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 30);
    summary_search(&h).click();
    h.run();
    summary_search(&h).type_text("zzzz");
    h.run();
    h.get_by_label("No matches");
}

#[test]
fn summary_no_data_state_reads_full_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.run();
    h.get_by_label("No data yet \u{2014} import your Excel tracker from Settings");
}

/// Goes to Sections (Chemistry is first) and clicks the grid cell named `cell`.
fn open_cell(h: &mut Harness<'static, TrackerApp>, cell: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Sections")
        .click();
    h.run();
    h.get_by_label(cell).click();
    h.run();
}

#[test]
fn grid_shows_pill_and_opens_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Sections")
        .click();
    h.run();
    let done = now_ts().date_naive() - chrono::Days::new(365);
    let due = fmt_date(done.checked_add_months(chrono::Months::new(6)).unwrap());
    assert!(h.query_by_label("Bram \u{b7} Architect").is_some());
    assert!(h.query_by_label("Close panel").is_none());
    let chemistry = h.query_all_by_label("Chemistry").count();
    h.get_by_label("Agnes \u{b7} Cobas").click();
    h.run();
    h.get_by_label("Close panel");
    // The panel's subtitle names the test, with its section as a chip.
    assert_eq!(h.query_all_by_label("Chemistry").count(), chemistry + 1);
    // The cell's pill and stage, as the panel shows them.
    h.get_by_label(&due);
    h.get_by_label("for Semi-annual");
}

/// Agnes has six Cobas completions, one stage a year apart.
fn six_completions(dir: &tempfile::TempDir) -> (Harness<'static, TrackerApp>, Vec<String>) {
    let mut s = session(dir, 1);
    let labels = [
        "Initial",
        "Semi-annual",
        "1st Annual",
        "2nd Annual",
        "3rd Annual",
        "4th Annual",
    ];
    let today = now_ts().date_naive();
    let lines: Vec<String> = (0..6u64)
        .map(|i| {
            // Each history entry's details toggle names its stage and date.
            format!(
                "Show details of {}, {}",
                labels[i as usize],
                fmt_date(today - chrono::Days::new(2200 - i * 365))
            )
        })
        .collect();
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: "Cobas".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        for i in 0..6u64 {
            let mut r = initial("p-agnes", "t-cobas", 2200 - i * 365);
            r.id = format!("r-{i}");
            r.stage_index = i as u32;
            r.stage_label = labels[i as usize].into();
            d.records.push(r);
        }
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    (h, lines)
}

#[test]
fn detail_shows_four_then_more() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, lines) = six_completions(&dir);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    for line in &lines[2..] {
        h.get_by_label(line);
    }
    for line in &lines[..2] {
        assert!(
            h.query_by_label(line).is_none(),
            "{line} is hidden at first"
        );
    }
    h.get_by_label("Show 2 older entries").click();
    h.run();
    // Oldest at the top, down to the latest.
    let tops: Vec<f32> = lines
        .iter()
        .map(|line| h.get_by_label(line).rect().top())
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");
    assert!(h.query_by_label("Show 2 older entries").is_none());
}

/// Every annual has its own line: six completions and the next due date.
#[test]
fn completed_view_gives_every_annual_its_line() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = six_completions(&dir);
    button(&h, "Sections").click();
    h.run();
    let due_h = h.get_by_label("Agnes \u{b7} Cobas").rect().height();
    button(&h, "Completed").click();
    h.run();
    let h2 = h.get_by_label("Agnes \u{b7} Cobas").rect().height();
    assert!(due_h < 50.0 && h2 >= 7.0 * 20.0, "{due_h} {h2}");
}

#[test]
fn section_rows_follow_the_default_order_and_the_sort() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: "Cobas".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        for (id, name) in [("p-a", "Amy"), ("p-b", "Bo"), ("p-c", "Cy"), ("p-z", "Al")] {
            d.staff.push(staff(id, name, "MLT", "t-cobas"));
        }
        d.records.push(initial("p-b", "t-cobas", 300));
        // As in the workbook; Al was added in the app.
        d.sections[0].staff_order = vec!["p-c".into(), "p-a".into(), "p-b".into()];
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    let order = |h: &Harness<'static, TrackerApp>| {
        let mut v: Vec<(f32, &str)> = ["Amy", "Bo", "Cy", "Al"]
            .into_iter()
            .map(|n| {
                let cell = format!("{n} \u{b7} Cobas");
                (h.get_by_label(&cell).rect().top(), n)
            })
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
    };
    let pick = |h: &mut Harness<'static, TrackerApp>, label: &str| {
        h.get_by_label_contains("Sort: ").click();
        h.run();
        button(h, label).click();
        h.run();
    };
    assert_eq!(order(&h), ["Cy", "Amy", "Bo", "Al"]);
    pick(&mut h, "Name A\u{2013}Z");
    assert_eq!(order(&h), ["Al", "Amy", "Bo", "Cy"]);
    pick(&mut h, "Name Z\u{2013}A");
    assert_eq!(order(&h), ["Cy", "Bo", "Amy", "Al"]);
    pick(&mut h, "Due date");
    assert_eq!(order(&h)[0], "Bo");
    // A column sorts too; picking an order undoes that.
    button(&h, "Cobas").click();
    h.run();
    pick(&mut h, "Default order");
    assert_eq!(order(&h), ["Cy", "Amy", "Bo", "Al"]);
}

#[test]
fn back_and_forward_retrace_the_pages() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    let back = |h: &mut Harness<'static, TrackerApp>| {
        button(h, "Back").click();
        h.run();
    };
    // Nowhere to go yet.
    back(&mut h);
    h.get_by_label("Staff 2");
    button(&h, "Open Chemistry in Sections").click();
    h.run();
    button(&h, "Haematology").click();
    h.run();
    button(&h, "Settings").click();
    h.run();
    back(&mut h);
    h.get_by_label("No test systems"); // Haematology has none
    back(&mut h);
    h.get_by_label("Agnes \u{b7} Cobas");
    back(&mut h);
    h.get_by_label("Staff 2");
    button(&h, "Forward").click();
    h.run();
    h.get_by_label("Agnes \u{b7} Cobas");
    // The keyboard does the same.
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowRight);
    h.run();
    h.get_by_label("No test systems");
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowLeft);
    h.run();
    // Back also closes the panel it opened after.
    h.get_by_label("Agnes \u{b7} Cobas").click();
    h.run();
    h.get_by_label("Close panel");
    back(&mut h);
    assert!(h.query_by_label("Close panel").is_none());
    h.get_by_label("Agnes \u{b7} Cobas");
    // Going somewhere new drops what was ahead.
    button(&h, "Personnel").click();
    h.run();
    button(&h, "Forward").click();
    h.run();
    assert!(h.query_by_label("No test systems").is_none());
    assert!(h.query_by_label("Close panel").is_none());
}

#[test]
fn search_is_centred_and_the_name_is_on_the_left() {
    let dir = tempfile::tempdir().unwrap();
    let h = seeded(&dir, 30);
    let name = h.get_by_label("Competency Tracker").rect();
    let forward = button(&h, "Forward").rect();
    assert!(forward.right() <= name.left() && name.right() < 640.0);
}

#[test]
fn summary_section_button_opens_the_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    button(&h, "Open Chemistry in Sections").click();
    h.run();
    h.get_by_label("Agnes \u{b7} Cobas");
    h.get_by_label_contains("Sort: ");
    assert!(
        h.query_by_label("Clear filters").is_none(),
        "the row underneath wasn't clicked"
    );
}

#[test]
fn history_hides_who_recorded_an_entry_until_opened() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, lines) = six_completions(&dir);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    assert!(h.query_by_label_contains("Recorded by").is_none());
    h.get_by_label(&lines[5]).click();
    h.run();
    h.get_by_label_contains("Recorded by tester on");
    // Editing actions live in the opened entry too.
    assert!(h.query_by_label_contains("  Edit").is_some());
    assert!(h.query_by_label_contains("  Delete").is_some());
    let hide = lines[5].replacen("Show", "Hide", 1);
    h.get_by_label(&hide).click();
    h.run();
    assert!(h.query_by_label_contains("Recorded by").is_none());
}

#[test]
fn editing_an_entry_opens_its_form_in_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, lines) = six_completions(&dir);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    h.get_by_label(&lines[5]).click();
    h.run();
    h.get_by_label_contains("  Edit").click();
    h.run();
    h.get_by_label("Edit entry");
    // The form sits below the entry's own details, not at the top.
    let form_y = h.get_by_label("Edit entry").rect().top();
    let by_y = h.get_by_label_contains("Recorded by").rect().top();
    let mark_y = button(&h, "Mark complete").rect().top();
    assert!(form_y > by_y && form_y > mark_y);
}

#[test]
fn display_scale_changes_zoom() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.run();
    button(&h, "Settings").click();
    h.run();
    h.get_by_label("General").click();
    h.run();
    h.get_by_label("120%").click();
    h.run();
    assert!((h.ctx.zoom_factor() - 1.2).abs() < 0.001);
    h.get_by_label("100%").click();
    h.run();
    assert!((h.ctx.zoom_factor() - 1.0).abs() < 0.001);
}

#[test]
fn mark_complete_disabled_when_viewing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: "Cobas".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
    })
    .unwrap();
    s.stop_editing(now_ts()).unwrap();
    assert!(!s.is_editing());
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    let button = h.get_by_role_and_label(egui::accesskit::Role::Button, "Mark complete");
    assert!(button.accesskit_node().is_disabled());
}

#[test]
fn mark_complete_adds_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    let before = h.state().session().data().records.len();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Mark complete")
        .click();
    h.run();
    let comment = h
        .query_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("comment box");
    comment.click();
    h.run();
    h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .unwrap()
        .type_text("Bench 2");
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save")
        .click();
    h.run();
    let records = &h.state().session().data().records;
    assert_eq!(records.len(), before + 1);
    let added = records.last().unwrap();
    assert_eq!(
        (added.by.as_str(), added.stage_index, added.comment.as_str()),
        ("tester", 1, "Bench 2")
    );
    assert_eq!(added.completed_on, now_ts().date_naive());
}

fn button<'a>(h: &'a Harness<'static, TrackerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Button, label)
}

fn has_button(h: &Harness<'static, TrackerApp>, label: &str) -> bool {
    h.query_by_role_and_label(egui::accesskit::Role::Button, label)
        .is_some()
}

#[test]
fn mark_na_closes_open_form() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    button(&h, "Mark complete").click();
    h.run();
    assert!(has_button(&h, "Save"));
    h.get_by_label_contains("Mark N/A / Unassign").click();
    h.run();
    button(&h, "Mark N/A").click();
    h.run();
    let agnes = h
        .state()
        .session()
        .data()
        .staff
        .iter()
        .find(|s| s.id == "p-agnes")
        .unwrap()
        .clone();
    assert_eq!(agnes.assignments["t-cobas"].state, AssignState::Na);
    assert!(!has_button(&h, "Save"), "the Mark complete form is gone");
}

#[test]
fn stopping_editing_clears_form() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    button(&h, "Mark complete").click();
    h.run();
    assert!(has_button(&h, "Save"));
    button(&h, "Stop editing").click();
    h.run();
    assert!(!h.state().session().is_editing());
    assert!(!has_button(&h, "Save"));
}

#[test]
fn skip_stage_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    button(&h, "Skip stage").click();
    h.run();
    assert!(
        h.state().session().data().skipped.is_empty(),
        "nothing happens before confirming"
    );
    h.get_by_label("Skip Semi-annual?");
    button(&h, "Skip").click();
    h.run();
    let skipped = &h.state().session().data().skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].stage_index, 1);
}

#[test]
fn clear_due_date_needs_an_override() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    button(&h, "Set due date").click();
    h.run();
    assert!(
        button(&h, "Clear").accesskit_node().is_disabled(),
        "rule-based due date: nothing to clear"
    );
    button(&h, "Set").click();
    h.run();
    assert_eq!(h.state().session().data().due_overrides.len(), 1);
    button(&h, "Set due date").click();
    h.run();
    assert!(!button(&h, "Clear").accesskit_node().is_disabled());
}

#[test]
fn escape_with_menu_open_keeps_panel() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    h.get_by_label_contains("Mark N/A / Unassign").click();
    h.run();
    assert!(has_button(&h, "Mark N/A"));
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(
        h.query_by_label("Close panel").is_some(),
        "Esc closed the menu, not the panel"
    );
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label("Close panel").is_none());
}

#[test]
fn reopening_same_pair_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = six_completions(&dir);
    open_cell(&mut h, "Agnes \u{b7} Cobas");
    h.get_by_label("Show 2 older entries").click();
    h.run();
    assert!(h.query_by_label("Show 2 older entries").is_none());
    h.get_by_label("Close panel").click();
    h.run();
    h.get_by_label("Agnes \u{b7} Cobas").click();
    h.run();
    h.get_by_label("Show 2 older entries");
}

#[test]
fn picking_a_section_draws_its_grid_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[1].tests = vec![TestSystem {
            id: "t-ruby".into(),
            name: "Ruby".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        d.staff.push(staff("p-cara", "Cara", "MLT", "t-ruby"));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    button(&h, "Haematology").click();
    h.step();
    assert!(
        h.query_by_label("Cara \u{b7} Ruby").is_some(),
        "grid drawn in the same frame"
    );
}

#[test]
fn wheel_over_names_scrolls_grid() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: "Cobas".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        for i in 0..40 {
            d.staff.push(staff(
                &format!("p-{i:02}"),
                &format!("P{i:02}"),
                "MLT",
                "t-cobas",
            ));
        }
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    assert!(
        h.query_by_label("P39 \u{b7} Cobas").is_none(),
        "last row starts out of view"
    );
    let first = h.get_by_label("P00 \u{b7} Cobas").rect();
    let over_names = egui::pos2(first.left() - 80.0, first.center().y);
    for _ in 0..6 {
        h.event(egui::Event::PointerMoved(over_names));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -2000.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        // Smooth scrolling keeps repainting for a few frames.
        h.run_steps(10);
    }
    assert!(
        h.query_by_label("P39 \u{b7} Cobas").is_some(),
        "the grid scrolled to the bottom"
    );
}

fn go_personnel(h: &mut Harness<'static, TrackerApp>) {
    button(h, "Personnel").click();
    h.run();
}

fn status_of(h: &Harness<'static, TrackerApp>, id: &str) -> Option<StaffStatus> {
    h.state()
        .session()
        .data()
        .staff
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.status)
}

#[test]
fn remove_staff_removes_from_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    button(&h, "Agnes");
    go_personnel(&mut h);
    assert!(!has_button(&h, "Hidden"), "no Hidden tab");
    button(&h, "Actions for Agnes").click();
    h.run();
    assert!(!has_button(&h, "Hide"), "no Hide option");
    button(&h, "Remove").click();
    h.run();
    assert_eq!(status_of(&h, "p-agnes"), Some(StaffStatus::Removed));
    button(&h, "Summary").click();
    h.run();
    assert!(
        h.query_by_label("Agnes").is_none(),
        "removed staff vanish from the summary"
    );
    h.get_by_label("Nothing is overdue.");
    h.get_by_label("Staff 1");
}

/// Ticks the `index`th checkbox on the page (row checkboxes have no text).
fn tick(h: &mut Harness<'static, TrackerApp>, index: usize) {
    h.query_all_by_role(egui::accesskit::Role::CheckBox)
        .nth(index)
        .expect("checkbox")
        .click();
    h.run();
}

#[test]
fn batch_remove_restore_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    // Header checkbox first, then Agnes, Bram.
    tick(&mut h, 0);
    h.get_by_label("2 people selected");
    button(&h, "Remove").click();
    h.run();
    assert_eq!(status_of(&h, "p-agnes"), Some(StaffStatus::Removed));
    assert_eq!(status_of(&h, "p-bram"), Some(StaffStatus::Removed));
    assert!(
        h.query_by_label_contains("selected").is_none(),
        "selection cleared"
    );

    button(&h, "Removed").click();
    h.run();
    tick(&mut h, 2); // Bram
    h.get_by_label("1 person selected");
    button(&h, "Restore").click();
    h.run();
    assert_eq!(status_of(&h, "p-bram"), Some(StaffStatus::Active));

    tick(&mut h, 1); // Agnes, the only one left here
    button(&h, "Delete permanently").click();
    h.run();
    h.get_by_label("Type delete to confirm");
    assert!(button(&h, "Delete 1 person").accesskit_node().is_disabled());
    h.event(egui::Event::Text("delete".into()));
    h.run();
    button(&h, "Delete 1 person").click();
    h.run();
    assert_eq!(status_of(&h, "p-agnes"), None);
    assert_eq!(status_of(&h, "p-bram"), Some(StaffStatus::Active));
}

#[test]
fn batch_assign_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    tick(&mut h, 1); // Agnes
    h.get_by_label_contains("Assign section").click();
    h.run();
    button(&h, "Chemistry").click();
    h.run();
    let data = h.state().session().data();
    let agnes = data.staff.iter().find(|s| s.id == "p-agnes").unwrap();
    assert_eq!(agnes.assignments.len(), 2, "both Chemistry tests");
}

#[test]
fn all_sections_lists_every_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        let test = |id: &str, name: &str| TestSystem {
            id: id.into(),
            name: name.into(),
            order: 0,
            retired: false,
            rule_id: None,
        };
        d.sections[0].tests = vec![test("t-cobas", "Cobas")];
        d.sections[1].tests = vec![test("t-ruby", "Ruby")];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        d.staff.push(staff("p-cara", "Cara", "MLT", "t-ruby"));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    assert!(
        h.query_by_label("Cara \u{b7} Ruby").is_none(),
        "one section at first"
    );
    button(&h, "All sections").click();
    h.run();
    h.get_by_label("Agnes \u{b7} Cobas");
    h.get_by_label("Cara \u{b7} Ruby");
    // A test system filter narrows to its column and the people in it.
    h.get_by_label_contains("Test system").click();
    h.run();
    h.get_by_label("Ruby \u{b7} Haematology").click();
    h.run();
    assert!(h.query_by_label("Agnes \u{b7} Cobas").is_none());
    assert!(h.query_by_label("Agnes \u{b7} Ruby").is_none());
    h.get_by_label("Cara \u{b7} Ruby");
}

#[test]
fn permanent_delete_requires_typed_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    button(&h, "Actions for Bram").click();
    h.run();
    assert!(
        !has_button(&h, "Delete permanently"),
        "only offered for removed staff"
    );
    button(&h, "Remove").click();
    h.run();
    assert_eq!(status_of(&h, "p-bram"), Some(StaffStatus::Removed));
    button(&h, "Removed").click();
    h.run();
    button(&h, "Actions for Bram").click();
    h.run();
    button(&h, "Delete permanently").click();
    h.run();
    h.get_by_label("Type Bram to confirm");
    assert!(button(&h, "Delete").accesskit_node().is_disabled());
    h.event(egui::Event::Text("Bra".into()));
    h.run();
    assert!(
        button(&h, "Delete").accesskit_node().is_disabled(),
        "partial name does not unlock"
    );
    h.event(egui::Event::Text("m".into()));
    h.run();
    assert!(!button(&h, "Delete").accesskit_node().is_disabled());
    button(&h, "Delete").click();
    h.run();
    assert_eq!(status_of(&h, "p-bram"), None);
    assert!(h
        .state()
        .session()
        .data()
        .records
        .iter()
        .all(|r| r.staff_id != "p-bram"));
}

#[test]
fn add_staff_with_whole_section_assigned() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    h.get_by_label_contains("Add staff").click();
    h.run();
    h.event(egui::Event::Text("Dana".into()));
    h.run();
    h.query_all_by_label("Assign whole section")
        .next()
        .expect("a section with tests")
        .click();
    h.run();
    button(&h, "Save").click();
    h.run();
    let data = h.state().session().data();
    let dana = data.staff.iter().find(|s| s.name == "Dana").expect("added");
    assert_eq!(dana.assignments.len(), 2, "both Chemistry tests assigned");
    assert!(dana
        .assignments
        .values()
        .all(|a| a.state == AssignState::Assigned));
    assert!(!has_button(&h, "Save"), "editor closed");
}

#[test]
fn profile_opens_detail_for_a_test() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    h.get_by_label("Agnes").click();
    h.run();
    h.get_by_label("\u{2190} All staff");
    h.get_by_label("Cobas").click();
    h.run();
    h.get_by_label("Close panel");
    h.get_by_label("\u{2190} All staff").click();
    h.run();
    h.get_by_label("Bram");
}

fn open_agnes(h: &mut Harness<'static, TrackerApp>) {
    go_personnel(h);
    h.get_by_label("Agnes").click();
    h.run();
}

#[test]
fn profile_status_strip_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    h.get_by_label("Overdue 1");
    h.get_by_label("Due soon 0");
    h.get_by_label("Up to date 0");
    h.get_by_label("Not done 0");
}

#[test]
fn profile_next_up_opens_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    h.get_by_label_contains("Next up: Cobas").click();
    h.run();
    h.get_by_label("Close panel");
}

#[test]
fn profile_cards_show_last_done() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    h.get_by_label_contains("Last done ");
}

#[test]
fn profile_cards_narrow_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.set_size(egui::vec2(900.0, 800.0));
    h.run();
    open_agnes(&mut h);
    h.get_by_label("Cobas");
    h.get_by_label("Overdue 1");
}

#[test]
fn profile_table_view() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    h.get_by_label("Table").click();
    h.run();
    h.get_by_label("Last done");
    h.get_by_label("Cobas").click();
    h.run();
    h.get_by_label("Close panel");
}

#[test]
fn profile_filter_survives_view_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = agnes_two_tests(&dir);
    open_agnes(&mut h);
    h.get_by_label("Overdue 1").click();
    h.run();
    h.get_by_label("Table").click();
    h.run();
    h.get_by_label("Cobas");
    assert!(h.query_by_label("Architect").is_none(), "filtered out");
}

/// Agnes on Cobas (overdue) and Architect (on track).
fn agnes_two_tests(dir: &tempfile::TempDir) -> Harness<'static, TrackerApp> {
    let mut s = session(dir, 1);
    s.mutate(now_ts(), |d, _| {
        let test = |id: &str, name: &str, order| TestSystem {
            id: id.into(),
            name: name.into(),
            order,
            retired: false,
            rule_id: None,
        };
        d.sections[0].tests = vec![test("t-cobas", "Cobas", 0), test("t-arch", "Architect", 1)];
        let mut agnes = staff("p-agnes", "Agnes", "MLT", "t-cobas");
        let since = agnes.assignments["t-cobas"].since;
        agnes.assignments.insert(
            "t-arch".into(),
            Assignment {
                state: AssignState::Assigned,
                since,
            },
        );
        d.staff.push(agnes);
        d.records.push(initial("p-agnes", "t-cobas", 365));
        d.records.push(initial("p-agnes", "t-arch", 30));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    h
}

#[test]
fn profile_view_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    h.get_by_label("Table").click();
    h.run();
    assert_eq!(
        h.state().prefs().profile_view,
        competency_tracker::prefs::ProfileView::Table
    );
}

fn go_settings_tab(h: &mut Harness<'static, TrackerApp>, tab: &str) {
    button(h, "Settings").click();
    h.run();
    button(h, tab).click();
    h.run();
}

#[test]
fn rename_section_reflects_in_nav() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_settings_tab(&mut h, "Sections & tests");
    button(&h, "Chemistry").click();
    h.run();
    // The field starts with the whole name selected: typing replaces it.
    h.event(egui::Event::Text("Clinical Chemistry".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        h.state().session().data().sections_sorted()[0].name,
        "Clinical Chemistry"
    );
    button(&h, "Sections").click();
    h.run();
    button(&h, "Clinical Chemistry");
    assert!(
        !has_button(&h, "Chemistry"),
        "the old name is gone from the section tabs"
    );
}

#[test]
fn audit_filter_by_user() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        let entry = |by: &str, detail: &str| competency_tracker::model::AuditEntry {
            at: now_ts(),
            by: by.into(),
            action: "staff.add".into(),
            detail: detail.into(),
            refs: Default::default(),
        };
        d.audit.push(entry("jtan", "Added Bob to personnel"));
        d.audit.push(entry("mlee", "Added Cara to personnel"));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "History").click();
    h.run();
    h.get_by_label("Added Bob to personnel");
    h.get_by_label("Added Cara to personnel");
    h.get_by_label_contains("User").click();
    h.run();
    h.get_by_label("mlee").click();
    h.run();
    assert!(
        h.query_by_label("Added Bob to personnel").is_none(),
        "jtan's entry is filtered out"
    );
    h.get_by_label("Added Cara to personnel");
}

const LONG_TEST: &str =
    "Cobas c311 Integrated Chemistry and Immunoassay Analyzer Module with Sample Handling Unit and ISE";

/// Agnes (overdue) on a test with a very long name; returns her pill's text.
fn seeded_long(dir: &tempfile::TempDir) -> (Harness<'static, TrackerApp>, String) {
    let mut s = session(dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: LONG_TEST.into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        d.records.push(initial("p-agnes", "t-cobas", 365));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    let done = now_ts().date_naive() - chrono::Days::new(365);
    let due = fmt_date(done.checked_add_months(chrono::Months::new(6)).unwrap());
    (h, due)
}

/// The left edge of the open detail panel (its heading is the right-most "Agnes").
fn panel_left(h: &Harness<'static, TrackerApp>) -> f32 {
    h.query_all_by_label("Agnes")
        .map(|n| n.rect().left())
        .fold(f32::MIN, f32::max)
}

#[test]
fn profile_table_fits_beside_panel() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, due) = seeded_long(&dir);
    open_agnes(&mut h);
    h.get_by_label("Table").click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, LONG_TEST)
        .click();
    h.run();
    h.run();
    let left = panel_left(&h);
    // The panel shows the same pill; only the page's own ones count.
    let pills: Vec<_> = h
        .query_all_by_label(&due)
        .filter(|p| p.rect().left() < left)
        .collect();
    assert!(!pills.is_empty());
    for pill in pills {
        assert!(pill.rect().right() <= left, "pill under the panel");
    }
}

#[test]
fn profile_cards_fit_beside_panel() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, due) = seeded_long(&dir);
    open_agnes(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Button, LONG_TEST)
        .click();
    h.run();
    h.run();
    let left = panel_left(&h);
    // The panel shows the same pill; only the page's own ones count.
    let pills: Vec<_> = h
        .query_all_by_label(&due)
        .filter(|p| p.rect().left() < left)
        .collect();
    assert!(!pills.is_empty());
    for pill in pills {
        assert!(pill.rect().right() <= left, "pill under the panel");
    }
    let next = h.get_by_label_contains("Next up:");
    assert!(next.rect().right() <= left, "next-up line under the panel");
}

#[test]
fn profile_filter_resets_on_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = agnes_two_tests(&dir);
    open_agnes(&mut h);
    h.get_by_label("Overdue 1").click();
    h.run();
    assert!(h.query_by_label("Architect").is_none(), "filtered");
    h.get_by_label("\u{2190} All staff").click();
    h.run();
    h.get_by_label("Agnes").click();
    h.run();
    h.get_by_label("Architect");
}

/// History with one entry whose refs name `section_id`.
fn history_with_section(
    dir: &tempfile::TempDir,
    section_id: Option<&str>,
) -> Harness<'static, TrackerApp> {
    let mut s = session(dir, 1);
    s.mutate(now_ts(), |d, _| {
        let sec = section_id.map_or_else(|| d.sections_sorted()[0].id.clone(), str::to_owned);
        d.audit.push(competency_tracker::model::AuditEntry {
            at: now_ts(),
            by: "jtan".into(),
            action: "record.add".into(),
            detail: "Marked Cobas complete for Bob".into(),
            refs: [("section_id".to_string(), sec)].into(),
        });
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "History").click();
    h.run();
    h
}

#[test]
fn history_row_shows_section_chip() {
    let dir = tempfile::tempdir().unwrap();
    let h = history_with_section(&dir, None);
    h.get_by_label("Marked Cobas complete for Bob");
    h.get_by_label("Chemistry");
}

#[test]
fn history_row_deleted_section() {
    let dir = tempfile::tempdir().unwrap();
    let h = history_with_section(&dir, Some("s-gone"));
    h.get_by_label("Marked Cobas complete for Bob");
    assert!(h.query_by_label("Chemistry").is_none());
}

#[test]
fn detail_subtitle_has_section_chip() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    open_agnes(&mut h);
    assert_eq!(h.query_all_by_label("Chemistry").count(), 1, "card heading");
    h.get_by_label("Cobas").click();
    h.run();
    assert_eq!(
        h.query_all_by_label("Chemistry").count(),
        2,
        "plus the panel's chip"
    );
}

#[test]
fn confirm_all_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem {
            id: "t-cobas".into(),
            name: "Cobas".into(),
            order: 0,
            retired: false,
            rule_id: None,
        }];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        for i in 0..2u64 {
            let mut r = initial("p-agnes", "t-cobas", 400 - i * 200);
            r.id = format!("r-{i}");
            r.stage_index = i as u32;
            r.imported = true;
            r.unconfirmed = true;
            d.records.push(r);
        }
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    go_settings_tab(&mut h, "Import & export");
    button(&h, "Confirm all").click();
    h.run();
    assert_eq!(
        h.state()
            .session()
            .data()
            .records
            .iter()
            .filter(|r| r.unconfirmed)
            .count(),
        2,
        "asks first"
    );
    h.get_by_label("Confirm all 2 records?");
    button(&h, "Confirm").click();
    h.run();
    assert_eq!(
        h.state()
            .session()
            .data()
            .records
            .iter()
            .filter(|r| r.unconfirmed)
            .count(),
        0
    );
}

#[test]
fn settings_change_recomputes_pills() {
    let dir = tempfile::tempdir().unwrap();
    // Agnes's Semi-annual falls due 45 days from today.
    let today = now_ts().date_naive();
    let done = (today + chrono::Days::new(45))
        .checked_sub_months(chrono::Months::new(6))
        .unwrap();
    let mut h = seeded(&dir, (today - done).num_days() as u64);
    h.get_by_label("Due soon 0");
    go_settings_tab(&mut h, "General");
    let due_soon = h
        .query_all_by_role(egui::accesskit::Role::SpinButton)
        .next()
        .expect("due-soon field");
    due_soon.click();
    h.run();
    h.event(egui::Event::Text("60".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    button(&h, "Save settings").click();
    h.run();
    assert_eq!(h.state().session().data().settings.due_soon_days, 60);
    button(&h, "Summary").click();
    h.run();
    h.get_by_label("Due soon 1");
}

/// The Summary page's search box (the last text input in the tree).
fn summary_search<'a>(h: &'a Harness<'static, TrackerApp>) -> egui_kittest::Node<'a> {
    h.query_all_by_role(egui::accesskit::Role::TextInput)
        .last()
        .expect("search box")
}
