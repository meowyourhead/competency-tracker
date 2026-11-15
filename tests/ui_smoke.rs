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
    let data_dir = DataDir { root: dir.path().join("CompetencyData") };
    Session::open(data_dir, "tester", "pc", pid, now_ts(), Instant::now()).unwrap()
}

#[test]
fn shows_sidebar_nav() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    for label in ["Summary", "Sections", "Personnel", "Audit log", "Settings"] {
        h.get_by_role_and_label(egui::accesskit::Role::Button, label);
    }
    assert!(h.query_by_label("Competency Tracker").is_some());
}

#[test]
fn viewer_banner_when_lock_held() {
    let dir = tempfile::tempdir().unwrap();
    // Another process (pid 999) takes the edit lock first.
    let other = {
        let data_dir = DataDir { root: dir.path().join("CompetencyData") };
        Session::open(data_dir, "agnes", "pc2", 999, now_ts(), Instant::now()).unwrap()
    };
    assert!(other.is_editing());
    let h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.get_by_label_contains("agnes is editing");
}

#[test]
fn failed_save_on_close_cancels_and_offers_retry() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(&dir, 1);
    assert!(s.is_editing());
    let mut h = harness(TrackerApp::new_for_test(s));
    // Make saving impossible: replace the snapshots folder with a file.
    let snaps = dir.path().join("CompetencyData").join("snapshots");
    std::fs::remove_dir_all(&snaps).unwrap();
    std::fs::write(&snaps, b"not a folder").unwrap();

    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
    let commands = &h.output().viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(commands.iter().any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
    h.run();
    h.get_by_label_contains("Couldn't save your changes:");
    h.get_by_label("Retry");
    h.get_by_label("Quit anyway");
    assert!(h.state().session().is_editing(), "edits and lock are kept until the user decides");
}

fn close_window(h: &mut Harness<'static, TrackerApp>) {
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
}

fn root_commands(h: &Harness<'static, TrackerApp>) -> Vec<egui::ViewportCommand> {
    h.output().viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

#[test]
fn close_after_lock_lost_shows_conflict_then_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| d.settings.due_soon_days = 45).unwrap();
    // Another process takes over the lock while our edits are unsaved.
    let data_dir = DataDir { root: dir.path().join("CompetencyData") };
    std::fs::remove_file(data_dir.lock_path()).unwrap();
    let other = Session::open(data_dir, "agnes", "pc2", 999, now_ts(), Instant::now()).unwrap();
    assert!(other.is_editing());

    let mut h = harness(TrackerApp::new_for_test(s));
    close_window(&mut h);
    assert!(root_commands(&h).iter().any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
    h.run();
    h.get_by_label_contains("Someone else took over editing. Your changes were saved to");
    h.get_by_label("Close").click();
    h.step();
    assert!(root_commands(&h).iter().any(|c| matches!(c, egui::ViewportCommand::Close)));
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
        assignments: [(test_id.to_string(), Assignment { state: AssignState::Assigned, since })].into(),
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
        let test = |id: &str, name: &str, order| TestSystem { id: id.into(), name: name.into(), order, retired: false, rule_id: None };
        d.sections[0].tests = vec![test("t-cobas", "Cobas", 0), test("t-arch", "Architect", 1)];
        d.staff.push(staff("p-agnes", "Agnes", "MLT", "t-cobas"));
        d.staff.push(staff("p-bram", "Bram", "Senior MLT", "t-arch"));
        d.records.push(initial("p-agnes", "t-cobas", agnes_days_ago));
        d.records.push(initial("p-bram", "t-arch", 30));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    h
}

#[test]
fn summary_shows_overdue_count_and_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = seeded(&dir, 365);
    h.get_by_label("Overdue 1");
    h.get_by_label("Due within 30 days 0");
    h.get_by_label("On track 1");
    h.get_by_label("Delayed 0");
    h.get_by_label("Agnes");
}

#[test]
fn summary_stat_card_filters_and_toggles_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    assert!(h.query_by_label("Bram").is_some());
    h.get_by_label("Overdue 1").click();
    h.run();
    assert!(h.query_by_label("Bram").is_none(), "only overdue rows are listed");
    assert!(h.query_by_label("Agnes").is_some());
    h.get_by_label("Overdue 1").click();
    h.run();
    assert!(h.query_by_label("Bram").is_some(), "a second click resets to all");
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
fn summary_groups_by_test() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    // By person: one card per person, the section only in the row details.
    assert!(h.query_by_label("Chemistry").is_none());
    h.get_by_label("By test").click();
    h.run();
    // By test: a card per test with its section in the header.
    assert_eq!(h.query_all_by_label("Chemistry").count(), 2);
    h.get_by_label("Agnes \u{b7} Cobas");
}

#[test]
fn summary_empty_states() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    h.run();
    h.get_by_label_contains("No data yet");

    let dir = tempfile::tempdir().unwrap();
    let h = seeded(&dir, 30); // both on track
    h.get_by_label("Everyone is up to date");
    assert!(h.query_by_label("Agnes").is_none());
}

