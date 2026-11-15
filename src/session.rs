//! Edit/view session over a shared data folder: one editor at a time via
//! `edit.lock`, debounced autosave, inactivity logout, and viewers that reload
//! newer snapshots. A save never overwrites data when the lock is not ours.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::dates::now_ts;
use crate::lock::{self, Acquire, LockInfo, HEARTBEAT_SECS};
use crate::model::{AuditEntry, Data, Ts};
use crate::ops::Actor;
use crate::snapshot::{self, DataDir, Loaded, Newest, ScanCache};

pub const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(3);
pub const POLL_EVERY: Duration = Duration::from_secs(10);
pub const WARNING_BEFORE: Duration = Duration::from_secs(60);
/// Save at the latest this long after the data first became dirty, even if
/// changes keep resetting the debounce.
const AUTOSAVE_MAX_WAIT: Duration = Duration::from_secs(30);
/// After a failed save, wait this long before retrying.
const SAVE_RETRY: Duration = Duration::from_secs(10);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(HEARTBEAT_SECS as u64);
const NEWER_SCHEMA: &str = "Data was saved by a newer version of the app";

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Editing,
    /// `holder: None` means editing is available.
    Viewing {
        holder: Option<LockInfo>,
    },
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
    /// `saved_at` of the snapshot our data builds on (loaded or last saved).
    /// Every save is stamped later than it, whatever this PC's clock says.
    /// `None` until anything was loaded or saved.
    base_saved_at: Option<Ts>,
    mode: Mode,
    revision: u64,
    dirty: bool,
    /// When the data last went from clean to dirty (bounds the debounce).
    dirty_since: Option<Ts>,
    /// Unsaved edits that could not be exported after losing the lock. While
    /// set they are never replaced by a reload; export is retried every poll.
    unrescued: bool,
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
    /// Where `abandon` writes unsaved data (`%LOCALAPPDATA%\CompetencyTracker\rescue`).
    rescue_dir: PathBuf,
    /// Remembers the snapshot files already read, so polls only read changes.
    scan: ScanCache,
    /// The newest snapshot as last seen (loaded, saved by us, or judged not
    /// newer than ours); a poll that finds it unchanged parses nothing.
    seen: Option<Newest>,
    /// Interval of the background heartbeat thread; `None` = beat from `tick`.
    heartbeat_every: Option<Duration>,
    /// Running while Editing (when `heartbeat_every` is set and it could start).
    heartbeat: Option<HeartbeatThread>,
}

/// State shared between a [`Session`] and its heartbeat thread.
struct HeartbeatShared {
    /// Cleared (under `io`) when editing ends; the thread then stops.
    editing: AtomicBool,
    /// Set by the thread when the lock turned out to belong to someone else.
    lost: AtomicBool,
    /// Held around each heartbeat and around the session's own lock-file
    /// checks, so a late heartbeat can never recreate a released lock.
    io: Mutex<()>,
}

impl HeartbeatShared {
    fn guard(&self) -> MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Rewrites the lock's heartbeat every interval while Editing, independent of
/// `tick`, which stops while the window is minimised or a blocking file dialog
/// is open. Dropping it stops the thread.
struct HeartbeatThread {
    shared: Arc<HeartbeatShared>,
    /// Dropping the sender wakes the sleeping thread so it exits at once.
    _wake: mpsc::Sender<()>,
}

impl HeartbeatThread {
    fn spawn(path: PathBuf, me: LockInfo, every: Duration) -> Option<HeartbeatThread> {
        let shared = Arc::new(HeartbeatShared {
            editing: AtomicBool::new(true),
            lost: AtomicBool::new(false),
            io: Mutex::new(()),
        });
        let (wake, sleep) = mpsc::channel::<()>();
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lock-heartbeat".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = sleep.recv_timeout(every) {
                    let _io = theirs.guard();
                    if !theirs.editing.load(Ordering::SeqCst) {
                        break;
                    }
                    // An I/O error is retried at the next interval; a lock taken
                    // over meanwhile is also caught by the owner check before every save.
                    if let Ok(false) = lock::heartbeat(&path, &me, now_ts()) {
                        theirs.lost.store(true, Ordering::SeqCst);
                        theirs.editing.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            })
            .ok()?;
        Some(HeartbeatThread {
            shared,
            _wake: wake,
        })
    }

    fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::SeqCst)
    }
}

impl Drop for HeartbeatThread {
    fn drop(&mut self) {
        // Waits for a heartbeat in progress, so none follows the release.
        let _io = self.shared.guard();
        self.shared.editing.store(false, Ordering::SeqCst);
    }
}

fn default_rescue_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("CompetencyTracker")
        .join("rescue")
}

/// True when at least `d` has passed from `since` to `now` (or the clock went back).
fn elapsed(now: Ts, since: Ts, d: Duration) -> bool {
    (now - since).to_std().map_or(true, |x| x >= d)
}

fn unknown_holder(now: Ts) -> LockInfo {
    LockInfo {
        user: "unknown".into(),
        machine: "unknown".into(),
        pid: 0,
        acquired_at: now,
        heartbeat_at: now,
    }
}

/// Writes `"{file stem} - {label} {HHmmss}.json"` in `folder` (created if needed).
fn write_copy(
    folder: &Path,
    data: &Data,
    file_name: &str,
    label: &str,
    now: Ts,
) -> io::Result<PathBuf> {
    fs::create_dir_all(folder)?;
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = folder.join(format!("{stem} - {label} {}.json", now.format("%H%M%S")));
    fs::write(
        &path,
        serde_json::to_vec_pretty(data).map_err(io::Error::other)?,
    )?;
    Ok(path)
}

impl Session {
    /// Loads the newest snapshot and takes the lock if it is free (or stale).
    /// If the data cannot be read, starts Viewing with `load_error` set and
    /// never takes the lock; polls keep retrying.
    pub fn open(
        dir: DataDir,
        user: &str,
        machine: &str,
        pid: u32,
        now: Ts,
        inst: Instant,
    ) -> io::Result<Session> {
        Self::open_with(
            dir,
            user,
            machine,
            pid,
            now,
            inst,
            Some(HEARTBEAT_EVERY),
            || {},
        )
    }

