//! The update channel: new versions are published as GitHub releases of
//! [`REPO`], each carrying the program as an asset named [`ASSET`]. The app
//! checks the latest release in the background, and on request downloads it
//! next to the running program and swaps the two, ready for a restart.
//!
//! Downloads go through Windows' own `URLDownloadToFileW`, so they use the
//! system's proxy settings and certificates and need no TLS library here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

/// `owner/name` of the GitHub repository whose releases are the channel.
pub const REPO: &str = "meowyourhead/competency-tracker";
/// The release asset holding the program.
pub const ASSET: &str = "competency-tracker.exe";
/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A published release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// As tagged, e.g. `v1.1`.
    pub tag: String,
    /// The release's page on GitHub.
    pub page_url: String,
    /// Where to download [`ASSET`] from.
    pub download_url: String,
    /// The release notes (may be empty).
    pub notes: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// `v1.2.3`, `1.2` or `1` as (major, minor, patch); missing parts are 0.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || -> Option<u64> {
        match parts.next() {
            None => Some(0),
            Some(p) => p.parse().ok(),
        }
    };
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// Whether `tag` names a later version than `current`.
pub fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(t), Some(c)) => t > c,
        _ => false,
    }
}

/// Reads GitHub's "latest release" JSON: the release if it is newer than
/// `current` and carries the program, `None` if this build is up to date.
pub fn parse_latest(json: &str, current: &str) -> Result<Option<Release>, String> {
    let r: ApiRelease =
        serde_json::from_str(json).map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    if r.draft || r.prerelease || !is_newer(&r.tag_name, current) {
        return Ok(None);
    }
    let asset = r
        .assets
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case(ASSET))
        .ok_or_else(|| format!("Release {} has no {ASSET} to download", r.tag_name))?;
    Ok(Some(Release {
        tag: r.tag_name,
        page_url: r.html_url,
        download_url: asset.browser_download_url,
        notes: r.body.unwrap_or_default().trim().to_owned(),
    }))
}

#[cfg(windows)]
fn download(url: &str, dest: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::Urlmon::URLDownloadToFileW;
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let url_w = wide(std::ffi::OsStr::new(url));
    let dest_w = wide(dest.as_os_str());
    // SAFETY: both strings are NUL-terminated and outlive the call; no
    // callback is passed. CoInitializeEx may report "already initialised",
    // which is fine.
    let hr = unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32);
        URLDownloadToFileW(
            std::ptr::null_mut(),
            url_w.as_ptr(),
            dest_w.as_ptr(),
            0,
            std::ptr::null_mut(),
        )
    };
    if hr == 0 {
        Ok(())
    } else {
        Err(format!(
            "Couldn't reach GitHub (error 0x{:08X}). Check the internet connection.",
            hr as u32
        ))
    }
}

#[cfg(not(windows))]
fn download(_url: &str, _dest: &Path) -> Result<(), String> {
    Err("Updates are only available on Windows".to_owned())
}

/// A file in the temp folder unique to this process and call.
fn temp_file(stem: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "competency-tracker-{stem}-{}-{n}",
        std::process::id()
    ))
}

/// Asks GitHub for the latest release.
pub fn fetch_latest() -> Result<Option<Release>, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // The query string keeps Windows' download cache from answering.
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest?t={stamp}");
    let tmp = temp_file("latest.json");
    let result = download(&url, &tmp).and_then(|()| {
        std::fs::read_to_string(&tmp).map_err(|e| format!("Couldn't read GitHub's reply: {e}"))
    });
    let _ = std::fs::remove_file(&tmp);
    parse_latest(&result?, CURRENT)
}

/// The running program, and where its replaced copy is parked.
fn exe_paths() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Couldn't locate the program file: {e}"))?;
    let with = |ext: &str| {
        let mut p = exe.clone().into_os_string();
        p.push(ext);
        PathBuf::from(p)
    };
    let (new, old) = (with(".new"), with(".old"));
    Ok((exe, new, old))
}

/// Downloads `release` next to the program and swaps it in. The running
/// program keeps working (Windows lets a running program be renamed); the
/// new version starts at the next launch.
pub fn install(release: &Release) -> Result<(), String> {
    let (exe, new, old) = exe_paths()?;
    let _ = std::fs::remove_file(&new);
    download(&release.download_url, &new)?;
    let ok = std::fs::read(&new)
        .map(|b| b.len() > 1_000_000 && b.starts_with(b"MZ"))
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&new);
        return Err("The download wasn't a complete program file".to_owned());
    }
    // A copy left from the last update; it can't go while someone still runs it.
    if old.exists() && std::fs::remove_file(&old).is_err() {
        let _ = std::fs::remove_file(&new);
        return Err(format!(
            "Couldn't remove {} (is an older copy still open on another computer?)",
            old.display()
        ));
    }
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        format!("Couldn't replace the program file (no permission to change its folder?): {e}")
    })?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("Couldn't put the new version in place: {e}"));
    }
    Ok(())
}

