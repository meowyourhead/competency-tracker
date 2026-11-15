use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;

use crate::dates::fmt_date;
use crate::model::{Data, Ts, SCHEMA_VERSION};

const CONFIG_FILE: &str = "tracker.config.json";
const DEFAULT_DIR: &str = "CompetencyData";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    pub root: PathBuf,
}

impl DataDir {
    pub fn snapshots(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn archive(&self) -> PathBuf {
        self.snapshots().join("archive")
    }

    pub fn conflicts(&self) -> PathBuf {
        self.snapshots().join("conflicts")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join("edit.lock")
    }

    pub fn import_report(&self) -> PathBuf {
        self.root.join("import-report.txt")
    }

    /// Creates the snapshots, archive and conflicts folders.
    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(self.archive())?;
        fs::create_dir_all(self.conflicts())
    }
}

/// `<exe_dir>\CompetencyData`, or the `data_dir` from `tracker.config.json`
/// beside the exe (relative paths resolve against the exe dir). An unreadable
/// or invalid config falls back to the default.
pub fn resolve_data_dir(exe_dir: &Path) -> DataDir {
    resolve_data_dir_with_source(exe_dir).0
}

/// As [`resolve_data_dir`], also saying whether `tracker.config.json` chose it.
pub fn resolve_data_dir_with_source(exe_dir: &Path) -> (DataDir, bool) {
    let configured = fs::read_to_string(exe_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("data_dir").and_then(|d| d.as_str()).map(str::to_owned))
        .filter(|s| !s.trim().is_empty());
    let from_config = configured.is_some();
    let root = match configured {
        Some(dir) => exe_dir.join(dir.trim()),
        None => exe_dir.join(DEFAULT_DIR),
    };
    (DataDir { root }, from_config)
}

/// The first-run question shown before a missing data folder is created, or
/// `None` when the folder already exists.
pub fn first_run_prompt(dir: &DataDir, from_config: bool) -> Option<String> {
    if dir.root.exists() {
        return None;
    }
    // Show a full path (".." resolved) even when the config gave a relative one.
    let path = std::path::absolute(&dir.root).unwrap_or_else(|_| dir.root.clone());
    let source = if from_config {
        "This location comes from tracker.config.json."
    } else {
        "This is in the same folder as the application."
    };
    Some(format!("No data folder was found. A new data folder will be created here:\n\n{}\n\n{source}", path.display()))
}

pub fn sanitize_user(user: &str) -> String {
    let cleaned: String = user
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == ' ' || c == '.');
    if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed.into()
    }
}

pub fn snapshot_file_name(date: NaiveDate, user: &str) -> String {
    format!("{} - {}.json", fmt_date(date), sanitize_user(user))
}

/// Writes `<name>.tmp` in the same folder, syncs it, then renames over the target.
pub fn save_snapshot(dir: &DataDir, data: &Data, file_name: &str) -> io::Result<PathBuf> {
    let folder = dir.snapshots();
    fs::create_dir_all(&folder)?;
    let target = folder.join(file_name);
    let tmp = folder.join(format!("{file_name}.tmp"));
    let json = serde_json::to_vec_pretty(data).map_err(io::Error::other)?;
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        // Windows sharing violations (a reader or sync client holding the
        // target) surface as PermissionDenied.
        retry_io(|| fs::rename(&tmp, &target), is_permission_denied)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(target)
}

const IO_RETRIES: u32 = 3;
const IO_RETRY_DELAY: Duration = Duration::from_millis(150);

/// Runs `op`, retrying up to `IO_RETRIES` more times (150 ms apart) while
/// `retry_on` accepts the error. Shared folders often hold files open briefly
/// (antivirus, sync clients).
fn retry_io<T>(mut op: impl FnMut() -> io::Result<T>, retry_on: impl Fn(&io::Error) -> bool) -> io::Result<T> {
    let mut retries = 0;
    loop {
        match op() {
            Err(e) if retries < IO_RETRIES && retry_on(&e) => {
                retries += 1;
                std::thread::sleep(IO_RETRY_DELAY);
            }
            other => return other,
        }
    }
}

fn is_permission_denied(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
}

#[derive(Debug)]
pub struct Loaded {
    pub data: Data,
    pub path: PathBuf,
    pub corrupt: Vec<PathBuf>,
    pub read_only_newer_schema: bool,
}

