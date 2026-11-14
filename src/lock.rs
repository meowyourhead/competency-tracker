//! Single-editor lock file (`edit.lock`) with a heartbeat.

use crate::model::Ts;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub const HEARTBEAT_SECS: i64 = 30;
pub const STALE_SECS: i64 = 120;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LockInfo {
    pub user: String,
    pub machine: String,
    pub pid: u32,
    pub acquired_at: Ts,
    pub heartbeat_at: Ts,
}

#[derive(Debug, PartialEq)]
pub enum Acquire {
    Acquired,
    TookOverStale(LockInfo),
    HeldBy(LockInfo),
}

/// Same editor: user, machine and pid all equal.
pub fn same_owner(a: &LockInfo, b: &LockInfo) -> bool {
    a.user == b.user && a.machine == b.machine && a.pid == b.pid
}

/// Stale when the heartbeat is more than `STALE_SECS` old. A future heartbeat is never stale.
pub fn is_stale(info: &LockInfo, now: Ts) -> bool {
    (now - info.heartbeat_at).num_seconds() > STALE_SECS
}

/// Reads the lock file. `Ok(None)` only when the file does not exist.
/// An unparseable file yields `Err(io::ErrorKind::InvalidData)`; callers
/// ([`try_acquire`]) treat that as a stale, takeover-able lock.
pub fn read_lock(path: &Path) -> io::Result<Option<LockInfo>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn placeholder(now: Ts) -> LockInfo {
    LockInfo {
        user: "unknown".into(),
        machine: "unknown".into(),
        pid: 0,
        acquired_at: now,
        heartbeat_at: now,
    }
}

fn to_json(info: &LockInfo) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(info).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Tries `create_new`; `Ok(true)` if we created the file.
fn create_new(path: &Path, me: &LockInfo) -> io::Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            f.write_all(&to_json(me)?)?;
            f.sync_all()?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

fn mtime_age_secs(path: &Path) -> io::Result<i64> {
    let m = fs::metadata(path)?.modified()?;
    Ok(match std::time::SystemTime::now().duration_since(m) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0, // mtime in the future: age 0
    })
}