#[test]
fn fonts_register_serif_and_medium_and_headings_render() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(TrackerApp::new_for_test(session(&dir, 1)));
    // Fonts installed on the first pass take effect on the next one.
    h.run();
    h.step();
    let families: Vec<egui::FontFamily> = h.ctx.fonts(|f| f.definitions().families.keys().cloned().collect());
    for name in ["serif", "serif-regular", "medium"] {
        assert!(families.contains(&egui::FontFamily::Name(name.into())), "missing family {name}");
    }
    let regular = h.ctx.fonts(|f| f.definitions().families[&egui::FontFamily::Proportional][0].clone());
    assert_eq!(regular, "Inter-Regular");
    // The Summary page title is a serif heading and must render without panicking.
    assert!(h.query_all_by_label("Summary").count() >= 2, "sidebar entry and serif page title");
}

#[test]
fn summary_search_narrowing_on_track_person_lists_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 30); // both on track
    h.get_by_label("Everyone is up to date");
    summary_search(&h).click();
    h.run();
    summary_search(&h).type_text("Agnes");
    h.run();
    assert!(h.query_by_label("Everyone is up to date").is_none(), "a narrowing filter lists rows instead");
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
    assert!(h.query_by_label("Everyone is up to date").is_none());
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Sections").click();
    h.run();
    h.get_by_label(cell).click();
    h.run();
}

#[test]
fn grid_shows_pill_and_opens_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Sections").click();
    h.run();
    let done = now_ts().date_naive() - chrono::Days::new(365);
    let due = fmt_date(done.checked_add_months(chrono::Months::new(6)).unwrap());
    h.get_by_label(&due);
    // Both people's next stage is Semi-annual: one label under each pill.
    assert_eq!(h.query_all_by_label("Semi-annual").count(), 2, "stage label under the pill");
    assert!(h.query_by_label("Bram \u{b7} Architect").is_some());
    assert!(h.query_by_label("Close panel").is_none());
    h.get_by_label("Agnes \u{b7} Cobas").click();
    h.run();
    h.get_by_label("Close panel");
    h.get_by_label("Cobas \u{b7} Chemistry");
}

/// Agnes has six Cobas completions, one stage a year apart.
fn six_completions(dir: &tempfile::TempDir) -> (Harness<'static, TrackerApp>, Vec<String>) {
    let mut s = session(dir, 1);
    let labels = ["Initial", "Semi-annual", "1st Annual", "2nd Annual", "3rd Annual", "4th Annual"];
    let today = now_ts().date_naive();
    let lines: Vec<String> = (0..6u64)
        .map(|i| format!("{} \u{b7} {}", labels[i as usize], fmt_date(today - chrono::Days::new(2200 - i * 365))))
        .collect();
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem { id: "t-cobas".into(), name: "Cobas".into(), order: 0, retired: false, rule_id: None }];
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
        assert!(h.query_by_label(line).is_none(), "{line} is hidden at first");
    }
    h.get_by_label("Show 2 older entries").click();
    h.run();
    for line in &lines {
        h.get_by_label(line);
    }
    assert!(h.query_by_label("Show 2 older entries").is_none());
}

#[test]
fn mark_complete_disabled_when_viewing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem { id: "t-cobas".into(), name: "Cobas".into(), order: 0, retired: false, rule_id: None }];
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Mark complete").click();
    h.run();
    let comment = h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).next().expect("comment box");
    comment.click();
    h.run();
    h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).next().unwrap().type_text("Bench 2");
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.run();
    let records = &h.state().session().data().records;
    assert_eq!(records.len(), before + 1);
    let added = records.last().unwrap();
    assert_eq!((added.by.as_str(), added.stage_index, added.comment.as_str()), ("tester", 1, "Bench 2"));
    assert_eq!(added.completed_on, now_ts().date_naive());
}

fn button<'a>(h: &'a Harness<'static, TrackerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Button, label)
}

fn has_button(h: &Harness<'static, TrackerApp>, label: &str) -> bool {
    h.query_by_role_and_label(egui::accesskit::Role::Button, label).is_some()
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
    let agnes = h.state().session().data().staff.iter().find(|s| s.id == "p-agnes").unwrap().clone();
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
    assert!(h.state().session().data().skipped.is_empty(), "nothing happens before confirming");
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
    assert!(button(&h, "Clear").accesskit_node().is_disabled(), "rule-based due date: nothing to clear");
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
    assert!(h.query_by_label("Close panel").is_some(), "Esc closed the menu, not the panel");
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
        d.sections[1].tests = vec![TestSystem { id: "t-ruby".into(), name: "Ruby".into(), order: 0, retired: false, rule_id: None }];
        d.staff.push(staff("p-cara", "Cara", "MLT", "t-ruby"));
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    button(&h, "Haematology").click();
    h.step();
    assert!(h.query_by_label("Cara \u{b7} Ruby").is_some(), "grid drawn in the same frame");
}