/// Parsed snapshots and the paths of unparseable ones.
type ScanResult = (Vec<(PathBuf, Data)>, Vec<PathBuf>);

/// Every `*.json` directly in the snapshots folder, parsed. Unparseable files
/// are returned separately. A candidate that still cannot be *read* after
/// retries is an error, so an older snapshot never loads silently in its place;
/// one that vanished since the listing is skipped.
fn scan(dir: &DataDir) -> io::Result<ScanResult> {
    let mut good = Vec::new();
    let mut corrupt = Vec::new();
    let entries = match fs::read_dir(dir.snapshots()) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((good, corrupt)),
        Err(e) => return Err(e),
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = match retry_io(|| fs::read(&path), |e| e.kind() != io::ErrorKind::NotFound) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        match serde_json::from_slice::<Data>(&bytes) {
            Ok(d) => good.push((path, d)),
            Err(_) => corrupt.push(path),
        }
    }
    Ok((good, corrupt))
}

/// One snapshot file as listed. The same path, length and modified time are
/// taken to mean the same contents (snapshots are replaced by rename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileKey {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// The snapshot with the newest `saved_at`, as found by [`ScanCache::load_latest`].
#[derive(Debug, Clone, PartialEq)]
pub struct Newest {
    pub key: FileKey,
    pub saved_at: Ts,
}

/// What [`ScanCache::load_latest`] found.
#[derive(Debug)]
pub struct Latest {
    pub newest: Option<Newest>,
    /// The newest snapshot, fully loaded; `None` when it is the one the caller already has.
    pub loaded: Option<Loaded>,
    pub corrupt: Vec<PathBuf>,
}

/// Remembers each snapshot file's `saved_at` (or that it does not parse) by
/// [`FileKey`], so a poll only reads files that are new or changed.
#[derive(Debug, Default)]
pub struct ScanCache {
    files: HashMap<PathBuf, (FileKey, Option<Ts>)>,
    reads: u64,
}

/// How often [`ScanCache::load_latest`] rescans when the winner changes under it.
const SCAN_ATTEMPTS: u32 = 3;

impl ScanCache {
    /// How many snapshot files were read so far.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// Finds the snapshot with the newest `saved_at`, reading only new or
    /// changed files, and fully loads it unless it is `have`.
    ///
    /// A listed file that still cannot be *read* after retries is an error, so
    /// an older snapshot never loads silently in its place; one that vanished
    /// since the listing is skipped; one that does not parse is `corrupt`.
    pub fn load_latest(&mut self, dir: &DataDir, have: Option<&Newest>) -> io::Result<Latest> {
        let mut attempt = 1;
        loop {
            match self.try_load_latest(dir, have)? {
                Some(latest) => return Ok(latest),
                // The winner vanished or changed between the scan and its load.
                None if attempt < SCAN_ATTEMPTS => attempt += 1,
                None => return Err(io::Error::other("The newest snapshot kept changing while it was loaded")),
            }
        }
    }

    /// One scan; `None` when the winner could not be loaded after all.
    fn try_load_latest(&mut self, dir: &DataDir, have: Option<&Newest>) -> io::Result<Option<Latest>> {
        let keys = list_snapshots(dir)?;
        self.files.retain(|path, _| keys.iter().any(|k| k.path == *path));
        let mut corrupt = Vec::new();
        // The best so far, with its data when it was parsed during this scan.
        let mut best: Option<(Newest, Option<Data>)> = None;
        for key in keys {
            let (saved_at, fresh) = match self.files.get(&key.path) {
                Some((cached, saved_at)) if *cached == key => (*saved_at, None),
                _ => {
                    let Some(bytes) = self.read(&key.path)? else { continue };
                    let parsed = serde_json::from_slice::<Data>(&bytes).ok();
                    let saved_at = parsed.as_ref().map(|d| d.saved_at);
                    self.files.insert(key.path.clone(), (key.clone(), saved_at));
                    (saved_at, parsed)
                }
            };
            let Some(saved_at) = saved_at else {
                corrupt.push(key.path);
                continue;
            };
            // Paths are sorted; on a tie the later one wins.
            if best.as_ref().is_none_or(|(b, _)| saved_at >= b.saved_at) {
                best = Some((Newest { key, saved_at }, fresh));
            }
        }
        let Some((newest, fresh)) = best else {
            return Ok(Some(Latest { newest: None, loaded: None, corrupt }));
        };
        if have == Some(&newest) {
            return Ok(Some(Latest { newest: Some(newest), loaded: None, corrupt }));
        }
        let data = match fresh {
            Some(d) => d,
            None => match self.read(&newest.key.path)?.and_then(|b| serde_json::from_slice::<Data>(&b).ok()) {
                Some(d) => d,
                None => {
                    self.files.remove(&newest.key.path);
                    return Ok(None);
                }
            },
        };
        let loaded = loaded(data, newest.key.path.clone(), corrupt.clone());
        let newest = Newest { saved_at: loaded.data.saved_at, ..newest };
        Ok(Some(Latest { newest: Some(newest), loaded: Some(loaded), corrupt }))
    }

