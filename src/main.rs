#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use competency_tracker::dates::now_ts;
use competency_tracker::identity::{machine_name, windows_username};
use competency_tracker::prefs::{load_prefs_from, prefs_path, Prefs};
use competency_tracker::session::Session;
use competency_tracker::snapshot::{first_run_prompt, resolve_data_dir_with_source};
use competency_tracker::startup::{
    diagnostics_dir, init_log, load_state, log_path, note, save_state, state_path, Graphics,
    GraphicsState,
};
use competency_tracker::ui::app::TrackerApp;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;

const TITLE: &str = "Competency Tracker";

/// Frames a graphics mode must draw before it counts as working.
const FRAMES_TO_CONFIRM: u64 = 3;
/// How long a graphics mode may take to draw them before it counts as stuck.
const STUCK_AFTER: Duration = Duration::from_secs(30);

/// The message of the most recent panic, for the error dialog.
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

fn open_session() -> Result<Session, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Couldn't locate the program file: {e}"))?;
    let exe_dir = exe.parent().ok_or("Couldn't locate the program folder")?;
    let (dir, from_config) = resolve_data_dir_with_source(exe_dir);
    if let Some(question) = first_run_prompt(&dir, from_config) {
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Info)
            .set_title(TITLE)
            .set_description(question)
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show();
        if answer != rfd::MessageDialogResult::Ok {
            // Cancelled: leave the disk untouched.
            std::process::exit(0);
        }
    }
    let root = dir.root.clone();
    Session::open(
        dir,
        &windows_username(),
        &machine_name(),
        std::process::id(),
        now_ts(),
        Instant::now(),
    )
    .map_err(|e| format!("Couldn't open the data folder {}:\n{e}", root.display()))
}

fn fail(message: &str) -> ! {
    note(&format!("FAILED: {message}"));
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(TITLE)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1);
}

fn wgpu_on(backends: wgpu::Backends) -> WgpuConfiguration {
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends;
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

fn options(graphics: Graphics) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 600.0])
        // Opens filling the screen; "Restore down" returns to the size above.
        .with_maximized(true)
        // The app draws its own minimise / maximise / close buttons.
        .with_decorations(false)
        .with_resizable(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app.png")) {
        viewport = viewport.with_icon(icon);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match graphics {
        Graphics::Dx12 => {
            options.renderer = eframe::Renderer::Wgpu;
            options.wgpu_options = wgpu_on(wgpu::Backends::DX12);
        }
        Graphics::Vulkan => {
            options.renderer = eframe::Renderer::Wgpu;
            options.wgpu_options = wgpu_on(wgpu::Backends::VULKAN | wgpu::Backends::GL);
        }
        Graphics::OpenGl => options.renderer = eframe::Renderer::Glow,
    }
    options
}

/// One try at starting a graphics mode, shared with its [`watch`] thread.
struct Attempt {
    graphics: Graphics,
    /// Set once the window exists.
    ctx: Mutex<Option<egui::Context>>,
    /// The mode drew its first frames.
    confirmed: AtomicBool,
    /// `run` returned (or panicked).
    finished: AtomicBool,
}