#[test]
fn wheel_over_names_scrolls_grid() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem { id: "t-cobas".into(), name: "Cobas".into(), order: 0, retired: false, rule_id: None }];
        for i in 0..40 {
            d.staff.push(staff(&format!("p-{i:02}"), &format!("P{i:02}"), "MLT", "t-cobas"));
        }
    })
    .unwrap();
    let mut h = harness(TrackerApp::new_for_test(s));
    h.run();
    button(&h, "Sections").click();
    h.run();
    assert!(h.query_by_label("P39 \u{b7} Cobas").is_none(), "last row starts out of view");
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
    assert!(h.query_by_label("P39 \u{b7} Cobas").is_some(), "the grid scrolled to the bottom");
}

fn go_personnel(h: &mut Harness<'static, TrackerApp>) {
    button(h, "Personnel").click();
    h.run();
}

fn status_of(h: &Harness<'static, TrackerApp>, id: &str) -> Option<StaffStatus> {
    h.state().session().data().staff.iter().find(|s| s.id == id).map(|s| s.status)
}

#[test]
fn hide_staff_removes_from_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    h.get_by_label("Agnes");
    go_personnel(&mut h);
    button(&h, "Actions for Agnes").click();
    h.run();
    button(&h, "Hide").click();
    h.run();
    assert_eq!(status_of(&h, "p-agnes"), Some(StaffStatus::Hidden));
    button(&h, "Summary").click();
    h.run();
    assert!(h.query_by_label("Agnes").is_none(), "hidden staff vanish from the summary");
    h.get_by_label("Everyone is up to date");
}

#[test]
fn permanent_delete_requires_typed_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = seeded(&dir, 365);
    go_personnel(&mut h);
    button(&h, "Actions for Bram").click();
    h.run();
    assert!(!has_button(&h, "Delete permanently"), "only offered for removed staff");
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
    assert!(button(&h, "Delete").accesskit_node().is_disabled(), "partial name does not unlock");
    h.event(egui::Event::Text("m".into()));
    h.run();
    assert!(!button(&h, "Delete").accesskit_node().is_disabled());
    button(&h, "Delete").click();
    h.run();
    assert_eq!(status_of(&h, "p-bram"), None);
    assert!(h.state().session().data().records.iter().all(|r| r.staff_id != "p-bram"));
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
    h.query_all_by_label("Assign whole section").next().expect("a section with tests").click();
    h.run();
    button(&h, "Save").click();
    h.run();
    let data = h.state().session().data();
    let dana = data.staff.iter().find(|s| s.name == "Dana").expect("added");
    assert_eq!(dana.assignments.len(), 2, "both Chemistry tests assigned");
    assert!(dana.assignments.values().all(|a| a.state == AssignState::Assigned));
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
    assert_eq!(h.state().session().data().sections_sorted()[0].name, "Clinical Chemistry");
    button(&h, "Sections").click();
    h.run();
    button(&h, "Clinical Chemistry");
    assert!(!has_button(&h, "Chemistry"), "the old name is gone from the section tabs");
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
    button(&h, "Audit log").click();
    h.run();
    h.get_by_label("Added Bob to personnel");
    h.get_by_label("Added Cara to personnel");
    h.get_by_label_contains("User").click();
    h.run();
    h.get_by_label("mlee").click();
    h.run();
    assert!(h.query_by_label("Added Bob to personnel").is_none(), "jtan's entry is filtered out");
    h.get_by_label("Added Cara to personnel");
}

#[test]
fn confirm_all_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&dir, 1);
    s.mutate(now_ts(), |d, _| {
        d.sections[0].tests = vec![TestSystem { id: "t-cobas".into(), name: "Cobas".into(), order: 0, retired: false, rule_id: None }];
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
    assert_eq!(h.state().session().data().records.iter().filter(|r| r.unconfirmed).count(), 2, "asks first");
    h.get_by_label("Confirm all 2 records?");
    button(&h, "Confirm").click();
    h.run();
    assert_eq!(h.state().session().data().records.iter().filter(|r| r.unconfirmed).count(), 0);
}

#[test]
fn settings_change_recomputes_pills() {
    let dir = tempfile::tempdir().unwrap();
    // Agnes's Semi-annual falls due 45 days from today.
    let today = now_ts().date_naive();
    let done = (today + chrono::Days::new(45)).checked_sub_months(chrono::Months::new(6)).unwrap();
    let mut h = seeded(&dir, (today - done).num_days() as u64);
    h.get_by_label("Due within 30 days 0");
    go_settings_tab(&mut h, "General");
    let due_soon = h.query_all_by_role(egui::accesskit::Role::SpinButton).next().expect("due-soon field");
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
    h.get_by_label("Due within 60 days 1");
}

/// The Summary page's search box (the last text input in the tree).
fn summary_search<'a>(h: &'a Harness<'static, TrackerApp>) -> egui_kittest::Node<'a> {
    h.query_all_by_role(egui::accesskit::Role::TextInput).last().expect("search box")
}