    /// Records a snapshot we just wrote, so the next scan need not read it.
    /// Returns it as the newest. The key comes from a folder listing, like a
    /// scan's (a listing's times can differ from the file's own metadata).
    pub fn note_saved(&mut self, dir: &DataDir, path: &Path, saved_at: Ts) -> Option<Newest> {
        let key = list_snapshots(dir).ok()?.into_iter().find(|k| k.path == path)?;
        self.files.insert(key.path.clone(), (key.clone(), Some(saved_at)));
        Some(Newest { key, saved_at })
    }

    /// Reads a snapshot (with retries). `Ok(None)` when it vanished.
    fn read(&mut self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        self.reads += 1;
        match retry_io(|| fs::read(path), |e| e.kind() != io::ErrorKind::NotFound) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        }
    }
}

/// Every `*.json` directly in the snapshots folder with its length and
/// modified time, sorted by path.
fn list_snapshots(dir: &DataDir) -> io::Result<Vec<FileKey>> {
    let entries = match fs::read_dir(dir.snapshots()) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut keys = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("json")) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        // A directory named *.json is never cached: its read fails every time.
        keys.push(FileKey { path, len: meta.len(), modified: meta.modified().ok() });
    }
    keys.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(keys)
}

fn loaded(mut data: Data, path: PathBuf, corrupt: Vec<PathBuf>) -> Loaded {
    data.ensure_valid();
    let read_only_newer_schema = data.schema_version > SCHEMA_VERSION;
    Loaded { data, path, corrupt, read_only_newer_schema }
}

/// Loads the snapshot with the newest `saved_at`.
pub fn load_latest(dir: &DataDir) -> io::Result<Option<Loaded>> {
    ScanCache::default().load_latest(dir, None).map(|l| l.loaded)
}