/// Removes the copy a previous update parked beside the program. Quietly
/// leaves it while another computer still has it open.
pub fn clean_up_previous() {
    if let Ok((_, _, old)) = exe_paths() {
        let _ = std::fs::remove_file(old);
    }
}

/// Starts the program file again (the new version, after [`install`]).
pub fn relaunch() -> Result<(), String> {
    let (exe, _, _) = exe_paths()?;
    std::process::Command::new(&exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start the new version: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// Not checked yet.
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    /// Installed; the new version runs after a restart.
    Installed(Release),
    Failed(String),
}

/// Runs checks and installs on a background thread; the UI polls [`state`].
///
/// [`state`]: Updater::state
#[derive(Default)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
    /// Restart into the new version once the app has closed cleanly.
    pub restart_requested: bool,
}

impl Updater {
    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn busy(&self) -> bool {
        matches!(
            self.state(),
            UpdateState::Checking | UpdateState::Installing(_)
        )
    }

    fn run(
        &self,
        start: UpdateState,
        ctx: Option<egui::Context>,
        job: impl FnOnce() -> UpdateState + Send + 'static,
    ) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = start;
        let state = self.state.clone();
        std::thread::spawn(move || {
            let result = job();
            *state.lock().unwrap_or_else(|e| e.into_inner()) = result;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Checks GitHub for a newer release, unless already busy or installed.
    pub fn check(&self, ctx: Option<egui::Context>) {
        if self.busy() || matches!(self.state(), UpdateState::Installed(_)) {
            return;
        }
        self.run(UpdateState::Checking, ctx, || match fetch_latest() {
            Ok(Some(r)) => UpdateState::Available(r),
            Ok(None) => UpdateState::UpToDate,
            Err(e) => UpdateState::Failed(e),
        });
    }

    /// Downloads and swaps in `release`.
    pub fn install(&self, release: Release, ctx: Option<egui::Context>) {
        if self.busy() {
            return;
        }
        let r = release.clone();
        self.run(
            UpdateState::Installing(release),
            ctx,
            move || match install(&r) {
                Ok(()) => UpdateState::Installed(r),
                Err(e) => UpdateState::Failed(e),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.1"), Some((1, 1, 0)));
        assert_eq!(parse_version("1.10.2"), Some((1, 10, 2)));
        assert_eq!(parse_version("v2"), Some((2, 0, 0)));
        assert_eq!(parse_version("v1.2-beta"), Some((1, 2, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v1.1", "1.0.0"));
        assert!(is_newer("v1.10", "1.9.0"));
        assert!(!is_newer("v1.1", "1.1.0"));
        assert!(!is_newer("v1.0", "1.1.0"));
        assert!(!is_newer("junk", "1.1.0"));
    }

    fn json(tag: &str, asset: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/x/y/releases/tag/{tag}",
               "body":" Notes ","draft":false,"prerelease":false,
               "assets":[{{"name":"{asset}","browser_download_url":"https://dl/{asset}"}}]}}"#
        )
    }

    #[test]
    fn newer_release_with_the_program_is_offered() {
        let r = parse_latest(&json("v1.2", ASSET), "1.1.0")
            .unwrap()
            .unwrap();
        assert_eq!(r.tag, "v1.2");
        assert_eq!(r.download_url, format!("https://dl/{ASSET}"));
        assert_eq!(r.notes, "Notes");
    }

    #[test]
    fn same_or_older_release_is_not_offered() {
        assert_eq!(parse_latest(&json("v1.1", ASSET), "1.1.0").unwrap(), None);
        assert_eq!(parse_latest(&json("v1.0", ASSET), "1.1.0").unwrap(), None);
    }

    /// Talks to GitHub: `cargo test -- --ignored live_channel`.
    #[test]
    #[ignore]
    fn live_channel_offers_the_latest_release_to_older_builds() {
        assert_eq!(fetch_latest(), Ok(None), "this build is the latest");
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let tmp = temp_file("live.json");
        download(&url, &tmp).unwrap();
        let json = std::fs::read_to_string(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let r = parse_latest(&json, "0.0.1").unwrap().expect("offered");
        assert!(r.download_url.ends_with(ASSET), "{}", r.download_url);
    }

    #[test]
    fn newer_release_without_the_program_is_an_error() {
        assert!(parse_latest(&json("v1.2", "other.zip"), "1.1.0").is_err());
        assert!(parse_latest("not json", "1.1.0").is_err());
    }
}
