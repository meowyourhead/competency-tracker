//! Smoke tests for the app shell, driven headlessly with egui_kittest.

use std::time::Instant;

use competency_tracker::dates::now_ts;
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