/// Moves snapshots older than `keep_days` into the archive folder; the newest
/// snapshot is never moved. Returns how many were moved.
pub fn archive_old(dir: &DataDir, keep_days: i64, today: NaiveDate) -> io::Result<usize> {
    let (good, _) = scan(dir)?;
    let Some(newest) = good.iter().map(|(_, d)| d.saved_at).max() else {
        return Ok(0);
    };
    let mut skipped_newest = false;
    let mut moved = 0;
    fs::create_dir_all(dir.archive())?;
    for (path, d) in good {
        if d.saved_at == newest && !skipped_newest {
            skipped_newest = true;
            continue;
        }
        if (today - d.saved_at.date_naive()).num_days() > keep_days {
            if let Some(name) = path.file_name() {
                fs::rename(&path, dir.archive().join(name))?;
                moved += 1;
            }
        }
    }
    Ok(moved)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Data, Ts};
    use chrono::{Duration, NaiveDate, TimeZone};
    use std::fs;
    use std::time::{Duration as StdDuration, SystemTime};

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn ts(y: i32, m: u32, d: u32) -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600).unwrap().with_ymd_and_hms(y, m, d, 9, 0, 0).unwrap()
    }

    fn data_at(t: Ts, by: &str) -> Data {
        Data::new_empty(by, t)
    }

    fn set_mtime(path: &Path, secs_from_epoch: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + StdDuration::from_secs(secs_from_epoch)).unwrap();
    }

    fn dir_in(tmp: &tempfile::TempDir) -> DataDir {
        let d = DataDir { root: tmp.path().to_path_buf() };
        d.ensure().unwrap();
        d
    }

    #[test]
    fn file_name_format() {
        assert_eq!(snapshot_file_name(ymd(2026, 11, 14), "jtan"), "14 Nov 2026 - jtan.json");
        assert_eq!(snapshot_file_name(ymd(2026, 12, 2), "jtan"), "2 Dec 2026 - jtan.json");
    }

    #[test]
    fn file_name_sanitises_username() {
        assert_eq!(snapshot_file_name(ymd(2026, 11, 14), "a:b*c"), "14 Nov 2026 - a_b_c.json");
        assert_eq!(snapshot_file_name(ymd(2026, 11, 14), ""), "14 Nov 2026 - unknown.json");
        assert_eq!(sanitize_user(" ..x\\y/z? "), "x_y_z_");
        assert_eq!(sanitize_user("a\"<>|b\u{7}"), "a____b_");
        assert_eq!(sanitize_user(" . "), "unknown");
    }

    #[test]
    fn data_dir_paths_and_ensure() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dir_in(&tmp);
        assert_eq!(d.snapshots(), tmp.path().join("snapshots"));
        assert_eq!(d.archive(), tmp.path().join("snapshots").join("archive"));
        assert_eq!(d.conflicts(), tmp.path().join("snapshots").join("conflicts"));
        assert_eq!(d.lock_path(), tmp.path().join("edit.lock"));
        assert_eq!(d.import_report(), tmp.path().join("import-report.txt"));
        assert!(d.archive().is_dir() && d.conflicts().is_dir());
    }

    #[test]
    fn save_then_load_latest_picks_newest_saved_at() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        let newer = data_at(ts(2026, 11, 14), "newer");
        let older = data_at(ts(2026, 11, 10), "older");
        let p_newer = save_snapshot(&dir, &newer, "14 Nov 2026 - newer.json").unwrap();
        let p_older = save_snapshot(&dir, &older, "10 Nov 2026 - older.json").unwrap();
        // The file with the newer saved_at has the OLDER modified time.
        set_mtime(&p_newer, 1_000_000);
        set_mtime(&p_older, 2_000_000);
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.saved_by, "newer");
        assert_eq!(loaded.path, p_newer);
        assert!(loaded.corrupt.is_empty() && !loaded.read_only_newer_schema);
    }

    #[test]
    fn load_latest_empty_or_missing_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = DataDir { root: tmp.path().join("nope") };
        assert!(load_latest(&missing).unwrap().is_none());
        let dir = dir_in(&tmp);
        assert!(load_latest(&dir).unwrap().is_none());
    }

    #[test]
    fn corrupt_and_tmp_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        save_snapshot(&dir, &data_at(ts(2026, 11, 14), "ok"), "ok.json").unwrap();
        let bad = dir.snapshots().join("x.json");
        fs::write(&bad, "{\"schema_version\": 1, \"saved_a").unwrap();
        fs::write(dir.snapshots().join("y.json.tmp"), "garbage").unwrap();
        // Files in subfolders are not considered.
        fs::write(dir.archive().join("z.json"), "garbage").unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.corrupt, vec![bad]);
        assert_eq!(loaded.data.saved_by, "ok");
    }

    #[test]
    fn unreadable_candidate_is_an_error_not_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        save_snapshot(&dir, &data_at(ts(2026, 11, 10), "older"), "older.json").unwrap();
        // A *.json path that cannot be read (here: a directory) must not let the
        // older snapshot load silently.
        fs::create_dir(dir.snapshots().join("x.json")).unwrap();
        assert!(load_latest(&dir).is_err());
        // A parse failure is still just reported as corrupt.
        fs::remove_dir(dir.snapshots().join("x.json")).unwrap();
        fs::write(dir.snapshots().join("x.json"), "{ nope").unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.saved_by, "older");
        assert_eq!(loaded.corrupt, vec![dir.snapshots().join("x.json")]);
    }

    #[test]
    fn scan_cache_reads_only_new_or_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        save_snapshot(&dir, &data_at(ts(2026, 11, 10), "a"), "a.json").unwrap();
        save_snapshot(&dir, &data_at(ts(2026, 11, 12), "b"), "b.json").unwrap();
        let mut cache = ScanCache::default();
        let first = cache.load_latest(&dir, None).unwrap();
        assert_eq!(cache.reads(), 2, "each file read once; the winner is not read again");
        assert_eq!(first.loaded.as_ref().unwrap().data.saved_by, "b");
        let newest = first.newest.clone().unwrap();
        assert_eq!(newest.saved_at, ts(2026, 11, 12));
        assert_eq!(newest.key.path, dir.snapshots().join("b.json"));

        // Nothing changed and the caller already has the winner: no reads at all.
        let again = cache.load_latest(&dir, Some(&newest)).unwrap();
        assert_eq!(cache.reads(), 2);
        assert!(again.loaded.is_none());
        assert_eq!(again.newest.as_ref(), Some(&newest));

        // Without it, only the winner is read.
        let full = cache.load_latest(&dir, None).unwrap();
        assert_eq!(cache.reads(), 3);
        assert_eq!(full.loaded.unwrap().data.saved_by, "b");

        // A new file: only it is read, and it wins.
        save_snapshot(&dir, &data_at(ts(2026, 11, 14), "c"), "c.json").unwrap();
        let newer = cache.load_latest(&dir, Some(&newest)).unwrap();
        assert_eq!(cache.reads(), 4);
        assert_eq!(newer.loaded.unwrap().data.saved_by, "c");

        // A changed file (same name, new contents) is read again.
        let c = newer.newest.unwrap();
        let mut c2 = data_at(ts(2026, 11, 15), "c");
        c2.settings.due_soon_days = 7;
        save_snapshot(&dir, &c2, "c.json").unwrap();
        set_mtime(&dir.snapshots().join("c.json"), 3_000_000);
        let changed = cache.load_latest(&dir, Some(&c)).unwrap();
        assert_eq!(cache.reads(), 5);
        assert_eq!(changed.loaded.unwrap().data.settings.due_soon_days, 7);

        // A corrupt file is read once, then remembered as corrupt.
        let bad = dir.snapshots().join("x.json");
        fs::write(&bad, "{ nope").unwrap();
        let newest = cache.load_latest(&dir, None).unwrap().newest.unwrap();
        assert_eq!(cache.reads(), 7, "x.json plus the winner");
        let l = cache.load_latest(&dir, Some(&newest)).unwrap();
        assert_eq!(cache.reads(), 7);
        assert_eq!(l.corrupt, vec![bad]);
    }

    #[test]
    fn scan_cache_keeps_read_errors_and_forgets_removed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        save_snapshot(&dir, &data_at(ts(2026, 11, 10), "older"), "older.json").unwrap();
        save_snapshot(&dir, &data_at(ts(2026, 11, 12), "newer"), "newer.json").unwrap();
        let mut cache = ScanCache::default();
        assert_eq!(cache.load_latest(&dir, None).unwrap().loaded.unwrap().data.saved_by, "newer");
        // An unreadable candidate is still an error, every time.
        fs::create_dir(dir.snapshots().join("x.json")).unwrap();
        assert!(cache.load_latest(&dir, None).is_err());
        assert!(cache.load_latest(&dir, None).is_err());
        fs::remove_dir(dir.snapshots().join("x.json")).unwrap();
        // The winner is deleted: the next-newest loads.
        fs::remove_file(dir.snapshots().join("newer.json")).unwrap();
        let l = cache.load_latest(&dir, None).unwrap();
        assert_eq!(l.loaded.unwrap().data.saved_by, "older");
        assert_eq!(l.newest.unwrap().key.path, dir.snapshots().join("older.json"));
    }

    #[test]
    fn retry_io_retries_transient_errors() {
        let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
        let is_denied = |e: &io::Error| e.kind() == io::ErrorKind::PermissionDenied;
        // Fails twice, then succeeds.
        let mut calls = 0;
        let r = retry_io(
            || {
                calls += 1;
                if calls < 3 { Err(denied()) } else { Ok(7) }
            },
            is_denied,
        );
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls, 3);
        // Always failing: one try plus three retries, then the error.
        let mut calls = 0;
        let r: io::Result<()> = retry_io(
            || {
                calls += 1;
                Err(denied())
            },
            is_denied,
        );
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 4);
        // Errors the predicate rejects are not retried.
        let mut calls = 0;
        let r: io::Result<()> = retry_io(
            || {
                calls += 1;
                Err(io::Error::from(io::ErrorKind::NotFound))
            },
            is_denied,
        );
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn same_user_same_day_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        let name = snapshot_file_name(ymd(2026, 11, 14), "jtan");
        save_snapshot(&dir, &data_at(ts(2026, 11, 14), "jtan"), &name).unwrap();
        let mut second = data_at(ts(2026, 11, 14), "jtan");
        second.saved_at = ts(2026, 11, 15);
        save_snapshot(&dir, &second, &name).unwrap();
        let entries: Vec<_> = fs::read_dir(dir.snapshots())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(load_latest(&dir).unwrap().unwrap().data.saved_at, ts(2026, 11, 15));
    }

    #[test]
    fn newer_schema_opens_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        let mut d = data_at(ts(2026, 11, 14), "future");
        d.schema_version = 99;
        let mut v = serde_json::to_value(&d).unwrap();
        v.as_object_mut().unwrap().insert("brand_new_field".into(), serde_json::json!({"a": 1}));
        fs::write(dir.snapshots().join("f.json"), serde_json::to_string(&v).unwrap()).unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert!(loaded.read_only_newer_schema);
        assert!(loaded.corrupt.is_empty());
    }

    #[test]
    fn load_latest_normalises_missing_default_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        let mut d = data_at(ts(2026, 11, 14), "x");
        d.rules.clear();
        d.default_rule_id = "r-gone".into();
        save_snapshot(&dir, &d, "x.json").unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.default_rule_id, "r-global");
        assert_eq!(loaded.data.rules.len(), 1);
    }

    #[test]
    fn resolve_data_dir_default_and_config() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path();
        assert_eq!(resolve_data_dir(exe).root, exe.join("CompetencyData"));
        fs::write(exe.join("tracker.config.json"), r#"{"data_dir":"..\\Shared"}"#).unwrap();
        assert_eq!(resolve_data_dir(exe).root, exe.join("..\\Shared"));
        fs::write(exe.join("tracker.config.json"), "not json").unwrap();
        assert_eq!(resolve_data_dir(exe).root, exe.join("CompetencyData"));
        fs::write(exe.join("tracker.config.json"), r#"{"data_dir":"  "}"#).unwrap();
        assert_eq!(resolve_data_dir(exe).root, exe.join("CompetencyData"));
    }

    #[test]
    fn first_run_prompt_only_when_folder_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path();
        let (dir, from_config) = resolve_data_dir_with_source(exe);
        assert!(!from_config);
        let text = first_run_prompt(&dir, from_config).expect("missing folder asks");
        assert!(text.starts_with("No data folder was found. A new data folder will be created here:\n\n"));
        assert!(text.contains(&exe.join("CompetencyData").display().to_string()));
        assert!(text.ends_with("This is in the same folder as the application."));

        fs::create_dir_all(&dir.root).unwrap();
        assert_eq!(first_run_prompt(&dir, from_config), None, "existing folder: no question");
    }

    #[test]
    fn first_run_prompt_names_config_override() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path();
        fs::write(exe.join("tracker.config.json"), r#"{"data_dir":"Shared\\Data"}"#).unwrap();
        let (dir, from_config) = resolve_data_dir_with_source(exe);
        assert!(from_config);
        let text = first_run_prompt(&dir, from_config).unwrap();
        assert!(text.contains(&exe.join("Shared\\Data").display().to_string()));
        assert!(text.ends_with("This location comes from tracker.config.json."));
        assert!(!text.contains("same folder as the application"));
    }

    #[test]
    fn archive_keeps_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_in(&tmp);
        let today = ymd(2026, 11, 14);
        let old1 = today - Duration::days(200);
        let old2 = today - Duration::days(100);
        let recent = today - Duration::days(5);
        for (label, day) in [("a", old1), ("b", old2), ("c", recent)] {
            let t = chrono::FixedOffset::east_opt(8 * 3600)
                .unwrap()
                .from_local_datetime(&day.and_hms_opt(9, 0, 0).unwrap())
                .unwrap();
            save_snapshot(&dir, &data_at(t, label), &format!("{label}.json")).unwrap();
        }
        assert_eq!(archive_old(&dir, 90, today).unwrap(), 2);
        assert!(dir.snapshots().join("c.json").exists());
        assert!(dir.archive().join("a.json").exists() && dir.archive().join("b.json").exists());
        assert!(!dir.snapshots().join("a.json").exists());

        // Even when everything is old, the newest is never archived.
        assert_eq!(archive_old(&dir, 0, today + Duration::days(1000)).unwrap(), 0);
        assert!(dir.snapshots().join("c.json").exists());
    }
}
