//! Edit/view session over a shared data folder: one editor at a time via
//! `edit.lock`, debounced autosave, inactivity logout, and viewers that reload
//! newer snapshots. A save never overwrites data when the lock is not ours.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::lock::{self, Acquire, LockInfo, HEARTBEAT_SECS};
use crate::model::{AuditEntry, Data, Ts};
use crate::ops::Actor;
use crate::snapshot::{self, DataDir, Loaded};

pub const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(3);
pub const POLL_EVERY: Duration = Duration::from_secs(10);
pub const WARNING_BEFORE: Duration = Duration::from_secs(60);
/// After a failed save, wait this long before retrying.
const SAVE_RETRY: Duration = Duration::from_secs(10);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(HEARTBEAT_SECS as u64);
const NEWER_SCHEMA: &str = "Data was saved by a newer version of the app";

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Editing,
    /// `holder: None` means editing is available.
    Viewing { holder: Option<LockInfo> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveReason {
    Inactivity,
    Manual,
    Conflict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    InactivityWarning { secs_left: u64 },
    LeftEditMode(LeaveReason),
    Saved,
    SaveFailed(String),
    Reloaded,
    EditingAvailable,
    Conflict(PathBuf),
    TookOverStale(LockInfo),
}

pub struct Session {
    dir: DataDir,
    /// Our lock identity (user, machine, pid).
    me: LockInfo,
    data: Data,
    /// `data` is `Data::new_empty` and was never loaded or edited: any loaded
    /// snapshot replaces it, whatever its `saved_at`.
    placeholder: bool,
    mode: Mode,
    revision: u64,
    dirty: bool,
    saved_this_session: bool,
    changed_at: Option<Ts>,
    failed_at: Option<Ts>,
    save_error: Option<String>,
    load_error: Option<String>,
    newer_schema: bool,
    corrupt: Vec<PathBuf>,
    last_activity: Instant,
    last_heartbeat: Instant,
    last_poll: Instant,
    /// `EditingAvailable` was already emitted for the current free-lock spell.
    available_announced: bool,
    /// Events produced outside `tick` (or before it), returned by the next `tick`.
    pending: Vec<SessionEvent>,
}

/// True when at least `d` has passed from `since` to `now` (or the clock went back).
fn elapsed(now: Ts, since: Ts, d: Duration) -> bool {
    (now - since).to_std().map_or(true, |x| x >= d)
}

fn unknown_holder(now: Ts) -> LockInfo {
    LockInfo { user: "unknown".into(), machine: "unknown".into(), pid: 0, acquired_at: now, heartbeat_at: now }
}

/// Writes `"{file stem} - conflict {HHmmss}.json"` in the conflicts folder.
fn write_conflict(dir: &DataDir, data: &Data, file_name: &str, now: Ts) -> io::Result<PathBuf> {
    let folder = dir.conflicts();
    fs::create_dir_all(&folder)?;
    let stem = Path::new(file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = folder.join(format!("{stem} - conflict {}.json", now.format("%H%M%S")));
    fs::write(&path, serde_json::to_vec_pretty(data).map_err(io::Error::other)?)?;
    Ok(path)
}

impl Session {
    /// Loads the newest snapshot and takes the lock if it is free (or stale).
    /// If the data cannot be read, starts Viewing with `load_error` set and
    /// never takes the lock; polls keep retrying.
    pub fn open(dir: DataDir, user: &str, machine: &str, pid: u32, now: Ts, inst: Instant) -> io::Result<Session> {
        dir.ensure()?;
        let me = LockInfo { user: user.into(), machine: machine.into(), pid, acquired_at: now, heartbeat_at: now };
        let mut s = Session {
            dir,
            me,
            data: Data::new_empty(user, now),
            placeholder: true,
            mode: Mode::Viewing { holder: None },
            revision: 0,
            dirty: false,
            saved_this_session: false,
            changed_at: None,
            failed_at: None,
            save_error: None,
            load_error: None,
            newer_schema: false,
            corrupt: Vec::new(),
            last_activity: inst,
            last_heartbeat: inst,
            last_poll: inst,
            available_announced: false,
            pending: Vec::new(),
        };
        if s.load(false).is_err() {
            return Ok(s);
        }
        if s.newer_schema {
            s.mode = Mode::Viewing { holder: s.lock_holder(now).flatten() };
            return Ok(s);
        }
        match lock::try_acquire(&s.dir.lock_path(), &s.me, now) {
            Ok(Acquire::Acquired) => s.enter_editing(inst),
            Ok(Acquire::TookOverStale(prev)) => {
                s.enter_editing(inst);
                s.note_takeover(&prev, now);
                s.pending.push(SessionEvent::TookOverStale(prev));
            }
            Ok(Acquire::HeldBy(h)) => s.mode = Mode::Viewing { holder: Some(h) },
            // Unknown lock state: stay Viewing; the next poll decides.
            Err(_) => {}
        }
        Ok(s)
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn is_editing(&self) -> bool {
        self.mode == Mode::Editing
    }

    pub fn data(&self) -> &Data {
        &self.data
    }

    /// Bumped on every mutation and reload; the UI caches by it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn user(&self) -> &str {
        &self.me.user
    }

    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Set while the data folder cannot be read; the session stays Viewing.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn read_only_reason(&self) -> Option<&str> {
        self.newer_schema.then_some(NEWER_SCHEMA)
    }

    pub fn corrupt_files(&self) -> &[PathBuf] {
        &self.corrupt
    }

    pub fn inactivity_timeout(&self) -> Duration {
        Duration::from_secs(self.data.settings.inactivity_minutes.max(1) as u64 * 60)
    }

    pub fn note_activity(&mut self, inst: Instant) {
        self.last_activity = inst;
    }

    /// Runs `f` on the data when Editing; marks it dirty for autosave.
    pub fn mutate<R>(&mut self, now: Ts, f: impl FnOnce(&mut Data, &Actor) -> R) -> Option<R> {
        if !self.is_editing() {
            return None;
        }
        let actor = Actor { user: self.me.user.clone(), now };
        let r = f(&mut self.data, &actor);
        self.mark_dirty(now);
        Some(r)
    }

    /// Replaces all data (import) and saves immediately. Editing only.
    /// Returns true when the data was replaced and saved; a failed save is
    /// retried by `tick` as usual.
    pub fn replace_data(&mut self, now: Ts, data: Data) -> bool {
        if !self.is_editing() {
            return false;
        }
        self.data = data;
        self.mark_dirty(now);
        self.save_now(now).is_ok()
    }

    pub fn tick(&mut self, now: Ts, inst: Instant) -> Vec<SessionEvent> {
        if self.is_editing() {
            self.tick_editing(now, inst);
        } else if inst.saturating_duration_since(self.last_poll) >= POLL_EVERY {
            self.last_poll = inst;
            self.poll(now);
        }
        std::mem::take(&mut self.pending)
    }

    /// Takes the lock (if free or stale), then loads the newest snapshot so
    /// editing never starts on stale data.
    pub fn start_editing(&mut self, now: Ts, inst: Instant) -> io::Result<Acquire> {
        if self.is_editing() {
            return Ok(Acquire::Acquired);
        }
        if self.newer_schema {
            return Err(io::Error::other(NEWER_SCHEMA));
        }
        self.me.acquired_at = now;
        self.me.heartbeat_at = now;
        let path = self.dir.lock_path();
        let result = lock::try_acquire(&path, &self.me, now)?;
        if let Acquire::HeldBy(h) = &result {
            self.mode = Mode::Viewing { holder: Some(h.clone()) };
            self.available_announced = false;
            return Ok(result);
        }
        let refreshed = self.refresh_data(false);
        if refreshed.is_err() || self.newer_schema {
            let _ = lock::release(&path, &self.me);
            refreshed?;
            return Err(io::Error::other(NEWER_SCHEMA));
        }
        self.enter_editing(inst);
        if let Acquire::TookOverStale(prev) = &result {
            self.note_takeover(prev, now);
        }
        Ok(result)
    }

    /// Saves (if dirty or not yet saved this session), releases the lock and
    /// switches to Viewing. A failed save keeps Editing and returns the error.
    pub fn stop_editing(&mut self, now: Ts) -> io::Result<()> {
        if !self.is_editing() {
            return Ok(());
        }
        self.leave(now, LeaveReason::Manual)
    }

    /// Saves to today's named snapshot after checking we still own the lock.
    /// If someone else owns it, the data goes to the conflicts folder instead
    /// and the session switches to Viewing (returns `Err`).
    pub fn save_now(&mut self, now: Ts) -> Result<PathBuf, String> {
        if !self.is_editing() {
            return Err("Not in edit mode".into());
        }
        match lock::is_owner(&self.dir.lock_path(), &self.me) {
            Ok(true) => {}
            Ok(false) => return Err(self.conflict(now)),
            Err(e) => return Err(self.save_failed(now, format!("Could not check the edit lock: {e}"))),
        }
        self.data.saved_at = now;
        self.data.saved_by = self.me.user.clone();
        match snapshot::save_snapshot(&self.dir, &self.data, &self.file_name(now)) {
            Ok(path) => {
                self.dirty = false;
                self.placeholder = false;
                self.saved_this_session = true;
                self.changed_at = None;
                self.failed_at = None;
                self.save_error = None;
                self.pending.push(SessionEvent::Saved);
                Ok(path)
            }
            Err(e) => Err(self.save_failed(now, format!("Could not save: {e}"))),
        }
    }

    /// Editing: save (so the named file exists) and release. Viewing: nothing.
    pub fn on_exit(&mut self, now: Ts) {
        if !self.is_editing() {
            return;
        }
        if self.dirty || !self.saved_this_session {
            let _ = self.save_now(now);
        }
        if self.is_editing() {
            let _ = lock::release(&self.dir.lock_path(), &self.me);
            self.mode = Mode::Viewing { holder: None };
        }
    }

    fn tick_editing(&mut self, now: Ts, inst: Instant) {
        if inst.saturating_duration_since(self.last_heartbeat) >= HEARTBEAT_EVERY {
            // An I/O error is retried at the next interval; a lock taken over
            // meanwhile is caught by the owner check before every save.
            self.last_heartbeat = inst;
            if let Ok(false) = lock::heartbeat(&self.dir.lock_path(), &self.me, now) {
                self.lock_lost(now);
                return;
            }
        }
        if self.dirty {
            let due = match (self.failed_at, self.changed_at) {
                (Some(failed), _) => elapsed(now, failed, SAVE_RETRY),
                (None, Some(changed)) => elapsed(now, changed, AUTOSAVE_DEBOUNCE),
                (None, None) => true,
            };
            if due {
                let _ = self.save_now(now);
                if !self.is_editing() {
                    return;
                }
            }
        }
        let idle = inst.saturating_duration_since(self.last_activity);
        let timeout = self.inactivity_timeout();
        if idle >= timeout {
            let retry_wait = self.failed_at.is_some_and(|f| !elapsed(now, f, SAVE_RETRY));
            if !retry_wait {
                let _ = self.leave(now, LeaveReason::Inactivity);
            }
        } else if idle + WARNING_BEFORE >= timeout {
            let left = timeout - idle;
            let secs_left = left.as_secs() + u64::from(left.subsec_nanos() > 0);
            self.pending.push(SessionEvent::InactivityWarning { secs_left });
        }
    }

    /// Viewing poll: reload a newer snapshot, then report whether editing is available.
    fn poll(&mut self, now: Ts) {
        if self.refresh_data(false).is_err() || self.newer_schema {
            return;
        }
        let Some(holder) = self.lock_holder(now) else { return };
        let free = holder.is_none();
        self.mode = Mode::Viewing { holder };
        if free && !self.available_announced {
            self.pending.push(SessionEvent::EditingAvailable);
        }
        self.available_announced = free;
    }

    /// Loads the newest snapshot; adopts it when newer than ours (or `force`,
    /// or ours is a placeholder) and returns whether it did. On error keeps
    /// the data and sets `load_error`.
    fn load(&mut self, force: bool) -> io::Result<bool> {
        let found = match snapshot::load_latest(&self.dir) {
            Ok(found) => found,
            Err(e) => {
                self.load_error = Some(format!("Could not read the data folder: {e}"));
                return Err(e);
            }
        };
        self.load_error = None;
        let Some(loaded) = found else { return Ok(false) };
        if force || self.placeholder || loaded.data.saved_at > self.data.saved_at {
            self.adopt(loaded);
            return Ok(true);
        }
        self.corrupt = loaded.corrupt;
        Ok(false)
    }

    /// `load`, announcing a replacement with `Reloaded`.
    fn refresh_data(&mut self, force: bool) -> io::Result<()> {
        if self.load(force)? {
            self.revision += 1;
            self.pending.push(SessionEvent::Reloaded);
        }
        Ok(())
    }

    fn adopt(&mut self, loaded: Loaded) {
        self.data = loaded.data;
        self.corrupt = loaded.corrupt;
        self.newer_schema = loaded.read_only_newer_schema;
        self.placeholder = false;
    }

    /// `Some(None)` = free (missing, stale, or corrupt and old); `Some(Some(h))`
    /// = held; `None` = could not tell.
    fn lock_holder(&self, now: Ts) -> Option<Option<LockInfo>> {
        let path = self.dir.lock_path();
        match lock::read_lock(&path) {
            Ok(None) => Some(None),
            Ok(Some(info)) => match lock::is_stale_at(&path, &info, now) {
                Ok(true) => Some(None),
                Ok(false) => Some(Some(info)),
                Err(_) => None,
            },
            Err(e) if e.kind() == io::ErrorKind::InvalidData => match lock::corrupt_is_stale(&path) {
                Ok(true) => Some(None),
                Ok(false) => Some(Some(unknown_holder(now))),
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    fn enter_editing(&mut self, inst: Instant) {
        self.mode = Mode::Editing;
        self.last_activity = inst;
        self.last_heartbeat = inst;
        self.dirty = false;
        self.saved_this_session = false;
        self.changed_at = None;
        self.failed_at = None;
        self.save_error = None;
        self.available_announced = false;
    }

    fn note_takeover(&mut self, prev: &LockInfo, now: Ts) {
        self.data.audit.push(AuditEntry {
            at: now,
            by: self.me.user.clone(),
            action: "lock.takeover".into(),
            detail: format!("Took over edit lock from {} on {} (stale)", prev.user, prev.machine),
            refs: BTreeMap::new(),
        });
        self.mark_dirty(now);
    }

    fn mark_dirty(&mut self, now: Ts) {
        self.dirty = true;
        self.placeholder = false;
        self.changed_at = Some(now);
        self.revision += 1;
    }

    fn file_name(&self, now: Ts) -> String {
        snapshot::snapshot_file_name(now.date_naive(), &self.me.user)
    }

    fn save_failed(&mut self, now: Ts, msg: String) -> String {
        self.failed_at = Some(now);
        self.save_error = Some(msg.clone());
        self.pending.push(SessionEvent::SaveFailed(msg.clone()));
        msg
    }

    /// Someone else owns the lock: export our data to the conflicts folder,
    /// switch to Viewing and show the real latest data. Returns the message.
    fn conflict(&mut self, now: Ts) -> String {
        let mut copy = self.data.clone();
        copy.saved_at = now;
        copy.saved_by = self.me.user.clone();
        let written = write_conflict(&self.dir, &copy, &self.file_name(now), now);
        self.mode = Mode::Viewing { holder: self.lock_holder(now).flatten() };
        self.available_announced = false;
        let msg = match written {
            Ok(path) => {
                self.dirty = false;
                self.changed_at = None;
                self.failed_at = None;
                self.save_error = None;
                let msg = format!("Another user has the edit lock; your changes were saved to {}", path.display());
                self.pending.push(SessionEvent::Conflict(path));
                self.pending.push(SessionEvent::LeftEditMode(LeaveReason::Conflict));
                let _ = self.refresh_data(true);
                return msg;
            }
            Err(e) => format!("Another user has the edit lock and your changes could not be exported: {e}"),
        };
        // Keep the unsaved data on screen; it is not reloaded over.
        self.save_error = Some(msg.clone());
        self.pending.push(SessionEvent::SaveFailed(msg.clone()));
        self.pending.push(SessionEvent::LeftEditMode(LeaveReason::Conflict));
        msg
    }

    /// The heartbeat found the lock is no longer ours: same as a conflict.
    /// With nothing unsaved there is nothing to export.
    fn lock_lost(&mut self, now: Ts) {
        if self.dirty {
            self.conflict(now);
            return;
        }
        self.mode = Mode::Viewing { holder: self.lock_holder(now).flatten() };
        self.available_announced = false;
        self.pending.push(SessionEvent::LeftEditMode(LeaveReason::Conflict));
        let _ = self.refresh_data(false);
    }

    /// Save if needed, release, Viewing. A failed save keeps Editing.
    fn leave(&mut self, now: Ts, reason: LeaveReason) -> io::Result<()> {
        if self.dirty || !self.saved_this_session {
            if let Err(msg) = self.save_now(now) {
                // A conflict has already left edit mode.
                return if self.is_editing() { Err(io::Error::other(msg)) } else { Ok(()) };
            }
        }
        let released = lock::release(&self.dir.lock_path(), &self.me);
        self.mode = Mode::Viewing { holder: None };
        self.available_announced = true;
        self.pending.push(SessionEvent::LeftEditMode(reason));
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{read_lock, LockInfo};
    use crate::snapshot::{load_latest, save_snapshot};
    use chrono::DateTime;
    use std::fs;
    use std::path::Path;

    const FILE: &str = "14 Nov 2026 - jtan.json";

    fn t0() -> Ts {
        DateTime::parse_from_rfc3339("2026-11-14T10:00:00+08:00").unwrap()
    }

    fn at(secs: i64) -> Ts {
        t0() + chrono::Duration::seconds(secs)
    }

    struct Clock {
        inst: Instant,
    }

    impl Clock {
        fn at(&self, secs: u64) -> Instant {
            self.inst + Duration::from_secs(secs)
        }
    }

    fn setup() -> (tempfile::TempDir, DataDir, Clock) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir { root: tmp.path().to_path_buf() };
        (tmp, dir, Clock { inst: Instant::now() })
    }

    fn open(dir: &DataDir, user: &str, pid: u32, clock: &Clock) -> Session {
        Session::open(dir.clone(), user, "PC1", pid, t0(), clock.at(0)).unwrap()
    }

    fn change(s: &mut Session, now: Ts) {
        s.mutate(now, |d, _| d.settings.due_soon_days = 45).unwrap();
    }

    fn write_lock(dir: &DataDir, info: &LockInfo) {
        fs::write(dir.lock_path(), serde_json::to_vec(info).unwrap()).unwrap();
    }

    fn age(path: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    fn other(user: &str, machine: &str, pid: u32, hb: Ts) -> LockInfo {
        LockInfo { user: user.into(), machine: machine.into(), pid, acquired_at: hb, heartbeat_at: hb }
    }

    fn files_in(p: &Path) -> Vec<PathBuf> {
        fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_file()).collect()
    }

    #[test]
    fn opens_editing_when_free_and_viewing_when_held() {
        let (_t, dir, clock) = setup();
        let a = open(&dir, "jtan", 1, &clock);
        assert!(a.is_editing());
        assert_eq!(a.mode(), &Mode::Editing);
        assert_eq!(a.user(), "jtan");
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap().user, "jtan");
        let b = open(&dir, "agnes", 2, &clock);
        assert!(!b.is_editing());
        match b.mode() {
            Mode::Viewing { holder: Some(h) } => assert_eq!(h.user, "jtan"),
            m => panic!("unexpected {m:?}"),
        }
    }

    #[test]
    fn two_sessions_only_one_edits() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        let mut b = open(&dir, "jtan", 2, &clock);
        assert!(a.is_editing() ^ b.is_editing());
        assert!(b.mutate(t0(), |_, _| ()).is_none());
        assert!(a.mutate(t0(), |_, _| ()).is_some());
        assert_eq!(b.start_editing(at(1), clock.at(1)).unwrap(), Acquire::HeldBy(read_lock(&dir.lock_path()).unwrap().unwrap()));
        assert!(!b.is_editing());
    }

    #[test]
    fn autosave_after_debounce_writes_named_file() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let rev = s.revision();
        change(&mut s, t0());
        assert!(s.revision() > rev);
        let file = dir.snapshots().join(FILE);
        assert!(!s.tick(at(2), clock.at(2)).contains(&SessionEvent::Saved));
        assert!(!file.exists());
        assert!(s.tick(at(3), clock.at(3)).contains(&SessionEvent::Saved));
        assert!(file.exists());
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.settings.due_soon_days, 45);
        assert_eq!(loaded.data.saved_by, "jtan");
        assert_eq!(loaded.data.saved_at, at(3));
        // Nothing dirty: no further saves.
        assert!(s.tick(at(10), clock.at(10)).is_empty());
    }

    #[test]
    fn inactivity_warns_then_releases() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        assert_eq!(s.inactivity_timeout(), Duration::from_secs(600));
        change(&mut s, t0());
        let ev = s.tick(at(539), clock.at(539));
        assert!(!ev.iter().any(|e| matches!(e, SessionEvent::InactivityWarning { .. })));
        let ev = s.tick(at(540), clock.at(540));
        assert!(ev.contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        let ev = s.tick(at(570), clock.at(570));
        assert!(ev.contains(&SessionEvent::InactivityWarning { secs_left: 30 }));
        assert!(s.is_editing());
        change(&mut s, at(580));
        let ev = s.tick(at(600), clock.at(600));
        assert!(ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Inactivity)));
        assert_eq!(s.mode(), &Mode::Viewing { holder: None });
        assert!(!dir.lock_path().exists());
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.settings.due_soon_days, 45);
        assert_eq!(loaded.data.saved_at, at(600));
    }

    #[test]
    fn activity_resets_timer() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        assert!(s.tick(at(540), clock.at(540)).contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        s.note_activity(clock.at(540));
        let ev = s.tick(at(600), clock.at(600));
        assert!(!ev.iter().any(|e| matches!(e, SessionEvent::InactivityWarning { .. } | SessionEvent::LeftEditMode(_))));
        assert!(s.is_editing());
        assert!(s.tick(at(1080), clock.at(1080)).contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        assert!(s.tick(at(1140), clock.at(1140)).contains(&SessionEvent::LeftEditMode(LeaveReason::Inactivity)));
    }

    #[test]
    fn viewer_reloads_newer_snapshot_and_sees_available() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        let mut b = open(&dir, "agnes", 2, &clock);
        assert!(!b.is_editing());
        let rev = b.revision();
        change(&mut a, at(1));
        a.save_now(at(1)).unwrap();
        a.stop_editing(at(2)).unwrap();
        assert!(!dir.lock_path().exists());
        assert!(b.tick(at(5), clock.at(5)).is_empty());
        let ev = b.tick(at(10), clock.at(10));
        assert!(ev.contains(&SessionEvent::Reloaded), "{ev:?}");
        assert!(ev.contains(&SessionEvent::EditingAvailable), "{ev:?}");
        assert!(b.revision() > rev);
        assert_eq!(b.data().settings.due_soon_days, 45);
        assert_eq!(b.mode(), &Mode::Viewing { holder: None });
        assert!(!b.is_editing(), "viewers never auto-acquire");
        assert!(!dir.lock_path().exists());
        // Announced once per transition.
        assert!(b.tick(at(20), clock.at(20)).is_empty());
    }

    #[test]
    fn start_editing_from_viewing_loads_latest_first() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        let mut b = open(&dir, "agnes", 2, &clock);
        change(&mut a, at(1));
        a.stop_editing(at(2)).unwrap();
        // B has not polled yet, but must not start editing on stale data.
        assert_eq!(b.start_editing(at(3), clock.at(3)).unwrap(), Acquire::Acquired);
        assert!(b.is_editing());
        assert_eq!(b.data().settings.due_soon_days, 45);
    }

    #[test]
    fn stale_lock_takeover_on_open_emits_event() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        let prev = other("agnes", "PC2", 9, t0() - chrono::Duration::minutes(10));
        write_lock(&dir, &prev);
        age(&dir.lock_path(), 600);
        let mut s = open(&dir, "jtan", 1, &clock);
        assert!(s.is_editing());
        let ev = s.tick(t0(), clock.at(0));
        assert!(ev.contains(&SessionEvent::TookOverStale(prev)), "{ev:?}");
        let last = s.data().audit.last().unwrap();
        assert_eq!(last.action, "lock.takeover");
        assert_eq!(last.detail, "Took over edit lock from agnes on PC2 (stale)");
        assert_eq!(last.by, "jtan");
        // The audit entry is autosaved.
        assert!(s.tick(at(3), clock.at(3)).contains(&SessionEvent::Saved));
    }

    #[test]
    fn conflict_when_lock_stolen() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let saved = s.save_now(t0()).unwrap();
        let before = fs::read(&saved).unwrap();
        let thief = other("agnes", "PC2", 9, t0());
        write_lock(&dir, &thief);
        change(&mut s, t0());
        let ev = s.tick(at(3), clock.at(3));
        let conflict = ev.iter().find_map(|e| match e {
            SessionEvent::Conflict(p) => Some(p.clone()),
            _ => None,
        });
        let conflict = conflict.expect("Conflict event");
        assert!(ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)));
        assert_eq!(conflict.parent().unwrap(), dir.conflicts());
        assert_eq!(conflict.file_name().unwrap(), "14 Nov 2026 - jtan - conflict 100003.json");
        let exported: Data = serde_json::from_slice(&fs::read(&conflict).unwrap()).unwrap();
        assert_eq!(exported.settings.due_soon_days, 45);
        assert_eq!(fs::read(&saved).unwrap(), before, "snapshot unchanged");
        assert_eq!(files_in(&dir.snapshots()), vec![saved]);
        assert_eq!(s.mode(), &Mode::Viewing { holder: Some(thief.clone()) });
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap(), thief);
    }

    #[test]
    fn heartbeat_lost_leaves_edit_mode() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let thief = other("agnes", "PC2", 9, t0());
        write_lock(&dir, &thief);
        let ev = s.tick(at(30), clock.at(30));
        assert!(ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)), "{ev:?}");
        assert!(!s.is_editing());
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap(), thief);
    }

    #[test]
    fn heartbeat_refreshes_lock() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.tick(at(29), clock.at(29));
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at, t0());
        s.tick(at(30), clock.at(30));
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at, at(30));
    }

    #[test]
    fn save_failure_keeps_editing_and_retries() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        fs::remove_dir_all(dir.snapshots()).unwrap();
        fs::write(dir.snapshots(), "not a folder").unwrap();
        change(&mut s, t0());
        let ev = s.tick(at(3), clock.at(3));
        assert!(ev.iter().any(|e| matches!(e, SessionEvent::SaveFailed(_))), "{ev:?}");
        assert!(s.is_editing());
        assert!(s.save_error().is_some());
        // No retry before 10 s.
        assert!(!s.tick(at(8), clock.at(8)).iter().any(|e| matches!(e, SessionEvent::SaveFailed(_) | SessionEvent::Saved)));
        fs::remove_file(dir.snapshots()).unwrap();
        dir.ensure().unwrap();
        let ev = s.tick(at(13), clock.at(13));
        assert!(ev.contains(&SessionEvent::Saved), "{ev:?}");
        assert!(s.save_error().is_none());
        assert!(dir.snapshots().join(FILE).exists());
    }

    #[test]
    fn newer_schema_never_saves() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        let mut d = Data::new_empty("future", t0());
        d.schema_version = 99;
        let path = save_snapshot(&dir, &d, "f.json").unwrap();
        let before = fs::read(&path).unwrap();
        let mut s = open(&dir, "jtan", 1, &clock);
        assert!(!s.is_editing());
        assert_eq!(s.read_only_reason(), Some("Data was saved by a newer version of the app"));
        assert!(!dir.lock_path().exists());
        assert!(s.mutate(t0(), |d, _| d.settings.due_soon_days = 1).is_none());
        assert!(!s.replace_data(t0(), Data::new_empty("jtan", t0())));
        assert!(s.start_editing(at(1), clock.at(1)).is_err());
        assert!(!dir.lock_path().exists());
        let ev = s.tick(at(10), clock.at(10));
        assert!(!ev.contains(&SessionEvent::EditingAvailable));
        assert!(s.save_now(at(11)).is_err());
        s.on_exit(at(12));
        assert_eq!(files_in(&dir.snapshots()), vec![path.clone()]);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn exit_saves_and_releases() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.on_exit(at(1));
        assert!(dir.snapshots().join(FILE).exists());
        assert!(!dir.lock_path().exists());
        // A viewer's exit does nothing.
        let mut a = open(&dir, "agnes", 2, &clock);
        let mut b = open(&dir, "jtan", 3, &clock);
        assert!(a.is_editing() && !b.is_editing());
        b.on_exit(at(2));
        assert!(dir.lock_path().exists());
        a.on_exit(at(3));
        assert!(!dir.lock_path().exists());
    }

    #[test]
    fn stop_editing_saves_releases_and_views() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.stop_editing(at(1)).unwrap();
        assert_eq!(s.mode(), &Mode::Viewing { holder: None });
        assert!(dir.snapshots().join(FILE).exists());
        assert!(!dir.lock_path().exists());
        assert!(s.tick(at(2), clock.at(2)).contains(&SessionEvent::LeftEditMode(LeaveReason::Manual)));
        assert!(s.mutate(at(3), |_, _| ()).is_none());
    }

    #[test]
    fn replace_data_saves_immediately() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let rev = s.revision();
        let mut imported = Data::new_empty("jtan", t0());
        imported.settings.due_soon_days = 7;
        assert!(s.replace_data(at(1), imported));
        assert!(s.revision() > rev);
        assert_eq!(load_latest(&dir).unwrap().unwrap().data.settings.due_soon_days, 7);
    }

    #[test]
    fn load_error_on_open_views_and_recovers() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        save_snapshot(&dir, &Data::new_empty("agnes", t0() - chrono::Duration::days(1)), "old.json").unwrap();
        let blocker = dir.snapshots().join("x.json");
        fs::create_dir(&blocker).unwrap();
        let mut s = open(&dir, "jtan", 1, &clock);
        assert!(!s.is_editing());
        assert!(s.load_error().is_some());
        assert!(!dir.lock_path().exists(), "no lock without data");
        assert!(s.start_editing(at(1), clock.at(1)).is_err());
        assert!(!dir.lock_path().exists());
        let ev = s.tick(at(10), clock.at(10));
        assert!(!ev.contains(&SessionEvent::EditingAvailable));
        assert!(s.load_error().is_some());
        fs::remove_dir(&blocker).unwrap();
        let ev = s.tick(at(20), clock.at(20));
        assert!(s.load_error().is_none());
        assert!(ev.contains(&SessionEvent::EditingAvailable), "{ev:?}");
        assert!(ev.contains(&SessionEvent::Reloaded), "{ev:?}");
        assert_eq!(s.data().saved_by, "agnes");
        assert!(!s.is_editing() && !dir.lock_path().exists());
    }

    #[test]
    fn viewing_poll_error_keeps_data() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        change(&mut a, t0());
        a.save_now(t0()).unwrap();
        let mut b = open(&dir, "agnes", 2, &clock);
        assert_eq!(b.data().settings.due_soon_days, 45);
        fs::create_dir(dir.snapshots().join("x.json")).unwrap();
        b.tick(at(10), clock.at(10));
        assert!(b.load_error().is_some());
        assert_eq!(b.data().settings.due_soon_days, 45);
    }

    #[test]
    fn corrupt_files_reported() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        let bad = dir.snapshots().join("bad.json");
        fs::write(&bad, "{").unwrap();
        save_snapshot(&dir, &Data::new_empty("agnes", t0()), "ok.json").unwrap();
        let s = open(&dir, "jtan", 1, &clock);
        assert_eq!(s.corrupt_files(), &[bad]);
    }
}
