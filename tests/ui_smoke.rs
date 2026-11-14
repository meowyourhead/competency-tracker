//! Smoke tests for the app shell, driven headlessly with egui_kittest.

use std::time::Instant;

use competency_tracker::dates::now_ts;
use competency_tracker::model::{AssignState, Assignment, Record, Staff, StaffStatus, TestSystem};
use competency_tracker::session::Session;
use competency_tracker::snapshot::DataDir;
use competency_tracker::ui::app::TrackerApp;
use egui_kittest::kittest::Queryable;
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