/// Waits for the attempt to draw its first frames, then records the mode as
/// working. A mode that draws nothing for [`STUCK_AFTER`] is left marked as
/// failed (so the next start skips it) and the program closes with a message
/// rather than sitting invisible.
fn watch(attempt: Arc<Attempt>, state_file: PathBuf, mut state: GraphicsState) {
    let started = Instant::now();
    let frames = |a: &Attempt| {
        let ctx = a.ctx.lock().unwrap_or_else(|e| e.into_inner());
        ctx.as_ref().map(|c| {
            c.request_repaint();
            c.cumulative_frame_nr()
        })
    };
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if attempt.finished.load(Ordering::SeqCst) {
            return;
        }
        let drawn = frames(&attempt).unwrap_or(0);
        if drawn >= FRAMES_TO_CONFIRM {
            break;
        }
        if started.elapsed() > STUCK_AFTER {
            let label = attempt.graphics.label();
            note(&format!(
                "{label}: no window after {STUCK_AFTER:?} ({drawn} frames)"
            ));
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title(TITLE)
                .set_description(format!(
                    "The window didn't appear using {label} graphics.\n\n\
                     Close this message and start the program again: it will use a \
                     different graphics mode."
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            if attempt.finished.load(Ordering::SeqCst) {
                return;
            }
            if frames(&attempt).unwrap_or(0) >= FRAMES_TO_CONFIRM {
                // It got there while the message was up.
                break;
            }
            std::process::exit(1);
        }
    }
    attempt.confirmed.store(true, Ordering::SeqCst);
    state.trying = None;
    let _ = save_state(&state_file, &state);
    note(&format!("{}: window is drawing", attempt.graphics.label()));
}

fn run(
    attempt: &Arc<Attempt>,
    session: &RefCell<Option<Session>>,
    prefs: &Prefs,
    path: &Option<PathBuf>,
) -> eframe::Result {
    eframe::run_native(
        TITLE,
        options(attempt.graphics),
        Box::new(|cc| {
            // A failed first attempt may already have used the session.
            let session = match session.borrow_mut().take() {
                Some(s) => s,
                None => open_session()?,
            };
            let app = TrackerApp::new(cc, session, prefs.clone(), path.clone());
            *attempt.ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let text = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".to_string());
        let message = match info.location() {
            Some(l) => format!("{text} (at {}:{})", l.file(), l.line()),
            None => text,
        };
        note(&format!("PANIC: {message}"));
        *LAST_PANIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
    }));
}

fn last_panic() -> String {
    LAST_PANIC
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .unwrap_or_else(|| "unknown error".to_string())
}

fn start(diagnostics: &Path) {
    let session = open_session().unwrap_or_else(|e| fail(&e));
    note("data folder opened");
    let path = prefs_path();
    let prefs = path.as_deref().map(load_prefs_from).unwrap_or_default();
    let session = RefCell::new(Some(session));

    let state_file = state_path(diagnostics);
    let mut state = load_state(&state_file);
    if let Some(g) = state.trying {
        note(&format!(
            "the last run stopped while starting {}; skipping it",
            g.label()
        ));
    }
    let order = state.plan();
    let details = format!("Details are in:\n{}", log_path(diagnostics).display());

    let mut errors = Vec::new();
    for graphics in order {
        note(&format!("starting {}", graphics.label()));
        state.trying = Some(graphics);
        let _ = save_state(&state_file, &state);
        let attempt = Arc::new(Attempt {
            graphics,
            ctx: Mutex::new(None),
            confirmed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let watcher = {
            let (attempt, state_file, state) = (attempt.clone(), state_file.clone(), state.clone());
            std::thread::spawn(move || watch(attempt, state_file, state))
        };
        let result = catch_unwind(AssertUnwindSafe(|| run(&attempt, &session, &prefs, &path)));
        attempt.finished.store(true, Ordering::SeqCst);
        let _ = watcher.join();
        // It ended by itself (cleanly or with an error we can report), so it
        // is not a mode to skip next time.
        state.trying = None;
        let _ = save_state(&state_file, &state);

        let worked = attempt.confirmed.load(Ordering::SeqCst);
        let error = match result {
            Ok(Ok(())) => {
                note("closed normally");
                return;
            }
            Ok(Err(e)) => e.to_string(),
            Err(_) => last_panic(),
        };
        if worked {
            // Not a start-up problem: the program was running.
            fail(&format!(
                "Competency Tracker hit an unexpected error and has to close.\n\n{error}\n\n{details}"
            ));
        }
        note(&format!("{} did not start: {error}", graphics.label()));
        errors.push(format!("{}: {error}", graphics.label()));
    }
    fail(&format!(
        "Couldn't start the graphics system.\n\n{}\n\n{details}",
        errors.join("\n")
    ));
}

fn main() {
    let diagnostics = diagnostics_dir();
    init_log(&diagnostics);
    install_panic_hook();
    competency_tracker::update::clean_up_previous();
    note(&format!(
        "Competency Tracker {} starting from {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("(unknown: {e})"))
    ));
    // A panic outside the graphics start-up (opening the data folder, say).
    if catch_unwind(AssertUnwindSafe(|| start(&diagnostics))).is_err() {
        fail(&format!(
            "Competency Tracker hit an unexpected error and has to close.\n\n{}\n\nDetails are in:\n{}",
            last_panic(),
            log_path(&diagnostics).display()
        ));
    }
}
