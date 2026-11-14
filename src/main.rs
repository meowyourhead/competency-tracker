#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use competency_tracker::dates::now_ts;
use competency_tracker::identity::{machine_name, windows_username};
use competency_tracker::prefs::{load_prefs_from, prefs_path, Prefs};
use competency_tracker::session::Session;
use competency_tracker::snapshot::resolve_data_dir;
use competency_tracker::ui::app::TrackerApp;

const TITLE: &str = "Competency Tracker";

fn open_session() -> Result<Session, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Couldn't locate the program file: {e}"))?;
    let exe_dir = exe.parent().ok_or("Couldn't locate the program folder")?;
    let dir = resolve_data_dir(exe_dir);
    let root = dir.root.clone();
    Session::open(dir, &windows_username(), &machine_name(), std::process::id(), now_ts(), Instant::now())
        .map_err(|e| format!("Couldn't open the data folder {}:\n{e}", root.display()))
}

fn fail(message: &str) -> ! {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(TITLE)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1);
}

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 600.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app.png")) {
        viewport = viewport.with_icon(icon);
    }
    eframe::NativeOptions { viewport, renderer, ..Default::default() }
}

fn run(renderer: eframe::Renderer, session: &RefCell<Option<Session>>, prefs: &Prefs, path: &Option<PathBuf>) -> eframe::Result {
    eframe::run_native(
        TITLE,
        options(renderer),
        Box::new(|cc| {
            // A failed first attempt may already have used the session.
            let session = match session.borrow_mut().take() {
                Some(s) => s,
                None => open_session()?,
            };
            Ok(Box::new(TrackerApp::new(cc, session, prefs.clone(), path.clone())))
        }),
    )
}

fn main() {
    let session = open_session().unwrap_or_else(|e| fail(&e));
    let path = prefs_path();
    let prefs = path.as_deref().map(load_prefs_from).unwrap_or_default();
    let session = RefCell::new(Some(session));
    if run(eframe::Renderer::Wgpu, &session, &prefs, &path).is_err() {
        if let Err(e) = run(eframe::Renderer::Glow, &session, &prefs, &path) {
            fail(&format!("Couldn't start the graphics system:\n{e}"));
        }
    }
}