/// Stale only when BOTH the recorded heartbeat and the file's mtime are older
/// than `STALE_SECS`. The mtime check guards against clock skew between PCs.
/// A missing file counts as stale (nothing to protect).
pub fn is_stale_at(path: &Path, info: &LockInfo, now: Ts) -> io::Result<bool> {
    if !is_stale(info, now) {
        return Ok(false);
    }
    match mtime_age_secs(path) {
        Ok(age) => Ok(age > STALE_SECS),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// An unparseable lock only counts as stale once its mtime is old; a fresh one
/// may be a peer between `create_new` and its first write.
pub fn corrupt_is_stale(path: &Path) -> io::Result<bool> {
    match mtime_age_secs(path) {
        Ok(age) => Ok(age > STALE_SECS),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

fn held_by_current(path: &Path, now: Ts) -> io::Result<Acquire> {
    match read_lock(path) {
        Ok(Some(i)) => Ok(Acquire::HeldBy(i)),
        Ok(None) => Ok(Acquire::HeldBy(placeholder(now))),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(Acquire::HeldBy(placeholder(now))),
        Err(e) => Err(e),
    }
}

pub fn try_acquire(path: &Path, me: &LockInfo, now: Ts) -> io::Result<Acquire> {
    if create_new(path, me)? {
        return Ok(Acquire::Acquired);
    }
    match read_lock(path) {
        Ok(None) => {
            // Vanished between create and read: one more try.
            if create_new(path, me)? {
                Ok(Acquire::Acquired)
            } else {
                held_by_current(path, now)
            }
        }
        Ok(Some(existing)) => {
            if same_owner(&existing, me) {
                // Never report Acquired without ownership.
                return if heartbeat(path, me, now)? {
                    Ok(Acquire::Acquired)
                } else {
                    held_by_current(path, now)
                };
            }
            if !is_stale_at(path, &existing, now)? {
                return Ok(Acquire::HeldBy(existing));
            }
            takeover_with(path, me, now, Some(existing), || {})
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            if corrupt_is_stale(path)? {
                takeover_with(path, me, now, None, || {})
            } else {
                Ok(Acquire::HeldBy(placeholder(now)))
            }
        }
        Err(e) => Err(e),
    }
}

/// Takes over a lock judged stale. `judged` is the parsed lock we judged
/// (`None` = unparseable and old). `before_remove` is a test hook that runs
/// between the judgement and the compare-then-delete re-check.
fn takeover_with(
    path: &Path,
    me: &LockInfo,
    now: Ts,
    judged: Option<LockInfo>,
    before_remove: impl FnOnce(),
) -> io::Result<Acquire> {
    before_remove();
    // Compare-then-delete: only remove what we judged.
    let still_judged = match (read_lock(path), &judged) {
        (Ok(None), _) => true,
        (Ok(Some(cur)), Some(j)) => cur == *j,
        (Err(e), None) if e.kind() == io::ErrorKind::InvalidData => corrupt_is_stale(path)?,
        (Err(e), _) if e.kind() != io::ErrorKind::InvalidData => return Err(e),
        _ => false,
    };
    if !still_judged {
        return held_by_current(path, now);
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if !create_new(path, me)? {
        return held_by_current(path, now);
    }
    // Read back to confirm we really own it.
    match read_lock(path) {
        Ok(Some(cur)) if same_owner(&cur, me) => {
            Ok(Acquire::TookOverStale(judged.unwrap_or_else(|| placeholder(now))))
        }
        Ok(Some(cur)) => Ok(Acquire::HeldBy(cur)),
        Ok(None) => Ok(Acquire::HeldBy(placeholder(now))),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(Acquire::HeldBy(placeholder(now))),
        Err(e) => Err(e),
    }
}

fn tmp_path(path: &Path, me: &LockInfo) -> std::path::PathBuf {
    let machine: String = me
        .machine
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}.{machine}.{}.tmp", me.pid))
}

/// Refreshes `heartbeat_at` via a per-owner tmp file + rename. Returns
/// `Ok(false)` if we are not (or are no longer) the owner: callers must treat
/// that as "lock lost" and go read-only immediately.
pub fn heartbeat(path: &Path, me: &LockInfo, now: Ts) -> io::Result<bool> {
    let mut cur = match read_lock(path) {
        Ok(Some(c)) if same_owner(&c, me) => c,
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(false),
        Err(e) => return Err(e),
    };
    cur.heartbeat_at = now;
    let tmp = tmp_path(path, me);
    let res = fs::write(&tmp, to_json(&cur)?).and_then(|_| fs::rename(&tmp, path));
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // We may have raced a takeover: confirm we still own what is on disk.
    is_owner(path, me)
}

pub fn is_owner(path: &Path, me: &LockInfo) -> io::Result<bool> {
    match read_lock(path) {
        Ok(Some(c)) => Ok(same_owner(&c, me)),
        Ok(None) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(false),
        Err(e) => Err(e),
    }
}

/// Removes the lock file only if we own it.
pub fn release(path: &Path, me: &LockInfo) -> io::Result<()> {
    if is_owner(path, me)? {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration};

    fn t0() -> Ts {
        DateTime::parse_from_rfc3339("2026-11-14T10:00:00+08:00").unwrap()
    }

    fn info(pid: u32, hb: Ts) -> LockInfo {
        LockInfo { user: "jtan".into(), machine: "PC1".into(), pid, acquired_at: hb, heartbeat_at: hb }
    }

    fn lock_path(d: &tempfile::TempDir) -> std::path::PathBuf {
        d.path().join("edit.lock")
    }

    /// Sets the file's mtime to `secs` seconds ago.
    fn age_file(p: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    }

    fn write_lock(p: &Path, i: &LockInfo) {
        fs::write(p, to_json(i).unwrap()).unwrap();
    }

    #[test]
    fn stale_heartbeat_but_fresh_mtime_is_held() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        let now = t0() + Duration::seconds(121);
        assert!(is_stale(&a, now));
        assert!(!is_stale_at(&p, &a, now).unwrap());
        let b = info(2, now);
        assert_eq!(try_acquire(&p, &b, now).unwrap(), Acquire::HeldBy(a));
    }

    #[test]
    fn empty_lock_fresh_mtime_is_held() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        fs::write(&p, b"").unwrap();
        match try_acquire(&p, &info(2, t0()), t0()).unwrap() {
            Acquire::HeldBy(h) => assert_eq!(h.user, "unknown"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(p.exists());
    }

    #[test]
    fn takeover_aborts_when_lock_changed_before_delete() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        write_lock(&p, &a);
        let c = info(3, t0());
        let me = info(2, t0());
        let r = takeover_with(&p, &me, t0(), Some(a), || write_lock(&p, &c)).unwrap();
        assert_eq!(r, Acquire::HeldBy(c.clone()));
        assert_eq!(read_lock(&p).unwrap().unwrap(), c);
    }

    #[test]
    fn heartbeat_and_is_owner_false_when_missing_or_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let me = info(1, t0());
        assert!(!heartbeat(&p, &me, t0()).unwrap());
        assert!(!is_owner(&p, &me).unwrap());
        fs::write(&p, b"garbage").unwrap();
        assert!(!heartbeat(&p, &me, t0()).unwrap());
        assert!(!is_owner(&p, &me).unwrap());
    }

    #[test]
    fn heartbeat_uses_per_owner_tmp_name() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let me = info(7, t0());
        assert_eq!(tmp_path(&p, &me).file_name().unwrap(), "edit.lock.PC1.7.tmp");
    }

    #[test]
    fn acquire_when_free() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let me = info(1, t0());
        assert_eq!(try_acquire(&p, &me, t0()).unwrap(), Acquire::Acquired);
        assert_eq!(read_lock(&p).unwrap().unwrap(), me);
    }

    #[test]
    fn own_lock_reacquired_with_fresh_heartbeat() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let me = info(1, t0());
        try_acquire(&p, &me, t0()).unwrap();
        let later = t0() + Duration::seconds(50);
        assert_eq!(try_acquire(&p, &me, later).unwrap(), Acquire::Acquired);
        assert_eq!(read_lock(&p).unwrap().unwrap().heartbeat_at, later);
    }

    #[test]
    fn second_owner_gets_held_by() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        let b = info(2, t0());
        assert_eq!(try_acquire(&p, &b, t0()).unwrap(), Acquire::HeldBy(a));
    }

    #[test]
    fn stale_lock_taken_over() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        age_file(&p, 121);
        let now = t0() + Duration::seconds(121);
        let b = info(2, now);
        assert_eq!(try_acquire(&p, &b, now).unwrap(), Acquire::TookOverStale(a));
        assert!(is_owner(&p, &b).unwrap());
    }

    #[test]
    fn fresh_lock_not_taken() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        let now = t0() + Duration::seconds(119);
        let b = info(2, now);
        assert_eq!(try_acquire(&p, &b, now).unwrap(), Acquire::HeldBy(a));
    }

    #[test]
    fn future_heartbeat_is_not_stale() {
        let a = info(1, t0() + Duration::minutes(5));
        assert!(!is_stale(&a, t0()));
    }

    #[test]
    fn corrupt_lock_taken_over() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        fs::write(&p, b"{ not json").unwrap();
        age_file(&p, 121);
        assert_eq!(read_lock(&p).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let me = info(2, t0());
        match try_acquire(&p, &me, t0()).unwrap() {
            Acquire::TookOverStale(prev) => assert_eq!(prev.user, "unknown"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(is_owner(&p, &me).unwrap());
    }

    #[test]
    fn heartbeat_updates_and_rejects_non_owner() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        let later = t0() + Duration::seconds(30);
        assert!(heartbeat(&p, &a, later).unwrap());
        assert_eq!(read_lock(&p).unwrap().unwrap().heartbeat_at, later);
        assert!(!d.path().join("edit.lock.tmp").exists());
        let b = info(2, later);
        assert!(!heartbeat(&p, &b, later + Duration::seconds(30)).unwrap());
        assert_eq!(read_lock(&p).unwrap().unwrap().heartbeat_at, later);
    }

    #[test]
    fn release_only_by_owner() {
        let d = tempfile::tempdir().unwrap();
        let p = lock_path(&d);
        let a = info(1, t0());
        try_acquire(&p, &a, t0()).unwrap();
        release(&p, &info(2, t0())).unwrap();
        assert!(p.exists());
        release(&p, &a).unwrap();
        assert!(!p.exists());
    }
}
