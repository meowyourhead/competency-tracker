use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;

use crate::dates::fmt_date;
use crate::model::{Data, SCHEMA_VERSION};

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
    let configured = fs::read_to_string(exe_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("data_dir").and_then(|d| d.as_str()).map(str::to_owned))
        .filter(|s| !s.trim().is_empty());
    let root = match configured {
        Some(dir) => exe_dir.join(dir.trim()),
        None => exe_dir.join(DEFAULT_DIR),
    };
    DataDir { root }
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

/// Every `*.json` directly in the snapshots folder, parsed. Unparseable files
/// are returned separately. A candidate that still cannot be *read* after
/// retries is an error, so an older snapshot never loads silently in its place;
/// one that vanished since the listing is skipped.
fn scan(dir: &DataDir) -> io::Result<(Vec<(PathBuf, Data)>, Vec<PathBuf>)> {
    let mut good = Vec::new();
    let mut corrupt = Vec::new();
    let entries = match fs::read_dir(dir.snapshots()) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((good, corrupt)),
        Err(e) => return Err(e),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
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

/// Loads the snapshot with the newest `saved_at`.
pub fn load_latest(dir: &DataDir) -> io::Result<Option<Loaded>> {
    let (good, corrupt) = scan(dir)?;
    let best = good.into_iter().max_by_key(|(_, d)| d.saved_at);
    Ok(best.map(|(path, mut data)| {
        data.ensure_valid();
        let read_only_newer_schema = data.schema_version > SCHEMA_VERSION;
        Loaded { data, path, corrupt, read_only_newer_schema }
    }))
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
