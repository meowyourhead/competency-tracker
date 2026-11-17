//! Start-up diagnostics: a log file, and a record of which graphics modes
//! have failed on this PC so the next start can skip them.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The ways the window can be drawn, in the order they are tried.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Graphics {
    /// wgpu on Direct3D 12 (includes Windows' built-in software renderer).
    Dx12,
    /// wgpu on Vulkan or OpenGL.
    Vulkan,
    /// Plain OpenGL (glow).
    OpenGl,
}

impl Graphics {
    pub const ALL: [Graphics; 3] = [Graphics::Dx12, Graphics::Vulkan, Graphics::OpenGl];

    pub fn label(self) -> &'static str {
        match self {
            Graphics::Dx12 => "Direct3D 12",
            Graphics::Vulkan => "Vulkan",
            Graphics::OpenGl => "OpenGL",
        }
    }
}

/// Saved between runs. Unknown or missing fields take their defaults.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default)]
pub struct GraphicsState {
    /// Modes that took the whole program down (or never showed a window).
    pub crashed: Vec<Graphics>,
    /// The mode being started; cleared once it has drawn a few frames. Still
    /// set at the next start means the program died while starting it.
    pub trying: Option<Graphics>,
}

impl GraphicsState {
    /// The modes to try this run, in order. A mode the last run died in is
    /// added to `crashed` and skipped; once every mode has crashed the list
    /// starts over, so a driver update gets another chance.
    pub fn plan(&mut self) -> Vec<Graphics> {
        if let Some(g) = self.trying.take() {
            if !self.crashed.contains(&g) {
                self.crashed.push(g);
            }
        }
        if Graphics::ALL.iter().all(|g| self.crashed.contains(g)) {
            self.crashed.clear();
        }
        Graphics::ALL
            .into_iter()
            .filter(|g| !self.crashed.contains(g))
            .collect()
    }
}

/// Defaults on any error (missing file, bad JSON).
pub fn load_state(path: &Path) -> GraphicsState {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(path: &Path, state: &GraphicsState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    fs::write(path, json)
}

/// `%APPDATA%\CompetencyTracker`, or the temp folder when `APPDATA` is unset.
pub fn diagnostics_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("CompetencyTracker"),
        None => std::env::temp_dir().join("CompetencyTracker"),
    }
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("graphics.json")
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("startup.log")
}

static LOG: Mutex<Option<File>> = Mutex::new(None);
static LOG_LINES: AtomicU32 = AtomicU32::new(0);
/// Lines accepted from libraries (wgpu, eframe) per run, so a warning repeated
/// every frame cannot grow the file without bound.
const LIBRARY_LINE_LIMIT: u32 = 2000;

/// Starts `startup.log` in `dir`, keeping the previous run's as
/// `startup.prev.log`. Failing to create it is not an error: the program
/// simply runs without a log.
pub fn init_log(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    let path = log_path(dir);
    let _ = fs::rename(&path, dir.join("startup.prev.log"));
    if let Ok(file) = File::create(&path) {
        *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    }
    let _ = log::set_logger(&LIBRARY_LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

/// Appends a line to the start-up log and flushes it to disk at once, so it
/// survives the program being killed straight after.
pub fn note(message: &str) {
    let mut guard = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(file) = guard.as_mut() {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "{now}  {message}");
        let _ = file.sync_data();
    }
}

/// Sends `log` records from the graphics libraries to the start-up log.
struct LibraryLogger;
static LIBRARY_LOGGER: LibraryLogger = LibraryLogger;

impl log::Log for LibraryLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let n = LOG_LINES.fetch_add(1, Ordering::Relaxed);
        if n < LIBRARY_LINE_LIMIT {
            note(&format!(
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            ));
        } else if n == LIBRARY_LINE_LIMIT {
            note("(further library messages not logged)");
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_tries_everything_in_order() {
        let mut s = GraphicsState::default();
        assert_eq!(s.plan(), Graphics::ALL.to_vec());
        assert_eq!(s, GraphicsState::default());
    }

    #[test]
    fn a_mode_the_last_run_died_in_is_skipped_from_then_on() {
        let mut s = GraphicsState {
            crashed: vec![],
            trying: Some(Graphics::Dx12),
        };
        assert_eq!(s.plan(), vec![Graphics::Vulkan, Graphics::OpenGl]);
        assert_eq!(s.crashed, vec![Graphics::Dx12]);
        assert_eq!(s.trying, None);
        // The next run still skips it.
        assert_eq!(s.plan(), vec![Graphics::Vulkan, Graphics::OpenGl]);
    }

    #[test]
    fn when_every_mode_has_crashed_the_list_starts_over() {
        let mut s = GraphicsState {
            crashed: vec![Graphics::Dx12, Graphics::Vulkan],
            trying: Some(Graphics::OpenGl),
        };
        assert_eq!(s.plan(), Graphics::ALL.to_vec());
        assert!(s.crashed.is_empty());
    }

    #[test]
    fn state_roundtrips_and_defaults_on_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir.path().join("nested"));
        assert_eq!(load_state(&path), GraphicsState::default());
        let s = GraphicsState {
            crashed: vec![Graphics::Vulkan],
            trying: Some(Graphics::OpenGl),
        };
        save_state(&path, &s).unwrap();
        assert_eq!(load_state(&path), s);
        fs::write(&path, "{nope").unwrap();
        assert_eq!(load_state(&path), GraphicsState::default());
        fs::write(&path, r#"{"crashed":["dx12"]}"#).unwrap();
        assert_eq!(load_state(&path).crashed, vec![Graphics::Dx12]);
    }
}