    /// `open` with test hooks: the heartbeat thread's interval (`None` = beat
    /// from `tick` instead) and a hook that runs between the first load and
    /// the lock attempt.
    #[allow(clippy::too_many_arguments)]
    fn open_with(
        dir: DataDir,
        user: &str,
        machine: &str,
        pid: u32,
        now: Ts,
        inst: Instant,
        heartbeat_every: Option<Duration>,
        before_acquire: impl FnOnce(),
    ) -> io::Result<Session> {
        dir.ensure()?;
        let me = LockInfo {
            user: user.into(),
            machine: machine.into(),
            pid,
            acquired_at: now,
            heartbeat_at: now,
        };
        let mut s = Session {
            dir,
            me,
            data: Data::new_empty(user, now),
            placeholder: true,
            base_saved_at: None,
            mode: Mode::Viewing { holder: None },
            revision: 0,
            dirty: false,
            dirty_since: None,
            unrescued: false,
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
            rescue_dir: default_rescue_dir(),
            scan: ScanCache::default(),
            seen: None,
            heartbeat_every,
            heartbeat: None,
        };
        if s.load(false).is_err() {
            return Ok(s);
        }
        if s.newer_schema {
            s.mode = Mode::Viewing {
                holder: s.lock_holder(now).flatten(),
            };
            return Ok(s);
        }
        before_acquire();
        let acquired = match lock::try_acquire(&s.dir.lock_path(), &s.me, now) {
            Ok(Acquire::HeldBy(h)) => {
                s.mode = Mode::Viewing { holder: Some(h) };
                return Ok(s);
            }
            Ok(a) => a,
            // Unknown lock state: stay Viewing; the next poll decides.
            Err(_) => return Ok(s),
        };
        if s.after_acquire(false).is_err() {
            return Ok(s);
        }
        s.enter_editing(inst);
        if let Acquire::TookOverStale(prev) = acquired {
            s.note_takeover(&prev, now);
            s.pending.push(SessionEvent::TookOverStale(prev));
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

    /// The shared data folder this session reads and saves.
    pub fn data_dir(&self) -> &DataDir {
        &self.dir
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
        let actor = Actor {
            user: self.me.user.clone(),
            now,
        };
        let r = f(&mut self.data, &actor);
        self.mark_dirty(now);
        Some(r)
    }

    /// Replaces all data (import) and saves immediately. Editing only.
    /// `Ok` once the data is replaced: a failed save shows in `save_error`
    /// and is retried by `tick` as usual. `Err` when nothing was replaced
    /// (not editing, or the backup failed), or when the save found the lock
    /// taken over (the data then went to the conflicts folder).
    pub fn replace_data(&mut self, now: Ts, data: Data) -> Result<(), String> {
        if !self.is_editing() {
            return Err("Editing ended, so nothing was imported.".into());
        }
        // The day's snapshot name is per user, so a same-day import would
        // overwrite it: keep the current data in the archive first.
        if !self.data.staff.is_empty() || !self.data.records.is_empty() {
            let name = self.file_name(now);
            if let Err(e) = write_copy(&self.dir.archive(), &self.data, &name, "pre-import", now) {
                return Err(format!(
                    "Couldn't back up the current data, so nothing was imported: {e}"
                ));
            }
        }
        self.data = data;
        self.mark_dirty(now);
        match self.save_now(now) {
            Err(msg) if !self.is_editing() => Err(msg),
            _ => Ok(()),
        }
    }

    pub fn tick(&mut self, now: Ts, inst: Instant) -> Vec<SessionEvent> {
        if self.is_editing() {
            self.tick_editing(now, inst);
            if !self.is_editing() {
                self.last_poll = inst;
            }
        } else if inst.saturating_duration_since(self.last_poll) >= POLL_EVERY {
            self.last_poll = inst;
            self.poll(now);
        }
        std::mem::take(&mut self.pending)
    }

    /// Events produced since the last `tick` (e.g. by `on_exit`), without ticking.
    pub fn take_events(&mut self) -> Vec<SessionEvent> {
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
        if self.unrescued {
            // Never start editing over edits that exist only in memory.
            self.export_unsaved(now).map_err(io::Error::other)?;
        }
        self.me.acquired_at = now;
        self.me.heartbeat_at = now;
        let result = lock::try_acquire(&self.dir.lock_path(), &self.me, now)?;
        if let Acquire::HeldBy(h) = &result {
            self.mode = Mode::Viewing {
                holder: Some(h.clone()),
            };
            self.available_announced = false;
            return Ok(result);
        }
        self.after_acquire(true)?;
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
        let io = self.lock_io();
        let guard = io.as_ref().map(|s| s.guard());
        let owner = lock::is_owner(&self.dir.lock_path(), &self.me);
        drop(guard);
        match owner {
            Ok(true) => {}
            Ok(false) => return Err(self.lose_lock(now)),
            Err(e) => {
                return Err(self.save_failed(now, format!("Could not check the edit lock: {e}")))
            }
        }
        // Stamped for the write; restored if it fails. Never older than the
        // data we build on, so a PC with a slow clock still saves the newest.
        let stamp = self.base_saved_at.map_or(now, |base| {
            now.max(base + chrono::Duration::milliseconds(1))
        });
        let prev_at = std::mem::replace(&mut self.data.saved_at, stamp);
        let prev_by = std::mem::replace(&mut self.data.saved_by, self.me.user.clone());
        match snapshot::save_snapshot(&self.dir, &self.data, &self.file_name(now)) {
            Ok(path) => {
                self.base_saved_at = Some(stamp);
                // Our own file is now the newest: later polls need not parse it.
                self.seen = self.scan.note_saved(&self.dir, &path, stamp);
                self.dirty = false;
                self.dirty_since = None;
                self.placeholder = false;
                self.saved_this_session = true;
                self.changed_at = None;
                self.failed_at = None;
                self.save_error = None;
                self.pending.push(SessionEvent::Saved);
                Ok(path)
            }
            Err(e) => {
                self.data.saved_at = prev_at;
                self.data.saved_by = prev_by;
                Err(self.save_failed(now, format!("Could not save: {e}")))
            }
        }
    }

    /// Editing: save (so the named file exists) and release. Returns `Err`
    /// (keeping the lock and the edits) when unsaved edits could not be
    /// written; the UI then offers Retry or Quit (`abandon`).
    pub fn on_exit(&mut self, now: Ts) -> Result<(), String> {
        if !self.is_editing() {
            return if self.unrescued {
                self.export_unsaved(now).map(|_| ())
            } else {
                Ok(())
            };
        }
        if self.dirty || !self.saved_this_session {
            if let Err(msg) = self.save_now(now) {
                if self.unrescued || (self.dirty && self.is_editing()) {
                    return Err(msg);
                }
                if !self.is_editing() {
                    // Lost the lock; any edits were exported to conflicts.
                    return Ok(());
                }
                // Nothing unsaved: the save was only for the named file.
                self.clear_save_failure();
            }
        }
        self.stop_heartbeat();
        let _ = lock::release(&self.dir.lock_path(), &self.me);
        self.mode = Mode::Viewing { holder: None };
        Ok(())
    }

    /// Quit without a proper save: best-effort copy of the current data to the
    /// local rescue folder, then release the lock (if ours) and go Viewing.
    /// Returns the rescue file when one was written.
    pub fn abandon(&mut self, now: Ts) -> Option<PathBuf> {
        let mut path = None;
        if self.is_editing() || self.unrescued {
            let mut copy = self.data.clone();
            copy.saved_at = now;
            copy.saved_by = self.me.user.clone();
            path = write_copy(&self.rescue_dir, &copy, &self.file_name(now), "rescue", now).ok();
            if path.is_some() {
                self.unrescued = false;
            }
        }
        if self.is_editing() {
            self.stop_heartbeat();
            let _ = lock::release(&self.dir.lock_path(), &self.me);
            self.mode = Mode::Viewing { holder: None };
        }
        path
    }

    #[cfg(test)]
    fn set_rescue_dir(&mut self, dir: PathBuf) {
        self.rescue_dir = dir;
    }

    #[cfg(test)]
    fn scan_reads(&self) -> u64 {
        self.scan.reads()
    }

    #[cfg(test)]
    fn heartbeat_lost(&self) -> bool {
        self.heartbeat.as_ref().is_some_and(HeartbeatThread::lost)
    }

    /// Stops the heartbeat thread (waiting out a beat in progress). Called
    /// before the lock is released, and when it is lost.
    fn stop_heartbeat(&mut self) {
        self.heartbeat = None;
    }

    /// Holds off the heartbeat thread while the session reads the lock file.
    fn lock_io(&self) -> Option<Arc<HeartbeatShared>> {
        self.heartbeat.as_ref().map(|h| Arc::clone(&h.shared))
    }

    fn tick_editing(&mut self, now: Ts, inst: Instant) {
        if let Some(hb) = &self.heartbeat {
            // The thread is the only heartbeat; it reports a lost lock here.
            if hb.lost() {
                self.lose_lock(now);
                return;
            }
        } else if inst.saturating_duration_since(self.last_heartbeat) >= HEARTBEAT_EVERY {
            // An I/O error is retried at the next interval; a lock taken over
            // meanwhile is caught by the owner check before every save.
            self.last_heartbeat = inst;
            if let Ok(false) = lock::heartbeat(&self.dir.lock_path(), &self.me, now) {
                self.lose_lock(now);
                return;
            }
        }
        if self.dirty {
            let overdue = self
                .dirty_since
                .is_some_and(|d| elapsed(now, d, AUTOSAVE_MAX_WAIT));
            let due = match (self.failed_at, self.changed_at) {
                (Some(failed), _) => elapsed(now, failed, SAVE_RETRY),
                (None, Some(changed)) => overdue || elapsed(now, changed, AUTOSAVE_DEBOUNCE),
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
            self.pending
                .push(SessionEvent::InactivityWarning { secs_left });
        }
    }

    /// Viewing poll: reload a newer snapshot, then report whether editing is available.
    fn poll(&mut self, now: Ts) {
        if self.unrescued && self.export_unsaved(now).is_err() {
            return;
        }
        if self.refresh_data(false).is_err() || self.newer_schema {
            return;
        }
        let Some(holder) = self.lock_holder(now) else {
            return;
        };
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
        // Unchanged since we last saw it: nothing to parse.
        let have = if force || self.placeholder {
            None
        } else {
            self.seen.as_ref()
        };
        let latest = match self.scan.load_latest(&self.dir, have) {
            Ok(latest) => latest,
            Err(e) => {
                self.load_error = Some(format!("Could not read the data folder: {e}"));
                return Err(e);
            }
        };
        self.load_error = None;
        self.corrupt = latest.corrupt;
        self.seen = latest.newest;
        let Some(loaded) = latest.loaded else {
            return Ok(false);
        };
        let wanted = force || self.placeholder || loaded.data.saved_at > self.data.saved_at;
        if wanted && !self.unrescued {
            self.adopt(loaded);
            return Ok(true);
        }
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
        self.base_saved_at = Some(loaded.data.saved_at);
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
            Ok(Some(info)) => {
                match lock::is_stale_at(&path, &info, lock::server_now(&path, &self.me)) {
                    Ok(true) => Some(None),
                    Ok(false) => Some(Some(info)),
                    Err(_) => None,
                }
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                match lock::corrupt_is_stale(&path, lock::server_now(&path, &self.me)) {
                    Ok(true) => Some(None),
                    Ok(false) => Some(Some(unknown_holder(now))),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    }

    /// Right after taking the lock: load again, so edits build on the newest
    /// snapshot (a peer may have saved since our last load). On failure (or
    /// newer-schema data) release the lock and stay Viewing.
    fn after_acquire(&mut self, announce: bool) -> io::Result<()> {
        let loaded = if announce {
            self.refresh_data(false)
        } else {
            self.load(false).map(|_| ())
        };
        if loaded.is_ok() && !self.newer_schema {
            return Ok(());
        }
        let _ = lock::release(&self.dir.lock_path(), &self.me);
        self.mode = Mode::Viewing { holder: None };
        self.available_announced = false;
        loaded?;
        Err(io::Error::other(NEWER_SCHEMA))
    }

    fn enter_editing(&mut self, inst: Instant) {
        debug_assert!(!self.unrescued, "unrescued edits must be exported first");
        // If the thread cannot start, `tick` beats instead.
        self.heartbeat = self
            .heartbeat_every
            .and_then(|every| HeartbeatThread::spawn(self.dir.lock_path(), self.me.clone(), every));
        self.mode = Mode::Editing;
        self.last_activity = inst;
        self.last_heartbeat = inst;
        self.dirty = false;
        self.dirty_since = None;
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
            detail: format!(
                "Took over edit lock from {} on {} (stale)",
                prev.user, prev.machine
            ),
            refs: BTreeMap::new(),
        });
        self.mark_dirty(now);
    }

    fn mark_dirty(&mut self, now: Ts) {
        if !self.dirty {
            self.dirty_since = Some(now);
        }
        self.dirty = true;
        self.placeholder = false;
        self.changed_at = Some(now);
        self.revision += 1;
    }

    fn file_name(&self, now: Ts) -> String {
        snapshot::snapshot_file_name(now.date_naive(), &self.me.user)
    }

    fn clear_save_failure(&mut self) {
        self.failed_at = None;
        self.save_error = None;
    }

    fn save_failed(&mut self, now: Ts, msg: String) -> String {
        self.failed_at = Some(now);
        self.save_error = Some(msg.clone());
        self.pending.push(SessionEvent::SaveFailed(msg.clone()));
        msg
    }

    /// Someone else owns the lock (save-time owner check or heartbeat): switch
    /// to Viewing; unsaved edits go to the conflicts folder. Returns the message.
    fn lose_lock(&mut self, now: Ts) -> String {
        self.stop_heartbeat();
        self.mode = Mode::Viewing {
            holder: self.lock_holder(now).flatten(),
        };
        self.available_announced = false;
        let msg = if self.dirty {
            match self.export_unsaved(now) {
                Ok(path) => format!(
                    "Another user has the edit lock; your changes were saved to {}",
                    path.display()
                ),
                Err(msg) => msg,
            }
        } else {
            let _ = self.refresh_data(false);
            "Another user has the edit lock".to_owned()
        };
        self.pending
            .push(SessionEvent::LeftEditMode(LeaveReason::Conflict));
        msg
    }

    /// Writes the in-memory edits to the conflicts folder. On success emits
    /// `Conflict` and reloads the real latest data; on failure marks them
    /// `unrescued` (kept on screen, retried every poll) and emits `SaveFailed`.
    fn export_unsaved(&mut self, now: Ts) -> Result<PathBuf, String> {
        let mut copy = self.data.clone();
        copy.saved_at = now;
        copy.saved_by = self.me.user.clone();
        match write_copy(
            &self.dir.conflicts(),
            &copy,
            &self.file_name(now),
            "conflict",
            now,
        ) {
            Ok(path) => {
                self.unrescued = false;
                self.dirty = false;
                self.dirty_since = None;
                self.changed_at = None;
                self.failed_at = None;
                self.save_error = None;
                self.pending.push(SessionEvent::Conflict(path.clone()));
                let _ = self.refresh_data(true);
                Ok(path)
            }
            Err(e) => {
                self.unrescued = true;
                let msg = format!(
                    "Another user has the edit lock and your changes could not be exported: {e}"
                );
                self.save_error = Some(msg.clone());
                self.pending.push(SessionEvent::SaveFailed(msg.clone()));
                Err(msg)
            }
        }
    }

    /// Save if needed, release, Viewing. A failed save keeps Editing; a lost
    /// lock whose edits could not be exported is an error too.
    fn leave(&mut self, now: Ts, reason: LeaveReason) -> io::Result<()> {
        if self.dirty || !self.saved_this_session {
            if let Err(msg) = self.save_now(now) {
                if self.unrescued || (self.dirty && self.is_editing()) {
                    return Err(io::Error::other(msg));
                }
                if !self.is_editing() {
                    // Lost the lock; any edits were exported to conflicts.
                    return Ok(());
                }
                // Nothing unsaved: the save was only for the named file.
                self.clear_save_failure();
            }
        }
        self.stop_heartbeat();
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
        let dir = DataDir {
            root: tmp.path().to_path_buf(),
        };
        (
            tmp,
            dir,
            Clock {
                inst: Instant::now(),
            },
        )
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
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    fn other(user: &str, machine: &str, pid: u32, hb: Ts) -> LockInfo {
        LockInfo {
            user: user.into(),
            machine: machine.into(),
            pid,
            acquired_at: hb,
            heartbeat_at: hb,
        }
    }

    fn files_in(p: &Path) -> Vec<PathBuf> {
        fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .collect()
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
        assert_eq!(
            b.start_editing(at(1), clock.at(1)).unwrap(),
            Acquire::HeldBy(read_lock(&dir.lock_path()).unwrap().unwrap())
        );
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
        assert!(!ev
            .iter()
            .any(|e| matches!(e, SessionEvent::InactivityWarning { .. })));
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
        assert!(s
            .tick(at(540), clock.at(540))
            .contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        s.note_activity(clock.at(540));
        let ev = s.tick(at(600), clock.at(600));
        assert!(!ev.iter().any(|e| matches!(
            e,
            SessionEvent::InactivityWarning { .. } | SessionEvent::LeftEditMode(_)
        )));
        assert!(s.is_editing());
        assert!(s
            .tick(at(1080), clock.at(1080))
            .contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        assert!(s
            .tick(at(1140), clock.at(1140))
            .contains(&SessionEvent::LeftEditMode(LeaveReason::Inactivity)));
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
    fn viewer_polls_read_only_changed_snapshots() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        change(&mut a, at(1));
        a.save_now(at(1)).unwrap();
        let mut b = open(&dir, "agnes", 2, &clock);
        assert_eq!(b.data().settings.due_soon_days, 45);
        let reads = b.scan_reads();
        // Nothing new: polls read no files at all.
        b.tick(at(10), clock.at(10));
        b.tick(at(20), clock.at(20));
        assert_eq!(b.scan_reads(), reads);
        // A new save: read once (parsed with the cache entry), then adopted.
        a.mutate(at(21), |d, _| d.settings.due_soon_days = 50)
            .unwrap();
        a.save_now(at(21)).unwrap();
        let ev = b.tick(at(30), clock.at(30));
        assert!(ev.contains(&SessionEvent::Reloaded), "{ev:?}");
        assert_eq!(b.data().settings.due_soon_days, 50);
        assert_eq!(b.scan_reads(), reads + 1);
        b.tick(at(40), clock.at(40));
        assert_eq!(b.scan_reads(), reads + 1);
        // The editor's own save is remembered: after stopping, its polls read nothing.
        a.stop_editing(at(41)).unwrap();
        let a_reads = a.scan_reads();
        a.tick(at(60), clock.at(60));
        assert_eq!(a.scan_reads(), a_reads);
    }

    #[test]
    fn start_editing_from_viewing_loads_latest_first() {
        let (_t, dir, clock) = setup();
        let mut a = open(&dir, "jtan", 1, &clock);
        let mut b = open(&dir, "agnes", 2, &clock);
        change(&mut a, at(1));
        a.stop_editing(at(2)).unwrap();
        // B has not polled yet, but must not start editing on stale data.
        assert_eq!(
            b.start_editing(at(3), clock.at(3)).unwrap(),
            Acquire::Acquired
        );
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
        assert_eq!(
            conflict.file_name().unwrap(),
            "14 Nov 2026 - jtan - conflict 100003.json"
        );
        let exported: Data = serde_json::from_slice(&fs::read(&conflict).unwrap()).unwrap();
        assert_eq!(exported.settings.due_soon_days, 45);
        assert_eq!(fs::read(&saved).unwrap(), before, "snapshot unchanged");
        assert_eq!(files_in(&dir.snapshots()), vec![saved]);
        assert_eq!(
            s.mode(),
            &Mode::Viewing {
                holder: Some(thief.clone())
            }
        );
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap(), thief);
    }

    /// Opens with the heartbeat thread beating every `every`, or with the
    /// in-tick fallback heartbeat when `None`.
    fn open_hb(
        dir: &DataDir,
        user: &str,
        pid: u32,
        clock: &Clock,
        every: Option<Duration>,
    ) -> Session {
        Session::open_with(
            dir.clone(),
            user,
            "PC1",
            pid,
            t0(),
            clock.at(0),
            every,
            || {},
        )
        .unwrap()
    }

    /// Polls `f` for up to 5 s.
    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    #[test]
    fn heartbeat_lost_leaves_edit_mode() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, None);
        let thief = other("agnes", "PC2", 9, t0());
        write_lock(&dir, &thief);
        let ev = s.tick(at(30), clock.at(30));
        assert!(
            ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)),
            "{ev:?}"
        );
        assert!(!s.is_editing());
        assert_eq!(read_lock(&dir.lock_path()).unwrap().unwrap(), thief);
    }

    #[test]
    fn heartbeat_refreshes_lock() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, None);
        s.tick(at(29), clock.at(29));
        assert_eq!(
            read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at,
            t0()
        );
        s.tick(at(30), clock.at(30));
        assert_eq!(
            read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at,
            at(30)
        );
    }

    #[test]
    fn heartbeat_thread_keeps_lock_fresh_without_ticks() {
        let (_t, dir, clock) = setup();
        let s = open_hb(&dir, "jtan", 1, &clock, Some(Duration::from_millis(30)));
        assert!(s.is_editing());
        // No tick at all (window minimised, or a blocking file dialog open).
        assert!(
            wait_until(|| read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at != t0()),
            "heartbeat never refreshed"
        );
        let first = read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at;
        assert!(
            wait_until(|| read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at > first),
            "heartbeat refreshed only once"
        );
        assert!(s.is_editing());
    }

    #[test]
    fn heartbeat_thread_is_the_only_heartbeat() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, Some(Duration::from_secs(3600)));
        s.tick(at(30), clock.at(30));
        assert_eq!(
            read_lock(&dir.lock_path()).unwrap().unwrap().heartbeat_at,
            t0(),
            "tick does not beat while the thread runs"
        );
    }

    #[test]
    fn heartbeat_thread_lost_lock_triggers_conflict_path() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, Some(Duration::from_millis(30)));
        s.save_now(t0()).unwrap();
        change(&mut s, t0());
        let thief = other("agnes", "PC2", 9, t0());
        write_lock(&dir, &thief);
        assert!(
            wait_until(|| s.heartbeat_lost()),
            "thread never noticed the takeover"
        );
        // The very next tick (well before any save is due) runs the lock-lost path.
        let ev = s.tick(at(1), clock.at(1));
        assert!(
            ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)),
            "{ev:?}"
        );
        let path = conflict_path(&ev).expect("unsaved edits exported");
        let exported: Data = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(exported.settings.due_soon_days, 45);
        assert!(!s.is_editing());
        assert_eq!(
            read_lock(&dir.lock_path()).unwrap().unwrap(),
            thief,
            "thief's lock untouched"
        );
    }

    #[test]
    fn heartbeat_thread_stops_on_release() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, Some(Duration::from_millis(10)));
        s.stop_editing(at(1)).unwrap();
        assert!(!dir.lock_path().exists());
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !dir.lock_path().exists(),
            "a late heartbeat recreated the lock"
        );
        // Editing again starts a fresh thread.
        assert_eq!(
            s.start_editing(at(2), clock.at(2)).unwrap(),
            Acquire::Acquired
        );
        assert!(wait_until(|| read_lock(&dir.lock_path())
            .unwrap()
            .unwrap()
            .heartbeat_at
            != at(2)));
        drop(s);
        let before = fs::read(dir.lock_path()).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            fs::read(dir.lock_path()).unwrap(),
            before,
            "thread kept beating after drop"
        );
    }

    #[test]
    fn save_failure_keeps_editing_and_retries() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        fs::remove_dir_all(dir.snapshots()).unwrap();
        fs::write(dir.snapshots(), "not a folder").unwrap();
        change(&mut s, t0());
        let ev = s.tick(at(3), clock.at(3));
        assert!(
            ev.iter().any(|e| matches!(e, SessionEvent::SaveFailed(_))),
            "{ev:?}"
        );
        assert!(s.is_editing());
        assert!(s.save_error().is_some());
        // No retry before 10 s.
        assert!(!s
            .tick(at(8), clock.at(8))
            .iter()
            .any(|e| matches!(e, SessionEvent::SaveFailed(_) | SessionEvent::Saved)));
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
        assert_eq!(
            s.read_only_reason(),
            Some("Data was saved by a newer version of the app")
        );
        assert!(!dir.lock_path().exists());
        assert!(s
            .mutate(t0(), |d, _| d.settings.due_soon_days = 1)
            .is_none());
        assert!(s.replace_data(t0(), Data::new_empty("jtan", t0())).is_err());
        assert!(s.start_editing(at(1), clock.at(1)).is_err());
        assert!(!dir.lock_path().exists());
        let ev = s.tick(at(10), clock.at(10));
        assert!(!ev.contains(&SessionEvent::EditingAvailable));
        assert!(s.save_now(at(11)).is_err());
        s.on_exit(at(12)).unwrap();
        assert_eq!(files_in(&dir.snapshots()), vec![path.clone()]);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn exit_saves_and_releases() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.on_exit(at(1)).unwrap();
        assert!(dir.snapshots().join(FILE).exists());
        assert!(!dir.lock_path().exists());
        // A viewer's exit does nothing.
        let mut a = open(&dir, "agnes", 2, &clock);
        let mut b = open(&dir, "jtan", 3, &clock);
        assert!(a.is_editing() && !b.is_editing());
        b.on_exit(at(2)).unwrap();
        assert!(dir.lock_path().exists());
        a.on_exit(at(3)).unwrap();
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
        assert!(s
            .tick(at(2), clock.at(2))
            .contains(&SessionEvent::LeftEditMode(LeaveReason::Manual)));
        assert!(s.mutate(at(3), |_, _| ()).is_none());
    }

    #[test]
    fn replace_data_saves_immediately() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let rev = s.revision();
        let mut imported = Data::new_empty("jtan", t0());
        imported.settings.due_soon_days = 7;
        s.replace_data(at(1), imported).unwrap();
        assert!(s.revision() > rev);
        assert_eq!(
            load_latest(&dir)
                .unwrap()
                .unwrap()
                .data
                .settings
                .due_soon_days,
            7
        );
    }

    fn with_staff(name: &str) -> Data {
        let mut d = Data::new_empty("jtan", t0());
        d.staff.push(crate::model::Staff {
            id: format!("p-{name}"),
            name: name.into(),
            title: String::new(),
            joined: None,
            status: crate::model::StaffStatus::Active,
            assignments: Default::default(),
        });
        d
    }

    fn archived_names(dir: &DataDir) -> Vec<Vec<String>> {
        files_in(&dir.archive())
            .iter()
            .map(|p| {
                let d: Data = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
                d.staff.into_iter().map(|s| s.name).collect()
            })
            .collect()
    }

    #[test]
    fn same_day_reimport_keeps_the_first_import_in_archive() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.replace_data(at(1), with_staff("Ann")).unwrap();
        assert!(
            archived_names(&dir).is_empty(),
            "nothing to back up before the first import"
        );
        s.replace_data(at(2), with_staff("Bea")).unwrap();
        assert_eq!(archived_names(&dir), vec![vec!["Ann".to_string()]]);
        assert_eq!(s.data().staff[0].name, "Bea");
    }

    #[test]
    fn replace_data_is_ok_once_replaced_even_if_the_save_fails() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.replace_data(at(1), with_staff("Ann")).unwrap();
        // Today's snapshot cannot be written (a folder is in its place).
        let target = dir.snapshots().join(FILE);
        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        s.replace_data(at(2), with_staff("Bea"))
            .expect("replaced, so Ok; the save is retried");
        assert_eq!(s.data().staff[0].name, "Bea");
        assert!(s.save_error().is_some(), "the banner shows the failed save");
        assert!(s.is_editing());
        assert_eq!(
            archived_names(&dir),
            vec![vec!["Ann".to_string()]],
            "one backup"
        );
        // The usual retry saves it; no second import, no second backup.
        fs::remove_dir(&target).unwrap();
        let ev = s.tick(at(12), clock.at(12));
        assert!(ev.contains(&SessionEvent::Saved), "{ev:?}");
        assert!(s.save_error().is_none());
        assert_eq!(
            load_latest(&dir).unwrap().unwrap().data.staff[0].name,
            "Bea"
        );
        assert_eq!(archived_names(&dir).len(), 1);
    }

    #[test]
    fn replace_data_errs_when_the_lock_was_lost() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        write_lock(&dir, &other("agnes", "PC2", 9, t0()));
        assert!(s.replace_data(at(1), with_staff("Ann")).is_err());
        assert!(!s.is_editing());
        assert_eq!(
            files_in(&dir.conflicts()).len(),
            1,
            "the import went to the conflicts folder"
        );
    }

    #[test]
    fn failed_backup_stops_the_replace() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.replace_data(at(1), with_staff("Ann")).unwrap();
        fs::remove_dir_all(dir.archive()).ok();
        fs::write(dir.archive(), b"not a folder").unwrap();
        let err = s.replace_data(at(2), with_staff("Bea")).unwrap_err();
        assert!(
            err.starts_with("Couldn't back up the current data, so nothing was imported:"),
            "{err}"
        );
        assert_eq!(s.data().staff[0].name, "Ann", "data is untouched");
    }

    #[test]
    fn load_error_on_open_views_and_recovers() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        save_snapshot(
            &dir,
            &Data::new_empty("agnes", t0() - chrono::Duration::days(1)),
            "old.json",
        )
        .unwrap();
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

    /// Makes `path` a plain file so writes into it as a folder fail.
    fn block_dir(path: &Path) {
        fs::remove_dir_all(path).unwrap();
        fs::write(path, "not a folder").unwrap();
    }

    fn unblock_dir(path: &Path) {
        fs::remove_file(path).unwrap();
        fs::create_dir_all(path).unwrap();
    }

    fn has_save_failed(ev: &[SessionEvent]) -> bool {
        ev.iter().any(|e| matches!(e, SessionEvent::SaveFailed(_)))
    }

    fn conflict_path(ev: &[SessionEvent]) -> Option<PathBuf> {
        ev.iter().find_map(|e| match e {
            SessionEvent::Conflict(p) => Some(p.clone()),
            _ => None,
        })
    }

    #[test]
    fn exit_with_failed_save_keeps_editing_then_abandon_rescues() {
        let (tmp, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let rescue = tmp.path().join("rescue-base");
        s.set_rescue_dir(rescue.clone());
        block_dir(&dir.snapshots());
        change(&mut s, t0());
        assert!(s.on_exit(at(1)).is_err());
        assert!(s.is_editing());
        assert!(dir.lock_path().exists());
        let path = s.abandon(at(2)).expect("rescue file");
        assert_eq!(path, rescue.join("14 Nov 2026 - jtan - rescue 100002.json"));
        let rescued: Data = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(rescued.settings.due_soon_days, 45);
        assert!(!dir.lock_path().exists());
        assert!(!s.is_editing());
    }

    #[test]
    fn clean_exit_with_failed_save_still_releases_and_closes() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        block_dir(&dir.snapshots());
        // Nothing unsaved: a failed save must not block the close or offer a rescue.
        assert_eq!(s.on_exit(at(1)), Ok(()));
        assert!(!s.is_editing());
        assert!(!dir.lock_path().exists());
    }

    #[test]
    fn clean_stop_editing_with_failed_save_still_releases() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        block_dir(&dir.snapshots());
        s.stop_editing(at(1)).unwrap();
        assert_eq!(s.mode(), &Mode::Viewing { holder: None });
        assert!(!dir.lock_path().exists());
        assert!(
            s.save_error().is_none(),
            "no banner for a save nothing needed"
        );
        assert!(s
            .tick(at(2), clock.at(2))
            .contains(&SessionEvent::LeftEditMode(LeaveReason::Manual)));
    }

    #[test]
    fn failed_conflict_export_protects_unsaved_edits() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.save_now(t0()).unwrap();
        let thief = other("agnes", "PC2", 9, t0());
        write_lock(&dir, &thief);
        block_dir(&dir.conflicts());
        change(&mut s, t0());
        let ev = s.tick(at(3), clock.at(3));
        assert!(has_save_failed(&ev), "{ev:?}");
        assert!(ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)));
        assert!(!s.is_editing());
        assert_eq!(s.data().settings.due_soon_days, 45);
        // Someone else saves newer data: it must not replace our unexported edits.
        let mut newer = Data::new_empty("agnes", at(5));
        newer.settings.due_soon_days = 99;
        save_snapshot(&dir, &newer, "14 Nov 2026 - agnes.json").unwrap();
        let ev = s.tick(at(13), clock.at(13));
        assert!(!ev.contains(&SessionEvent::Reloaded), "{ev:?}");
        assert_eq!(s.data().settings.due_soon_days, 45);
        // Cannot start editing (which would discard them) or exit cleanly.
        fs::remove_file(dir.lock_path()).unwrap();
        assert!(s.start_editing(at(14), clock.at(14)).is_err());
        assert!(!s.is_editing() && !dir.lock_path().exists());
        assert!(s.on_exit(at(15)).is_err());
        // Folder back: the next poll exports, then reloads.
        unblock_dir(&dir.conflicts());
        let ev = s.tick(at(23), clock.at(23));
        let path = conflict_path(&ev).expect("Conflict event");
        let exported: Data = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(exported.settings.due_soon_days, 45);
        assert!(ev.contains(&SessionEvent::Reloaded), "{ev:?}");
        assert_eq!(s.data().settings.due_soon_days, 99);
        assert!(s.on_exit(at(24)).is_ok());
    }

    #[test]
    fn start_editing_retries_failed_export_first() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        write_lock(&dir, &other("agnes", "PC2", 9, t0()));
        block_dir(&dir.conflicts());
        change(&mut s, t0());
        s.tick(at(3), clock.at(3));
        assert!(!s.is_editing());
        fs::remove_file(dir.lock_path()).unwrap();
        unblock_dir(&dir.conflicts());
        assert_eq!(
            s.start_editing(at(4), clock.at(4)).unwrap(),
            Acquire::Acquired
        );
        assert_eq!(files_in(&dir.conflicts()).len(), 1);
        assert!(conflict_path(&s.tick(at(4), clock.at(4))).is_some());
    }

    #[test]
    fn stop_editing_errs_when_conflict_export_fails() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        write_lock(&dir, &other("agnes", "PC2", 9, t0()));
        block_dir(&dir.conflicts());
        change(&mut s, t0());
        assert!(s.stop_editing(at(1)).is_err());
        assert!(!s.is_editing());
        assert_eq!(s.data().settings.due_soon_days, 45);
    }

    #[test]
    fn lock_lost_with_unsaved_edits_and_failed_export_keeps_them() {
        let (_t, dir, clock) = setup();
        let mut s = open_hb(&dir, "jtan", 1, &clock, None);
        s.save_now(t0()).unwrap();
        write_lock(&dir, &other("agnes", "PC2", 9, t0()));
        block_dir(&dir.conflicts());
        change(&mut s, at(29));
        let ev = s.tick(at(30), clock.at(30));
        assert!(has_save_failed(&ev) && !s.is_editing(), "{ev:?}");
        let mut newer = Data::new_empty("agnes", at(31));
        newer.settings.due_soon_days = 99;
        save_snapshot(&dir, &newer, "14 Nov 2026 - agnes.json").unwrap();
        s.tick(at(40), clock.at(40));
        assert_eq!(s.data().settings.due_soon_days, 45);
    }

    #[test]
    fn clean_lock_loss_writes_no_conflict_file() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.save_now(t0()).unwrap();
        write_lock(&dir, &other("agnes", "PC2", 9, t0()));
        assert!(s.save_now(at(1)).is_err());
        assert!(!s.is_editing());
        assert!(files_in(&dir.conflicts()).is_empty());
        let ev = s.tick(at(2), clock.at(2));
        assert!(
            ev.contains(&SessionEvent::LeftEditMode(LeaveReason::Conflict)),
            "{ev:?}"
        );
        assert!(conflict_path(&ev).is_none());
    }

    #[test]
    fn open_reloads_after_acquiring() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        save_snapshot(
            &dir,
            &Data::new_empty("agnes", t0() - chrono::Duration::days(1)),
            "old.json",
        )
        .unwrap();
        let d2 = dir.clone();
        let s = Session::open_with(
            dir.clone(),
            "jtan",
            "PC1",
            1,
            t0(),
            clock.at(0),
            Some(HEARTBEAT_EVERY),
            move || {
                // A peer saves between our load and our lock attempt.
                let mut newer = Data::new_empty("agnes", t0() - chrono::Duration::hours(1));
                newer.settings.due_soon_days = 99;
                save_snapshot(&d2, &newer, "new.json").unwrap();
            },
        )
        .unwrap();
        assert!(s.is_editing());
        assert_eq!(s.data().settings.due_soon_days, 99);
    }

    #[test]
    fn open_falls_back_to_viewing_when_reload_after_acquire_fails() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        let blocker = dir.snapshots().join("x.json");
        let b2 = blocker.clone();
        let s = Session::open_with(
            dir.clone(),
            "jtan",
            "PC1",
            1,
            t0(),
            clock.at(0),
            Some(HEARTBEAT_EVERY),
            move || {
                fs::create_dir(&b2).unwrap();
            },
        )
        .unwrap();
        assert!(!s.is_editing());
        assert!(s.load_error().is_some());
        assert!(!dir.lock_path().exists());
    }

    #[test]
    fn failed_save_does_not_advance_saved_at() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        let before = s.data().saved_at;
        block_dir(&dir.snapshots());
        change(&mut s, t0());
        assert!(s.save_now(at(5)).is_err());
        assert_eq!(s.data().saved_at, before);
    }

    #[test]
    fn save_over_a_future_stamped_snapshot_still_wins() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        // Saved by a PC whose clock runs a day fast.
        let future = t0() + chrono::Duration::days(1);
        save_snapshot(
            &dir,
            &Data::new_empty("agnes", future),
            "15 Nov 2026 - agnes.json",
        )
        .unwrap();
        let mut s = open(&dir, "jtan", 1, &clock);
        assert!(s.is_editing());
        change(&mut s, at(1));
        s.save_now(at(1)).unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(
            loaded.data.saved_by, "jtan",
            "our save must be the newest snapshot"
        );
        assert_eq!(loaded.data.settings.due_soon_days, 45);
        assert_eq!(
            loaded.data.saved_at,
            future + chrono::Duration::milliseconds(1)
        );
        // The next save stays ahead of our own last stamp too.
        change(&mut s, at(2));
        s.save_now(at(2)).unwrap();
        assert_eq!(
            load_latest(&dir).unwrap().unwrap().data.saved_at,
            future + chrono::Duration::milliseconds(2)
        );
    }

    #[test]
    fn replace_data_save_stays_ahead_of_the_loaded_snapshot() {
        let (_t, dir, clock) = setup();
        dir.ensure().unwrap();
        let future = t0() + chrono::Duration::days(1);
        save_snapshot(
            &dir,
            &Data::new_empty("agnes", future),
            "15 Nov 2026 - agnes.json",
        )
        .unwrap();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.replace_data(at(1), with_staff("Ann")).unwrap();
        let loaded = load_latest(&dir).unwrap().unwrap();
        assert_eq!(loaded.data.staff[0].name, "Ann");
        assert!(loaded.data.saved_at > future);
    }

    #[test]
    fn autosave_forced_after_30s_of_continuous_changes() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        for t in (0..30).step_by(2) {
            change(&mut s, at(t));
            s.note_activity(clock.at(t as u64));
            assert!(
                !s.tick(at(t + 1), clock.at(t as u64 + 1))
                    .contains(&SessionEvent::Saved),
                "saved at {t}"
            );
        }
        change(&mut s, at(30));
        assert!(s.tick(at(30), clock.at(30)).contains(&SessionEvent::Saved));
    }

    #[test]
    fn short_inactivity_timeout_from_settings() {
        let (_t, dir, clock) = setup();
        let mut s = open(&dir, "jtan", 1, &clock);
        s.mutate(t0(), |d, _| d.settings.inactivity_minutes = 2)
            .unwrap();
        assert_eq!(s.inactivity_timeout(), Duration::from_secs(120));
        let ev = s.tick(at(59), clock.at(59));
        assert!(!ev
            .iter()
            .any(|e| matches!(e, SessionEvent::InactivityWarning { .. })));
        assert!(s
            .tick(at(60), clock.at(60))
            .contains(&SessionEvent::InactivityWarning { secs_left: 60 }));
        assert!(s
            .tick(at(120), clock.at(120))
            .contains(&SessionEvent::LeftEditMode(LeaveReason::Inactivity)));
        assert!(!dir.lock_path().exists());
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
