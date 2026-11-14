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

fn read_or_placeholder(path: &Path, now: Ts) -> io::Result<Option<LockInfo>> {
    match read_lock(path) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(Some(placeholder(now))),
        other => other,
    }
}

pub fn try_acquire(path: &Path, me: &LockInfo, now: Ts) -> io::Result<Acquire> {
    if create_new(path, me)? {
        return Ok(Acquire::Acquired);
    }
    let (existing, corrupt) = match read_lock(path) {
        Ok(Some(i)) => (i, false),
        Ok(None) => {
            // Vanished between create and read: one more try.
            return if create_new(path, me)? {
                Ok(Acquire::Acquired)
            } else {
                held_by_current(path, now)
            };
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidData => (placeholder(now), true),
        Err(e) => return Err(e),
    };
    if !corrupt {
        if same_owner(&existing, me) {
            heartbeat(path, me, now)?;
            return Ok(Acquire::Acquired);
        }
        if !is_stale(&existing, now) {
            return Ok(Acquire::HeldBy(existing));
        }
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if create_new(path, me)? {
        Ok(Acquire::TookOverStale(existing))
    } else {
        held_by_current(path, now)
    }
}

fn held_by_current(path: &Path, now: Ts) -> io::Result<Acquire> {
    Ok(Acquire::HeldBy(
        read_or_placeholder(path, now)?.unwrap_or_else(|| placeholder(now)),
    ))
}

/// Refreshes `heartbeat_at` via tmp + rename. `false` if we are not the owner.
pub fn heartbeat(path: &Path, me: &LockInfo, now: Ts) -> io::Result<bool> {
    let mut cur = match read_lock(path) {
        Ok(Some(c)) if same_owner(&c, me) => c,
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(false),
        Err(e) => return Err(e),
    };
    cur.heartbeat_at = now;
    let tmp = path.with_file_name("edit.lock.tmp");
    fs::write(&tmp, to_json(&cur)?)?;
    fs::rename(&tmp, path)?;
    Ok(true)
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
